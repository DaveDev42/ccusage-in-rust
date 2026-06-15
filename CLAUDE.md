# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`ccusage-rs` is a drop-in Rust reimplementation of upstream Node.js [ccusage](https://github.com/ryoppippi/ccusage) with **bit-exact JSON parity**. The contract: for any `(subcommand, flags, ~/.claude tree)`, our `--json` output must equal upstream's byte-for-byte (modulo a few documented extra fields like `totalTokens` that we add). Behavioral divergence from upstream is a bug, not a feature — even when upstream looks "wrong".

The CLI surface, flag parsing semantics, dedup logic, date filtering rules, model-name normalization, and pricing fallback chain are all mirrored from upstream. When in doubt, read the upstream source under `third_party/ccusage/` and match it.

## Common commands

```sh
# Build (release; first build downloads LiteLLM pricing snapshot into data/)
cargo build --release

# Unit + fixture tests (16 fixture cases under tests/compat.rs)
cargo test --all-targets

# Run a single test
cargo test --test compat single_session_daily_desc

# Regenerate fixture baselines after upstream version bump
#   requires `npm i -g ccusage` and `jq`
./scripts/regen-fixtures.sh

# Cross-language parity suite (62 cases against upstream's TS loader)
#   requires pnpm; initializes third_party/ccusage submodule on first run
tests/parity/run.sh

# Lint / format (these gate CI)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings

# Live diff against an installed ccusage binary
diff <(ccusage    daily --json --timezone Asia/Seoul --offline | jq -S .) \
     <(ccusage-rs daily --json --timezone Asia/Seoul --offline | jq -S .)

# Benchmark vs upstream (real ~/.claude data)
bench/run.sh
CCUSAGE_DATA_DIR=/path/to/.claude bench/run.sh
```

## Build-time pricing snapshot

`build.rs` ensures `data/litellm-bundled.json` exists at build time. The file is `include_str!`'d into the binary as a fallback so `--offline` works without network. Behavior:

- If the file is missing, `build.rs` fetches it from LiteLLM at compile time.
- `CCUSAGE_RS_REFRESH_PRICING=1 cargo build` forces a refresh.
- `CCUSAGE_RS_OFFLINE_BUILD=1` (set in CI) forbids any network fetch and panics if the snapshot is absent.

Three-tier pricing fallback at runtime: live LiteLLM fetch → on-disk cache (24h TTL) → bundled snapshot. `--offline` skips the live fetch.

## Architecture

The pipeline is **discover → parse → cost → aggregate → render**, and each stage lives in its own module:

```
src/
├── cli.rs              clap definitions for all subcommands + flag-to-LoadOptions translation
├── discover.rs         CLAUDE_CONFIG_DIR resolution, jsonl walk, project-name extraction
├── parse.rs            jsonl line → UsageEvent, with INTRA-file dedup by (message.id, requestId)
├── cache.rs            embedded-DuckDB incremental parsed-row cache + file manifest (I/O only)
├── pricing.rs          LiteLLM pricing fetch (live → disk cache → bundled), cost calculation
├── timezone.rs         IANA tz handling, ts parsing, YYYY-MM-DD bucketing
├── jq.rs               --jq passthrough (shells out to system jq)
├── aggregations/
│   ├── mod.rs          LoadOptions, EventWithCost, load_all_events, date_in_range
│   ├── daily.rs        per-day buckets
│   ├── weekly.rs       per-week buckets (configurable start-of-week)
│   ├── monthly.rs      per-month buckets
│   ├── session.rs      per-(project, session) rollup
│   ├── session_by_id.rs `session --id <id>` single-session detail
│   └── blocks.rs       5-hour billing block reconstruction with active-block projection
└── output/
    ├── json.rs         serde structs that exactly mirror upstream JSON shapes
    └── table.rs        comfy-table renderer for human mode
```

Key invariants to preserve when modifying:

- **Dedup happens once, globally**, keyed by `messageId:requestId`, with first-occurrence-in-global-order winning (matches upstream). Files are sorted by earliest timestamp first. As of the DuckDB cache, dedup is split: `parse_file` keeps an INTRA-file throwaway `HashSet` (so same-file dups drop at parse time, order-independent and safe to cache), and the CROSS-file dedup is replayed in `aggregations::load_all_events` over the ordered read-back with the shared `seen: HashSet<String>`. Net outcome is identical to the old single shared HashSet across the file loop.
- **Filter order matters**: project filter → file walk → per-line parse → dedup → cost compute → date filter → aggregate. Reordering changes results because date filtering applies to the aggregation key (e.g. `last_activity` for sessions), not the raw event timestamp.
- **`--mode` controls the cost source**: `display` reads `costUSD` from the line and never calls the pricer; `calculate` always recomputes from tokens; `auto` (default) prefers `costUSD`, falls back to recompute.
- **Synthetic models** (e.g. `<synthetic>`) are excluded from `models_used` and breakdowns but their tokens still count toward totals.
- **`session.rs` hardcodes desc order** even though the shared `--order` flag exists. This is intentional CLI parity — upstream's `commands/session.ts` strips `order` before delegating, so the CLI form always sorts desc. The library function honors `order`, but we are a CLI drop-in. Don't "fix" this without checking the upstream `commands/*.ts` thin wrappers.
- **Blocks projection requires a deterministic `now`** — `--now <ISO8601>` or `CCUSAGE_RS_NOW` overrides wall-clock so tests/benches can be reproducible.

## Incremental cache (`cache.rs`)

