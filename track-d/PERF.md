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
numbers. Steps 9-18 have not been on the board yet. Everything that was tried,
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
| 10 | The parse threads file every edge straight into per-owner buckets for the forward and the reverse graph, so the flat edge list and the separate bucketing copy never exist; the buckets are dropped as soon as each CSR is built. The query phase uses one worker per physical core (6) instead of 12 | pending | about equal (1017 vs 1075 ms median over 11 interleaved rounds) | 261 -> 192 | pending |
| 11 | The serial BFS relabelling is replaced by a parallel region-growing order (workers race on a claimed byte per node and grow BFS regions of up to 1024 nodes); the reverse CSR is built from the renamed forward CSR instead of beside the BFS; the forward CSR is built with two parse passes and atomic row cursors | pending | harness replica on a loaded laptop 1.35 -> 0.89 together with steps 12-13 | 190 -> 181 | pending |
| 12 | The per-side queue is a monotone radix heap instead of the 4-ary heap | pending | single-worker query phase 2100 -> about 1100 ms (native, same queries) | same | pending |
| 13 | Edges are filed under their owner worker before the forward and reverse CSR builds, so counting and placing need no atomics | pending | native interleaved min of 8: forward CSR 153 -> 74 ms, total 589 -> 503 ms (reverse CSR 32 -> 45 ms) | same | pending |
| 14 | Per-worker labels are pages of 16 ids from a pool (page table per worker) instead of an array over all nodes, which allows 12 query workers instead of 6; the reverse CSR is a transpose of the renamed forward CSR with atomic cursors instead of the bucket build | pending | docker interleaved 829 -> 595 ms (6 rounds, busy laptop) | 181 -> 123 | pending |
| 15 | CSR edges take one 32-bit word (neighbour + weight; a weight that does not fit takes a second word) | pending | docker interleaved 693 -> 654 ms (9 rounds) | 125 -> 98 | pending |
| 16 | Filed edges (the bucket build of the original-id CSR) are one `u64` each instead of 12 bytes | pending | neutral (native min of 7: 482 vs 465 ms) | 98 -> 86 | pending |
| 17 | Rows are copied out while regions grow and renamed into the final array in parallel (no separate permutation pass); `malloc_trim` between build phases | pending | docker interleaved 668 -> 599 ms (9 rounds); native min of 8: order + permute 152 -> 116 ms | peak is now the query phase, about 92 | pending |
| 18 | Numbers up to 7 digits are parsed from one 8-byte load (separators are the only input bytes without bit 4 set); region growth prefetches the rows it is about to read; the big buffers are not freed at exit | pending | docker interleaved 556 -> 488 ms (9 rounds); native min of 9: forward CSR 112 -> 82 ms (parser), order 85 -> 72 ms (prefetch) | about 93, same | pending |

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

## Where the unit tests live

