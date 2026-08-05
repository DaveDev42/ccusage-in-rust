#!/usr/bin/env bash
# Run the ccusage-rs parity test suite against upstream ccusage.
#
# What this does:
#   1. Ensures the ccusage submodule is initialized.
#   2. Installs upstream ccusage's pnpm dependencies (catalog/workspace).
#   3. Copies our parity.test.ts into the submodule's src/ tree so it can
#      use upstream's data-loader directly via in-source vitest.
#   4. Builds ccusage-rs in release mode.
#   5. Runs the parity test under TZ=UTC, pointing RUST_BIN at our binary.
#
# Environment:
#   RUST_BIN  Override the ccusage-rs binary path (default: target/release/ccusage-rs)
#
# This script is idempotent and safe to re-run.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SUBMODULE="$REPO_ROOT/third_party/ccusage"
CCUSAGE_APP="$SUBMODULE/apps/ccusage"
PARITY_SRC="$SCRIPT_DIR/parity.test.ts"
PARITY_DST="$CCUSAGE_APP/src/_parity-rs.test.ts"
RUST_BIN="${RUST_BIN:-$REPO_ROOT/target/release/ccusage-rs}"

if [[ ! -d "$SUBMODULE/.git" && ! -f "$SUBMODULE/.git" ]]; then
  echo "[parity] initializing ccusage submodule"
  git -C "$REPO_ROOT" submodule update --init --recursive third_party/ccusage
fi

if [[ ! -d "$CCUSAGE_APP/node_modules" ]]; then
  echo "[parity] installing ccusage dependencies (pnpm)"
  (cd "$SUBMODULE" && pnpm install --frozen-lockfile=false)
fi

echo "[parity] mounting parity.test.ts into submodule"
cp "$PARITY_SRC" "$PARITY_DST"

if [[ ! -x "$RUST_BIN" ]]; then
  echo "[parity] building ccusage-rs (release)"
  (cd "$REPO_ROOT" && cargo build --release)
fi

# Isolate the cache DB, and serialize the test files. Both are load-bearing.
#
# Without this every one of the 62 cases shares the developer's REAL
# ~/Library/Caches/ccusage-rs/cache.duckdb, and vitest runs the two test files in
# parallel. The second ccusage-rs process then cannot take the write lock, falls
# back to read-only (by design — see the open strategy in src/cache.rs), and serves
# a warm state that holds nothing for that case's fresh fixture tempdir. The case
# sees `[]` and fails. Measured flake: 2/62 to 10/62 failures per run, and it
# reproduces on older binaries too, so a failure here was never evidence about the
# change under test. Serialized + isolated it is 62/62 repeatably.
#
# The isolation also keeps the suite from leaving fixture-tempdir rows behind in the
# real cache, which is exactly the orphan accumulation retention has to clean up.
echo "[parity] running vitest"
cd "$CCUSAGE_APP"
PARITY_CACHE_DIR="$(mktemp -d)"
trap 'rm -rf "$PARITY_CACHE_DIR"' EXIT
TZ=UTC RUST_BIN="$RUST_BIN" CCUSAGE_RS_CACHE_DIR="$PARITY_CACHE_DIR" \
  pnpm exec vitest run src/_parity-rs.test.ts --no-file-parallelism
