//! Embedded-DuckDB incremental parsed-row cache + file manifest.
//!
//! DuckDB is demoted to a pure I/O cache: it stores the *parsed rows* `parse_file`
//! emits (PRE cross-file dedup, PRE cost) keyed by absolute path, plus a file
//! manifest `(path, mtime_ns, size_bytes, earliest_ts, schema_version)` that drives
//! the incremental "which files changed" decision. NO SQL aggregation, SUM, or
//! GROUP BY ever runs against these tables — every aggregation, float-sum, dedup,
//! cost resolution, and JSON serialization stays 100% Rust-native (see
//! `aggregations/mod.rs`), so output is byte-identical to the non-cached path.
//!
//! Per-run flow (`sync_and_load`, replaces the file loop in `load_all_events`):
//!   1. caller discovers the FULL file list (every base, every file).
//!   2. stat each file -> (mtime_ns, size_bytes).
//!   3. diff vs manifest -> CHANGED/NEW, DELETED (scoped to this run's bases), UNCHANGED.
//!   4. rayon-parse CHANGED/NEW (the only file reads): parse_file + earliest_timestamp.
//!   5. per-file INDIVIDUAL transaction upsert (DuckDB has NO SAVEPOINT / nested BEGIN).
//!   6. Rust-side file ordering (reuse the mod.rs comparator — never SQL ORDER BY).
//!   7. ONE bulk SELECT of all in-scope rows; order them in Rust by (file_pos, line_index).
//!
//! Read-only fallback: if another process holds the read-write lock, reopen read-only
//! and serve the last warm state (skip steps 4-5). DuckDB-rs has no busy_timeout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::types::{TimeUnit, Value};
use duckdb::{AccessMode, Config, Connection};
use rayon::prelude::*;

use crate::aggregations::file_order_cmp;
use crate::discover::DiscoveredFile;
use crate::parse::{UsageEvent, parse_file};

/// Compile-time hash of `src/parse.rs` + the cache column list (emitted by build.rs).
/// Any edit to the parsed-row shape bumps this -> every manifest row is re-parsed.
/// Stored as BIGINT (signed i64); build.rs masks the FNV-1a hash to 63 bits.
const SCHEMA_VERSION: i64 = {
    // env!() yields a &str literal at compile time; parse it in a const fn.
    parse_i64(env!("CCUSAGE_RS_SCHEMA_VERSION"))
};

/// Const-fn decimal parser (u64::from_str is not const on stable). Value is always
/// non-negative (build.rs masks to 63 bits), so a simple base-10 fold suffices.
const fn parse_i64(s: &str) -> i64 {
    let bytes = s.as_bytes();
    let mut acc: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(b >= b'0' && b <= b'9', "CCUSAGE_RS_SCHEMA_VERSION must be decimal");
        acc = acc * 10 + (b - b'0') as i64;
        i += 1;
    }
    acc
}

/// Layout marker for the table shapes themselves (independent of parse.rs hash).
/// Bump only when CREATE TABLE DDL changes in an incompatible way.
const SCHEMA_LAYOUT: &str = "1";

