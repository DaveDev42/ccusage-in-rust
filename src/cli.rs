use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::aggregations::weekly::WeekStartDay;
use crate::aggregations::{
    LoadOptions, SortOrder as AggSortOrder, blocks, daily, monthly, session, session_by_id, weekly,
};
use crate::jq;
use crate::output::table;
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
    /// Show usage report grouped by week
    Weekly(SharedArgs),
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

    /// Session ID lookup (session subcommand only) — outputs single-session detail
    #[arg(long, short = 'i')]
    pub id: Option<String>,

    /// Day to start the week on (weekly subcommand only)
    #[arg(
        long = "start-of-week",
        short = 'w',
        default_value = "sunday",
        value_enum
    )]
    pub start_of_week: CliWeekStartDay,

    /// jq filter expression — forces JSON mode and pipes output through `jq`
    #[arg(long, short = 'q')]
    pub jq: Option<String>,

    /// Show usage breakdown by project/instance (daily only — reshapes JSON to {projects, totals})
    #[arg(long)]
    pub instances: bool,

    /// Filter to a specific project name (matches `extractProjectFromPath` segment)
    #[arg(long, short = 'p')]
    pub project: Option<String>,

    /// Force compact table layout (table mode only — accepted for parity)
    #[arg(long)]
    pub compact: bool,

    /// Disable colored output (table mode — accepted for parity, no-op for now)
    #[arg(long = "no-color")]
    pub no_color: bool,

    /// Force colored output (table mode — accepted for parity, no-op for now)
    #[arg(long)]
    pub color: bool,

    /// How many sample mismatches to show in debug mode (accepted for parity)
    #[arg(long = "debug-samples", default_value_t = 5)]
    pub debug_samples: u32,

    /// Path to a config file (accepted for parity, no-op for now)
    #[arg(long, hide = true)]
    pub config: Option<String>,

    /// Comma-separated project name aliases (accepted for parity, no-op for now)
    #[arg(long = "project-aliases", hide = true)]
    pub project_aliases: Option<String>,

    /// Override "now" for active-block projection (ISO 8601 / RFC 3339 timestamp,
    /// or "@<unix-ms>"). Use this — or set `CCUSAGE_RS_NOW` — to get reproducible
    /// blocks output across runs. Without this, projection drifts as wall-clock advances.
    #[arg(long, env = "CCUSAGE_RS_NOW")]
    pub now: Option<String>,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliWeekStartDay {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl From<CliWeekStartDay> for WeekStartDay {
    fn from(v: CliWeekStartDay) -> Self {
        match v {
            CliWeekStartDay::Sunday => WeekStartDay::Sunday,
            CliWeekStartDay::Monday => WeekStartDay::Monday,
            CliWeekStartDay::Tuesday => WeekStartDay::Tuesday,
            CliWeekStartDay::Wednesday => WeekStartDay::Wednesday,
            CliWeekStartDay::Thursday => WeekStartDay::Thursday,
            CliWeekStartDay::Friday => WeekStartDay::Friday,
            CliWeekStartDay::Saturday => WeekStartDay::Saturday,
        }
    }
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
        Command::Daily(a)
        | Command::Monthly(a)
        | Command::Weekly(a)
        | Command::Session(a)
        | Command::Blocks(a) => a,
    };

    if args.debug {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }

    let tz = parse_tz(args.timezone.as_deref())?;
    // Upstream daily groups-by-project when EITHER `--instances` is set OR `--project NAME`
    // is set (the latter via `groupByProject: data => entry.project != null`). Match that.
    let group_by_project = matches!(cli.command, Command::Daily(_)) && args.instances;
    let now_override = match args.now.as_deref() {
        None => None,
        Some(s) => Some(parse_now_override(s)?),
    };
    let opts = LoadOptions {
        since: args.since.clone(),
        until: args.until.clone(),
        mode: args.mode.into(),
        order: args.order.into(),
        offline: args.offline,
        timezone: tz,
        project: args.project.clone(),
        group_by_project,
        now_override,
    };

    // Upstream: `useJson = json || jq != null`. jq forces JSON mode.
    let use_json = args.json || args.jq.is_some();
    let jq_expr = args.jq.as_deref();

    match cli.command {
        Command::Daily(_) => {
            let out = daily::build(&opts)?;
            if use_json {
                if out.daily.is_empty() {
                    // Upstream: empty short-circuits BEFORE jq, prints `JSON.stringify([])` raw.
                    println!("[]");
                } else if args.instances && out.daily.iter().any(|d| d.project.is_some()) {
                    let by_project = group_daily_by_project(out);
                    emit_or_jq(&by_project, jq_expr)?;
                } else {
                    emit_or_jq(&out, jq_expr)?;
                }
            } else {
                table::print_daily(&out.daily, &out.totals);
            }
        }
        Command::Monthly(_) => {
            let out = monthly::build(&opts)?;
            if use_json {
                if out.monthly.is_empty() {
                    // Upstream emits a pretty-printed empty bucket with totals first
                    // and totalTokens before totalCost (different order than the populated case).
                    emit_empty_monthly()?;
                } else {
                    emit_or_jq(&out, jq_expr)?;
                }
            } else {
                table::print_monthly(&out.monthly, &out.totals);
            }
        }
        Command::Weekly(_) => {
            let out = weekly::build(&opts, args.start_of_week.into())?;
            if use_json {
                if out.weekly.is_empty() {
                    emit_empty_weekly()?;
                } else {
                    emit_or_jq(&out, jq_expr)?;
                }
            } else {
                table::print_weekly(&out.weekly, &out.totals);
            }
        }
        Command::Session(_) => {
            if let Some(id) = args.id.as_deref() {
                let out = session_by_id::build(id, &opts)?;
                match out {
                    None => {
                        if use_json {
                            // Upstream: `JSON.stringify(null)` — literal `null`.
                            println!("null");
                        } else {
                            eprintln!("No session found with ID: {id}");
                        }
                    }
                    Some(out) => {
                        if use_json {
                            emit_or_jq(&out, jq_expr)?;
                        } else {
                            table::print_session_by_id(&out);
                        }
                    }
                }
            } else {
                let out = session::build(&opts)?;
                if use_json {
                    if out.sessions.is_empty() {
                        println!("[]");
                    } else {
                        emit_or_jq(&out, jq_expr)?;
                    }
                } else {
                    table::print_session(&out.sessions, &out.totals);
                }
            }
        }
        Command::Blocks(_) => {
            let out = blocks::build(&opts)?;
            if use_json {
                if out.blocks.is_empty() {
                    println!("{{\"blocks\":[]}}");
                } else {
                    emit_or_jq(&out, jq_expr)?;
                }
            } else {
                table::print_blocks(&out.blocks);
            }
        }
    }

    Ok(())
}

