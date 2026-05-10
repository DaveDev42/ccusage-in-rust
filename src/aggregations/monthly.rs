use std::collections::BTreeMap;

use anyhow::Result;

use crate::aggregations::{LoadOptions, SortOrder, daily};
use crate::output::json::{ModelBreakdown, MonthlyEntry, MonthlyOutput, Totals};

#[derive(Default)]
struct ModelAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
}

#[derive(Default)]
struct MonthAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    models_seen: Vec<String>,
    breakdowns: BTreeMap<String, ModelAgg>,
    breakdown_order: Vec<String>,
}

pub(crate) fn build(opts: &LoadOptions) -> Result<MonthlyOutput> {
    // Upstream's `loadBucketUsageData` calls `loadDailyUsageData(options)` with full options,
    // so since/until apply at the daily-entry level BEFORE month bucketing.
    let daily_out = daily::build(opts)?;

    let mut by_month: BTreeMap<String, MonthAgg> = BTreeMap::new();
    let mut month_order: Vec<String> = Vec::new();

    for d in &daily_out.daily {
        let month = d.date[..7].to_string();
        if !by_month.contains_key(&month) {
            month_order.push(month.clone());
            by_month.insert(month.clone(), MonthAgg::default());
        }
        let m = by_month.get_mut(&month).unwrap();
        m.input += d.input_tokens;
        m.output += d.output_tokens;
        m.cache_create += d.cache_creation_tokens;
        m.cache_read += d.cache_read_tokens;
        m.cost += d.total_cost;

        for model in &d.models_used {
            if !m.models_seen.contains(model) {
                m.models_seen.push(model.clone());
            }
        }

        for b in &d.model_breakdowns {
            let entry = m.breakdowns.entry(b.model_name.clone()).or_default();
            let is_new = entry.input == 0
                && entry.output == 0
                && entry.cache_create == 0
                && entry.cache_read == 0
                && entry.cost == 0.0;
            if is_new {
                m.breakdown_order.push(b.model_name.clone());
            }
            entry.input += b.input_tokens;
            entry.output += b.output_tokens;
            entry.cache_create += b.cache_creation_tokens;
            entry.cache_read += b.cache_read_tokens;
            entry.cost += b.cost;
        }
    }

    let mut entries: Vec<MonthlyEntry> = Vec::new();
    for month in &month_order {
        let m = by_month.get(month).unwrap();
        let mut breakdowns: Vec<ModelBreakdown> = m
            .breakdown_order
            .iter()
            .map(|name| {
                let agg = m.breakdowns.get(name).unwrap();
                ModelBreakdown {
                    model_name: name.clone(),
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

        let total_tokens = m.input + m.output + m.cache_create + m.cache_read;
        entries.push(MonthlyEntry {
            month: month.clone(),
            input_tokens: m.input,
            output_tokens: m.output,
            cache_creation_tokens: m.cache_create,
            cache_read_tokens: m.cache_read,
            total_tokens,
            total_cost: m.cost,
            models_used: m.models_seen.clone(),
            model_breakdowns: breakdowns,
        });
    }

    entries.sort_by(|a, b| match opts.order {
        SortOrder::Asc => a.month.cmp(&b.month),
        SortOrder::Desc => b.month.cmp(&a.month),
    });

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

    Ok(MonthlyOutput {
        monthly: entries,
        totals,
    })
}