/// Resolve the cache DB path: `$CCUSAGE_RS_CACHE_DIR/cache.duckdb`, else
/// `dirs::cache_dir()/ccusage-rs/cache.duckdb` (next to litellm.json).
fn cache_db_path() -> Result<PathBuf> {
    let dir = match std::env::var_os("CCUSAGE_RS_CACHE_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::cache_dir()
            .context("could not determine cache dir")?
            .join("ccusage-rs"),
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating cache dir {}", dir.display()))?;
    Ok(dir.join("cache.duckdb"))
}

const CREATE_DDL: &str = "
CREATE TABLE IF NOT EXISTS meta (
  key   VARCHAR PRIMARY KEY,
  value VARCHAR
);
CREATE TABLE IF NOT EXISTS file_manifest (
  path           VARCHAR PRIMARY KEY,
  base_dir       VARCHAR NOT NULL,
  mtime_ns       BIGINT  NOT NULL,
  size_bytes     BIGINT  NOT NULL,
  earliest_ts    TIMESTAMP,
  schema_version BIGINT  NOT NULL,
  ingested_at    TIMESTAMP NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
  path           VARCHAR NOT NULL,
  line_index     INTEGER NOT NULL,
  ts             TIMESTAMP NOT NULL,
  model          VARCHAR,
  display_model  VARCHAR,
  input_tokens   UBIGINT NOT NULL,
  output_tokens  UBIGINT NOT NULL,
  cache_creation UBIGINT NOT NULL,
  cache_read     UBIGINT NOT NULL,
  speed_fast     BOOLEAN NOT NULL,
  cost_usd       DOUBLE,
  dedup_key      VARCHAR,
  msg_id         VARCHAR,
  request_id     VARCHAR,
  base_dir       VARCHAR NOT NULL,
  PRIMARY KEY (path, line_index)
);
CREATE INDEX IF NOT EXISTS idx_messages_path ON messages(path);
";

/// Open the cache DB. Returns `(connection, is_read_only)`.
///
/// Strategy (duckdb-rs has NO busy_timeout):
///   1. Try read-write. On success with a compatible schema, use it (is_read_only=false).
///   2. If the RW open or schema-ensure fails because the file is LOCKED by a concurrent
///      writer, try read-only. If that succeeds AND the schema is compatible, serve the
///      warm state (is_read_only=true).
///   3. If the file is locked both ways (a concurrent writer also blocks RO), fall back
///      to an in-memory read-write DB and do a full cold parse into it: correct (NOT
///      empty, NOT stale) output, just not persisted. We NEVER delete a locked file.
///   4. If the open fails for a non-lock reason (corruption / on-disk format change),
///      delete the file and cold-rebuild on disk.
pub(crate) fn open_db() -> Result<(Connection, bool)> {
    let path = cache_db_path()?;

    // 1. read-write. (Connection::open takes the file lock, so if it succeeds we hold
    // it and ensure_schema cannot hit a lock error — any ensure_schema failure is a
    // schema/format problem -> cold rebuild.)
    match Connection::open(&path) {
        Ok(conn) => match ensure_schema(&conn) {
            Ok(()) => Ok((conn, false)),
            Err(_) => {
                drop(conn);
                recreate_from_scratch(&path)
            }
        },
        Err(e) => {
            if is_lock_error(&e) {
                open_locked_fallback(&path)
            } else {
                // Non-lock open failure: corruption / format change -> delete + rebuild.
                recreate_from_scratch(&path)
            }
        }
    }
}

/// File is locked by a concurrent RW holder. Try read-only; if even RO is blocked,
/// use an ephemeral in-memory DB (full cold parse, correct output, no persistence).
fn open_locked_fallback(path: &Path) -> Result<(Connection, bool)> {
    let cfg = Config::default().access_mode(AccessMode::ReadOnly)?;
    match Connection::open_with_flags(path, cfg) {
        Ok(conn) => {
            // Read-only: serve the warm state. (If incompatible we still serve RO; the
            // next RW run rebuilds — we never write while another process holds the lock.)
            Ok((conn, true))
        }
        Err(_) => {
            // Locked for RO too -> ephemeral in-memory RW (correct, unpersisted).
            let conn = Connection::open_in_memory()
                .context("opening in-memory fallback cache")?;
            ensure_schema(&conn)?;
            Ok((conn, false))
        }
    }
}

/// True if a duckdb error is a file-lock conflict (vs corruption / format change).
fn is_lock_error(e: &duckdb::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("lock") || msg.contains("being used by another")
}

fn recreate_from_scratch(path: &Path) -> Result<(Connection, bool)> {
    let _ = std::fs::remove_file(path);
    // DuckDB also writes a WAL sidecar.
    let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    match Connection::open(path) {
        Ok(conn) => {
            ensure_schema(&conn)?;
            Ok((conn, false))
        }
        Err(_) => {
            // Even a fresh on-disk open failed (path unwritable, etc.) -> in-memory.
            let conn = Connection::open_in_memory()
                .context("opening in-memory cache after on-disk reset failed")?;
            ensure_schema(&conn)?;
            Ok((conn, false))
        }
    }
}

/// Create tables if missing and verify compatibility markers. On a marker mismatch,
/// DROP + recreate (auto cold rebuild). Returns Err only on a real DB failure.
fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(CREATE_DDL)?;

    let crate_version = duckdb_crate_version();
    let stored_crate: Option<String> = read_meta(conn, "duckdb_crate_version");
    let stored_layout: Option<String> = read_meta(conn, "schema_layout");

    let crate_ok = stored_crate.as_deref() == Some(crate_version.as_str());
    let layout_ok = stored_layout.as_deref() == Some(SCHEMA_LAYOUT);

    if stored_crate.is_none() && stored_layout.is_none() {
        // Fresh DB: stamp markers.
        write_meta(conn, "duckdb_crate_version", &crate_version)?;
        write_meta(conn, "schema_layout", SCHEMA_LAYOUT)?;
        return Ok(());
    }

    if !crate_ok || !layout_ok {
        // Incompatible markers: drop everything and recreate.
        conn.execute_batch(
            "DROP TABLE IF EXISTS messages; DROP TABLE IF EXISTS file_manifest; DROP TABLE IF EXISTS meta;",
        )?;
        conn.execute_batch(CREATE_DDL)?;
        write_meta(conn, "duckdb_crate_version", &crate_version)?;
        write_meta(conn, "schema_layout", SCHEMA_LAYOUT)?;
    }
    Ok(())
}

