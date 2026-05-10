#!/usr/bin/env bash
# Benchmark ccusage-rs vs upstream Node ccusage on a Claude data directory.
#
# Inputs:
#   CCUSAGE_DATA_DIR    Path to a Claude data directory (default: $HOME/.claude).
#                       Both binaries are invoked with CLAUDE_CONFIG_DIR set to
#                       this value; the inherited CLAUDE_CONFIG_DIR is ignored.
#   RUST_BIN            ccusage-rs binary path (default: target/release/ccusage-rs)
#   NODE_BIN            ccusage CLI path (default: $(command -v ccusage))
#   OUT_DIR             Where to write results (default: bench/results/<timestamp>)
#   WARMUP              hyperfine warmup runs (default: 2)
#   RUNS                hyperfine measurement runs (default: 8)
#
# Output:
#   $OUT_DIR/env.txt        host/toolchain metadata
#   $OUT_DIR/<cmd>.json     hyperfine raw json per command
#   $OUT_DIR/<cmd>.md       hyperfine markdown table per command
#   $OUT_DIR/rss.tsv        peak RSS (KB) per command per binary
#   $OUT_DIR/summary.md     aggregated table

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Defensively ignore inherited CLAUDE_CONFIG_DIR; require explicit DATA_DIR.
DATA_DIR="${CCUSAGE_DATA_DIR:-$HOME/.claude}"
unset CLAUDE_CONFIG_DIR

RUST_BIN="${RUST_BIN:-$REPO_ROOT/target/release/ccusage-rs}"
NODE_BIN="${NODE_BIN:-$(command -v ccusage || true)}"
WARMUP="${WARMUP:-2}"
RUNS="${RUNS:-8}"
TS="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/bench/results/$TS}"
NOW="2099-01-01T00:00:00Z"

if [[ -z "$NODE_BIN" || ! -x "$NODE_BIN" ]]; then
  echo "error: ccusage (Node) not on PATH; npm i -g ccusage" >&2
  exit 1
fi
if [[ ! -d "$DATA_DIR/projects" ]]; then
  echo "error: $DATA_DIR/projects does not exist; set CCUSAGE_DATA_DIR" >&2
  exit 1
fi
if [[ ! -x "$RUST_BIN" ]]; then
  echo "[bench] building ccusage-rs (release)"
  (cd "$REPO_ROOT" && cargo build --release)
fi
if ! command -v hyperfine >/dev/null 2>&1; then
  echo "error: hyperfine not on PATH; brew install hyperfine" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

JSONL_COUNT="$(find "$DATA_DIR/projects" -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"
DATA_SIZE="$(du -sh "$DATA_DIR/projects" 2>/dev/null | awk '{print $1}')"

# ── Environment snapshot ──────────────────────────────────────────────────────
{
  echo "# bench env"
  echo "timestamp:        $TS"
  echo "host:             $(uname -srm)"
  echo "cpu:              $(sysctl -n machdep.cpu.brand_string 2>/dev/null || lscpu | grep -i 'model name' | head -1 || echo unknown)"
  echo "data_dir:         $DATA_DIR"
  echo "jsonl_count:      $JSONL_COUNT"
  echo "data_size:        $DATA_SIZE"
  echo "rust_bin:         $RUST_BIN"
  echo "rust_version:     $($RUST_BIN --version 2>&1 | head -1)"
  echo "node_bin:         $NODE_BIN"
  echo "node_version:     $($NODE_BIN --version 2>&1 | head -1)"
  echo "node_runtime:     $(node --version 2>&1 | head -1)"
  echo "hyperfine:        $(hyperfine --version)"
  echo "warmup:           $WARMUP"
  echo "runs:             $RUNS"
} > "$OUT_DIR/env.txt"
echo "[bench] env saved to $OUT_DIR/env.txt"
cat "$OUT_DIR/env.txt"
echo

COMMON_ARGS=(--json --offline --timezone Asia/Seoul)

# ── Time benchmarks ───────────────────────────────────────────────────────────
# `--ignore-failure` lets transient non-zero exits (e.g. the rare valibot parse
# error in upstream's data-loader on edge-case lines) not blow up the whole run.
# `--show-output` is intentionally off; output sizes are O(MB) and would
# dominate runtime. Failures (if any) will surface in the json's exit_codes.
for cmd in daily monthly session blocks; do
  echo "[bench] timing: $cmd"
  hyperfine \
    --warmup "$WARMUP" \
    --runs "$RUNS" \
    --shell=none \
    --ignore-failure \
    --export-json "$OUT_DIR/$cmd.json" \
    --export-markdown "$OUT_DIR/$cmd.md" \
    --command-name "ccusage (Node)    $cmd" \
    "env CLAUDE_CONFIG_DIR=$DATA_DIR $NODE_BIN $cmd ${COMMON_ARGS[*]}" \
    --command-name "ccusage-rs (Rust) $cmd" \
    "env CLAUDE_CONFIG_DIR=$DATA_DIR $RUST_BIN $cmd ${COMMON_ARGS[*]} --now $NOW" \
    >/dev/null
