use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::timezone::parse_ts;

/// Raw line shape — only the fields we care about. Mirrors ccusage `usageDataSchema`.
#[derive(Debug, Deserialize)]
struct RawLine {
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    // Outer None = field absent (valid). Inner None = explicit null (rejected by valibot's
    // optional(picklist), so upstream silently drops the line).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    speed: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Normalized usage event — one per dedup-unique line.
#[derive(Debug, Clone)]
pub(crate) struct UsageEvent {
    pub timestamp: DateTime<Utc>,
    pub model: Option<String>,
    /// Display name used for breakdown grouping. `None` if no model field present;
    /// otherwise `model` (with `-fast` suffix when `usage.speed == "fast"`).
    pub display_model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub speed_fast: bool,
    pub cost_usd: Option<f64>,
    pub source_file: PathBuf,
    pub source_base: PathBuf,
}

pub(crate) fn parse_file(
    path: &Path,
    base_dir: &Path,
    seen_hashes: &mut HashSet<String>,
    out: &mut Vec<UsageEvent>,
) -> Result<()> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // mirror ccusage: silently skip malformed / non-usage lines
        };

        let Some(message) = raw.message else { continue };
        let Some(usage) = message.usage else { continue };
        // valibot requires input_tokens & output_tokens as numbers — both required.
        let Some(input_tokens) = usage.input_tokens else {
            continue;
        };
        let Some(output_tokens) = usage.output_tokens else {
            continue;
        };
        let Some(timestamp_str) = raw.timestamp else {
            continue;
        };
        let Ok(timestamp) = parse_ts(&timestamp_str) else {
            continue;
        };

        // Upstream: `speed: optional(picklist(["standard","fast"]))`. valibot's `optional`
        // accepts `undefined` but NOT `null`, so a line with `"speed": null` (true for all
        // synthetic-model entries) fails schema validation and is silently skipped.
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

        // Dedup: requires BOTH messageId AND requestId.
        if let (Some(mid), Some(rid)) = (message.id.as_ref(), raw.request_id.as_ref()) {
            let hash = format!("{mid}:{rid}");
            if !seen_hashes.insert(hash) {
                continue;
            }
        }

        let speed_fast = speed_value == Some("fast");
        let display_model = message.model.as_deref().map(|m| {
            if speed_fast {
                format!("{m}-fast")
            } else {
                m.to_string()
            }
        });

        out.push(UsageEvent {
            timestamp,
            model: message.model,
            display_model,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            speed_fast,
            cost_usd: raw.cost_usd,
            source_file: path.to_path_buf(),
            source_base: base_dir.to_path_buf(),
        });
    }
    Ok(())
}

/// Skip filter for synthetic models (used for breakdowns + modelsUsed).
pub(crate) fn is_synthetic(model: &str) -> bool {
    model == "<synthetic>"
}
