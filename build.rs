use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bundled_path = manifest_dir.join("data").join("litellm-bundled.json");

    println!("cargo:rerun-if-changed=data/litellm-bundled.json");
    println!("cargo:rerun-if-env-changed=CCUSAGE_RS_REFRESH_PRICING");
    println!("cargo:rerun-if-env-changed=CCUSAGE_RS_OFFLINE_BUILD");

    // ---- Compile-time schema-version hash ----
    // Hashes src/parse.rs (which decides the row VALUES) plus the marker-delimited row
    // contract in src/cache.rs (which decides the row SHAPE and the field<->column
    // mapping). A change to either classifies every cached manifest row as CHANGED -> a
    // full re-parse, so it eliminates the hand-maintained-int forget hazard.
    // Masked to 63 bits so it fits DuckDB's signed BIGINT (and Rust's i64 SCHEMA_VERSION).
    //
    // It deliberately does NOT hash all of cache.rs. That was the original design and it
    // was far too broad: a comment or an added helper invalidated every cached row, which
    // on the 8 GB hub means re-reading ~9.6 GiB of JSONL across ~29k files for a change
    // that cannot affect a single stored byte. Measured: a pure retention/memory patch
    // triggered exactly that.
    emit_schema_version(&manifest_dir);

    let offline = env::var("CCUSAGE_RS_OFFLINE_BUILD").is_ok();

    if env::var("CCUSAGE_RS_REFRESH_PRICING").is_ok() && !offline {
        if let Err(err) = refresh_pricing(&bundled_path) {
            println!("cargo:warning=Failed to refresh LiteLLM pricing: {err}");
        }
    }

    if !bundled_path.exists() {
        if offline {
            panic!(
                "data/litellm-bundled.json missing and CCUSAGE_RS_OFFLINE_BUILD is set; \
                 commit a snapshot or unset the variable."
            );
        }
        if let Err(err) = refresh_pricing(&bundled_path) {
            panic!(
                "data/litellm-bundled.json missing and could not fetch from LiteLLM: {err}. \
                 Run with network access or commit a snapshot."
            );
        }
    }
}

fn emit_schema_version(manifest_dir: &Path) {
    let parse_rs = manifest_dir.join("src").join("parse.rs");
    let cache_rs = manifest_dir.join("src").join("cache.rs");
    println!("cargo:rerun-if-changed=src/parse.rs");
    println!("cargo:rerun-if-changed=src/cache.rs");

    // FNV-1a over parse.rs in full, then only the row-contract regions of cache.rs.
    // Both reads panic on failure: silently hashing nothing would leave the guard
    // vacuous, which is worse than a broken build (stale rows would be served as fresh).
    let parse_src = fs::read(&parse_rs)
        .unwrap_or_else(|e| panic!("schema hash: cannot read {}: {e}", parse_rs.display()));
    let cache_src = fs::read_to_string(&cache_rs)
        .unwrap_or_else(|e| panic!("schema hash: cannot read {}: {e}", cache_rs.display()));
    let contract = extract_row_contract(&cache_src, &cache_rs);

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for bytes in [parse_src.as_slice(), contract.as_bytes()] {
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    // Mask to 63 bits -> always fits signed i64 / DuckDB BIGINT.
    let masked = hash & 0x7fff_ffff_ffff_ffff;
    println!("cargo:rustc-env=CCUSAGE_RS_SCHEMA_VERSION={masked}");

    // Emit the resolved duckdb crate version for the meta on-disk-format guard.
    // (A duckdb minor bump can change the file format and refuse to open an old
    // cache.duckdb; the meta marker forces a cold rebuild instead of an error.)
    let duckdb_ver = resolve_duckdb_version(manifest_dir).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rustc-env=CCUSAGE_RS_DUCKDB_VERSION={duckdb_ver}");
}

const HASH_BEGIN: &str = "SCHEMA-HASH-BEGIN";
const HASH_END: &str = "SCHEMA-HASH-END";

/// Collect the marker-delimited "row contract" regions of cache.rs, in file order.
///
/// A region must enclose anything that decides what a stored row MEANS: the table DDL,
/// the writer's positional column list, and the reader's column list plus its index
/// mapping. Anything outside the markers — helpers, logging, retention, tests, prose —
/// cannot invalidate a cached row and so must not churn the hash.
///
/// Marker lines themselves are excluded, so relabelling a region is free. The bytes
/// INSIDE a region are hashed verbatim, comments included: keep explanation outside the
/// markers, because editing a comment within one costs every machine a full re-parse.
///
/// Every failure mode panics rather than degrading. An empty contract would silently
/// weaken the guard to "parse.rs only", and a stale row served as fresh is a wrong
/// answer, which is strictly worse than a build that stops.
fn extract_row_contract(src: &str, path: &Path) -> String {
    let mut out = String::new();
    let mut open = false;
    let mut regions = 0usize;

    for (n, line) in src.lines().enumerate() {
        let lineno = n + 1;
        if line.contains(HASH_BEGIN) {
            if open {
                panic!(
                    "schema hash: nested {HASH_BEGIN} at {}:{lineno}",
                    path.display()
                );
            }
            open = true;
            regions += 1;
            continue;
        }
        if line.contains(HASH_END) {
            if !open {
                panic!(
                    "schema hash: unmatched {HASH_END} at {}:{lineno}",
                    path.display()
                );
            }
            open = false;
            continue;
        }
        if open {
            out.push_str(line);
            out.push('\n');
        }
    }

    if open {
        panic!(
            "schema hash: unterminated {HASH_BEGIN} in {}",
            path.display()
        );
    }
    if regions == 0 || out.trim().is_empty() {
        panic!(
            "schema hash: no row-contract regions found in {} — the cache-invalidation \
             guard would be vacuous, so refusing to build",
            path.display()
        );
    }
    out
}

/// Best-effort: extract the resolved `duckdb` version from Cargo.lock.
fn resolve_duckdb_version(manifest_dir: &Path) -> Option<String> {
    let lock = fs::read_to_string(manifest_dir.join("Cargo.lock")).ok()?;
    let mut in_duckdb = false;
    for line in lock.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            in_duckdb = false;
        } else if t == "name = \"duckdb\"" {
            in_duckdb = true;
        } else if in_duckdb && t.starts_with("version = ") {
            return Some(
                t.trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string(),
            );
        }
    }
    None
}

fn refresh_pricing(target: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let body = client.get(LITELLM_URL).send()?.error_for_status()?.text()?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(target, body)?;
    Ok(())
}
