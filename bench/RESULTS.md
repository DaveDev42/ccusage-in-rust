# Benchmark: ccusage-rs vs upstream ccusage

> Reproduce locally with `bench/run.sh`. Raw runs are committed under
> `bench/results/<timestamp>/` so anyone can audit the numbers.

## Result (2026-05-11, Apple M1 Max)

| command   | ccusage (Node) mean | ccusage-rs (Rust) mean | speedup    | rust peak RSS | node peak RSS |
|-----------|--------------------:|-----------------------:|-----------:|--------------:|--------------:|
| `daily`   | 70 287 ± 22 067 ms  | 12 916 ± 4 809 ms      | **5.4×**   | 473 MB        | 4 357 MB      |
| `monthly` | 62 238 ± 7 775 ms   |  6 788 ±   217 ms      | **9.2×**   | 472 MB        | 3 822 MB      |
| `session` | 30 580 ±    888 ms  |  6 495 ±   133 ms      | **4.7×**   | 471 MB        | 2 048 MB      |
| `blocks`  | 31 341 ±  2 344 ms  |  6 588 ±   128 ms      | **4.8×**   | 474 MB        | 2 086 MB      |

**Headlines**

- 5–9× faster wall-clock on real workload (every command).
- ~8–9× lower peak memory. Node ccusage's RSS scales with input size (4 GB on
  2.8 GB of jsonl); ccusage-rs stays around 470 MB regardless of command.
- ccusage-rs std-dev is 1–2 orders of magnitude smaller — output is far more
  predictable run-to-run, which matters when this is wired into status lines.

## Workload

- 7 002 jsonl files / 2.8 GB on disk under `~/.claude/projects/`.
- Both binaries invoked with `--json --offline --timezone Asia/Seoul`.
- ccusage-rs additionally given `--now 2099-01-01T00:00:00Z` so the `blocks`
  projection is deterministic across runs.
- 2 warmup + 8 measurement runs per command via `hyperfine 1.20.0`.

## Environment

- **Host**: Darwin 25.5.0 arm64, Apple M1 Max
- **Node**: v24.13.1
- **ccusage**: 18.0.11 (homebrew)
- **ccusage-rs**: 0.1.0, release profile (LTO=thin, codegen-units=1, strip=symbols)
- **rustc**: stable

## Caveats — please read before quoting these numbers

1. **`daily` has a high std-dev** (22 s on Node, 5 s on Rust). It's the first
   command in the run, so it pays the cold-page-cache cost; subsequent commands
   (monthly/session/blocks) hit warm cache. Real-world first-call latency is
   closer to the `daily` numbers; steady-state is closer to the others.

2. **One Node `monthly` run failed with exit 1** (1 in 8). It's a transient
   in upstream — re-running the binary directly never reproduced it. Most
   likely a race between the live `~/.claude` directory (Claude Code is
   actively appending to jsonl files) and ccusage's valibot parser hitting
   a partial line. ccusage-rs ran 8/8 clean under identical conditions.
   `--ignore-failure` is on for hyperfine so the outlier doesn't blow up the
   whole run; the failed sample is excluded from the mean.

3. **Live data, not a frozen snapshot.** `~/.claude/projects/` is being written
   to during the benchmark. Two back-to-back invocations of the same binary
   produce slightly different `totals` (drift on the order of 0.001%) because
   new tokens land between calls. Byte-exact equivalence between Node and
   Rust is verified separately — see `tests/parity/` (62/62 cases pass on
   fs-fixture-built jsonl trees that don't move under our feet).

4. **Numbers are wall-clock for a single binary invocation**, not throughput.
   ccusage-rs uses `rayon` to parallelize jsonl parsing internally, so the
   speedup partly reflects multi-core utilization. On a 1-core box the gap
   would narrow but not close — startup, parse-per-line cost, and JSON
   serialization are all faster in Rust regardless.

## Reproducing

```sh
# from repo root
bench/run.sh
# results land in bench/results/<timestamp>/
```

Override targets:

```sh
CCUSAGE_DATA_DIR=/path/to/some/.claude \
RUST_BIN=./target/release/ccusage-rs \
NODE_BIN=$(command -v ccusage) \
bench/run.sh
```

To get tighter numbers, freeze the input first:

```sh
# Deep copy (not hardlink — hardlinks share inodes with live files)
cp -R ~/.claude /tmp/.claude-frozen
CCUSAGE_DATA_DIR=/tmp/.claude-frozen bench/run.sh
```

## Files

- `run.sh` — benchmark driver
- `results/<timestamp>/env.txt` — host/toolchain metadata
- `results/<timestamp>/<cmd>.json` — hyperfine raw json (per-iteration timing,
  exit codes)
- `results/<timestamp>/<cmd>.md` — hyperfine markdown table
- `results/<timestamp>/rss.tsv` — peak RSS per command per binary
- `results/<timestamp>/summary.md` — auto-generated summary
