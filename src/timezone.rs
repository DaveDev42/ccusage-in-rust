use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

pub(crate) fn parse_tz(name: Option<&str>) -> Result<Tz> {
    match name {
        Some(s) => s
            .parse::<Tz>()
            .map_err(|e| anyhow!("invalid timezone {s:?}: {e}")),
        // Upstream `formatDate` calls `Intl.DateTimeFormat(locale, { timeZone: undefined })`,
        // which falls back to Node's system timezone. Mirror that here.
        None => Ok(system_local_tz()),
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

/// Return the system local IANA timezone (mirrors Node's `Intl.DateTimeFormat`
/// fallback when no timezone is provided). Falls back to UTC if detection fails.
pub(crate) fn system_local_tz() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|n| n.parse::<Tz>().ok())
        .unwrap_or(Tz::UTC)
}

/// Mirror upstream `getDateWeek(new Date(data.date), startDay)`:
///   const d = new Date(data.date);            // YYYY-MM-DD parsed as UTC midnight
///   const shift = (d.getDay() - startDay + 7) % 7; // d.getDay() uses LOCAL tz
///   d.setDate(d.getDate() - shift);            // setDate uses LOCAL tz
///   return d.toISOString().substring(0, 10);   // back to UTC YYYY-MM-DD
///
/// `start_day` is 0=Sun..6=Sat. Returns a YYYY-MM-DD string.
pub(crate) fn get_date_week(date_ymd: &str, start_day: u32, system_tz: Tz) -> Option<String> {
    let nd = NaiveDate::parse_from_str(date_ymd, "%Y-%m-%d").ok()?;
    // d = new Date(data.date) — parsed as UTC midnight.
    let utc_midnight: DateTime<Utc> = Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0)?);
    // d.getDay() — local-tz day-of-week (0=Sun..6=Sat).
    let local = utc_midnight.with_timezone(&system_tz);
    let local_dow = local.weekday().num_days_from_sunday();
    let shift = (local_dow + 7 - start_day) % 7;
    // d.setDate(d.getDate() - shift) — reduce LOCAL date by `shift` days, keep local time-of-day.
    let new_local_naive = local.date_naive().checked_sub_signed(
        chrono::Duration::try_days(shift as i64)?,
    )?.and_hms_opt(local.hour(), local.minute(), local.second())?;
    let new_local = system_tz
        .from_local_datetime(&new_local_naive)
        .single()
        .or_else(|| {
            system_tz
                .from_local_datetime(&new_local_naive)
                .earliest()
        })?;
    // Back to ISO UTC, slice 0..10.
    let new_utc = new_local.with_timezone(&Utc);
    Some(new_utc.format("%Y-%m-%d").to_string())
}