fn duckdb_crate_version() -> String {
    // The resolved duckdb crate version this binary was compiled against (from
    // Cargo.lock via build.rs). A duckdb bump that changes the on-disk format thus
    // invalidates the cache -> cold rebuild instead of a hard open error.
    format!("duckdb={}", env!("CCUSAGE_RS_DUCKDB_VERSION"))
}

fn read_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0))
        .ok()
}

fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?, ?)",
        [key, value],
    )?;
    Ok(())
}

/// One parsed file's payload: the emitted events + the all-lines earliest timestamp.
struct ParsedFile {
    path: PathBuf,
    base_dir: PathBuf,
    mtime_ns: i64,
    size_bytes: i64,
    earliest_ts: Option<DateTime<Utc>>,
    events: Vec<UsageEvent>,
    /// True if parse_file returned an error (file unreadable mid-parse, etc.).
    parse_failed: bool,
}

/// stat -> (mtime_ns, size_bytes). Returns None on stat failure (file vanished).
fn stat_file(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?;
    let mtime_ns = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or_else(|e| -(e.duration().as_nanos() as i64));
    Some((mtime_ns, md.len() as i64))
}

/// Convert a DateTime<Utc> to DuckDB microseconds-since-epoch (lossless for our data).
fn dt_to_micros(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_micros()
}

fn micros_to_dt(micros: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_micros(micros).unwrap_or_else(|| {
        // Fallback: clamp via seconds. Should never trigger for real transcript data.
        DateTime::<Utc>::from_timestamp(micros / 1_000_000, 0).unwrap_or_else(Utc::now)
    })
}

