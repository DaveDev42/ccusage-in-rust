//! 5-hour billing block aggregator. Mirrors `_session-blocks.ts` in upstream ccusage.

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};

use crate::aggregations::{LoadOptions, SortOrder, load_all_events};
use crate::output::json::{
    BlockEntry, BlocksOutput, BurnRate as JBurnRate, Projection as JProjection, TokenCounts,
};
use crate::parse::is_synthetic;
use crate::timezone::format_date_ymd;

const BLOCK_DURATION_HOURS: i64 = 5;

#[derive(Debug, Clone)]
struct InternalEntry {
    timestamp: DateTime<Utc>,
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    model: String,
}

#[derive(Debug)]
struct InternalBlock {
    id: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    actual_end_time: Option<DateTime<Utc>>,
    is_active: bool,
    is_gap: bool,
    entries: Vec<InternalEntry>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation: u64,
    cache_read: u64,
    cost_usd: f64,
    models: Vec<String>,
}

fn floor_to_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(ts.year(), ts.month(), ts.day(), ts.hour(), 0, 0)
        .single()
        .unwrap_or(ts)
}

pub(crate) fn build(opts: &LoadOptions) -> Result<BlocksOutput> {
    let loaded = load_all_events(opts)?;
    let now = Utc::now();
    let session_dur = Duration::hours(BLOCK_DURATION_HOURS);

    let mut entries: Vec<InternalEntry> = loaded
        .events
        .iter()
        .map(|ev| InternalEntry {
            timestamp: ev.event.timestamp,
            input: ev.event.input_tokens,
            output: ev.event.output_tokens,
            cache_create: ev.event.cache_creation_input_tokens,
            cache_read: ev.event.cache_read_input_tokens,
            cost: ev.cost,
            model: ev
                .event
                .display_model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        })
        .collect();
    entries.sort_by_key(|e| e.timestamp);

    let mut blocks: Vec<InternalBlock> = Vec::new();
    let mut current_block_start: Option<DateTime<Utc>> = None;
    let mut current_block: Vec<InternalEntry> = Vec::new();

    for entry in entries.into_iter() {
        let entry_time = entry.timestamp;
        match current_block_start {
            None => {
                current_block_start = Some(floor_to_hour(entry_time));
                current_block.push(entry);
            }
            Some(start) => {
                let time_since_start = entry_time - start;
                let last_entry_time = current_block
                    .last()
                    .map(|e| e.timestamp)
                    .expect("non-empty block");
                let time_since_last = entry_time - last_entry_time;

                if time_since_start > session_dur || time_since_last > session_dur {
                    blocks.push(create_block(start, &current_block, now, session_dur));
                    if time_since_last > session_dur {
                        if let Some(gap) =
                            create_gap_block(last_entry_time, entry_time, session_dur)
                        {
                            blocks.push(gap);
                        }
                    }
                    current_block_start = Some(floor_to_hour(entry_time));
                    current_block.clear();
                    current_block.push(entry);
                } else {
                    current_block.push(entry);
                }
            }
        }
    }

    if let Some(start) = current_block_start {
        if !current_block.is_empty() {
            blocks.push(create_block(start, &current_block, now, session_dur));
        }
    }

    // Date filter on block start time, formatted as YYYY-MM-DD in user tz then YYYYMMDD compared.
    if opts.since.is_some() || opts.until.is_some() {
        blocks.retain(|b| {
            let date_str = format_date_ymd(b.start_time, opts.timezone);
            crate::aggregations::date_in_range(&date_str, &opts.since, &opts.until)
        });
    }

    blocks.sort_by_key(|b| b.start_time);
    if opts.order == SortOrder::Desc {
        blocks.reverse();
    }

    let json_blocks: Vec<BlockEntry> = blocks
        .iter()
        .map(|b| {
            let burn = if b.is_active {
                calculate_burn_rate(b)
            } else {
                None
            };
            let projection = if b.is_active {
                project_usage(b, now)
            } else {
                None
            };
            BlockEntry {
                id: b.id.clone(),
                start_time: iso_string(b.start_time),
                end_time: iso_string(b.end_time),
                actual_end_time: b.actual_end_time.map(iso_string),
                is_active: b.is_active,
                is_gap: b.is_gap,
                entries: b.entries.len(),
                token_counts: TokenCounts {
                    input_tokens: b.input_tokens,
                    output_tokens: b.output_tokens,
                    cache_creation_input_tokens: b.cache_creation,
                    cache_read_input_tokens: b.cache_read,
                },
                total_tokens: b.input_tokens + b.output_tokens + b.cache_creation + b.cache_read,
                cost_usd: b.cost_usd,
                models: b.models.clone(),
                burn_rate: burn,
                projection,
            }
        })
        .collect();

    Ok(BlocksOutput {
        blocks: json_blocks,
    })
}

