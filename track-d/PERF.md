# Track D: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset
(1,740,512 nodes, 5,000,000 edges, 2,000 queries), with the output hash
checked against the published SHA-256. Local runs swing 30-50%, so each row is
a before -> after median from interleaved or repeated runs, and rows are only
comparable within themselves. The laptop is slower than the scoring box: the
version of steps 1-8 (commit `db4cc76`) measured **0.475 s, 382 MiB, 736 lines,
K 9.48, 48.85 points on the live board** (run `run-890f67017a83`) and about
0.96 s, 384 MB on this laptop. The **Run ID** column stays *pending* for
steps without their own board run, and **Points** is only filled from board
numbers. Step 9 has not been on the board yet. Everything that was tried,
including the failures, is in [`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Bidirectional Dijkstra over forward and reverse CSR, 4-ary heap, reused per-worker state, stop when either queue empties, queries answered in parallel | pending | | | pending |
| 2 | Nodes relabelled in BFS order before building the CSRs (the input ids are a random relabelling of a road-like lattice) | pending | queries 2.4 -> 0.83 | lower | pending |
| 3 | Expand the side with the **smaller queue** instead of the smaller key | pending | 3.93 -> 2.58 (3x fewer heap pops) | about 505 | pending |
| 4 | Parallel line-aligned parse; each CSR built by workers that own contiguous node ranges and write disjoint slices | pending | 2.35 -> 1.49 (5 rounds) | about 520 | pending |
| 5 | One interleaved `[forward, backward]` label per node (distance + 1, so zero means unreached), packed `(neighbour, weight)` adjacency, one loop for both directions, sweep-clear after wide searches | pending | 1.03 -> 1.10 (9 rounds, about 7% slower, within noise) | 515 -> 381 | pending |
| 6 | Reverse CSR built on the idle cores while the serial BFS runs; both CSRs renamed into BFS order by a row copy (`permute_csr`) instead of relabelling the edge list and building both again | pending | about 0.95 -> 0.85 (Linux container, interleaved) | about 380 | pending |
| 7 | Relaxations at or above the best meeting distance are skipped | pending | query phase about 436 -> 420, inside the noise | 377 | pending |
| 8 | Each parse part files its edges under the worker that owns their node range before the CSR is counted and placed, so a worker scans 1/12 of the edges per pass instead of all of them | `run-890f67017a83` (commit `db4cc76`, steps 1-8 together) | original CSR phase 81 -> 56 ms; total 837 -> 780 ms (9 interleaved rounds) | 380 | 48.85 (board) |
| 9 | Each worker's distance labels are `[u32; 2]` instead of `[u64; 2]` (distance + 1; a distance that does not fit is marked `u32::MAX` and kept exactly in a small per-query table), halving the 12 per-worker arrays | pending | 1.04 -> 0.89 (9 interleaved rounds) | 384 -> 261 | pending |

Step 9 measured the board's way (5 runs, mean of the middle three, highest
memory peak) on the same laptop: **0.867 s and 262 MB, against 0.964 s and
384 MB** for the committed version, and 758 source lines against 745 (the
harness's own count). The peak is now the two CSRs (94 MB) plus the 12 label
arrays (14 MB each).

## Why step 3 mattered

On the real graph a handful of queries start at a node with no in-edges.
Balancing the two searches by key lets such a source starve: its own first
edges weigh about 2^32, so the other side keeps expanding and settles most
of the 1.74 M-node main body. Any alternation is correct under the stop
rule `top_forward + top_backward >= best`; balancing by queue size stopped
the starvation and cut the settled nodes in half.

## What the time is now

About 0.78-0.85 s locally: parse about 45-60 ms, original forward CSR about
56 ms, serial BFS about 200 ms (the reverse CSR is built beside it),
permuting the two CSRs about 45 ms each, queries about 410-475 ms (three
runs, total 0.84-0.96 s). The query phase is evenly balanced over the 12
workers (each busy 545-565 ms, slowest single query 94 ms), so it is
throughput-bound, not long-pole-bound.

## Tried and dropped

* **Parallel BFS** for the reorder. Not built: a level-synchronous BFS pays
  a barrier per level on a lattice with a long diameter, and the serial
  pass is only about 250 ms.
* **First version of the interleaved labels** (clear touched nodes one by
  one): 14% slower; the sweep-clear brought it to about 7%.

* **Packing the heap entry into one `u64`** (`dist << 21 | node`): query
  phase median 430 vs 436 ms over 10 interleaved runs, about 2-3% of the
  total, for about 8 lines and an assert that fires on huge distances.
  Reverted.

* **Dropping the sweep-clear** so that unreached label pages stay
  unmapped: peak memory unchanged (261 MB, the searches touch those pages
  anyway) and slightly slower (942 vs 901 ms median over 9 rounds).

## Caveats

* Step 9 has not run on the scoring box. Expect the board's T to differ.
* The earlier "61x" figure from a synthetic random graph was wrong for the
  real road network and has been removed from this log.
* Source size is the weak axis: the reference is 43 code lines.