/// THE chokepoint replacement. Given the FULL discovered file list (never project-
/// filtered) and the read-only flag, return the parsed events in EXACT event-arrival
/// order — file order = two-phase (per-base OsStr, then earliest-ts), within-file =
/// line_index ASC = parse_file push order. Cross-file dedup + cost are applied by the
/// caller in mod.rs over this ordered stream.
#[allow(unused_assignments)]
pub(crate) fn sync_and_load(
    conn: &Connection,
    discovered: &[DiscoveredFile],
    is_read_only: bool,
) -> Result<Vec<UsageEvent>> {
    let dbg_time = std::env::var_os("CCUSAGE_RS_DEBUG_TIME").is_some();
    macro_rules! lap {
        ($t:expr, $label:expr) => {
            if dbg_time {
                eprintln!("[cache] {} {:?}", $label, $t.elapsed());
                $t = std::time::Instant::now();
            }
        };
    }
    let mut t = std::time::Instant::now();
    // ---- STEP 2: STAT ----
    // path -> (mtime_ns, size_bytes, base_dir). Missing stat = skip (file vanished
    // between discover and stat; treated as not-present).
    let mut stats: HashMap<PathBuf, (i64, i64, PathBuf)> = HashMap::new();
    for f in discovered {
        if let Some((mtime_ns, size)) = stat_file(&f.path) {
            stats.insert(f.path.clone(), (mtime_ns, size, f.base_dir.clone()));
        }
    }

    // This run's discovered bases (for scoping DELETED).
    let scope_bases: std::collections::HashSet<String> = discovered
        .iter()
        .map(|f| path_to_str(&f.base_dir))
        .collect();

    lap!(t, "stat");
    // ---- STEP 3: DIFF (read manifest) ----
    struct ManifestRow {
        base_dir: String,
        mtime_ns: i64,
        size_bytes: i64,
        schema_version: i64,
    }
    let mut manifest: HashMap<String, ManifestRow> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT path, base_dir, mtime_ns, size_bytes, schema_version FROM file_manifest")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ManifestRow {
                    base_dir: r.get::<_, String>(1)?,
                    mtime_ns: r.get::<_, i64>(2)?,
                    size_bytes: r.get::<_, i64>(3)?,
                    schema_version: r.get::<_, i64>(4)?,
                },
            ))
        })?;
        for row in rows {
            let (path, mr) = row?;
            manifest.insert(path, mr);
        }
    }

    let force_rescan = std::env::var_os("CCUSAGE_RS_FORCE_RESCAN").is_some();

    // CHANGED/NEW: discovered files whose stat differs from manifest (or missing).
    let mut changed: Vec<DiscoveredFile> = Vec::new();
    for f in discovered {
        let key = path_to_str(&f.path);
        let Some((mtime_ns, size, _)) = stats.get(&f.path).cloned() else {
            // stat failed -> can't read it; skip (matches today's open-error -> no rows).
            continue;
        };
        let is_changed = force_rescan
            || match manifest.get(&key) {
                None => true,
                Some(m) => {
                    m.mtime_ns != mtime_ns
                        || m.size_bytes != size
                        || m.schema_version != SCHEMA_VERSION
                }
            };
        if is_changed {
            changed.push(DiscoveredFile {
                path: f.path.clone(),
                base_dir: f.base_dir.clone(),
            });
        }
    }

    // DELETED: manifest rows whose base_dir is in THIS run's scope AND not on disk.
    let discovered_set: std::collections::HashSet<String> =
        discovered.iter().map(|f| path_to_str(&f.path)).collect();
    let mut deleted: Vec<String> = Vec::new();
    for (path, mr) in &manifest {
        if scope_bases.contains(&mr.base_dir) && !discovered_set.contains(path) {
            deleted.push(path.clone());
        }
    }

    if dbg_time {
        eprintln!(
            "[cache] changed={} deleted={} manifest_rows={} discovered={}",
            changed.len(),
            deleted.len(),
            manifest.len(),
            discovered.len()
        );
    }
    lap!(t, "diff");
    // ---- STEP 4 + 5: PARSE + UPSERT (skip entirely in read-only mode) ----
    if !is_read_only {
        // STEP 4: rayon-parse CHANGED/NEW. Two independent passes per file:
        //   (a) parse_file -> emitted UsageEvents (with intra-file throwaway dedup)
        //   (b) earliest_timestamp -> all-lines scan (reads rejected lines too)
        let parsed: Vec<ParsedFile> = changed
            .par_iter()
            .map(|f| {
                let (mtime_ns, size_bytes, _) = stats
                    .get(&f.path)
                    .cloned()
                    .unwrap_or((0, 0, f.base_dir.clone()));
                let mut events: Vec<UsageEvent> = Vec::new();
                let mut throwaway: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let parse_res = parse_file(&f.path, &f.base_dir, &mut throwaway, &mut events);
                let earliest_ts = crate::aggregations::earliest_timestamp(&f.path);
                ParsedFile {
                    path: f.path.clone(),
                    base_dir: f.base_dir.clone(),
                    mtime_ns,
                    size_bytes,
                    earliest_ts,
                    events,
                    parse_failed: parse_res.is_err(),
                }
            })
            .collect();

        // STEP 5: per-file INDIVIDUAL transactions (DuckDB has no SAVEPOINT).
        // DELETED first, then CHANGED/NEW.
        for path in &deleted {
            // One transaction per deletion.
            upsert_delete(conn, path)?;
        }
        for pf in &parsed {
            upsert_file(conn, pf)?;
        }
    }

    lap!(t, "parse+upsert");
    // ---- STEP 6: FILE ORDERING (Rust; reuse mod.rs comparator) ----
    // Fetch (path, base_dir, earliest_ts) for IN-SCOPE manifest rows. In-scope =
    // files we will actually read back. We mirror today's behavior: only files that
    // are part of this run's discovered set contribute events.
    //
    // Phase 1 (per-base OsStr order) is implicit in the discovered list order
    // (discover_jsonl_files already sorted per-base). Phase 2 (global earliest-ts
    // stable sort) reuses file_order_cmp. We build the ordered path list from the
    // DISCOVERED files (preserving phase-1 order) keyed to their cached earliest_ts.

    // Build path -> earliest_ts map from the manifest (post-upsert).
    let mut earliest_map: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, earliest_ts FROM file_manifest")?;
        let rows = stmt.query_map([], |r| {
            let path: String = r.get(0)?;
            let ts: Option<i64> = ts_opt_from_row(r, 1)?;
            Ok((path, ts.map(micros_to_dt)))
        })?;
        for row in rows {
            let (path, ts) = row?;
            earliest_map.insert(path, ts);
        }
    }

    // Ordered in-scope files = discovered files (phase-1 order preserved), each with
    // its cached earliest_ts; then stable-sort by earliest_ts via file_order_cmp.
    // Only include files that have a manifest row (i.e. were successfully ingested at
    // some point) — a file that failed parse on its very first sight has no rows and
    // no manifest entry, exactly like today's parse-error -> no contribution.
    let mut ordered: Vec<(PathBuf, Option<DateTime<Utc>>)> = Vec::new();
    for f in discovered {
        let key = path_to_str(&f.path);
        if let Some(ts) = earliest_map.get(&key) {
            ordered.push((f.path.clone(), *ts));
        }
    }
    // Stable sort reproduces sort_files_by_earliest_timestamp exactly (phase-1 order
    // is the pre-sort order; stable tie-break preserves it).
    ordered.sort_by(|a, b| file_order_cmp(&a.1, &b.1));

    // position map: path -> ordinal.
    let mut pos: HashMap<String, usize> = HashMap::new();
    for (i, (p, _)) in ordered.iter().enumerate() {
        pos.insert(path_to_str(p), i);
    }

    lap!(t, "ordering");
    // ---- STEP 7: SINGLE-QUERY READ-BACK ----
    // ONE columnar scan of all message rows. We do NOT use SQL ORDER BY for file
    // order (collation/NULL drift); instead we order in Rust by (file_pos, line_index).
    // We include line_index in the SELECT and sort the materialized Vec.
    let mut rows: Vec<(usize, i32, UsageEvent)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT path, line_index, ts, model, display_model, input_tokens, output_tokens, \
             cache_creation, cache_read, speed_fast, cost_usd, msg_id, request_id, base_dir \
             FROM messages",
        )?;
        let mut q = stmt.query([])?;
        while let Some(r) = q.next()? {
            let path: String = r.get(0)?;
            // Skip rows whose file isn't in this run's ordered scope (out-of-scope base).
            let Some(&p) = pos.get(&path) else { continue };
            let line_index: i32 = r.get(1)?;
            let ts_micros: i64 = ts_i64_from_row(r, 2)?;
            let model: Option<String> = r.get(3)?;
            let display_model: Option<String> = r.get(4)?;
            let input_tokens: u64 = r.get(5)?;
            let output_tokens: u64 = r.get(6)?;
            let cache_creation: u64 = r.get(7)?;
            let cache_read: u64 = r.get(8)?;
            let speed_fast: bool = r.get(9)?;
            let cost_usd: Option<f64> = r.get(10)?;
            let msg_id: Option<String> = r.get(11)?;
            let request_id: Option<String> = r.get(12)?;
            let base_dir: String = r.get(13)?;

            let event = UsageEvent {
                timestamp: micros_to_dt(ts_micros),
                model,
                display_model,
                input_tokens,
                output_tokens,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                speed_fast,
                cost_usd,
                source_file: PathBuf::from(path),
                source_base: PathBuf::from(base_dir),
                msg_id,
                request_id,
            };
            rows.push((p, line_index, event));
        }
    }

    // Order by (file_position, line_index) — exact event-arrival order.
    lap!(t, "readback");
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    lap!(t, "sort");

    Ok(rows.into_iter().map(|(_, _, e)| e).collect())
}

