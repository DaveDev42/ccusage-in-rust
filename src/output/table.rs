//! Minimal table renderer for non-JSON mode.
//!
//! Pretty parity with ccusage's terminal output is not the v1 goal — this is good
//! enough for humans glancing at the data. Most users pass `--json`.

use comfy_table::{Cell, ContentArrangement, Table};

use crate::output::json::{
    BlockEntry, DailyEntry, MonthlyEntry, SessionByIdOutput, SessionEntry, Totals, WeeklyEntry,
};

fn money(v: f64) -> String {
    format!("${v:.2}")
}

fn nat(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn header(table: &mut Table, cols: &[&str]) {
    table.set_header(cols.iter().map(Cell::new));
    table.set_content_arrangement(ContentArrangement::Dynamic);
}

pub(crate) fn print_daily(rows: &[DailyEntry], totals: &Totals) {
    let mut t = Table::new();
    header(
        &mut t,
        &[
            "Date",
            "Models",
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
            "Cost",
        ],
    );
    for r in rows {
        t.add_row(vec![
            Cell::new(&r.date),
            Cell::new(r.models_used.join(", ")),
            Cell::new(nat(r.input_tokens)),
            Cell::new(nat(r.output_tokens)),
            Cell::new(nat(r.cache_creation_tokens)),
            Cell::new(nat(r.cache_read_tokens)),
            Cell::new(nat(r.total_tokens)),
            Cell::new(money(r.total_cost)),
        ]);
    }
    t.add_row(vec![
        Cell::new("Total"),
        Cell::new(""),
        Cell::new(nat(totals.input_tokens)),
        Cell::new(nat(totals.output_tokens)),
        Cell::new(nat(totals.cache_creation_tokens)),
        Cell::new(nat(totals.cache_read_tokens)),
        Cell::new(nat(totals.total_tokens)),
        Cell::new(money(totals.total_cost)),
    ]);
    println!("{t}");
}

pub(crate) fn print_monthly(rows: &[MonthlyEntry], totals: &Totals) {
    let mut t = Table::new();
    header(
        &mut t,
        &[
            "Month",
            "Models",
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
            "Cost",
        ],
    );
    for r in rows {
        t.add_row(vec![
            Cell::new(&r.month),
            Cell::new(r.models_used.join(", ")),
            Cell::new(nat(r.input_tokens)),
            Cell::new(nat(r.output_tokens)),
            Cell::new(nat(r.cache_creation_tokens)),
            Cell::new(nat(r.cache_read_tokens)),
            Cell::new(nat(r.total_tokens)),
            Cell::new(money(r.total_cost)),
        ]);
    }
    t.add_row(vec![
        Cell::new("Total"),
        Cell::new(""),
        Cell::new(nat(totals.input_tokens)),
        Cell::new(nat(totals.output_tokens)),
        Cell::new(nat(totals.cache_creation_tokens)),
        Cell::new(nat(totals.cache_read_tokens)),
        Cell::new(nat(totals.total_tokens)),
        Cell::new(money(totals.total_cost)),
    ]);
    println!("{t}");
}

pub(crate) fn print_weekly(rows: &[WeeklyEntry], totals: &Totals) {
    let mut t = Table::new();
    header(
        &mut t,
        &[
            "Week",
            "Models",
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
            "Cost",
        ],
    );
    for r in rows {
        t.add_row(vec![
            Cell::new(&r.week),
            Cell::new(r.models_used.join(", ")),
            Cell::new(nat(r.input_tokens)),
            Cell::new(nat(r.output_tokens)),
            Cell::new(nat(r.cache_creation_tokens)),
            Cell::new(nat(r.cache_read_tokens)),
            Cell::new(nat(r.total_tokens)),
            Cell::new(money(r.total_cost)),
        ]);
    }
    t.add_row(vec![
        Cell::new("Total"),
        Cell::new(""),
        Cell::new(nat(totals.input_tokens)),
        Cell::new(nat(totals.output_tokens)),
        Cell::new(nat(totals.cache_creation_tokens)),
        Cell::new(nat(totals.cache_read_tokens)),
        Cell::new(nat(totals.total_tokens)),
        Cell::new(money(totals.total_cost)),
    ]);
    println!("{t}");
}

pub(crate) fn print_session(rows: &[SessionEntry], totals: &Totals) {
    let mut t = Table::new();
    header(
        &mut t,
        &[
            "Session",
            "Last Activity",
            "Models",
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Total Tokens",
            "Cost",
        ],
    );
    for r in rows {
        t.add_row(vec![
            Cell::new(&r.session_id),
            Cell::new(&r.last_activity),
            Cell::new(r.models_used.join(", ")),
            Cell::new(nat(r.input_tokens)),
            Cell::new(nat(r.output_tokens)),
            Cell::new(nat(r.cache_creation_tokens)),
            Cell::new(nat(r.cache_read_tokens)),
            Cell::new(nat(r.total_tokens)),
            Cell::new(money(r.total_cost)),
        ]);
    }
    t.add_row(vec![
        Cell::new("Total"),
        Cell::new(""),
        Cell::new(""),
        Cell::new(nat(totals.input_tokens)),
        Cell::new(nat(totals.output_tokens)),
        Cell::new(nat(totals.cache_creation_tokens)),
        Cell::new(nat(totals.cache_read_tokens)),
        Cell::new(nat(totals.total_tokens)),
        Cell::new(money(totals.total_cost)),
    ]);
    println!("{t}");
}

pub(crate) fn print_blocks(rows: &[BlockEntry]) {
    let mut t = Table::new();
    header(
        &mut t,
        &["Block Start", "Status", "Models", "Total Tokens", "Cost"],
    );
    for r in rows {
        let status = if r.is_gap {
            "GAP".to_string()
        } else if r.is_active {
            "ACTIVE".to_string()
        } else {
            String::new()
        };
        t.add_row(vec![
            Cell::new(&r.start_time),
            Cell::new(status),
            Cell::new(r.models.join(", ")),
            Cell::new(nat(r.total_tokens)),
            Cell::new(money(r.cost_usd)),
        ]);
    }
    println!("{t}");
}

pub(crate) fn print_session_by_id(out: &SessionByIdOutput) {
    println!("Claude Code Session Usage - {}", out.session_id);
    println!("Total Cost: {}", money(out.total_cost));
    println!("Total Tokens: {}", nat(out.total_tokens));
    println!("Total Entries: {}", out.entries.len());
    println!();

    if out.entries.is_empty() {
        return;
    }

    let mut t = Table::new();
    header(
        &mut t,
        &[
            "Timestamp",
            "Model",
            "Input",
            "Output",
            "Cache Create",
            "Cache Read",
            "Cost (USD)",
        ],
    );
    for e in &out.entries {
        t.add_row(vec![
            Cell::new(&e.timestamp),
            Cell::new(&e.model),
            Cell::new(nat(e.input_tokens)),
            Cell::new(nat(e.output_tokens)),
            Cell::new(nat(e.cache_creation_tokens)),
            Cell::new(nat(e.cache_read_tokens)),
            Cell::new(money(e.cost_usd)),
        ]);
    }
    println!("{t}");
}
