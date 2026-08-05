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
    // Any edit to the parsed-row emission (src/parse.rs) or the cache column layout
    // (src/cache.rs) changes this hash, which classifies every cached manifest row as
    // CHANGED -> a full re-parse. Eliminates the hand-maintained-int forget hazard.
    // Masked to 63 bits so it fits DuckDB's signed BIGINT (and Rust's i64 SCHEMA_VERSION).
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

    // FNV-1a over the concatenated source bytes of both files.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for p in [&parse_rs, &cache_rs] {
        if let Ok(bytes) = fs::read(p) {
            for b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
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
