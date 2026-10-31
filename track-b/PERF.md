# Track B: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset,
with the output hash checked against the published SHA-256. Local runs swing
30-50%, so each row is a before -> after median from interleaved or repeated
runs, and rows are only comparable within themselves. The laptop is slower
than the scoring box: the version of steps 1-4 (commit `db4cc76`) measured
**0.426 s, 362 MiB, 905 lines, K 11.58, 53.19 points on the live board** (run
`run-c4d559ad6869`) and about 0.8-1.0 s, 350 MB on this laptop. The **Run ID**
column stays *pending* for steps without their own board run, and **Points**
is only filled from board numbers. Steps 5-7 have not been on the board yet.
Everything that was tried, including the failures, is in
[`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | Queries read first and their terms interned; the corpus tokenized once, in parallel chunks, into arrays of query-term ids; CSR postings built in two passes; no stored positions; queries answered in parallel blocks | pending | 3-5 -> about 1.4 | | pending |
| 2 | Open-addressing term table with the key inline (first 8 and last 8 bytes plus length) in place of `HashMap<&[u8], u32>` | pending | tokenize 0.73 -> 0.51 | | pending |
| 3 | `mmap` the corpus and the queries | pending | 2.04 -> 1.85 (5 rounds) | 593 -> 349 | pending |
| 4 | The per-document token loop is split into three tight loops (tokenize into keys, probe the term table for all of them, update the counters) so the independent table probes overlap their cache misses; no `unsafe`, no prefetch instruction | `run-c4d559ad6869` (commit `db4cc76`, steps 1-4 together) | 0.97 -> 0.84 (9 interleaved rounds) | 349 | 53.19 (board) |
| 5 | Each chunk counts, per term, the documents that contain it; once every chunk is counted the global offsets are known, and each chunk's worker writes its document ids straight into their final slots in one flat array (relaxed atomic stores through a small, documented `unsafe` view), instead of building per-chunk postings and joining them afterwards | pending | about equal (988 vs 1019 ms median over 15 interleaved rounds); the join step is gone | 349 -> 273 | pending |
| 6 | Rebuild around **pieces**: the corpus is cut into 4 MB line-aligned pieces; a worker tokenizes a piece (AVX2 token mask, branch-free key, bucketed term table with prefetch), builds u16 piece-local postings, runs all 50,000 queries on it, and folds each answer into per-worker count / sum / first-ten accumulators merged at the end. No global token array and no global postings. | pending | 1,479 -> 968 ms (7 interleaved rounds, loaded laptop) | 274 -> 118 | pending |
| 7 | 1 MB pieces; terms in at least `ndocs/64` documents of a piece also get a bitmap (word-wise AND/OR/NOT, six masked popcounts for a bitmap answer's sum); a presence bitset skips queries that cannot match the piece; a shared per-query marker stops logging ids that cannot be among the ten smallest | pending | 1,438 -> 901 ms vs the step-5 base (9 interleaved rounds); 845 -> 796 vs step 6 (7 rounds). Harness replica (5 runs): 0.664 s | 274 -> 70 | pending |
| 8 | Source trim and small memory wins: per-term stamp and count packed into one array, query-file-sized term table (no regrowth), plain merge for intersect (the binary-search branch never mattered on piece-sized lists), the `TRACK_B_TIMING` phase printing removed, `process::exit(0)` after the output is written | pending | 793 -> 797 ms (11 interleaved rounds, no change within noise) | 70 -> 64 | pending |
| 9 | The query file is parsed in one SIMD pass with the corpus tokenizer (upper case tokens are keywords, lower case ones terms) and terms are interned straight from the file (no per-line `split`, no padding-buffer copy, presized vectors). The serial parse before the workers start was 27-40 ms in Docker | pending | whole run 734 -> 647 ms median (7 interleaved rounds, loaded laptop); callgrind: conditional branches 17.0 M -> 9.6 M and mispredicts 946 k -> 545 k on a tiny corpus | 67 -> 65 | pending |
| 10 | Postings are filled with the term's start offset as its own write cursor (restored afterwards) instead of a per-piece `Vec<u32>` copy, and `info` is reserved at its exact size: one 300 KB allocation and one pass fewer per piece, no doubling slack | pending | not distinguishable from noise (8 interleaved rounds on a loaded laptop, medians 822 vs 795 ms) | 67 -> 59 (peak of 8 runs: 66.8 -> 58.9) | pending |

Source lines (the harness's count) went from 659 to 924 at step 7 and 889 at step 8: the AVX2 scanner, the
bucketed table, bitmaps and the merge cost 265 lines.

## What the time is now (steps 6-7)

Per-thread CPU on the laptop, single thread, full data: tokenize and build
about 1.9 s, of which the term lookup is about 40%; answering 50,000 queries
on 250 pieces about 1.5 s (the per-query-per-piece floor is 12.5 M presence
checks, about 0.45 s; 3.9 M of them survive and cost about 1 s, 85% BOOL).
Parse queries is serial and about 0.02-0.04 s.

## What the time was before step 6

Full data, 250,000 documents, 36.3 M tokens: parse queries about 0.04 s,
tokenize and build postings about 0.55-0.85 s depending on machine load,
answer 50,000 queries about 0.19-0.25 s (three runs, total 0.83-1.11 s). Inside the tokenize phase, per thread:
scan (classify, look up, count) 450-630 ms, second pass and prefix sums
33-92 ms, per-term join 34 ms.

## Why step 4 worked

The lookup, not the byte scan, was the cost (see below), and each probe is a
random access into a 6 MB table. In the old loop every token boundary was a
branch the predictor often missed, which kept the CPU from starting the next
token's probe while the last one waited on memory. With the probes in their
own loop over a document's keys there are no such branches between them and
the out-of-order core keeps many misses in flight at once.

## Measured, not worth it

* **The lookup dominates, not the byte scan.** A run that skipped the term
  lookup finished in 245 ms against 933 ms. 79% of the tokens are query
  terms, so rejecting non-query words early cannot help.
* **Table size.** 1.5x terms: no change; 8x terms: 15% slower.
* **Merging two per-term arrays into one cache line:** 1165 vs 1153 ms over
  9 rounds, noise; reverted.
* **AVX2 set operations.** Answering queries is 20-25% of the run; a 2x gain
  there is about 0.1 s and adds `unsafe` lines.
* **Dropping the id array to save memory.** The 19,000 phrase queries have
  214 k candidate documents covering 63 M tokens (1.7x the corpus), so
  re-tokenizing candidates would cost more than keeping the ids.

* **Branchless merges** for intersect, union and difference (cursors
  advanced with arithmetic): answer phase median 255 vs 236 ms over 9
  interleaved runs, slightly worse (the extra buffer writes cost more than
  the saved mispredictions); reverted.
* **Where the answer phase goes** (CPU time summed over 12 workers, about
  2.6 s for a 205-260 ms phase): BOOL 1.5-1.8 s (about 48 us per query),
  PHRASE 1.1-1.3 s (about 59 us). Only 100-140 BOOL and about 60 PHRASE
  queries take over 2 ms, and they are 30-40% of the CPU time, so the cost is
  spread over many small queries. A bitmap form for the biggest lists would
  speed those few up but needs about 35 MB and 30 lines, worth under 1% net.

## Why step 5

The peak was three large arrays alive together: the token ids (145 MB), every
chunk's own postings (119 MB) and the joined postings (119 MB). Counting
first and writing in place removes the middle one, which takes the peak from
349 to 273 MB, and removes the join (about 34 ms of copying). The source is
28 lines shorter (886 against 914, the harness's count).

## Where the unit tests live

The unit tests are in `tests/track_b.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_b.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 886 to 659.

## Why steps 6 and 7

Memory was the big lever: 145 MB of token ids plus 119 MB of postings only
exist so that queries can be answered later. Answering every query on each
piece right after tokenizing it lets a worker's token ids and postings live
in a few MB (1 MB of text is about 145 k tokens), reused for the next piece,
so the peak is just 12 workers x a few MB plus the accumulators. The merge is
exact because a worker takes pieces in ascending order: its first ten ids per
query are its ten smallest, and a shared per-query marker (`full[q]`, the
base of a piece after which some worker already holds ten ids) lets the
others skip logging ids that cannot be among the ten smallest.

Measured, single thread, 1 MB pieces: a query-piece pair costs about 37 ns
just to test presence, and the ~3.9 M surviving evaluations about 270 ns
each. A branch-free term lookup (`key`, a 3-way bucket compared without
branches, one rarely-taken slow path) cut scan + key from 154 to 61 ms per
64 MB; an explicit prefetch of the bucket when the key is made cut the lookup
from about 25 to about 15 ns per token.

## Tried and dropped (steps 6-7)

* **Piece size** 1, 2, 4 MB: native wall times were within noise (535-767 ms);
  1 MB chosen for memory (118 -> 76 MB at the same speed).
* **The presence prefilter alone** (before bitmaps): answer phase 1.7-2.0 s ->
  1.6 s single thread; only 31% of query-piece pairs survive, but the
  survivors, not the 12.5 M checks, are most of the cost.
* **Bitmap answers with the sum-of-positions popcount trick**: did not move
  the answer phase (1.53 s vs 1.56 s); the surviving evaluations spend most
  of their time outside the merges (about 180 cycles per operation, but
  500-800 per evaluation).
* **Per-query rdtsc / atomic profiling** perturbs the loop by about 70 cycles
  per query; phase-level differences (skip a query kind) were more reliable.
* **Parallel merge of the logs**: 33 -> 17 ms, kept; the log itself dropped
  from 1.46 M to 0.67 M entries with the `full[q]` marker.

## Tried and dropped (round 2)

* **Anchor index** (queries grouped by their rarest required term, so a piece
  only runs queries whose anchor it contains; anchors from a 1 MB sample, or
  from the first piece a worker finishes via a `OnceLock`): correct (hash
  matched) but not faster in interleaved A/B (852 vs 818, 726 vs 782, 812 vs
  884 ms against HEAD) and +57 source lines; discarded.
* **Shaving the serial parse** (about 25-45 ms in Docker): a presized table
  helps little; a parallel parse would need a merge of per-part tables and was
  not tried.
* Positional output writes from the workers and buffer reuse: the output is
  1.4 MB (render + write about 5 ms) and per-piece buffers are already reused;
  not worth the lines.

## Round 3 findings (steps 9-10)

* **Where the CPU goes** (perf, cpu-clock, full data, answering functions
  pinned with `inline(never)`): `Piece::flush` (table lookups) 16%, the
  per-token scan closure 13%, the scan loop and per-piece term-sized passes
  12%, `eval_bool` 8.5%, `intersect` 8%, `possible` 6.5%, `answer_piece` 6%,
  phrase evaluation 4%. Callgrind on 8 pieces: tokenize and build about 50
  instructions per input byte (about 190 per token), answering 21 M
  instructions per piece (about 430 per query-piece pair, spread thin: no
  function above 3%). Skipping the answering phase locally gave 330 ms
  against 650 ms, so the two halves are about equal.
* **Serial parts of the run** (Docker, `Instant` around each phase): query
  parse 27-37 ms (still 27 ms on a second, warm call, so it is work, not
  cold start), merge 8-12 ms, render + write 6 ms. Step 9 attacks the first.
* **Dense-term filter and AVX2 scan in PHRASE evaluation** (intersect a
  candidate list against the bitmap of a dense term instead of walking its
  list; find the pivot id in a document with 8-wide compares): 574.3 M ->
  572.5 M instructions on 8 pieces, no effect; dropped.
* **Parallel parse** (partitioned term table, per-partition insertion, one
  resolve pass) designed but not built: about 90 more source lines for at
  most 20 ms.

## Most promising next step

The 12.5 M presence checks and 3.9 M evaluations per run are the floor of
"every query on every piece". Group queries by an anchor term (the rarest of a
phrase's terms, the first term of a BOOL's AND prefix) and visit only the
queries whose anchor is present in the piece: that removes the 37 ns floor and
most evaluations. The anchor needs global term counts, which one or two sample
pieces can estimate.

## Caveats

* Step 5 has not run on the scoring box. Expect the board's T to differ.
* The tokenizer is deliberately byte-based (spec 2.2): anything above 0x7F
  separates tokens. A Unicode-aware tokenizer fails the hash.

## Round 4 (steps 11+)

Deterministic counts: callgrind on 8 pieces (8 MB of the full corpus, all 50,000
queries, one thread, `--branch-sim=yes`). Wall times on the shared laptop swung
500-900 ms between identical runs, so none are claimed.

| Step | Change | Run ID | Instructions (8 pieces) | Mispredicts |
|---|---|---|---|---|
| 11 | The token mask is 32 bytes wide (`u32`), not 64. Joining two 32-bit `movemask`s into one `u64` made LLVM turn the mask into 64 separate bools and rebuild it with `vpinsrb`: about 300 instructions per 64 bytes, 52 M of 580 M instructions. Also the scan loop walks the starts and pairs each with the next end, with no per-token `open` test | pending | 580.1 M -> 533.9 M (-8.0%) | 2.73 M -> 2.84 M |


<!-- board-runs:begin -->
## Board runs

Each row is one scored run of the named commit on the live board, copied from the public board feed (`board.json`). The feed keeps only each track's latest and best run, so runs before these cannot be listed; earlier steps in the table above stay local measurements. Board timing varies by about 1-2 ms between runs of identical code: every push re-runs all four tracks, so a row whose change names another track measured this track's unchanged code and shows that noise.

| Commit | Change | Run ID | T (s) | M (MiB) | S | Points |
|---|---|---|---|---|---|---|
| `32f54ce` | Track A: the writer thread exits the process after the last block (agent-a4 d262378) | `run-67ca8f767f8d` | 0.3209 | 61.0 | 869 | 64.13 |
| `b67a992` | Track A: link the C runtime statically (skips dynamic loading at start-up) | `run-9a0f2cb22439` | 0.3393 | 61.4 | 869 | 63.49 |
| `f299e99` | Track B: parse the queries in one SIMD token pass with leaner term interning (agent-b 8537 | `run-0185f7fa29f3` | 0.3143 | 60.9 | 860 | 64.40 |
| `33b8455` | Track B: postings fill uses the term offset as its own cursor (agent-b d49c673) | `run-604a4ee5a8cd` | 0.3165 | 56.8 | 864 | 64.61 |
| `b208288` | Track A: workers write their own hour block at its final offset; no writer thread (agent-a | `run-322d9de18375` | 0.3117 | 57.3 | 864 | 64.73 |
| `cd4c35e` | Revert "Track A: workers write their own hour block at its final offset; no writer thread  | `run-531bf965b055` | 0.3175 | 57.7 | 864 | 64.50 |

<!-- board-runs:end -->
