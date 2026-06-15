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

use crate::cache;
use crate::discover::{DiscoveredFile, claude_paths, discover_jsonl_files, extract_project_from_path};
use crate::parse::UsageEvent;
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
    let dbg_time = std::env::var_os("CCUSAGE_RS_DEBUG_TIME").is_some();
    let mut t = std::time::Instant::now();
    // STEP 1 DISCOVER (unchanged): per-base OsStr-sorted file list, FULL scope.
    let bases = claude_paths()?;
    let discovered = discover_jsonl_files(&bases);
    if dbg_time {
        eprintln!("[load] discover {:?} ({} files)", t.elapsed(), discovered.len());
        t = std::time::Instant::now();
    }

    // STEPS 2-7 in the cache layer: stat/diff/parse/upsert/order/read-back, returning
    // the parsed events in EXACT event-arrival order (file order via the same
    // two-phase ordering + within-file line_index = parse_file push order). The cache
    // ALWAYS receives the FULL discovered list — the --project filter is applied in
    // Rust at read-back (below), never by scoping the file list (which would evict
    // the cache for files outside the project).
    let (conn, is_read_only) = cache::open_db()?;
    let events: Vec<UsageEvent> = cache::sync_and_load(&conn, &discovered, is_read_only)?;
    drop(conn);
    if dbg_time {
        eprintln!("[load] sync_and_load {:?} ({} events)", t.elapsed(), events.len());
        t = std::time::Instant::now();
    }

    // Pricing fetcher only needed for non-display modes.
    let fetcher = if opts.mode == CostMode::Display {
        None
    } else {
        Some(PricingFetcher::load(opts.offline)?)
    };

    // STEP 8 PROJECT + DEDUP + COST (faithful port of the old load_all_events tail).
    // Apply --project at the FILE level (matches today's pre-parse file filter), then
    // run the shared cross-file dedup over the ordered stream, then compute cost.
    let mut seen: HashSet<String> = HashSet::new();
    let mut with_costs: Vec<EventWithCost> = Vec::new();
    for e in events {
        // FILE-level project filter (mirrors the old `filterByProject` over files).
        if let Some(target) = &opts.project {
            if &extract_project_from_path(&e.source_file) != target {
                continue;
            }
        }
        // Cross-file dedup: first-occurrence-in-global-order wins. Only lines with
        // BOTH message.id AND requestId participate (dedup_key NULL otherwise).
        if let (Some(mid), Some(rid)) = (e.msg_id.as_ref(), e.request_id.as_ref()) {
            let key = format!("{mid}:{rid}");
            if !seen.insert(key) {
                continue;
            }
        }
        let cost = compute_cost(&e, opts.mode, fetcher.as_ref());
        let project = extract_project_from_path(&e.source_file);
        with_costs.push(EventWithCost {
            event: e,
            cost,
            project,
        });
    }

    if dbg_time {
        eprintln!("[load] dedup+cost {:?} ({} kept)", t.elapsed(), with_costs.len());
    }

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

#[allow(dead_code)]
fn sort_files_by_earliest_timestamp(mut files: Vec<DiscoveredFile>) -> Vec<DiscoveredFile> {
    let mut keyed: Vec<(Option<DateTime<Utc>>, DiscoveredFile)> = files
        .drain(..)
        .map(|f| {
            let ts = earliest_timestamp(&f.path);
            (ts, f)
        })
        .collect();

    keyed.sort_by(|a, b| file_order_cmp(&a.0, &b.0));

    keyed.into_iter().map(|(_, f)| f).collect()
}

/// SSOT file-order comparator (phase-2 of the two-phase order). Reused by `cache.rs`
/// so the cached read-back reproduces the exact same global earliest-ts sort.
/// Both `Some` -> compare instants; `None` (no parseable timestamp) sorts LAST;
/// `Equal` for the both-`None` / equal-instant case (stable tie-break preserves the
/// pre-sort per-base OsStr order).
pub(crate) fn file_order_cmp(
    a: &Option<DateTime<Utc>>,
    b: &Option<DateTime<Utc>>,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
    }
}

/// All-lines earliest timestamp of a JSONL file. Reads EVERY line's `timestamp`
/// field — including lines that `parse_file` rejects (no usage, etc.) — so the file
/// sort key is identical to a pure timestamp scan. Promoted to top-level so both the
/// non-cache path and `cache.rs` call the IDENTICAL function.
pub(crate) fn earliest_timestamp(path: &std::path::Path) -> Option<DateTime<Utc>> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

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
