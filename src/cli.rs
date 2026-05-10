use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::aggregations::{
    LoadOptions, SortOrder as AggSortOrder, blocks, daily, monthly, session,
};
use crate::output::{json, table};
use crate::pricing::CostMode;
use crate::timezone::parse_tz;

#[derive(Debug, Parser)]
#[command(
    name = "ccusage-rs",
    version,
    about = "Drop-in Rust reimplementation of ccusage with bit-exact JSON parity",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show usage report grouped by date
    Daily(SharedArgs),
    /// Show usage report grouped by month
    Monthly(SharedArgs),
    /// Show usage report grouped by conversation session
    Session(SharedArgs),
    /// Show usage report grouped by 5-hour billing block
    Blocks(SharedArgs),
}

#[derive(Debug, Args)]
pub struct SharedArgs {
    /// Output JSON instead of a table
    #[arg(long, short = 'j')]
    pub json: bool,

    /// Filter to records on or after this date (YYYYMMDD)
    #[arg(long, short = 's')]
    pub since: Option<String>,

    /// Filter to records on or before this date (YYYYMMDD)
    #[arg(long, short = 'u')]
    pub until: Option<String>,

    /// IANA timezone for date bucketing (e.g. Asia/Seoul). Defaults to UTC.
    #[arg(long, short = 'z')]
    pub timezone: Option<String>,

    /// Locale for date/time formatting in table mode (ignored for JSON output)
    #[arg(long, short = 'l')]
    pub locale: Option<String>,

    /// Cost calculation mode
    #[arg(long, short = 'm', default_value = "auto", value_enum)]
    pub mode: CliCostMode,

    /// Sort order for date-keyed reports
    #[arg(long, short = 'o', default_value = "asc", value_enum)]
    pub order: CliSortOrder,

    /// Skip the live LiteLLM fetch and use the on-disk / bundled snapshot
    #[arg(long)]
    pub offline: bool,

    /// Show per-model breakdown (table mode only — JSON always includes breakdowns)
    #[arg(long, short = 'b')]
    pub breakdown: bool,

    /// Verbose logging
    #[arg(long, short = 'd')]
    pub debug: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliCostMode {
    Auto,
    Calculate,
    Display,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliSortOrder {
    Asc,
    Desc,
}

impl From<CliCostMode> for CostMode {
    fn from(v: CliCostMode) -> Self {
        match v {
            CliCostMode::Auto => CostMode::Auto,
            CliCostMode::Calculate => CostMode::Calculate,
            CliCostMode::Display => CostMode::Display,
        }
    }
}

impl From<CliSortOrder> for AggSortOrder {
    fn from(v: CliSortOrder) -> Self {
        match v {
            CliSortOrder::Asc => AggSortOrder::Asc,
            CliSortOrder::Desc => AggSortOrder::Desc,
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let args = match &cli.command {
        Command::Daily(a) | Command::Monthly(a) | Command::Session(a) | Command::Blocks(a) => a,
    };

    if args.debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }

    let tz = parse_tz(args.timezone.as_deref())?;
    let opts = LoadOptions {
        since: args.since.clone(),
        until: args.until.clone(),
        mode: args.mode.into(),
        order: args.order.into(),
        offline: args.offline,
        timezone: tz,
    };

    match cli.command {
        Command::Daily(_) => {
            let out = daily::build(&opts)?;
            if args.json {
                if out.daily.is_empty() {
                    // Upstream: `JSON.stringify([])` — non-pretty.
                    println!("[]");
                } else {
                    emit_json(&out)?;
                }
            } else {
                table::print_daily(&out.daily, &out.totals);
            }
        }
        Command::Monthly(_) => {
            let out = monthly::build(&opts)?;
            if args.json {
                if out.monthly.is_empty() {
                    // Upstream emits a pretty-printed empty bucket with totals first
                    // and totalTokens before totalCost (different order than the populated case).
                    emit_empty_monthly()?;
                } else {
                    emit_json(&out)?;
                }
            } else {
                table::print_monthly(&out.monthly, &out.totals);
            }
        }
        Command::Session(_) => {
            let out = session::build(&opts)?;
            if args.json {
                if out.sessions.is_empty() {
                    println!("[]");
                } else {
                    emit_json(&out)?;
                }
            } else {
                table::print_session(&out.sessions, &out.totals);
            }
        }
        Command::Blocks(_) => {
            let out = blocks::build(&opts)?;
            if args.json {
                if out.blocks.is_empty() {
                    println!("{{\"blocks\":[]}}");
                } else {
                    emit_blocks_json(&out)?;
                }
            } else {
                table::print_blocks(&out.blocks);
            }
        }
    }

    Ok(())
}

fn emit_empty_monthly() -> Result<()> {
    println!(
        "{{\n  \"monthly\": [],\n  \"totals\": {{\n    \"inputTokens\": 0,\n    \"outputTokens\": 0,\n    \"cacheCreationTokens\": 0,\n    \"cacheReadTokens\": 0,\n    \"totalTokens\": 0,\n    \"totalCost\": 0\n  }}\n}}"
    );
    Ok(())
}

/// Emit JSON with 2-space indentation, matching ccusage's `JSON.stringify(_, null, 2)`.
fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let buf = serialize_pretty(value)?;
    println!("{buf}");
    Ok(())
}

fn emit_blocks_json(value: &json::BlocksOutput) -> Result<()> {
    let buf = serialize_pretty(value)?;
    println!("{buf}");
    Ok(())
}

fn serialize_pretty<T: Serialize>(value: &T) -> Result<String> {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}