fn create_block(
    start: DateTime<Utc>,
    entries: &[InternalEntry],
    now: DateTime<Utc>,
    session_dur: Duration,
) -> InternalBlock {
    let end_time = start + session_dur;
    let actual_end_time = entries.last().map(|e| e.timestamp).unwrap_or(start);

    let is_active = (now - actual_end_time) < session_dur && now < end_time;

    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cache_creation = 0u64;
    let mut cache_read = 0u64;
    let mut cost_usd = 0.0_f64;
    let mut models: Vec<String> = Vec::new();
    for e in entries {
        input_tokens += e.input;
        output_tokens += e.output;
        cache_creation += e.cache_create;
        cache_read += e.cache_read;
        cost_usd += e.cost;
        if !is_synthetic(&e.model) && !models.contains(&e.model) {
            models.push(e.model.clone());
        } else if is_synthetic(&e.model) {
            // Upstream `models.push(entry.model)` then `uniq`. Synthetic models DO appear in
            // block.models (only filtered in modelsUsed elsewhere). Replicate that.
            if !models.contains(&e.model) {
                models.push(e.model.clone());
            }
        }
    }

    InternalBlock {
        id: iso_string(start),
        start_time: start,
        end_time,
        actual_end_time: Some(actual_end_time),
        is_active,
        is_gap: false,
        entries: entries.to_vec(),
        input_tokens,
        output_tokens,
        cache_creation,
        cache_read,
        cost_usd,
        models,
    }
}

fn create_gap_block(
    last_activity: DateTime<Utc>,
    next_activity: DateTime<Utc>,
    session_dur: Duration,
) -> Option<InternalBlock> {
    let gap_dur = next_activity - last_activity;
    if gap_dur <= session_dur {
        return None;
    }
    let gap_start = last_activity + session_dur;
    let gap_end = next_activity;
    Some(InternalBlock {
        id: format!("gap-{}", iso_string(gap_start)),
        start_time: gap_start,
        end_time: gap_end,
        actual_end_time: None,
        is_active: false,
        is_gap: true,
        entries: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation: 0,
        cache_read: 0,
        cost_usd: 0.0,
        models: Vec::new(),
    })
}

fn calculate_burn_rate(block: &InternalBlock) -> Option<JBurnRate> {
    if block.entries.is_empty() || block.is_gap {
        return None;
    }
    let first = block.entries.first()?.timestamp;
    let last = block.entries.last()?.timestamp;
    let duration_minutes = (last - first).num_milliseconds() as f64 / 60_000.0;
    if duration_minutes <= 0.0 {
        return None;
    }
    let total_tokens =
        (block.input_tokens + block.output_tokens + block.cache_creation + block.cache_read) as f64;
    let tokens_per_minute = total_tokens / duration_minutes;
    let non_cache = (block.input_tokens + block.output_tokens) as f64;
    let tokens_per_minute_for_indicator = non_cache / duration_minutes;
    let cost_per_hour = (block.cost_usd / duration_minutes) * 60.0;
    Some(JBurnRate {
        tokens_per_minute,
        tokens_per_minute_for_indicator,
        cost_per_hour,
    })
}

fn project_usage(block: &InternalBlock, now: DateTime<Utc>) -> Option<JProjection> {
    if !block.is_active || block.is_gap {
        return None;
    }
    let burn = calculate_burn_rate(block)?;
    let remaining = block.end_time - now;
    let remaining_minutes = (remaining.num_milliseconds() as f64 / 60_000.0).max(0.0);
    let current_tokens =
        (block.input_tokens + block.output_tokens + block.cache_creation + block.cache_read) as f64;
    let projected_additional = burn.tokens_per_minute * remaining_minutes;
    let total_tokens = current_tokens + projected_additional;
    let projected_additional_cost = (burn.cost_per_hour / 60.0) * remaining_minutes;
    let total_cost = block.cost_usd + projected_additional_cost;
    Some(JProjection {
        total_tokens: total_tokens.round() as u64,
        total_cost: (total_cost * 100.0).round() / 100.0,
        remaining_minutes: remaining_minutes.round() as u64,
    })
}

/// Render an instant as `YYYY-MM-DDTHH:mm:ss.sssZ` (matches JS `Date.toISOString()`).
fn iso_string(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