/// One transaction: delete a file's rows + manifest entry.
fn upsert_delete(conn: &Connection, path: &str) -> Result<()> {
    conn.execute_batch("BEGIN TRANSACTION;")?;
    let res = (|| -> Result<()> {
        conn.execute("DELETE FROM messages WHERE path = ?", [path])?;
        conn.execute("DELETE FROM file_manifest WHERE path = ?", [path])?;
        Ok(())
    })();
    match res {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// One transaction: re-ingest a single file (delete old rows, insert new, upsert
/// manifest). On parse failure, leave the manifest row STALE (so it stays CHANGED
/// next run) but ALSO clear its message rows so we never serve stale rows mixed with
/// fresh ones this cycle (matches today's empty-on-error contribution).
fn upsert_file(conn: &Connection, pf: &ParsedFile) -> Result<()> {
    let path = path_to_str(&pf.path);

    if pf.parse_failed {
        // Parse failed mid-file. Clear any existing rows for this path and do NOT
        // update the manifest mtime/size (so it re-attempts next run). Done in its
        // own transaction.
        conn.execute_batch("BEGIN TRANSACTION;")?;
        let res = (|| -> Result<()> {
            conn.execute("DELETE FROM messages WHERE path = ?", [path.as_str()])?;
            // Set manifest to a sentinel that never matches a real stat, ensuring
            // re-attempt, and earliest_ts = NULL (sorts last) so any prior cached
            // ordering value is not reused. Keep base_dir for scoping.
            conn.execute(
                "INSERT OR REPLACE INTO file_manifest \
                 (path, base_dir, mtime_ns, size_bytes, earliest_ts, schema_version, ingested_at) \
                 VALUES (?, ?, -1, -1, NULL, ?, current_localtimestamp())",
                duckdb::params![path.as_str(), path_to_str(&pf.base_dir).as_str(), SCHEMA_VERSION],
            )?;
            Ok(())
        })();
        return match res {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        };
    }

    conn.execute_batch("BEGIN TRANSACTION;")?;
    let res = (|| -> Result<()> {
        conn.execute("DELETE FROM messages WHERE path = ?", [path.as_str()])?;

        // Bulk insert via Appender (transactional in duckdb 1.4.x — verified).
        {
            let mut app = conn.appender("messages")?;
            for (idx, ev) in pf.events.iter().enumerate() {
                let dedup_key: Option<String> =
                    match (ev.msg_id.as_ref(), ev.request_id.as_ref()) {
                        (Some(mid), Some(rid)) => Some(format!("{mid}:{rid}")),
                        _ => None,
                    };
                app.append_row(duckdb::params![
                    path.as_str(),
                    idx as i32,
                    Value::Timestamp(TimeUnit::Microsecond, dt_to_micros(ev.timestamp)),
                    ev.model.as_deref(),
                    ev.display_model.as_deref(),
                    ev.input_tokens,
                    ev.output_tokens,
                    ev.cache_creation_input_tokens,
                    ev.cache_read_input_tokens,
                    ev.speed_fast,
                    ev.cost_usd,
                    dedup_key.as_deref(),
                    ev.msg_id.as_deref(),
                    ev.request_id.as_deref(),
                    path_to_str(&ev.source_base).as_str(),
                ])?;
            }
            app.flush()?;
        }

        let earliest = pf
            .earliest_ts
            .map(|dt| Value::Timestamp(TimeUnit::Microsecond, dt_to_micros(dt)));
        conn.execute(
            "INSERT OR REPLACE INTO file_manifest \
             (path, base_dir, mtime_ns, size_bytes, earliest_ts, schema_version, ingested_at) \
             VALUES (?, ?, ?, ?, ?, ?, current_localtimestamp())",
            duckdb::params![
                path.as_str(),
                path_to_str(&pf.base_dir).as_str(),
                pf.mtime_ns,
                pf.size_bytes,
                earliest.unwrap_or(Value::Null),
                SCHEMA_VERSION,
            ],
        )?;
        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

fn path_to_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Read a nullable TIMESTAMP column as Option<i64> micros.
fn ts_opt_from_row(r: &duckdb::Row, idx: usize) -> duckdb::Result<Option<i64>> {
    use duckdb::types::ValueRef;
    match r.get_ref(idx)? {
        ValueRef::Null => Ok(None),
        ValueRef::Timestamp(TimeUnit::Microsecond, v) => Ok(Some(v)),
        ValueRef::Timestamp(unit, v) => Ok(Some(normalize_ts(unit, v))),
        ValueRef::BigInt(v) => Ok(Some(v)),
        _ => Ok(None),
    }
}

/// Read a non-null TIMESTAMP column as i64 micros.
fn ts_i64_from_row(r: &duckdb::Row, idx: usize) -> duckdb::Result<i64> {
    use duckdb::types::ValueRef;
    match r.get_ref(idx)? {
        ValueRef::Timestamp(TimeUnit::Microsecond, v) => Ok(v),
        ValueRef::Timestamp(unit, v) => Ok(normalize_ts(unit, v)),
        ValueRef::BigInt(v) => Ok(v),
        other => Err(duckdb::Error::FromSqlConversionFailure(
            idx,
            other.data_type(),
            "expected TIMESTAMP".into(),
        )),
    }
}

fn normalize_ts(unit: TimeUnit, v: i64) -> i64 {
    match unit {
        TimeUnit::Second => v.saturating_mul(1_000_000),
        TimeUnit::Millisecond => v.saturating_mul(1_000),
        TimeUnit::Microsecond => v,
        TimeUnit::Nanosecond => v / 1_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    /// EMPIRICAL VALIDATION (per ops-lens correction): does `ROLLBACK` undo rows
    /// inserted via the Appender inside a manual transaction? The whole per-file
    /// atomicity design depends on YES. If this ever fails, the Appender must be
    /// replaced with prepared-statement bulk INSERT.
    #[test]
    fn appender_rows_roll_back_with_transaction() {
        let conn = mem_db();
        conn.execute_batch("BEGIN TRANSACTION;").unwrap();
        {
            let mut app = conn.appender("messages").unwrap();
            app.append_row(duckdb::params![
                "/x/file.jsonl",
                0i32,
                Value::Timestamp(TimeUnit::Microsecond, 1_000_000),
                Some("m"),
                Some("m"),
                1u64,
                2u64,
                0u64,
                0u64,
                false,
                Option::<f64>::None,
                Option::<&str>::None,
                Option::<&str>::None,
                Option::<&str>::None,
                "/x/projects",
            ])
            .unwrap();
            app.flush().unwrap();
        }
        conn.execute_batch("ROLLBACK;").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "Appender rows survived ROLLBACK -> per-file txn isolation is broken");
    }

    /// Per-file COMMIT persists; a separate file's ROLLBACK leaves the first intact.
    #[test]
    fn per_file_individual_txn_isolation() {
        let conn = mem_db();
        // File A: commit 2 rows.
        conn.execute_batch("BEGIN TRANSACTION;").unwrap();
        {
            let mut app = conn.appender("messages").unwrap();
            for i in 0..2i32 {
                app.append_row(duckdb::params![
                    "/a.jsonl", i, Value::Timestamp(TimeUnit::Microsecond, 1_000_000),
                    Option::<&str>::None, Option::<&str>::None,
                    1u64, 1u64, 0u64, 0u64, false, Option::<f64>::None,
                    Option::<&str>::None, Option::<&str>::None, Option::<&str>::None, "/p",
                ])
                .unwrap();
            }
            app.flush().unwrap();
        }
        conn.execute_batch("COMMIT;").unwrap();

        // File B: insert then rollback.
        conn.execute_batch("BEGIN TRANSACTION;").unwrap();
        {
            let mut app = conn.appender("messages").unwrap();
            app.append_row(duckdb::params![
                "/b.jsonl", 0i32, Value::Timestamp(TimeUnit::Microsecond, 2_000_000),
                Option::<&str>::None, Option::<&str>::None,
                5u64, 5u64, 0u64, 0u64, false, Option::<f64>::None,
                Option::<&str>::None, Option::<&str>::None, Option::<&str>::None, "/p",
            ])
            .unwrap();
            app.flush().unwrap();
        }
        conn.execute_batch("ROLLBACK;").unwrap();

        let a: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages WHERE path = '/a.jsonl'", [], |r| r.get(0))
            .unwrap();
        let b: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages WHERE path = '/b.jsonl'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(a, 2);
        assert_eq!(b, 0);
    }

    /// TIMESTAMP round-trips DateTime<Utc> losslessly through micros.
    #[test]
    fn timestamp_micros_roundtrip() {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-04-12T08:01:02.345678Z")
            .unwrap()
            .with_timezone(&Utc);
        let micros = dt_to_micros(dt);
        assert_eq!(micros_to_dt(micros), dt);
    }

    /// earliest_timestamp reads ALL lines, including timestamped-but-parse-rejected
    /// ones (no usage). The cache must store this exact value.
    #[test]
    fn earliest_ts_includes_rejected_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("t.jsonl");
        std::fs::write(
            &f,
            concat!(
                r#"{"timestamp":"2026-04-10T07:00:00.000Z","type":"summary"}"#, "\n",
                r#"{"type":"assistant","timestamp":"2026-04-12T08:00:00.000Z","requestId":"r1","message":{"id":"m1","model":"x","usage":{"input_tokens":1,"output_tokens":2}}}"#, "\n",
            ),
        )
        .unwrap();
        // The all-lines scan must pick the rejected line's earlier timestamp.
        let direct = crate::aggregations::earliest_timestamp(&f).unwrap();
        assert_eq!(
            direct,
            chrono::DateTime::parse_from_rfc3339("2026-04-10T07:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    /// schema_version bump forces a full reparse: prime, set manifest schema_version
    /// stale, re-sync, assert all rows were re-ingested (ingested_at advanced).
    #[test]
    fn stale_schema_version_forces_reparse() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let dir = base.join("projects/-p/s");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("t.jsonl"),
            concat!(r#"{"type":"assistant","timestamp":"2026-04-12T08:00:00.000Z","requestId":"r1","message":{"id":"m1","model":"x","usage":{"input_tokens":1,"output_tokens":2}}}"#, "\n"),
        )
        .unwrap();

        let discovered = vec![DiscoveredFile {
            path: dir.join("t.jsonl"),
            base_dir: base.join("projects"),
        }];

        let conn = mem_db();
        let ev1 = sync_and_load(&conn, &discovered, false).unwrap();
        assert_eq!(ev1.len(), 1);
        let ingested_1: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_manifest WHERE schema_version = ?", duckdb::params![SCHEMA_VERSION], |r| r.get(0))
            .unwrap();
        assert_eq!(ingested_1, 1);

        // Make the manifest schema_version stale.
        conn.execute("UPDATE file_manifest SET schema_version = 1", []).unwrap();

        // Re-sync: the stale row must be re-classified CHANGED and re-parsed back to
        // the current SCHEMA_VERSION.
        let ev2 = sync_and_load(&conn, &discovered, false).unwrap();
        assert_eq!(ev2.len(), 1);
        let restamped: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_manifest WHERE schema_version = ?", duckdb::params![SCHEMA_VERSION], |r| r.get(0))
            .unwrap();
        assert_eq!(restamped, 1, "stale schema_version row was not re-parsed");
    }

    /// Cross-base byte-identical duplicate (same msg_id:request_id in two files):
    /// the cache stores BOTH copies (no UNIQUE on dedup_key); dedup happens in mod.rs.
    /// Here we assert the cache returns both, in correct event-arrival order, so the
    /// downstream dedup can pick the first.
    #[test]
    fn cross_base_dupes_both_cached_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let base_a = tmp.path().join("a");
        let base_b = tmp.path().join("b");
        let da = base_a.join("projects/-p/s");
        let db = base_b.join("projects/-p/s");
        std::fs::create_dir_all(&da).unwrap();
        std::fs::create_dir_all(&db).unwrap();
        let dup = concat!(r#"{"type":"assistant","timestamp":"2026-04-12T08:00:00.000Z","requestId":"r1","message":{"id":"m1","model":"x","usage":{"input_tokens":1,"output_tokens":2}}}"#, "\n");
        std::fs::write(da.join("t.jsonl"), dup).unwrap();
        std::fs::write(db.join("t.jsonl"), dup).unwrap();

        let discovered = vec![
            DiscoveredFile { path: da.join("t.jsonl"), base_dir: base_a.join("projects") },
            DiscoveredFile { path: db.join("t.jsonl"), base_dir: base_b.join("projects") },
        ];
        let conn = mem_db();
        let evs = sync_and_load(&conn, &discovered, false).unwrap();
        assert_eq!(evs.len(), 2, "both cross-base copies must be cached (dedup is downstream)");
        // Both have the same dedup components.
        for e in &evs {
            assert_eq!(e.msg_id.as_deref(), Some("m1"));
            assert_eq!(e.request_id.as_deref(), Some("r1"));
        }
    }

    /// Read-only mode skips parse/upsert and serves whatever is already in the DB.
    #[test]
    fn read_only_skips_ingest() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let dir = base.join("projects/-p/s");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("t.jsonl"),
            concat!(r#"{"type":"assistant","timestamp":"2026-04-12T08:00:00.000Z","requestId":"r1","message":{"id":"m1","model":"x","usage":{"input_tokens":1,"output_tokens":2}}}"#, "\n"),
        )
        .unwrap();
        let discovered = vec![DiscoveredFile {
            path: dir.join("t.jsonl"),
            base_dir: base.join("projects"),
        }];

        let conn = mem_db();
        // Read-only on an EMPTY db: must skip ingest, return nothing.
        let evs = sync_and_load(&conn, &discovered, true).unwrap();
        assert_eq!(evs.len(), 0, "read-only mode must not ingest");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);

        // Now write (read-write), then read-only serves the warm state.
        let _ = sync_and_load(&conn, &discovered, false).unwrap();
        let evs_ro = sync_and_load(&conn, &discovered, true).unwrap();
        assert_eq!(evs_ro.len(), 1);
    }
}
