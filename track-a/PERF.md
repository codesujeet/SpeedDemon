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

Steps 6-9 together, measured the way the board measures (5 runs, mean of the
middle three, highest memory peak): **0.093 s and 47 MB, against 0.214 s and
78 MB for the committed version** on the same laptop. Source size grows from
546 to 607 code lines (the harness's own count).

## What the time is now

Full data, 3.1 M lines, 239 MB (Linux container, 12 threads, local): the
parallel scan (parse, counting sort) is the larger part, about 75 ms in the
run where the whole program took 120-130 ms; merge, format and write is about
30 ms. Peak memory is 45-47 MB: about 12 MB of latencies, 6 MB of group
records, 3 MB of per-morsel endpoint tables, and the output file's page cache.

## Tried and dropped

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

## Caveats

* Steps 6-9 have not run on the scoring box. Expect the board's T to differ.
* The step 9 parser and the step 8 table rely on what the spec guarantees:
  well-formed lines, at most 72 hours and about 1,800 endpoints. A morsel with
  more than 2,048 distinct hours stops with an error message.
* Source size is the weak axis: the reference is 56 code lines and this is
  about 600. Every optimisation above had to be worth its lines.
