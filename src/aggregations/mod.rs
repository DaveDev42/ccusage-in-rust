//! Shared loading + per-event cost calculation, plus the four report shapes.

pub(crate) mod blocks;
pub(crate) mod daily;
pub(crate) mod monthly;
pub(crate) mod session;
pub(crate) mod session_by_id;
pub(crate) mod weekly;

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::discover::{
    DiscoveredFile, claude_paths, discover_jsonl_files, extract_project_from_path,
};
use crate::parse::{UsageEvent, parse_file};
use crate::pricing::{CostMode, PricingFetcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub(crate) struct LoadOptions {
    pub since: Option<String>, // YYYYMMDD
    pub until: Option<String>, // YYYYMMDD
    pub mode: CostMode,
    pub order: SortOrder,
    pub offline: bool,
    pub timezone: chrono_tz::Tz,
    /// Filter to a specific project name (compared against `extractProjectFromPath`).
    /// Mirrors upstream `--project` flag.
    pub project: Option<String>,
    /// Group daily entries by project (so each (date, project) is its own row).
    /// Mirrors upstream `groupByProject` option set when `--instances` is passed.
    pub group_by_project: bool,
    /// Override "now" for active-block projection determinism. Set via `--now <ISO8601>`
    /// or `CCUSAGE_RS_NOW`. When `None`, uses real wall-clock `Utc::now()`.
    pub now_override: Option<chrono::DateTime<chrono::Utc>>,
}

/// Container for fully-loaded events with per-event cost already resolved.
pub(crate) struct LoadedEvents {
    pub events: Vec<EventWithCost>,
}

#[derive(Debug, Clone)]
pub(crate) struct EventWithCost {
    pub event: UsageEvent,
    pub cost: f64,
    /// Project name extracted from the source file path (mirrors `extractProjectFromPath`).
    pub project: String,
}

pub(crate) fn load_all_events(opts: &LoadOptions) -> Result<LoadedEvents> {
    let bases = claude_paths()?;
    let files = discover_jsonl_files(&bases);
    let files = sort_files_by_earliest_timestamp(files);
    // Upstream applies `filterByProject` BEFORE per-line read, so we mirror that.
    let files: Vec<DiscoveredFile> = match &opts.project {
        Some(target) => files
            .into_iter()
            .filter(|f| &extract_project_from_path(&f.path) == target)
            .collect(),
        None => files,
    };

    // Pricing fetcher only needed for non-display modes.
    let fetcher = if opts.mode == CostMode::Display {
        None
    } else {
        Some(PricingFetcher::load(opts.offline)?)
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut events: Vec<UsageEvent> = Vec::new();

    for f in &files {
        let _ = parse_file(&f.path, &f.base_dir, &mut seen, &mut events);
    }

    let with_costs: Vec<EventWithCost> = events
        .into_iter()
        .map(|e| {
            let cost = compute_cost(&e, opts.mode, fetcher.as_ref());
            let project = extract_project_from_path(&e.source_file);
            EventWithCost {
                event: e,
                cost,
                project,
            }
        })
        .collect();

    Ok(LoadedEvents { events: with_costs })
}

fn compute_cost(event: &UsageEvent, mode: CostMode, fetcher: Option<&PricingFetcher>) -> f64 {
    match mode {
        CostMode::Display => event.cost_usd.unwrap_or(0.0),
        CostMode::Calculate => match (&event.model, fetcher) {
            (Some(model), Some(f)) => f.calculate_cost(
                model,
                event.input_tokens,
                event.output_tokens,
                event.cache_creation_input_tokens,
                event.cache_read_input_tokens,
                event.speed_fast,
            ),
            _ => 0.0,
        },
        CostMode::Auto => {
            if let Some(c) = event.cost_usd {
                return c;
            }
            match (&event.model, fetcher) {
                (Some(model), Some(f)) => f.calculate_cost(
                    model,
                    event.input_tokens,
                    event.output_tokens,
                    event.cache_creation_input_tokens,
                    event.cache_read_input_tokens,
                    event.speed_fast,
                ),
                _ => 0.0,
            }
        }
    }
}

fn sort_files_by_earliest_timestamp(mut files: Vec<DiscoveredFile>) -> Vec<DiscoveredFile> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let mut keyed: Vec<(Option<DateTime<Utc>>, DiscoveredFile)> = files
        .drain(..)
        .map(|f| {
            let ts = earliest_timestamp(&f.path);
            (ts, f)
        })
        .collect();

    keyed.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.cmp(y),
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
    });

    return keyed.into_iter().map(|(_, f)| f).collect();

    fn earliest_timestamp(path: &std::path::Path) -> Option<DateTime<Utc>> {
        let f = File::open(path).ok()?;
        let r = BufReader::new(f);
        let mut earliest: Option<DateTime<Utc>> = None;
        for line in r.lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            #[derive(serde::Deserialize)]
            struct Tsl {
                timestamp: Option<String>,
            }
            if let Ok(parsed) = serde_json::from_str::<Tsl>(trimmed) {
                if let Some(s) = parsed.timestamp {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                        let dt_utc = dt.with_timezone(&Utc);
                        earliest = match earliest {
                            None => Some(dt_utc),
                            Some(prev) if dt_utc < prev => Some(dt_utc),
                            other => other,
                        };
                    }
                }
            }
        }
        earliest
    }
}

/// Filter a YYYY-MM-DD or YYYY-MM string by ccusage's date filter rules
/// (substring(0,10).replace('-','') as YYYYMMDD compared lexically).
pub(crate) fn date_in_range(date: &str, since: &Option<String>, until: &Option<String>) -> bool {
    let stripped: String = date.chars().filter(|c| *c != '-').take(8).collect();
    if let Some(s) = since {
        if stripped.as_str() < s.as_str() {
            return false;
        }
    }
    if let Some(u) = until {
        if stripped.as_str() > u.as_str() {
            return false;
        }
    }
    true
}
