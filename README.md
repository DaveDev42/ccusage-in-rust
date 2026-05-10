# ccusage-rs

Drop-in Rust reimplementation of [ccusage](https://github.com/ryoppippi/ccusage) with bit-exact JSON parity.

`ccusage` is a CLI that summarizes Claude Code token usage by reading the JSONL transcripts under `~/.claude/projects/`. It works great — but on a small VPS each invocation cold-starts Node and pulls in ~1 GB of dependencies, so calling it from a status bar or hook costs more memory than the rest of the shell. `ccusage-rs` is a single static binary that produces the same JSON byte-for-byte, with no Node runtime.

## Status

v1 covers the four main commands: `daily`, `monthly`, `session`, `blocks`. Output is verified bit-exact against ccusage 18.0.11 across both synthetic fixtures and real `~/.claude/projects` trees. Pricing comes from the same LiteLLM snapshot upstream uses, with a bundled fallback so the binary works fully offline.

Not yet ported: `--id` session lookup, weekly/yearly buckets, `--jq`, table compact mode, debug/mismatch reporting.

## Install

```sh
cargo install --path .
# or, after a release:
cargo install ccusage-rs
```

The binary installs as `ccusage-rs`. Symlink or alias to `ccusage` if you want it as a true drop-in.

## Usage

```sh
ccusage-rs daily   --json --timezone Asia/Seoul
ccusage-rs monthly --json --order desc
ccusage-rs session --json --since 20260401
ccusage-rs blocks  --json --offline
```

All upstream flags supported: `--json/-j`, `--since/-s`, `--until/-u`, `--timezone/-z`, `--locale/-l`, `--mode/-m {auto,calculate,display}`, `--order/-o {asc,desc}`, `--offline`, `--breakdown/-b`, `--debug/-d`.

`CLAUDE_CONFIG_DIR` is honored exactly as upstream: comma-separated list of root directories, each containing a `projects/` subtree.

## Parity

`tests/compat.rs` runs against checked-in fixtures with pre-recorded ccusage outputs:

```sh
cargo test --test compat
```

To verify against a live ccusage install:

```sh
diff <(ccusage daily --json --timezone Asia/Seoul --offline | jq -S .) \
     <(ccusage-rs daily --json --timezone Asia/Seoul --offline | jq -S .)
```

To regenerate fixture baselines after upstream changes:

```sh
npm i -g ccusage
./scripts/regen-fixtures.sh
```

## Build

```sh
cargo build --release
```

The first build downloads a LiteLLM pricing snapshot into `data/litellm-bundled.json` and embeds it in the binary. Set `CCUSAGE_RS_REFRESH_PRICING=1` to force a refresh on the next build.

## License

MIT — see `LICENSE`.