done

# ── Peak RSS ──────────────────────────────────────────────────────────────────
echo "[bench] measuring peak RSS"
{
  printf 'binary\tcmd\tpeak_rss_kb\n'
  for cmd in daily monthly session blocks; do
    for bin_label in "node:$NODE_BIN" "rust:$RUST_BIN"; do
      label="${bin_label%%:*}"
      bin="${bin_label#*:}"
      extra=()
      if [[ "$label" == "rust" ]]; then extra=(--now "$NOW"); fi
      if [[ "$(uname -s)" == "Darwin" ]]; then
        # /usr/bin/time -l prints "<bytes>  maximum resident set size" on stderr
        rss_bytes="$(env CLAUDE_CONFIG_DIR="$DATA_DIR" /usr/bin/time -l \
          "$bin" "$cmd" "${COMMON_ARGS[@]}" "${extra[@]}" 2>&1 >/dev/null \
          | awk '/maximum resident set size/ {print $1}')"
        rss_kb=$(( rss_bytes / 1024 ))
      else
        rss_kb="$(env CLAUDE_CONFIG_DIR="$DATA_DIR" /usr/bin/time -v \
          "$bin" "$cmd" "${COMMON_ARGS[@]}" "${extra[@]}" 2>&1 >/dev/null \
          | awk -F': ' '/Maximum resident set size/ {print $2}')"
      fi
      printf '%s\t%s\t%s\n' "$label" "$cmd" "$rss_kb"
    done
  done
} > "$OUT_DIR/rss.tsv"
column -t -s $'\t' "$OUT_DIR/rss.tsv"

# ── Summary ───────────────────────────────────────────────────────────────────
python3 - "$OUT_DIR" <<'PY' > "$OUT_DIR/summary.md"
import json, os, sys
out_dir = sys.argv[1]
print("# Benchmark summary")
print()
print("Source data:", end=" ")
with open(os.path.join(out_dir, 'env.txt')) as fh:
    env = {}
    for line in fh:
        if ':' in line:
            k, v = line.split(':', 1)
            env[k.strip()] = v.strip()
print(f"{env.get('jsonl_count', '?')} jsonl files, {env.get('data_size', '?')} on disk · "
      f"{env.get('cpu', '?')} · ccusage {env.get('node_version', '?')} on "
      f"node {env.get('node_runtime', '?')} · ccusage-rs {env.get('rust_version', '?')}")
print()
print("| command | ccusage (Node) | ccusage-rs (Rust) | speedup | rust peak RSS | node peak RSS |")
print("|---|---:|---:|---:|---:|---:|")
rss = {}
with open(os.path.join(out_dir, 'rss.tsv')) as fh:
    next(fh)
    for line in fh:
        b, c, kb = line.strip().split('\t')
        rss[(b, c)] = int(kb)
for cmd in ('daily', 'monthly', 'session', 'blocks'):
    p = os.path.join(out_dir, f'{cmd}.json')
    if not os.path.exists(p):
        continue
    data = json.load(open(p))
    by_name = {b['command']: b for b in data['results']}
    node = next(v for k, v in by_name.items() if 'Node' in k)
    rust = next(v for k, v in by_name.items() if 'Rust' in k)
    speedup = node['mean'] / rust['mean'] if rust['mean'] > 0 else float('inf')
    fmt_rss = lambda kb: f"{kb/1024:.1f} MB" if kb >= 1024 else f"{kb} KB"
    print(f"| `{cmd}` | {node['mean']*1000:.1f} ms ± {node['stddev']*1000:.1f} | "
          f"{rust['mean']*1000:.1f} ms ± {rust['stddev']*1000:.1f} | "
          f"**{speedup:.1f}×** | "
          f"{fmt_rss(rss.get(('rust', cmd), 0))} | "
          f"{fmt_rss(rss.get(('node', cmd), 0))} |")
PY

echo
echo "[bench] summary written to $OUT_DIR/summary.md"
cat "$OUT_DIR/summary.md"
