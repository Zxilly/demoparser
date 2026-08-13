# Parser benchmark

Harness: `src/parser/src/bin/parse_bench.rs`. It benchmarks a representative,
segment-safe tick query. In `both` mode it verifies that `df` and
`df_per_player` match exactly before timing.

Reproduce on the bundled fixture:

```text
cargo run --release --bin parse_bench -- test_demo.dem 21 both 16
```

Validated 2026-08-13 on Windows, AMD Ryzen 9 9950X3D, using the 60.6 MB
`test_demo.dem` fixture:

| Mode | Median | Throughput |
|------|--------|------------|
| Single-threaded | 0.654 s | 92.7 MB/s |
| 16 workers | 0.173 s | 351.2 MB/s |

Speedup: 3.79x. On this host, 16 workers were faster than 32 workers.

Velocity properties are stateful and therefore excluded from this parallel
workload. Normal parsing automatically falls back to the serial path when they
are requested.
