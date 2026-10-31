# Track A: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset,
with the output hash checked against the published SHA-256. Local runs swing
30-50%, so each row is an interleaved before -> after median over the stated
rounds, and rows are only comparable within themselves. The laptop is
slower than the scoring box: the version of steps 1-5 (commit `0bbd5d0`)
measured **0.122 s, 79 MB, 537 lines, K 14.24, 57.68 points on the live
board** and about 0.21 s, 78 MB on this laptop. The **Run ID** column stays
*pending* until a step has its own board run, and **Points** is only filled
from board numbers. Steps 6-9 have not been on the board yet. Everything
that was tried, including the failures, is in
[`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Morsel-parallel scan (2 MiB morsels from an atomic cursor, each snapped to a line start), fused space-or-LF SWAR scan per field, per-thread FxHash maps, exact `select_nth_unstable` percentiles; input read into memory | pending | about 1.1 | 332 | pending |
| 2 | `mmap` the input instead of reading it (the host pre-warms `/data`, so mapped pages are not charged to the run) | pending | 1.13 -> 1.05 (5 rounds) | 332 -> 104 | pending |
| 3 | Group keys are slices of the mapped input (no allocation per line); merge, percentiles and formatting done in parallel over blocks of sorted keys | pending | 0.70 -> 0.42 (7 rounds) | 104 -> 83 | pending |
| 4 | Newline search at morsel boundaries is a plain `position` (one call per 2 MiB), deleting a 24-line SWAR function | pending | 0.33 -> 0.30 (7 rounds) | 80 | pending |
| 5 | Merge by hour: the workers' groups are collected as (key, data) pairs and ordered by hour alone (an integer sort); each hour is then sorted by endpoint and merged by one worker, so no map is searched and no pointer-keyed sort of duplicated keys runs serially | board commit `0bbd5d0` (run ID not recorded) | 0.28 -> 0.21 (9 rounds); merge phase 150 -> 50 ms | 78 | 57.68 (board) |
| 6 | No per-worker hash maps of growing vectors. Each 2 MiB morsel is parsed into a small array of packed `u64` lines (hour slot, endpoint number, error flag, latency), which is sorted into one run of latencies per (hour, endpoint). The final merge takes an hour at a time from the few morsels that touch it | pending | 0.194 -> 0.150 (11 rounds) | 77 -> 53 | pending |
| 7 | Each hour's output block is written to the file as soon as all earlier hours are, instead of all blocks being held and then written | pending | 0.191 -> 0.177 (11 rounds) | 53 -> 47 | pending |
| 8 | The morsel's lines are placed by a counting sort over a dense (hour slot x endpoint) table instead of a comparison sort | pending | 0.181 -> 0.160 (11 rounds) | 47 | pending |
| 9 | Line parsing uses the spec's fixed layout: a 27-byte timestamp and a 3-digit status are sliced by offset, so three delimiter scans per line instead of five (the parser no longer tolerates blank or malformed lines, which the spec rules out) | pending | 0.155 -> 0.130 (13 rounds) | 46 | pending |
| 10 | One 64-byte AVX2 window per line finds the method, endpoint, status and line-feed delimiters at once (byte-wise path kept for lines near the end of the data), replacing three byte-wise scans | pending | 0.155 -> 0.134 (15 interleaved rounds) | 45 | pending |
| 11 | No per-morsel results and no merge across morsels. Each worker appends one packed `u64` per line (endpoint number, error flag, latency) to its own list for the line's hour, with a worker-lifetime endpoint table backed by one shared table of numbers; after the scan each hour is counted per endpoint, placed in one run per endpoint, and visited in endpoint-byte order (sorted once). Latency parsed with SWAR, hour found by comparing 13 timestamp bytes, per-hour lists start with room for 8192 lines, the process skips freeing them | pending | 1 CPU (`--cpuset-cpus=2`): 0.42-0.57 -> 0.28 s wall/CPU (3 interleaved runs); 12 threads: no clear change on this loaded laptop (A/B medians 188 -> 185 ms, harness replica 0.132 -> 0.152 s, within the +-20% noise of four agents sharing the machine) | 46 -> 45 | pending |
| 12 | Branchless endpoint check: four masked 32-byte compares replace the length-dependent `bcmp` | pending | 1 CPU, min of 8 interleaved runs: scan 189 -> 168 ms, user+sys 259 -> 235 ms | 45 | pending |
| 13 | Lines are 4 bytes: `error << 31, endpoint << 16, latency in the low 16 bits`, with latencies of 65535 or more stored as 65535 and their real value in a side list per hour list (endpoint numbers must stay below 32,768; the spec's catalogue has 1,842) | pending | 1 CPU: scan 188 -> 187 ms, phase 2 78 -> 71 ms (min of 8); peak 45 -> 38.8 MB | 45 -> 39 | pending |
| 14 | Alternative design (branch agent-a2), rewritten from first principles: one pass, no atomics in the hot loop except the chunk cursor; each stretch of up to ~1000 delimiters is listed as offsets first (AVX2 mask, 8 offsets written unconditionally), then lines are read five offsets at a time so no line is parsed to find the next; `u32` lines (endpoint 11 bits, error, 20-bit latency, longer values followed by a whole word); per-worker endpoint table with a private copy of each name and a two-compare (first/last 32 bytes) exact check; per-worker arena in scan order with a run list per hour change; barrier, then one hour per task | pending | 1 CPU (`--cpuset-cpus=2`), min of 7 CPU user+sys: 285 -> 251 ms (speed-demon HEAD vs this); scan alone, best of 25 in-process repeats: 102 -> 78 ms | 36 -> 25 | pending |
| 15 | Groups of up to 32 latencies are fully sorted instead of selected three times | pending | 1 CPU min CPU 251 -> 235 ms (7-9 runs) | 25 | pending |
| 16 | Each hour gives the pages of the arenas that hold only its lines back to the system (`madvise(MADV_DONTNEED)`) as soon as they are copied into the hour's placement buffer, so the output file's page cache is paid for by input freed | pending | 12 CPUs: 7 runs, CPU unchanged within noise; 1 CPU min CPU 193 -> 171 ms (7 interleaved runs) | cgroup `memory.peak` max of 5-7 runs: 12 CPUs 33.7 -> 24.6 MiB; 1 CPU 24.6 -> 15.6 MiB | pending |
| 17 | Memory per worker: endpoint names are stored back to back (no 64 padding bytes each; the compares already mask what lies outside a name) and the endpoint table has 8,192 slots instead of 16,384 (1,842 names, load 22%) | pending | 1 CPU CPU min (7 interleaved runs) 174 -> 168 ms (noise level); 12 CPUs: no change in CPU | cgroup anon at the end of the scan with 12 workers 17.05 -> 15.75 MiB (step 16 -> here), `memory.peak` max of 7 runs 25.1 -> 24.1 | pending |

Steps 6-9 together, measured the way the board measures (5 runs, mean of the
middle three, highest memory peak): **0.093 s and 47 MB, against 0.214 s and
78 MB for the committed version** on the same laptop. Source size grows from
546 to 607 code lines (the harness's own count).

## What the time is now

Full data, 3.1 M lines, 239 MB (Linux container, local): on one core the whole
program takes about 0.28 s (scan about 0.2 s, about 58 ns per line; count,
select and format about 0.06 s). Peak memory is 44-47 MB: about 12 MB of
packed lines (4 bytes each since step 13), the endpoint tables and the output file's page
cache (10 MB), plus stacks.

## Tried and dropped


* **Listing delimiters in a separate pass only (no parse)**: 20 ms for 3.1 M lines on one core, so listing is cheap. By ablation (one core, best of 25 in-process repeats) the rest of the 100 ms scan is: error flag, latency and hour parse 26 ms, endpoint table 43 ms, arena writes 12 ms.
* **MAP_POPULATE and touching all pages first**: no gain in CPU time (page faults are about 25% of the 1-core CPU, but pre-faulting only moves them).
* Comparing against the mapped first occurrence instead of a private copy of each name: the copy was kept, 5 to 10 ms of scan faster (names sit together).
* **Cascading selection** (find p50, then search only the part above it for
  p95, then above that for p99). Correct and tested, but 463 vs 501 ms over
  11 interleaved rounds, so no gain; reverted. The merge phase is not
  dominated by selection.
* **An approximate histogram for the percentiles.** Not an option: the
  output is checked by an exact hash and the rank is nearest-rank.
* **Interning endpoints per worker** (a small hot table for the endpoint,
  then a cheap `(hour, number)` key for the group) while still using big
  per-worker maps: 207 vs 191 ms median over 11 interleaved rounds, no gain.
  Step 6 uses a per-morsel table instead and does not keep the maps.
* **Parsing a batch of 256 lines before the map updates**: 188 -> 179 ms
  median over 11 interleaved rounds, inside the noise; reverted.
* **Morsel size** (1 MiB, 2 MiB, 4 MiB with the step 9 parser): median 131,
  125 and 137 ms, peak 53, 45 and 44 MB. 2 MiB stays.

* **Memory, steps 16-17** (12 CPUs, interleaved runs of 9, cgroup `memory.peak`; median / max): start of this work 33.0 / 33.3 MiB, now 23.3 / 24.1 MiB, CPU unchanged. The peak is now just after the scan: about 12.4 MiB of lines, 1.4 MiB of kernel memory, 2 MiB of tables and stores, then 12 threads allocating their hour buffers (about 0.4 MiB each) before their releases catch up. Pages shared by two hours' slices of one arena are not released (about 700 slices, 3 MiB).
* **Dropped: writing each output block to disk and evicting it from the page cache** (`sync_file_range` + `posix_fadvise(DONTNEED)`): the output's 10 MiB of cache is gone at the end, but the peak is at the start of the output phase, so `memory.peak` only moved 24.6 -> 23.0 MiB; not kept.
* **Dropped: 2 and 4 MiB chunks** (fewer, larger arena slices, so fewer unreleased shared pages): peak median 23.5 -> 22.9 MiB, CPU min 385 -> 408 ms; not kept.
* **Phase 2 by sub-phase** (1 CPU, TSC cycles, best of 3): count 19 M, place 10 M, release 3 M, percentiles 54 M, format 27 M (about 45 ms of 235). The percentile sorts are half of it.

## Where the unit tests live

The unit tests are in `tests/track_a.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_a.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 607 to 434.

* **`madvise(MADV_DONTNEED)` on every morsel after it is parsed**, to drop
  the mapped input from the process: peak 46 vs 45 MB, no effect. The
  pre-warmed `/data` pages are not charged to the run in the first place (the
  run scans a 239 MB mapped file and reports about 44 MiB).

* **Profile-guided build** (a `.profdata` recorded on the public dataset, same
  rustc and LLVM): board 0.0645 s against 0.0655 s, K +0.4%, inside the noise;
  reverted.

## Caveats

* Steps 6-9 have not run on the scoring box. Expect the board's T to differ.
* The step 9 parser and the step 8 table rely on what the spec guarantees:
  well-formed lines, at most 72 hours and about 1,800 endpoints. A morsel with
  more than 2,048 distinct hours stops with an error message.
* Source size is the weak axis: the reference is 56 code lines and this is
  about 600. Every optimisation above had to be worth its lines.

* **Step 11 follow-ups that did nothing measurable.** `madvise(MADV_POPULATE_READ)` on every morsel before the scan (11 interleaved rounds: 115 vs 111 ms median, noise; removed to save source lines). Thread count and morsel size sweeps (5 interleaved rounds, 12 threads vs 8 vs 6; 256 KiB vs 1 MiB vs 4 MiB): medians 131/118/106 ms for 12/8/6 threads, but the machine was shared with other builds, so no setting was adopted; morsel size showed no trend. A bare 12-thread pass over the whole mapped input (touching every page, or summing every 8 bytes) takes 4 and 7-10 ms, so the input mapping is not the bottleneck: the scan is compute-bound at about 58 ns per line on one core.
* **Local measurement caveat for step 11.** With other agents building and benchmarking on the same laptop, the 12-thread wall time cannot resolve a 1.5x change in work (CPU time with 12 threads was 0.9 s user + 0.3-0.7 s sys for the old design, 0.5 s + 0.25 s for the new one, but wall time did not follow). Pinning to one core (`--cpuset-cpus=2`) removes the scheduling noise and shows the real work: 0.42-0.57 s against 0.28 s.
