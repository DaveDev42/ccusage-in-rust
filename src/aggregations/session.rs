use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::aggregations::{LoadOptions, date_in_range, load_all_events};
use crate::discover::session_and_project;
use crate::output::json::{ModelBreakdown, SessionEntry, SessionOutput, Totals};
use crate::parse::is_synthetic;
use crate::timezone::format_date_ymd;

#[derive(Default)]
struct ModelAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
}

struct SessionAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    last_activity: DateTime<Utc>,
    last_session_id: String,
    last_project_path: String,
    models_seen: Vec<String>,
    breakdowns: BTreeMap<String, ModelAgg>,
    breakdown_order: Vec<String>,
}

pub(crate) fn build(opts: &LoadOptions) -> Result<SessionOutput> {
    let loaded = load_all_events(opts)?;

    let mut by_key: BTreeMap<String, SessionAgg> = BTreeMap::new();
    let mut key_order: Vec<String> = Vec::new();

    for ev in &loaded.events {
        let (session_id, project_path) =
            session_and_project(&ev.event.source_file, &ev.event.source_base);
        let key = format!("{project_path}/{session_id}");

        if !by_key.contains_key(&key) {
            key_order.push(key.clone());
            by_key.insert(
                key.clone(),
                SessionAgg {
                    input: 0,
                    output: 0,
                    cache_create: 0,
                    cache_read: 0,
                    cost: 0.0,
                    last_activity: ev.event.timestamp,
                    last_session_id: session_id.clone(),
                    last_project_path: project_path.clone(),
                    models_seen: Vec::new(),
                    breakdowns: BTreeMap::new(),
                    breakdown_order: Vec::new(),
                },
            );
        }
        let agg = by_key.get_mut(&key).unwrap();
        agg.input += ev.event.input_tokens;
        agg.output += ev.event.output_tokens;
        agg.cache_create += ev.event.cache_creation_input_tokens;
        agg.cache_read += ev.event.cache_read_input_tokens;
        agg.cost += ev.cost;

        if ev.event.timestamp > agg.last_activity {
            agg.last_activity = ev.event.timestamp;
            agg.last_session_id = session_id;
            agg.last_project_path = project_path;
        }

        if let Some(model) = ev.event.display_model.as_ref() {
            if !is_synthetic(model) {
                if !agg.models_seen.contains(model) {
                    agg.models_seen.push(model.clone());
                }
                let entry = agg.breakdowns.entry(model.clone()).or_default();
                let is_new = entry.input == 0
                    && entry.output == 0
                    && entry.cache_create == 0
                    && entry.cache_read == 0
                    && entry.cost == 0.0;
                if is_new {
                    agg.breakdown_order.push(model.clone());
                }
                entry.input += ev.event.input_tokens;
                entry.output += ev.event.output_tokens;
                entry.cache_create += ev.event.cache_creation_input_tokens;
                entry.cache_read += ev.event.cache_read_input_tokens;
                entry.cost += ev.cost;
            }
        }
    }

    let mut entries: Vec<SessionEntry> = Vec::new();
    for key in &key_order {
        let agg = by_key.get(key).unwrap();

        let last_activity = format_date_ymd(agg.last_activity, opts.timezone);
        if !date_in_range(&last_activity, &opts.since, &opts.until) {
            continue;
        }

        let mut breakdowns: Vec<ModelBreakdown> = agg
            .breakdown_order
            .iter()
            .map(|name| {
                let m = agg.breakdowns.get(name).unwrap();
                ModelBreakdown {
                    model_name: name.clone(),
                    input_tokens: m.input,
                    output_tokens: m.output,
                    cache_creation_tokens: m.cache_create,
                    cache_read_tokens: m.cache_read,
                    cost: m.cost,
                }
            })
            .collect();
        breakdowns.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let total_tokens = agg.input + agg.output + agg.cache_create + agg.cache_read;
        entries.push(SessionEntry {
            session_id: agg.last_session_id.clone(),
            input_tokens: agg.input,
            output_tokens: agg.output,
            cache_creation_tokens: agg.cache_create,
            cache_read_tokens: agg.cache_read,
            total_tokens,
            total_cost: agg.cost,
            last_activity,
            models_used: agg.models_seen.clone(),
            model_breakdowns: breakdowns,
            project_path: agg.last_project_path.clone(),
        });
    }

    // Upstream session command strips the `order` arg and falls through to sortByDate's
    // default of `desc`, so session output is always desc regardless of `--order`.
    entries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    let mut totals = Totals {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        total_cost: 0.0,
        total_tokens: 0,
    };
    for e in &entries {
        totals.input_tokens += e.input_tokens;
        totals.output_tokens += e.output_tokens;
        totals.cache_creation_tokens += e.cache_creation_tokens;
        totals.cache_read_tokens += e.cache_read_tokens;
        totals.total_cost += e.total_cost;
    }
    totals.total_tokens = totals.input_tokens
        + totals.output_tokens
        + totals.cache_creation_tokens
        + totals.cache_read_tokens;

    Ok(SessionOutput {
        sessions: entries,
        totals,
    })
}
