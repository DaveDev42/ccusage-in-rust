use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::aggregations::{LoadOptions, SortOrder, daily};
use crate::output::json::{ModelBreakdown, Totals, WeeklyEntry, WeeklyOutput};
use crate::timezone::{get_date_week, system_local_tz};

#[derive(Debug, Clone, Copy)]
pub(crate) enum WeekStartDay {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl WeekStartDay {
    fn as_num(self) -> u32 {
        match self {
            WeekStartDay::Sunday => 0,
            WeekStartDay::Monday => 1,
            WeekStartDay::Tuesday => 2,
            WeekStartDay::Wednesday => 3,
            WeekStartDay::Thursday => 4,
            WeekStartDay::Friday => 5,
            WeekStartDay::Saturday => 6,
        }
    }
}

#[derive(Default)]
struct ModelAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
}

#[derive(Default)]
struct WeekAgg {
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    cost: f64,
    models_seen: Vec<String>,
    breakdowns: BTreeMap<String, ModelAgg>,
    breakdown_order: Vec<String>,
}

pub(crate) fn build(opts: &LoadOptions, start_day: WeekStartDay) -> Result<WeeklyOutput> {
    // Mirror upstream loadWeeklyUsageData: builds on daily, since/until applied at daily level.
    let daily_out = daily::build(opts)?;
    let system_tz = system_local_tz();
    let start = start_day.as_num();

    let mut by_week: BTreeMap<String, WeekAgg> = BTreeMap::new();
    let mut week_order: Vec<String> = Vec::new();

    for d in &daily_out.daily {
        let week = match get_date_week(&d.date, start, system_tz) {
            Some(w) => w,
            None => continue,
        };
        if !by_week.contains_key(&week) {
            week_order.push(week.clone());
            by_week.insert(week.clone(), WeekAgg::default());
        }
        let w = by_week.get_mut(&week).unwrap();
        w.input += d.input_tokens;
        w.output += d.output_tokens;
        w.cache_create += d.cache_creation_tokens;
        w.cache_read += d.cache_read_tokens;
        w.cost += d.total_cost;

        for model in &d.models_used {
            if !w.models_seen.contains(model) {
                w.models_seen.push(model.clone());
            }
        }

        for b in &d.model_breakdowns {
            let entry = w.breakdowns.entry(b.model_name.clone()).or_default();
            let is_new = entry.input == 0
                && entry.output == 0
                && entry.cache_create == 0
                && entry.cache_read == 0
                && entry.cost == 0.0;
            if is_new {
                w.breakdown_order.push(b.model_name.clone());
            }
            entry.input += b.input_tokens;
            entry.output += b.output_tokens;
            entry.cache_create += b.cache_creation_tokens;
            entry.cache_read += b.cache_read_tokens;
            entry.cost += b.cost;
        }
    }

    let mut entries: Vec<WeeklyEntry> = Vec::new();
    for week in &week_order {
        let w = by_week.get(week).unwrap();
        let mut breakdowns: Vec<ModelBreakdown> = w
            .breakdown_order
            .iter()
            .map(|name| {
                let agg = w.breakdowns.get(name).unwrap();
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

        let total_tokens = w.input + w.output + w.cache_create + w.cache_read;
        entries.push(WeeklyEntry {
            week: week.clone(),
            input_tokens: w.input,
            output_tokens: w.output,
            cache_creation_tokens: w.cache_create,
            cache_read_tokens: w.cache_read,
            total_tokens,
            total_cost: w.cost,
            models_used: w.models_seen.clone(),
            model_breakdowns: breakdowns,
        });
    }

    // Upstream `sortByDate` parses the bucket key with `new Date(...)` then sorts numerically.
    // Week keys are YYYY-MM-DD so lex sort matches numeric sort.
    entries.sort_by(|a, b| match opts.order {
        SortOrder::Asc => parse_date_key(&a.week).cmp(&parse_date_key(&b.week)),
        SortOrder::Desc => parse_date_key(&b.week).cmp(&parse_date_key(&a.week)),
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

    Ok(WeeklyOutput {
        weekly: entries,
        totals,
    })
}

fn parse_date_key(s: &str) -> DateTime<Utc> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| Utc.from_utc_datetime(&n))
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
}
