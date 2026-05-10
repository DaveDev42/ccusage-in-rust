use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::aggregations::LoadOptions;
use crate::discover::claude_paths;
use crate::output::json::{SessionByIdEntry, SessionByIdOutput};
use crate::pricing::{CostMode, PricingFetcher};

#[derive(Debug, Deserialize)]
struct RawLine {
    timestamp: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    // Same as parse.rs: outer None = absent (ok), inner None = explicit null (rejected by valibot).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    speed: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Output for `session --id <sessionId>`. Returns `None` if no matching file exists
/// (caller emits literal `null` to match upstream `JSON.stringify(null)`).
pub(crate) fn build(session_id: &str, opts: &LoadOptions) -> Result<Option<SessionByIdOutput>> {
    let bases = claude_paths()?;
    let target_filename = format!("{session_id}.jsonl");
    let file = match find_session_file(&bases, &target_filename) {
        Some(p) => p,
        None => return Ok(None),
    };

    let fetcher = if opts.mode == CostMode::Display {
        None
    } else {
        Some(PricingFetcher::load(opts.offline)?)
    };

    let f = File::open(&file)?;
    let reader = BufReader::new(f);

    let mut entries: Vec<SessionByIdEntry> = Vec::new();
    let mut total_cost = 0.0_f64;
    let mut total_tokens: u64 = 0;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(message) = raw.message else { continue };
        let Some(usage) = message.usage else { continue };
        let Some(input) = usage.input_tokens else {
            continue;
        };
        let Some(output) = usage.output_tokens else {
            continue;
        };
        let Some(timestamp) = raw.timestamp else {
            continue;
        };

        // Mirror upstream `usageDataSchema`: speed is optional(picklist(["standard","fast"])).
        // Absent = ok. Explicit null = silently dropped. Other strings = silently dropped.
        let speed_value: Option<&str> = match &usage.speed {
            Some(Some(s)) => {
                if s != "standard" && s != "fast" {
                    continue;
                }
                Some(s.as_str())
            }
            Some(None) => continue,
            None => None,
        };
        let cache_create = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let speed_fast = speed_value == Some("fast");

        let cost = match opts.mode {
            CostMode::Display => raw.cost_usd.unwrap_or(0.0),
            CostMode::Calculate => match (&message.model, fetcher.as_ref()) {
                (Some(m), Some(f)) => {
                    f.calculate_cost(m, input, output, cache_create, cache_read, speed_fast)
                }
                _ => 0.0,
            },
            CostMode::Auto => {
                if let Some(c) = raw.cost_usd {
                    c
                } else {
                    match (&message.model, fetcher.as_ref()) {
                        (Some(m), Some(f)) => {
                            f.calculate_cost(m, input, output, cache_create, cache_read, speed_fast)
                        }
                        _ => 0.0,
                    }
                }
            }
        };
        total_cost += cost;
        total_tokens += input + output + cache_create + cache_read;

        entries.push(SessionByIdEntry {
            timestamp,
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: cache_create,
            cache_read_tokens: cache_read,
            model: message.model.unwrap_or_else(|| "unknown".to_string()),
            cost_usd: raw.cost_usd.unwrap_or(0.0),
        });
    }

    Ok(Some(SessionByIdOutput {
        session_id: session_id.to_string(),
        total_cost,
        total_tokens,
        entries,
    }))
}

/// Mirror upstream's glob `<base>/projects/**/<sessionId>.jsonl` across all bases.
/// Upstream takes `jsonlFiles[0]` — first match wins. tinyglobby returns matches sorted
/// by full relative-path string (same convention as `discover_jsonl_files`).
fn find_session_file(bases: &[PathBuf], target_filename: &str) -> Option<PathBuf> {
    for base in bases {
        let projects = base.join("projects");
        if !projects.is_dir() {
            continue;
        }
        let mut matches: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(&projects)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if p.file_name().is_some_and(|n| n == target_filename) {
                matches.push(p.to_path_buf());
            }
        }
        matches.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
        if let Some(first) = matches.into_iter().next() {
            return Some(first);
        }
    }
    None
}
