#!/usr/bin/env bash
# Regenerate expected/*.json from upstream ccusage for every fixture × command × order.
# Requires: `npm i -g ccusage` and `jq` on PATH.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v ccusage >/dev/null 2>&1; then
  echo "error: ccusage not on PATH (npm i -g ccusage)" >&2
  exit 1
fi

CCUSAGE_VERSION="$(ccusage --version 2>/dev/null || echo unknown)"
echo "Regenerating fixtures using ccusage $CCUSAGE_VERSION"

for fixture in tests/fixtures/*/; do
  name="$(basename "$fixture")"
  mkdir -p "$fixture/expected"
  for cmd in daily monthly session blocks; do
    for order in desc asc; do
      out="$fixture/expected/$cmd-$order.json"
      CLAUDE_CONFIG_DIR="$PWD/$fixture" ccusage "$cmd" \
        --json --timezone Asia/Seoul --offline --order "$order" \
        > "$out"
      echo "  wrote $out"
    done
  done
done

echo "Done. ccusage version baked into fixtures: $CCUSAGE_VERSION"