The unit tests are in `tests/track_d.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_d.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 758 to 582.

* **Query worker count** (12, 8, 6, 4, 3 workers, interleaved): 989, 948,
  963 (816 in a later run), 832 and 928 ms; the peak was 262 MB for all of
  12, 8 and 6 workers before step 10, because the peak was in the graph
  build (edge list, buckets, original CSRs), not the queries. After step 10,
  6 workers is the best balance; 4 workers saves 12 MB for about 2% time.

## Steps 11-13 in detail

Measured natively on the Windows laptop (same machine, other jobs running,
so only interleaved or min-of-N comparisons mean anything) and with the
Docker harness replica (`local_harness.py`, 5 runs, mean of the middle three):
the version before step 11 gave **1.345 s, 190.5 MB** (a batch the harness flagged as too noisy: runs 1.10-1.79 s) and this version
**0.892 s, 181.2 MB** in back-to-back batches while the laptop was loaded
(source lines by the harness's counter: 572 -> about 520). The scoring box
is faster than this laptop, so these are not board numbers.

The phases now (native, min of 8, ms): original-id CSR 74, ordering 63,
renaming 45, reverse CSR 45, queries about 205-230, plus reading the file
(on Linux it is mapped, and costs nothing here). The ordering and CSR phases
are limited by random memory access; the query phase was limited by the
heap: single worker, 2000 queries, 7.6 M pops and 21 M relaxations, about
280 ns per pop with the 4-ary heap and about 140 ns with the radix heap.

## Tried and dropped in steps 11-13

* **Queries in source order with chained regions** (each region's seed is
  next to the end of the previous one, so ids are spatially coherent, and
  queries are taken in source-id order in chunks of 1-64): no change in the
  query phase (within +-5% noise), single worker or 6 workers. Cross-query
  cache reuse is not what limits the search.
* **Each worker scans the whole edge stream and keeps only its own keys**
  (no filing, no atomics): forward CSR 150-165 ms and reverse CSR 176-320 ms
  vs 100-170 and 33-46 ms, worse because 12 workers each walk 5 M edges twice.
* **Hash labels** instead of the per-worker `[u32; 2]` array (open addressing,
  a home slot that keeps runs of 32 consecutive ids together, doubling when
  half full): 3 MB instead of 14 MB per worker, but 300-370 ms vs 245-285 ms
  for the query phase at 12 workers (identity hash: 770-880 ms from
  clustering). Dropped; about break-even in K once the extra time is counted.
* **12 query workers with the radix heap** are about 20% faster in the query
  phase than 6 (200-277 vs 256-384 ms) but would add 84 MB of label arrays,
  so 6 stays.
* **Atomic row cursors for the original-id CSR** (step 11's first build): about
  60-75 ms for the placement pass because locked instructions serialise the
  cache misses; replaced by step 13.

## Steps 14-17 in detail

Same caveats as before: a shared, loaded laptop, so only interleaved A/B or
min-of-N comparisons mean anything. The peak-memory story: before step 14 the
peak was the query phase (two CSRs plus 6 x 14 MB label arrays) and, nearly
as high, the reverse-CSR build (a second copy of every edge in buckets);
step 14 removed both, step 15 halved the CSRs, step 16 shrank the buckets of
the first build, and what is left (about 90 MB) is the query phase again: 54
MB of CSRs plus the label pools of 12 workers, found by reading `VmHWM` at the
end of each phase (it rose only during the queries).

Harness replica (5 runs, mean of the middle three, highest peak) back to back
on the loaded laptop: the state before step 14 gave **0.911 s, 181 MB** (batch
flagged too noisy), the current state **0.651 s, 99 MB** (runs 0.67-0.71 s,
peaks 91 MB except one batch run at 99 MB). The harness's source-line count
went from 520 to 662, because the 4-byte edge encoding, the run/rename code and
the paged labels are real code; that costs about 0.8 points of K against the
time and memory gains.

## Step 18 in detail

Parsing alone (12 threads, no filing) took 54-72 ms of the 112 ms the
original-id CSR needed, about 7 ns per input byte of CPU, so the digit loop was
the next target. The word-at-a-time reader brought the whole phase to 82 ms.
Prefetching the row of the node 6 places ahead in the region queue and the
offset entry of each newly claimed node cut the ordering from 85 to 72 ms
(minimum of 9); a deeper variant (rows 12 ahead plus the claimed byte of the
neighbours 4 ahead) was equal in the minimum, with a lower mean, and was not
kept for its extra lines. The exit change (`mem::forget` of the CSRs and the
worker pools) is below the noise on its own.

## Tried and dropped in steps 14-17

* **Label page size** 8, 16, 32, 64 ids: no difference within noise
  (native query phase 193-340 ms for all, minimum 191-211 ms).
* **Hash labels** and **12 workers with array labels** are in the earlier
  list; the paged pool is what made 12 workers affordable (queries about 20%
  faster than with 6, the pool costs no measurable time at 6 workers).

## Caveats

* Steps 9-18 have not run on the scoring box. Expect the board's T to differ.
* The earlier "61x" figure from a synthetic random graph was wrong for the
  real road network and has been removed from this log.
* Source size is the weak axis: the reference is 43 code lines.

## Replacement by an alternative implementation (not yet on the board)

`src/main.rs` was replaced by a different implementation drafted with
ChatGPT by the project owner (per-part bucketed CSR build, blob BFS
renumbering, bidirectional Dijkstra with 4-ary heaps and paged distances). It
was reviewed and tested here and is not part of the step table above, whose
rows describe the earlier code.

Local comparison against the previous version (this laptop, Docker with the
board's run flags, full public dataset, 15 interleaved rounds, hash checked
every run): median wall 321 ms against 455 ms, `memory.peak` median 67.2 MiB
against 93.7 MiB. Source: about 520 code lines after `rustfmt` (Dockerfile
included) against about 680, because the unit tests are gone. Also correct on
the generated datasets of seeds 2, 3 and 7 (10 runs each, no failure), on the
tiny and sample scales, and on the whole edge-case suite (66 cases with LF
line endings). Review found a panic: a prefetch indexed the edge array at the
offset of a node with no out-edges when that node is the last in id order
(probability about 2e-5 per dataset on the public graph's 42 sinks); it is
fixed (the prefetch uses a wrapping pointer) and checked on a graph made to
trigger it. `cargo clippy -D warnings` is clean after four small changes.
Points are estimated, not measured: scaling the last board result by these
ratios gives about 69 against 63; a board run will replace this estimate.


<!-- board-runs:begin -->
## Board runs

Each row is one scored run of the named commit on the live board, copied from the public board feed (`board.json`). The feed keeps only each track's latest and best run, so runs before these cannot be listed; earlier steps in the table above stay local measurements. Board timing varies by about 1-2 ms between runs of identical code: every push re-runs all four tracks, so a row whose change names another track measured this track's unchanged code and shows that noise.

| Commit | Change | Run ID | T (s) | M (MiB) | S | Points |
|---|---|---|---|---|---|---|
| `32f54ce` | Track A: the writer thread exits the process after the last block (agent-a4 d262378) | `run-0a5e5f081b09` | 0.2308 | 94.3 | 680 | 63.02 |
| `b67a992` | Track A: link the C runtime statically (skips dynamic loading at start-up) | `run-0de3473d90a5` | 0.2355 | 98.2 | 680 | 62.63 |
| `f299e99` | Track B: parse the queries in one SIMD token pass with leaner term interning (agent-b 8537 | `run-dcf9357d596b` | 0.2350 | 96.9 | 680 | 62.71 |
| `33b8455` | Track B: postings fill uses the term offset as its own cursor (agent-b d49c673) | `run-ef86756aeaae` | 0.2285 | 97.2 | 680 | 63.00 |
| `b208288` | Track A: workers write their own hour block at its final offset; no writer thread (agent-a | `run-b2c218f84bee` | 0.2309 | 98.1 | 680 | 62.85 |
| `cd4c35e` | Revert "Track A: workers write their own hour block at its final offset; no writer thread  | `run-b67625fd44dc` | 0.2280 | 97.5 | 680 | 63.01 |
| `e804869` | PERF.md: add a Board runs table with real run IDs from the public feed to every track | `run-a39e58686087` | 0.2485 | 95.5 | 680 | 62.16 |
| `36f4296` | Tracks A and C: profile-guided optimization from a committed profile (no source or Dockerf | `run-584b4976e052` | 0.2352 | 97.6 | 680 | 62.66 |
| `158f18f` | Track B: 32-byte token mask (agent-b 0054521); Track A PERF.md header with current board n | `run-2008d5099086` | 0.2278 | 95.9 | 680 | 63.09 |
| `49548fa` | Track B: merge and render in parallel from per-range logs (agent-b a34f4cf) | `run-792a3c586a77` | 0.2332 | 97.0 | 680 | 62.79 |
| `3286475` | Track C: 111-bit name fingerprint (second independent hash compared on every tag match), d | `run-457e766d4d47` | 0.2323 | 97.9 | 680 | 62.79 |
| `280509c` | Track B: vector-compare term lookup, CRC32C hash, fused dedupe (agent-b) | `run-31018b933ee2` | 0.2401 | 96.1 | 680 | 62.51 |
| `892d831` | Track C: regenerate the PGO profile for the 111-bit fingerprint source | `run-6267765c6698` | 0.2296 | 93.8 | 680 | 63.10 |
| `2ca5698` | Track C: back to the 36f4296 source and profile (the wider fingerprint cost 3-5 ms on the  | `run-fef1434b1953` | 0.2550 | 98.8 | 680 | 61.73 |
| `b1497a5` | Track B: 2 MB pieces (agent-b c51c3b0) | `run-43449fd7246e` | 0.2338 | 97.2 | 680 | 62.75 |
| `f00df83` | Revert Track B 2 MB pieces: T -17 ms but M +12 MiB, 65.82 vs 65.94 on the board | `run-1c23341dba98` | 0.2323 | 90.6 | 680 | 63.13 |
| `b18c263` | PERF: board run IDs through f00df83 (16 scored runs per track) | `run-3b47a1b08a6b` | 0.2674 | 93.3 | 680 | 61.47 |
| `44a450f` | Track A: mallopt M_MMAP_THRESHOLD 32 KiB, memory.peak 23.0 -> 20.4 MiB locally (agent-a4 c | `run-b8240df3c2b1` | 0.2315 | 96.4 | 680 | 62.89 |
| `b7bb4d3` | Track A: PGO profile regenerated for the mallopt source (agent-a4 90f380c) | `run-4b8b3425c70a` | 0.2347 | 94.9 | 680 | 62.81 |
| `4c454fe` | Track A: three-phase port with exact output sizes, every thread formats into the mapped ou | `run-39cd48090665` | 0.2437 | 96.6 | 680 | 62.33 |
| `5476630` | Re-run board on the Track A port (noise check) | `run-31afd8f4fccc` | 0.2326 | 99.7 | 680 | 62.70 |
| `6c9bc1e` | Tracks B and D: user-drafted Rust solutions, reviewed (alt-bd c90b4a8): B 603 lines, D 509 | `run-f3df23fd5cf3` | 0.1842 | 67.4 | 517 | 67.82 |
| `f47d313` | Track C: user-drafted C++ design ported to Rust, CRLF chunk cut, 440 lines (agent-c3 7827d | `run-f832cffbcdf3` | 0.1728 | 67.2 | 517 | 68.53 |
| `c4832fa` | Re-run board on the Track C port (noise check) | `run-e4147edf6dcb` | 0.1705 | 66.6 | 517 | 68.71 |
| `4f9e4bd` | Track D: PAGE 512 -> 256 (experiment) | `run-237237831d54` | 0.1692 | 66.0 | 517 | 68.83 |
| `7a3ceb9` | Track D: BLOB 512 -> 1024 (experiment) | `run-4f454bc94f8d` | 0.1717 | 67.3 | 517 | 68.59 |
| `0b9abd0` | Track D: BLOB back to 512, CHUNK 1024 -> 4096 (experiment) | `run-e4bc2534c91a` | 0.1745 | 65.4 | 517 | 68.53 |
| `1a4d82e` | Track D: PAGE 256 -> 128 (experiment) | `run-9f0a731d2fc7` | 0.1869 | 65.8 | 517 | 67.77 |
| `a2fa18e` | Track B: persistent thread pool instead of 12 fresh threads per phase (experiment) | `run-e6949fbac529` | 0.1705 | 65.6 | 517 | 68.78 |
| `b573d30` | Track C: C++ (gcc 15, user-drafted design: pclmul quote mask, dense-id name table); Rust v | `run-b982832f8f82` | 0.1799 | 66.4 | 517 | 68.14 |
| `17ce1b0` | Track C: static link and _exit(0) after the output (experiment) | `run-f0d72b3c2c4e` | 0.1746 | 65.6 | 517 | 68.51 |
| `9e9667f` | Re-run board on static+_exit Track C (noise check) | `run-1dfc0257aa37` | 0.2038 | 66.1 | 517 | 66.80 |
| `308cbff` | Track A: C++ (gcc 15, static, _exit after the output); Rust version stays at tag best-19 | `run-6ed34af0f2f4` | 0.1724 | 65.7 | 517 | 68.65 |
| `4d1d805` | Tracks B and D: exit right after the output so the big arrays are not freed one by one | `run-f19739011738` | 0.1711 | 65.3 | 518 | 68.75 |
| `d68c42e` | Untrack Cargo.lock files added by mistake | `run-d36722b5d600` | 0.1738 | 66.1 | 518 | 68.53 |
| `23561b4` | Track C: LAG 16 -> 32 (experiment) | `run-13f7637d3f4f` | 0.1985 | 65.4 | 518 | 67.13 |
| `a302a61` | Track C: LAG back to 16 (32 scored 75.61, no gain) | `run-590fdacc489a` | 0.1923 | 65.6 | 518 | 67.47 |
| `70fee14` | Track A: 16 pieces per thread (experiment) | `run-7ea14a500420` | 0.1722 | 65.4 | 518 | 68.67 |
| `8d367d9` | Track C: new user-drafted C++ (64-bit hash key, field-by-field parse, distinct counts from | `run-ba337fa71396` | 0.1902 | 65.4 | 518 | 67.60 |
| `2ce9b21` | Track B: new user-drafted Rust (token id stream, CRC hash, bitmap for big lists, persisten | `run-0637a97b287a` | 0.1726 | 66.1 | 518 | 68.60 |
| `a46cccc` | Track A: new user-drafted C++ (branch-free width, 64-byte name compare, latency buffer siz | `run-5bb911630f51` | 0.1712 | 66.3 | 518 | 68.68 |
| `428ae7e` | Track A: new file without the sizing pass over all records (experiment) | `run-a2ae390e4894` | 0.1712 | 65.9 | 518 | 68.71 |
| `8b2268b` | Write-ups for the second A, B and C drafts, with board run IDs | `run-0e3ac4fbb477` | 0.1792 | 66.0 | 518 | 68.20 |
| `f34afab` | Track B: BIG 768 -> 1536 (experiment) | `run-afafed8933d3` | 0.1782 | 65.9 | 518 | 68.27 |
| `576267e` | Tracks A and C: each thread releases a consumed input piece's pages at once (from the new  | `run-b16343da8915` | 0.1731 | 65.9 | 518 | 68.59 |
| `a94d376` | Track B: each thread releases its share of docs.txt after the scan | `run-d53ce99c8597` | 0.1730 | 65.8 | 518 | 68.60 |
| `4dde5ce` | Track B: DIAGNOSTIC (answers computed twice) to measure the answer phase on the board; wil | `run-97f90d34d610` | 0.1810 | 65.9 | 518 | 68.10 |

<!-- board-runs:end -->
