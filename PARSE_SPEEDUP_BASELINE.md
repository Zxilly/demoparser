# Parse benchmark log

## Current validated benchmark — 2026-08-13

Harness: `src/parser/src/bin/parse_bench.rs`. The current workload is a tick parse
with representative player state, position, input, weapon and economy properties;
events, projectiles, `wanted_other_props`, and stateful velocity properties are
excluded. `both` mode now parses once in each mode and asserts exact equality of
`df` and `df_per_player` before starting the timed runs. It also reports throughput
and ST/MT speedup. An optional final argument selects the private Rayon pool size.

Machine: AMD Ryzen 9 9950X3D, 16 physical cores / 32 logical processors, Windows.
Fixture: `src/parser/test_demo.dem`, 60.6 MB. Release build, one warm-up, median of
21 timed parses:

```text
target/release/parse_bench.exe test_demo.dem 21 both 16
verify: ST and MT outputs match (14 columns)
ST: median 0.654 s, 92.7 MB/s
MT: median 0.173 s, 351.2 MB/s
speedup: 3.79x
```

The host was noisy during these runs (for example, ST min/median/max was
0.459/0.654/0.912 s), so small changes are accepted only after an A/B run using
the same workload and session. Caching the current player entity in the collection
loop improved the 21-run medians from 0.606/0.152 s (ST/MT) to 0.586/0.149 s,
about 3.3%/2.0%. A row-buffer/transposition experiment regressed both modes and was
reverted.

Thread-count sweep, 11 timed MT parses per setting:

| Threads | Median | Throughput |
|---------|--------|------------|
| 2 | 0.365 s | 166.1 MB/s |
| 4 | 0.212 s | 285.4 MB/s |
| 8 | 0.171 s | 353.4 MB/s |
| 16 | **0.146 s** | **415.2 MB/s** |
| 32 | 0.208 s | 292.0 MB/s |

On this 16-core CPU, 16 workers are the best tested setting; using every logical
processor is slower. Reproduce with:

```text
cargo run --release --bin parse_bench -- test_demo.dem 21 both 16
```

### Correctness correction

The previous harness combined velocity properties with `ForceMultiThreaded`.
Rechecking it produced different ST and MT checksums (`35e81514d0837e55` versus
`dd96a2cc68cd6886`), because velocity depends on prior output rows and cannot be
computed independently by segment. The old performance numbers below remain a
historical record, but their claim of equivalent output for that combined workload
was invalid.

`velocity`, `velocity_X`, `velocity_Y`, and `velocity_Z` are now in the automatic
non-multithreadable set, including when used as property-state filters. Normal mode
therefore falls back to the correct serial path. `ForceMultiThreaded` remains an
explicit unsafe override, so the benchmark intentionally uses only segment-safe
properties and guards them with exact output comparison.

## Historical baseline — 2026-06-16

The workload and correctness guard differ from the current harness, so these values
must not be compared directly with the current table.

Harness: `src/parser/src/bin/parse_bench.rs` — representative full parse (player props +
all events + ticks, `parse_ents=true`), release build (`lto=true, codegen-units=1`),
1 warm-up + 5 timed iterations, wall-clock min/median/max.

Machine/run: 2026-06-16, Windows. Revalidated on the exact demo set below. Numbers are median of 5.

| Demo | Size | Baseline ST median | Baseline MT median | Baseline MT/ST | Baseline ST throughput |
|------|------|--------------------|--------------------|----------------|------------------------|
| NaVi-vs-TheMongolz nuke | 429.8 MB | **10.946 s** | **2.239 s** | 4.9× | ~39 MB/s |
| test_demo (fixture) | 60.6 MB | 2.148 s | 0.616 s | 3.5× | ~28 MB/s |
| de_ancient HLTV | 230.6 MB | 4.331 s | 1.096 s | 4.0× | ~53 MB/s |

Primary reference for optimization work: **NaVi nuke** (largest, most representative).
`Baseline MT/ST` is the multi-threaded speedup over the same run's single-threaded baseline.
`Baseline ST throughput` is computed as `demo size / baseline ST median`.

## Targets (from PARSE_SPEEDUP_GOAL.md)
- ≥1.5× (target 2×) end-to-end vs these numbers, output byte-identical, all tests green.
- Reproduce: `cargo run --release --bin parse_bench -- <demo> 5 both` (or `st`/`mt`).

## Reproduce baseline
```
cargo build --release --bin parse_bench
./target/release/parse_bench "<nuke.dem>" 5 both
```

---

### Historical progress — results vs baseline

Same historical harness/machine. Median of 5; full suite at the time had 333 tests.

| Demo | ST base | ST now | ST× | MT base | MT now | MT× |
|------|---------|--------|-----|---------|--------|-----|
| NaVi nuke  | 10.946 s | **3.700 s** | **2.96×** | 2.239 s | **0.646 s** | **3.47×** |
| test_demo  | 2.148 s | **0.764 s** | **2.81×** | 0.616 s | **0.161 s** | **3.83×** |
| de_ancient | 4.331 s | **1.771 s** | **2.45×** | 1.096 s | **0.323 s** | **3.39×** |

The raw timings cleared the historical 2× target on every demo. As explained in
the correctness correction above, the combined velocity workload did not establish
equivalent ST/MT output and these figures are retained for historical context only.

## Bottleneck → fix (the wins)

Profiled with env-gated phase timers (`CS2_PROF=1`) during the optimization session.

1. **Per-field event Vec allocation.**
   `decode_entity_update` called `listen_for_events()` for every updated field and
   received a fresh `Vec<GameEventInfo>` back, usually empty, then extended the packet
   event buffer. The function now appends directly into the existing `events_to_emit`
   buffer. Output checksum stayed identical.

2. **String dispatch in `create_custom_prop()`.**
   `collect_entities` previously matched custom props by `prop_name: &str` for every
   player and requested prop. The hot cases now dispatch on stable numeric `prop_info.id`;
   string matching remains only for the two full-name fallback props.

3. **Post-processing clone churn in `combine_outputs()`.**
   Single-threaded parsing has exactly one `SecondPassOutput`, but the old merge still
   cloned the full dataframe through `iter().map(|x| x.df.clone())`. ST now moves that
   output directly. The multi-threaded merge also moves per-segment data before combining
   instead of cloning whole segment outputs. `CS2_PROF=1` on NaVi showed
   `combine_outputs` drop from about `0.4 s` to `0.001 s` in ST.

4. **Serial dataframe merge in the multi-threaded path (`combine_dfs`).**
   With the clone churn removed (#3), the MT merge still concatenated each prop column
   across all chunks on a single thread — the dominant serial section (~`0.34 s` of
   ~`0.5 s` serial on the 430 MB reference demo at 16 threads, the Amdahl ceiling behind
   the MT scaling plateau). The merge now pre-groups columns into per-prop ordered buckets
   (moving column structs, no row-data copy, offset order preserved) and concatenates each
   prop's segments in parallel via rayon. `CS2_PROF` on the reference demo showed
   `combine_dfs` drop from `0.337 s` to `0.226 s`. Columns are independent and per-prop
   order is preserved, so the result is byte-identical (333 tests, `df` checksum unchanged
   ST+MT); the ST path is unaffected (single chunk → early return).

Supporting infra: `CS2_PROF` phase timers and the `CS2_CKSUM` golden-checksum mode in
`parse_bench`.

## Next bottleneck (for a future iteration)
Likely next levers remain `decode_entity_update` (`find_field`, `get_propinfo`,
`bitreader.decode`, `insert_field`) and the per-tick hashmap `entry`+`push` churn in
`collect_*`.
