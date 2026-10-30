# Track A: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements, not
board results: a laptop running Docker with the board's run flags on the full
public dataset, with the output hash checked against the published SHA-256.
Local runs swing 30-50%, so each row is an interleaved before -> after median
over the stated rounds, and rows are only comparable within themselves. The
**Run ID** column stays *pending* until the step has been measured on the live
board, and **Points** is only filled from board numbers. Nothing in this file
has run on the scoring box yet. Everything that was tried, including the
failures, is in [`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Morsel-parallel scan (2 MiB morsels from an atomic cursor, each snapped to a line start), fused space-or-LF SWAR scan per field, per-thread FxHash maps, exact `select_nth_unstable` percentiles; input read into memory | pending | about 1.1 | 332 | pending |
| 2 | `mmap` the input instead of reading it (the host pre-warms `/data`, so mapped pages are not charged to the run) | pending | 1.13 -> 1.05 (5 rounds) | 332 -> 104 | pending |
| 3 | Group keys are slices of the mapped input (no allocation per line); merge, percentiles and formatting done in parallel over blocks of sorted keys | pending | 0.70 -> 0.42 (7 rounds) | 104 -> 83 | pending |
| 4 | Newline search at morsel boundaries is a plain `position` (one call per 2 MiB), deleting a 24-line SWAR function | pending | 0.33 -> 0.30 (7 rounds) | 80 | pending |
| 5 | Merge by hour: the workers' groups are collected as (key, data) pairs and ordered by hour alone (an integer sort); each hour is then sorted by endpoint and merged by one worker, so no map is searched and no pointer-keyed sort of duplicated keys runs serially | pending | 0.28 -> 0.21 (9 rounds); merge phase 150 -> 50 ms | 78 | pending |

## What the time is now

Full data, 3.1 M lines, 239 MB (Linux container, 12 threads, local): parse and
group about 110-125 ms, merge + percentiles + formatting about 50 ms, write
about 7 ms, total 0.18-0.21 s. Before step 5 the merge phase spent about
67 ms in a serial collect, sort and dedup of 298 k keys (126,908 distinct)
and then did 12 hash lookups per distinct key.

## Tried and dropped

* **Cascading selection** (find p50, then search only the part above it for
  p95, then above that for p99). Correct and tested, but 463 vs 501 ms over
  11 interleaved rounds, so no gain; reverted. The merge phase is not
  dominated by selection.
* **An approximate histogram for the percentiles.** Not an option: the
  output is checked by an exact hash and the rank is nearest-rank.
* **Interning endpoints per worker** (a small hot table for the endpoint,
  then a cheap `(hour, number)` key for the group): 207 vs 191 ms median
  over 11 interleaved rounds, no gain; reverted.
* **Parsing a batch of 256 lines before the map updates** (so the lookups
  run back to back): 188 -> 179 ms median over 11 interleaved rounds, inside
  the noise, for about 8 lines; reverted.

## Caveats

* Nothing here has run on the scoring box. Expect the board's T to differ.
* Source size is the weak axis: the reference is 56 code lines and this is
  several hundred. Every optimisation above had to be worth its lines.
