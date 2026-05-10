use std::collections::BTreeMap;

use anyhow::Result;

use crate::aggregations::{LoadOptions, SortOrder, date_in_range, load_all_events};
use crate::output::json::{DailyEntry, DailyOutput, ModelBreakdown, Totals};
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

#[derive(Default)]
struct DayAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    /// LinkedHashMap-equivalent: insertion order of models seen.
    model_order: Vec<String>,
    by_model: BTreeMap<String, ModelAgg>,
}

pub(crate) fn build(opts: &LoadOptions) -> Result<DailyOutput> {
    let loaded = load_all_events(opts)?;
    // When --instances or --project is in effect, upstream groups by `${date}\x00${project}`.
    let group_by_project = opts.group_by_project || opts.project.is_some();
    let mut by_day: BTreeMap<String, DayAgg> = BTreeMap::new();
    let mut day_order: Vec<String> = Vec::new();

    for ev in &loaded.events {
        let date = format_date_ymd(ev.event.timestamp, opts.timezone);
        let key = if group_by_project {
            format!("{date}\x00{}", ev.project)
        } else {
            date.clone()
        };
        if !by_day.contains_key(&key) {
            day_order.push(key.clone());
            by_day.insert(key.clone(), DayAgg::default());
        }
        let day = by_day.get_mut(&key).unwrap();
        day.input += ev.event.input_tokens;
        day.output += ev.event.output_tokens;
        day.cache_create += ev.event.cache_creation_input_tokens;
        day.cache_read += ev.event.cache_read_input_tokens;
        day.cost += ev.cost;

        // Per-model aggregation: synthetic models excluded from breakdown but ARE counted in totals
        // (matches upstream — `aggregateByModel` skips synthetic, but `calculateTotals` doesn't).
        if let Some(model) = ev.event.display_model.as_ref() {
            if !is_synthetic(model) {
                let entry = day.by_model.entry(model.clone()).or_default();
                if entry.input == 0
                    && entry.output == 0
                    && entry.cache_create == 0
                    && entry.cache_read == 0
                    && entry.cost == 0.0
                {
                    day.model_order.push(model.clone());
                }
                entry.input += ev.event.input_tokens;
                entry.output += ev.event.output_tokens;
                entry.cache_create += ev.event.cache_creation_input_tokens;
                entry.cache_read += ev.event.cache_read_input_tokens;
                entry.cost += ev.cost;
            }
        }
    }

    let mut entries: Vec<DailyEntry> = Vec::new();
    for key in &day_order {
        let day = by_day.get(key).unwrap();
        let (date, project) = if group_by_project {
            let mut parts = key.splitn(2, '\0');
            let d = parts.next().unwrap_or("").to_string();
            let p = parts.next().map(|s| s.to_string());
            (d, p)
        } else {
            (key.clone(), None)
        };
        if !date_in_range(&date, &opts.since, &opts.until) {
            continue;
        }

        let mut breakdowns: Vec<ModelBreakdown> = day
            .model_order
            .iter()
            .map(|m| {
                let agg = day.by_model.get(m).unwrap();
                ModelBreakdown {
                    model_name: m.clone(),
                    input_tokens: agg.input,
                    output_tokens: agg.output,
                    cache_creation_tokens: agg.cache_create,
                    cache_read_tokens: agg.cache_read,
                    cost: agg.cost,
                }
            })
            .collect();
        breakdowns.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let total_tokens = day.input + day.output + day.cache_create + day.cache_read;
        entries.push(DailyEntry {
            date,
            input_tokens: day.input,
            output_tokens: day.output,
            cache_creation_tokens: day.cache_create,
            cache_read_tokens: day.cache_read,
            total_tokens,
            total_cost: day.cost,
            models_used: day.model_order.clone(),
            model_breakdowns: breakdowns,
            project,
        });
    }

    sort_entries(&mut entries, opts.order);

    let totals = totalize(&entries);
    Ok(DailyOutput {
        daily: entries,
        totals,
    })
}

fn sort_entries(entries: &mut [DailyEntry], order: SortOrder) {
    entries.sort_by(|a, b| match order {
        SortOrder::Asc => a.date.cmp(&b.date),
        SortOrder::Desc => b.date.cmp(&a.date),
    });
}

fn totalize(entries: &[DailyEntry]) -> Totals {
    let mut t = Totals {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        total_cost: 0.0,
        total_tokens: 0,
    };
    for e in entries {
        t.input_tokens += e.input_tokens;
        t.output_tokens += e.output_tokens;
        t.cache_creation_tokens += e.cache_creation_tokens;
        t.cache_read_tokens += e.cache_read_tokens;
        t.total_cost += e.total_cost;
    }
    t.total_tokens =
        t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens;
    t
}