`load_all_events`'s file loop is backed by an embedded DuckDB at `$CCUSAGE_RS_CACHE_DIR/cache.duckdb` (default `dirs::cache_dir()/ccusage-rs/cache.duckdb`). DuckDB is **purely an I/O cache** — it stores the parsed rows `parse_file` emits (PRE cross-file-dedup, PRE cost) plus a `(path, mtime_ns, size_bytes, earliest_ts, schema_version)` manifest. **No SQL aggregation, SUM, or GROUP BY ever runs against it.** All aggregation, float-sum, dedup, cost, model ordering, and JSON serialization stay 100% Rust-native, so output is byte-identical to the non-cached path (proven by the cold/warm/touch matrix in `tests/cache.rs` + the real-pool harness).

Per run: discover (full file list, never project-filtered) → stat → diff vs manifest → rayon-parse only CHANGED/NEW files (`parse_file` + the promoted top-level `earliest_timestamp`) → per-file INDIVIDUAL transaction upsert (DuckDB has NO SAVEPOINT) → reconstruct the exact two-phase file order in Rust (per-base OsStr from `discover_jsonl_files`, then `file_order_cmp` over cached `earliest_ts` — never SQL `ORDER BY`) → ONE bulk `SELECT * FROM messages`, ordered in Rust by `(file_position, line_index)`. The `--project`/`--instances` filter is applied in Rust at read-back, never by scoping the discovered file list (that would evict the cache). DELETED is scoped to this run's bases.

Invariants:
- **`SCHEMA_VERSION` is a compile-time FNV-1a hash of `src/parse.rs` + `src/cache.rs`** (emitted by `build.rs` as `CCUSAGE_RS_SCHEMA_VERSION`, masked to 63 bits → fits `BIGINT`). Any edit to the parsed-row emission re-classifies every manifest row as CHANGED → full reparse. No human step. (This is why editing `cache.rs` invalidates an existing cache — expected.)
- **`ts` is a DuckDB `TIMESTAMP`** (micros), round-tripping `DateTime<Utc>` losslessly; all output instants still flow through `blocks.rs::iso_string()` (`%.3fZ`). No raw-string timestamp column. `session_by_id.rs` is NOT cache-backed (keeps its raw-string `timestamp` and no-dedup semantics — unchanged).
- **`open_db` tries read-write; on a file-lock conflict it falls back to read-only** (serve warm state, skip ingest), and if even read-only is blocked it uses an ephemeral in-memory DB (correct, unpersisted — never deletes a locked file). Non-lock open failure (corruption / on-disk format change, guarded by the `meta.duckdb_crate_version` marker) → delete + cold rebuild.
- **Escape hatches**: `CCUSAGE_RS_FORCE_RESCAN=1` treats all files as CHANGED; deleting `cache.duckdb` cold-rebuilds. `CCUSAGE_RS_DEBUG_TIME=1` prints per-phase timings to stderr (never affects stdout). Deleting the cache does NOT reset `litellm.json` (pricing) — costs after a cold rebuild match the current pricing snapshot, same non-determinism as today under live pricing.

## Testing strategy

There are three layers, in order of granularity:

1. **`tests/compat.rs`** — 16 cases across 2 fixtures × 4 commands × 2 orders. Compares our JSON against `tests/fixtures/*/expected/*.json` baselines pre-recorded from a known ccusage version (currently 18.0.11). Fast; runs in CI on every push.

2. **`tests/parity/`** — black-box suite that mounts our `parity.test.ts` into the upstream submodule's `src/` tree, calls upstream's TypeScript loaders directly for the expected value, spawns our binary for the actual, and deep-compares shared keys. 62 cases covering edge behaviors (timezone boundaries, date filters, dedup, synthetic models, etc.). Requires `pnpm` and the `third_party/ccusage` submodule. Runs in CI in a separate `parity` job.

3. **`bench/run.sh`** — wall-clock + peak-RSS benchmark vs upstream. Not a correctness test; informational only. Raw artifacts land under `bench/results/<timestamp>/`.

When a parity test fails, the question is always: did we drift, or did upstream change semantics? Bump the submodule (`git submodule update --remote third_party/ccusage`) and re-run before assuming it's our bug.

When fixture-baseline tests fail after a deliberate upstream-tracking change, regenerate baselines via `scripts/regen-fixtures.sh` rather than editing the JSON files by hand.

## Working with upstream

`third_party/ccusage` is a git submodule pinned to upstream `main`. It is **read-only** from our perspective — never edit files there. The submodule's role is purely to provide:

- TypeScript loaders for the parity harness to call.
- A reference for matching upstream behavior when investigating divergences.

License attribution: this repo is BSD-3-Clause; the upstream MIT license text lives at `LICENSES/ccusage-MIT.txt`. The single file we adapted from upstream (`tests/parity/parity.test.ts`) is documented in `tests/parity/README.md`.

## Things we explicitly don't do

- **Don't add features upstream doesn't have.** This is a port, not a fork. New flags/output fields break the parity contract.
- **Don't refactor for "cleanliness" if it changes JSON output.** Field ordering matters (we use `serde_json` with `preserve_order` and `indexmap` for a reason).
- **Don't normalize floats.** Upstream's accumulated rounding errors are part of the contract; we match them, including the cases where they look wrong.
- **Don't implement the not-yet-ported list** without checking if it requires a new behavioral surface: `--id` ✅ done, `--jq` ✅ done, weekly ✅ done. Remaining: table compact mode, `--debug`/`--mismatch` reporting.
