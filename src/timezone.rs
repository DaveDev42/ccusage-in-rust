use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;

pub(crate) fn parse_tz(name: Option<&str>) -> Result<Tz> {
    match name {
        Some(s) => s
            .parse::<Tz>()
            .map_err(|e| anyhow!("invalid timezone {s:?}: {e}")),
        None => Ok(Tz::UTC),
    }
}

/// Format a UTC instant to YYYY-MM-DD in the target timezone.
/// Mirrors ccusage `formatDate(timestamp, timezone, 'en-CA')`.
pub(crate) fn format_date_ymd(ts: DateTime<Utc>, tz: Tz) -> String {
    ts.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

/// Parse an ISO8601 timestamp from a JSONL line. ccusage uses `new Date(...)` which is permissive,
/// but transcripts always emit RFC3339, so we mirror that.
pub(crate) fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("failed to parse timestamp {s:?}"))
}

/// Convert a YYYY-MM-DD date plus an hour-of-day in the user's tz into a UTC instant.
/// Not used in v1; kept for symmetry with date-utils.
#[allow(dead_code)]
pub(crate) fn local_date_to_utc(year: i32, month: u32, day: u32, tz: Tz) -> Option<DateTime<Utc>> {
    tz.with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}
