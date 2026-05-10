# Parity test harness

Black-box equivalence test suite that verifies `ccusage-rs` produces the
same JSON output as upstream Node.js [ccusage](https://github.com/ryoppippi/ccusage)
across daily / monthly / weekly / session / session-by-id / blocks commands.

## How it works

Each test:

1. Builds a fixture jsonl tree with `fs-fixture`.
2. Calls upstream's TypeScript loader (`loadDailyUsageData` etc.) — this is
   the **expected** value.
3. Spawns `ccusage-rs` with `CLAUDE_CONFIG_DIR=fixture.path` and parses its
   `--json` stdout — this is the **actual** value.
4. Deep-compares only the shared keys (ccusage-rs emits a few extra fields
   like `totalTokens` that upstream loaders don't — they are stripped).

To get upstream's TypeScript loader, we use `third_party/ccusage` (a git
submodule) directly. The runner script mounts this directory's
`parity.test.ts` into the submodule's `src/` and runs vitest there, so
the in-source vitest pattern and pnpm catalog dependencies upstream relies
on continue to work without us having to mirror them.

## Running

Prereqs: `pnpm`, `cargo`, `git`.

```sh
# from the repo root
tests/parity/run.sh
```

The script is idempotent: it initializes the submodule, installs upstream's
dependencies, copies `parity.test.ts` into place, builds `ccusage-rs` if
needed, and runs vitest under `TZ=UTC`.

To use a custom `ccusage-rs` binary:

```sh
RUST_BIN=/path/to/ccusage-rs tests/parity/run.sh
```

## Updating against upstream

```sh
git submodule update --remote third_party/ccusage
git -C third_party/ccusage checkout main
git add third_party/ccusage
git commit -m "Bump ccusage submodule"
```

Then re-run `tests/parity/run.sh`. If a test starts failing, either
ccusage-rs has drifted or upstream has changed semantics — investigate
which.

## License

`parity.test.ts` was originally adapted from ccusage upstream
(MIT, © 2025 ryoppippi). See [`LICENSES/ccusage-MIT.txt`](../../LICENSES/ccusage-MIT.txt)
for the upstream license text. The rest of this directory is BSD-3-Clause
along with the rest of ccusage-rs.
