//! Parity tests against pre-recorded ccusage outputs.
//!
//! Each fixture under `tests/fixtures/<name>/` contains:
//!   - `projects/...jsonl` — the synthetic claude config tree
//!   - `expected/<cmd>-<order>.json` — the bit-exact JSON ccusage produced
//!
//! These tests do not require Node/ccusage to be installed; CI regenerates
//! the `expected/` files via `scripts/regen-fixtures.sh`.
//!
//! Run with: `cargo test --test compat`

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push(if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    });
    p.push("ccusage-rs");
    p
}

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

fn run(fixture_name: &str, cmd: &str, order: &str) -> String {
    let out = Command::new(binary())
        .env("CLAUDE_CONFIG_DIR", fixture(fixture_name))
        .args([
            cmd,
            "--json",
            "--timezone",
            "Asia/Seoul",
            "--offline",
            "--order",
            order,
        ])
        .output()
        .expect("run ccusage-rs");
    assert!(
        out.status.success(),
        "ccusage-rs {cmd} failed:\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn expected(fixture_name: &str, cmd: &str, order: &str) -> Option<String> {
    let mut p = fixture(fixture_name);
    p.push("expected");
    p.push(format!("{cmd}-{order}.json"));
    std::fs::read_to_string(p).ok()
}

fn assert_parity(fixture_name: &str, cmd: &str, order: &str) {
    let Some(exp) = expected(fixture_name, cmd, order) else {
        eprintln!(
            "skipping {fixture_name}/{cmd}/{order}: no expected file (run scripts/regen-fixtures.sh)"
        );
        return;
    };
    let got = run(fixture_name, cmd, order);
    let exp_v: serde_json::Value = serde_json::from_str(&exp).expect("parse expected");
    let got_v: serde_json::Value = serde_json::from_str(&got).expect("parse got");
    assert_eq!(
        exp_v, got_v,
        "{fixture_name}/{cmd}/{order} parity mismatch.\nExpected:\n{exp}\nGot:\n{got}"
    );
}

#[test]
fn single_session_daily_desc() {
    assert_parity("single_session", "daily", "desc");
}

#[test]
fn single_session_daily_asc() {
    assert_parity("single_session", "daily", "asc");
}

#[test]
fn single_session_monthly_desc() {
    assert_parity("single_session", "monthly", "desc");
}

#[test]
fn single_session_monthly_asc() {
    assert_parity("single_session", "monthly", "asc");
}

#[test]
fn single_session_session_desc() {
    assert_parity("single_session", "session", "desc");
}

#[test]
fn single_session_session_asc() {
    assert_parity("single_session", "session", "asc");
}

#[test]
fn single_session_blocks_desc() {
    assert_parity("single_session", "blocks", "desc");
}

#[test]
fn single_session_blocks_asc() {
    assert_parity("single_session", "blocks", "asc");
}

#[test]
fn gap_daily_desc() {
    assert_parity("multi_session_with_gap", "daily", "desc");
}

#[test]
fn gap_daily_asc() {
    assert_parity("multi_session_with_gap", "daily", "asc");
}

#[test]
fn gap_monthly_desc() {
    assert_parity("multi_session_with_gap", "monthly", "desc");
}

#[test]
fn gap_monthly_asc() {
    assert_parity("multi_session_with_gap", "monthly", "asc");
}

#[test]
fn gap_session_desc() {
    assert_parity("multi_session_with_gap", "session", "desc");
}

#[test]
fn gap_session_asc() {
    assert_parity("multi_session_with_gap", "session", "asc");
}

#[test]
fn gap_blocks_desc() {
    assert_parity("multi_session_with_gap", "blocks", "desc");
}

#[test]
fn gap_blocks_asc() {
    assert_parity("multi_session_with_gap", "blocks", "asc");
}