fn parse_now_override(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Some(rest) = s.strip_prefix('@') {
        let ms: i64 = rest
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --now millis {s:?}: {e}"))?;
        return chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .ok_or_else(|| anyhow::anyhow!("--now millis out of range: {ms}"));
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| anyhow::anyhow!("invalid --now {s:?}: {e}"))
}

fn group_daily_by_project(
    out: crate::output::json::DailyOutput,
) -> crate::output::json::DailyByProjectOutput {
    use crate::output::json::{DailyByProjectOutput, ProjectDailyEntry};
    let mut projects: indexmap::IndexMap<String, Vec<ProjectDailyEntry>> =
        indexmap::IndexMap::new();
    for entry in out.daily {
        let project_name = entry
            .project
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let pde = ProjectDailyEntry {
            date: entry.date,
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            cache_creation_tokens: entry.cache_creation_tokens,
            cache_read_tokens: entry.cache_read_tokens,
            total_tokens: entry.total_tokens,
            total_cost: entry.total_cost,
            models_used: entry.models_used,
            model_breakdowns: entry.model_breakdowns,
        };
        projects.entry(project_name).or_default().push(pde);
    }
    DailyByProjectOutput {
        projects,
        totals: out.totals,
    }
}

fn emit_empty_monthly() -> Result<()> {
    println!(
        "{{\n  \"monthly\": [],\n  \"totals\": {{\n    \"inputTokens\": 0,\n    \"outputTokens\": 0,\n    \"cacheCreationTokens\": 0,\n    \"cacheReadTokens\": 0,\n    \"totalTokens\": 0,\n    \"totalCost\": 0\n  }}\n}}"
    );
    Ok(())
}

fn emit_empty_weekly() -> Result<()> {
    // Upstream weekly empty: same shape as monthly empty.
    println!(
        "{{\n  \"weekly\": [],\n  \"totals\": {{\n    \"inputTokens\": 0,\n    \"outputTokens\": 0,\n    \"cacheCreationTokens\": 0,\n    \"cacheReadTokens\": 0,\n    \"totalTokens\": 0,\n    \"totalCost\": 0\n  }}\n}}"
    );
    Ok(())
}

/// Emit JSON with 2-space indentation, matching ccusage's `JSON.stringify(_, null, 2)`,
/// or pipe through `jq` if a filter expression is provided.
fn emit_or_jq<T: Serialize>(value: &T, jq_expr: Option<&str>) -> Result<()> {
    if let Some(expr) = jq_expr {
        let out = jq::run(value, expr)?;
        println!("{out}");
    } else {
        let buf = serialize_pretty(value)?;
        println!("{buf}");
    }
    Ok(())
}

fn serialize_pretty<T: Serialize>(value: &T) -> Result<String> {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}
