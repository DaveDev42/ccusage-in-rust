# Benchmark summary

Source data: 7002 jsonl files, 2.8G on disk · Apple M1 Max · ccusage 18.0.11 on node v24.13.1 · ccusage-rs ccusage-rs 0.1.0

| command | ccusage (Node) | ccusage-rs (Rust) | speedup | rust peak RSS | node peak RSS |
|---|---:|---:|---:|---:|---:|
| `daily` | 70287.2 ms ± 22067.4 | 12915.6 ms ± 4808.8 | **5.4×** | 473.3 MB | 4357.0 MB |
| `monthly` | 62237.7 ms ± 7774.5 | 6787.6 ms ± 216.8 | **9.2×** | 472.5 MB | 3821.9 MB |
| `session` | 30579.5 ms ± 888.2 | 6495.1 ms ± 132.6 | **4.7×** | 471.5 MB | 2048.4 MB |
| `blocks` | 31340.6 ms ± 2344.0 | 6588.4 ms ± 127.5 | **4.8×** | 473.7 MB | 2086.4 MB |
