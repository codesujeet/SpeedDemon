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
| 12 | The serial tail is parallel and shorter: each worker logs its first ten ids into one log per range of queries, each merge thread reads only its own range (it used to scan every log and sort its share), picks the ten smallest by insertion, and renders its own lines; the main thread only writes the chunks. The 2.8 MB `Answer` array, the serial `render` and the "done in" timing print are gone | pending | after the last worker: 18-20 ms -> 6.5-11 ms (merge 11.5 -> 4-6.6, render 5.5 -> 0, write 1.5-5; `Instant` timestamps, Docker harness flags, 8 runs on a loaded laptop) | not measured |
| 13 | Term lookup: the length is folded into the second key word (one bit per byte in the unused top bit, `pdep`), so a bucket's three entries are matched with two vector compares; CRC32C bucket hash instead of two multiplies; a miss reads a spare slot (the sink id) instead of branching; the dedupe of a document's terms is fused with its lookups; `intern` moved to the tests | pending | 533.9 M -> 490.5 M (-8.1%); flush 98 M -> 46 M; parse 27 M -> 19 M | 2.84 M -> 3.02 M (flush 54 k -> 38 k; the rest is code layout in query evaluation) |
| 14 | `intersect` binary-searches the longer list when it is more than 16 times the shorter (a phrase's rare anchor list against a common term's list, `A AND rare`); a phrase starts from its rarest term by one linear min instead of sorting a copy of its terms | pending | 479.9 M -> 448.8 M instructions (-6.5%); 4 interleaved dock runs, 4 of 4 faster (median 598 vs 750 ms, loaded laptop) | 3.03 M -> 2.93 M |


Tried and dropped in round 4 (callgrind, 8 pieces, instructions): the same binary
search in `union` and `difference` (453.4 M -> 453.5 M); bitmap threshold
`df >= w * k` for k = 1/4, 1/2, 1, 2, 4, 8 and none: 498.9 M, 486.9 M, 479.9 M,
478.6 M, 480.7 M, 488.3 M, 547.1 M, so the existing `w` stays (k = 2 is within
0.3%); a ratio of 4, 8, 16 or 32 for the binary search: 453.0-453.7 M, flat.

## Replacement by an alternative implementation (not yet on the board)

`src/main.rs` was replaced by a different implementation drafted with
ChatGPT by the project owner (a full scan of `docs.txt` with query terms only,
phrases checked at their rarest term while the text is in cache, bitmaps for
common BOOL terms, 16-bit posting lists). It was reviewed and tested here
and is not part of the step table above, whose rows describe the earlier code.

Local comparison against the previous version (this laptop, Docker with the
board's run flags, full public dataset, 15 interleaved rounds, hash checked
every run): median wall 495 ms against 555 ms, `memory.peak` median 49.6 MiB
against 58.4 MiB. Source: about 610 code lines after `rustfmt` (Dockerfile
included) against about 880, because the unit tests are gone. Also correct on
the generated datasets of seeds 2, 3 and 7 (10 runs each, no failure), on the
tiny and sample scales, and on the edge-case suite apart from inputs outside
the spec (empty query file, a corpus of a few bytes, more than 393,000
documents). `cargo clippy -D warnings` is clean after changing one statement.
Points are estimated, not measured: scaling the last board result by these
ratios gives about 69 against 66; a board run will replace this estimate.


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
| `e804869` | PERF.md: add a Board runs table with real run IDs from the public feed to every track | `run-f64748d5e5f3` | 0.3134 | 57.2 | 864 | 64.69 |
| `36f4296` | Tracks A and C: profile-guided optimization from a committed profile (no source or Dockerf | `run-43d319d067db` | 0.3270 | 57.3 | 864 | 64.22 |
| `158f18f` | Track B: 32-byte token mask (agent-b 0054521); Track A PERF.md header with current board n | `run-62fab5acecb4` | 0.3075 | 57.2 | 866 | 64.88 |
| `49548fa` | Track B: merge and render in parallel from per-range logs (agent-b a34f4cf) | `run-9bbde7f89a2e` | 0.3134 | 58.5 | 857 | 64.62 |
| `3286475` | Track C: 111-bit name fingerprint (second independent hash compared on every tag match), d | `run-d49f29c34b28` | 0.2984 | 58.7 | 857 | 65.13 |
| `280509c` | Track B: vector-compare term lookup, CRC32C hash, fused dedupe (agent-b) | `run-adaee4189cd3` | 0.2924 | 58.4 | 870 | 65.32 |
| `892d831` | Track C: regenerate the PGO profile for the 111-bit fingerprint source | `run-b8432c174907` | 0.2940 | 58.2 | 870 | 65.28 |
| `2ca5698` | Track C: back to the 36f4296 source and profile (the wider fingerprint cost 3-5 ms on the  | `run-adb26f28d060` | 0.2739 | 59.1 | 882 | 65.94 |
| `b1497a5` | Track B: 2 MB pieces (agent-b c51c3b0) | `run-37cd73b3c901` | 0.2569 | 71.3 | 882 | 65.82 |
| `f00df83` | Revert Track B 2 MB pieces: T -17 ms but M +12 MiB, 65.82 vs 65.94 on the board | `run-bb89766a881e` | 0.2648 | 59.7 | 882 | 66.26 |
| `b18c263` | PERF: board run IDs through f00df83 (16 scored runs per track) | `run-67a58afa3baf` | 0.2717 | 59.3 | 882 | 66.01 |
| `44a450f` | Track A: mallopt M_MMAP_THRESHOLD 32 KiB, memory.peak 23.0 -> 20.4 MiB locally (agent-a4 c | `run-72ec53983faa` | 0.2722 | 58.5 | 882 | 66.05 |
| `b7bb4d3` | Track A: PGO profile regenerated for the mallopt source (agent-a4 90f380c) | `run-e60e1b087bfa` | 0.2945 | 59.2 | 882 | 65.14 |
| `4c454fe` | Track A: three-phase port with exact output sizes, every thread formats into the mapped ou | `run-014c6b1aaf05` | 0.2686 | 58.2 | 882 | 66.21 |
| `5476630` | Re-run board on the Track A port (noise check) | `run-8ba90ced8ffc` | 0.2731 | 59.1 | 882 | 65.97 |
| `6c9bc1e` | Tracks B and D: user-drafted Rust solutions, reviewed (alt-bd c90b4a8): B 603 lines, D 509 | `run-523ddc469568` | 0.2363 | 49.5 | 611 | 69.51 |
| `f47d313` | Track C: user-drafted C++ design ported to Rust, CRLF chunk cut, 440 lines (agent-c3 7827d | `run-6268f725a495` | 0.2331 | 49.0 | 611 | 69.70 |
| `c4832fa` | Re-run board on the Track C port (noise check) | `run-d18be8f9268b` | 0.2402 | 52.2 | 611 | 69.10 |
| `4f9e4bd` | Track D: PAGE 512 -> 256 (experiment) | `run-bd0646debd8b` | 0.2348 | 49.1 | 611 | 69.61 |
| `7a3ceb9` | Track D: BLOB 512 -> 1024 (experiment) | `run-d6e5090d20ce` | 0.2395 | 53.3 | 611 | 69.04 |
| `0b9abd0` | Track D: BLOB back to 512, CHUNK 1024 -> 4096 (experiment) | `run-08de56e336b8` | 0.2390 | 49.1 | 611 | 69.42 |
| `1a4d82e` | Track D: PAGE 256 -> 128 (experiment) | `run-b25f263a3bf3` | 0.2693 | 50.7 | 611 | 67.98 |
| `a2fa18e` | Track B: persistent thread pool instead of 12 fresh threads per phase (experiment) | `run-ecdc074c9d6f` | 0.2347 | 50.9 | 632 | 69.35 |
| `b573d30` | Track C: C++ (gcc 15, user-drafted design: pclmul quote mask, dense-id name table); Rust v | `run-da6a37a31458` | 0.2502 | 57.1 | 611 | 68.26 |
| `17ce1b0` | Track C: static link and _exit(0) after the output (experiment) | `run-203a2a4c2b34` | 0.2355 | 48.9 | 611 | 69.59 |
| `9e9667f` | Re-run board on static+_exit Track C (noise check) | `run-46b0cfef039b` | 0.2378 | 48.7 | 611 | 69.51 |
| `308cbff` | Track A: C++ (gcc 15, static, _exit after the output); Rust version stays at tag best-19 | `run-3376333db36a` | 0.2675 | 53.2 | 611 | 67.85 |
| `4d1d805` | Tracks B and D: exit right after the output so the big arrays are not freed one by one | `run-67cd42ebed11` | 0.2336 | 49.5 | 612 | 69.62 |
| `d68c42e` | Untrack Cargo.lock files added by mistake | `run-9c57c827fbde` | 0.2397 | 49.9 | 612 | 69.31 |
| `23561b4` | Track C: LAG 16 -> 32 (experiment) | `run-9278efa3d8ca` | 0.2329 | 49.6 | 612 | 69.65 |
| `a302a61` | Track C: LAG back to 16 (32 scored 75.61, no gain) | `run-3ffa11504876` | 0.2377 | 49.7 | 612 | 69.42 |
| `70fee14` | Track A: 16 pieces per thread (experiment) | `run-bd19791e600a` | 0.2321 | 49.6 | 612 | 69.69 |
| `8d367d9` | Track C: new user-drafted C++ (64-bit hash key, field-by-field parse, distinct counts from | `run-c679335928ce` | 0.2597 | 55.9 | 612 | 67.95 |
| `2ce9b21` | Track B: new user-drafted Rust (token id stream, CRC hash, bitmap for big lists, persisten | `run-42bec5c66bdc` | 0.1767 | 47.2 | 609 | 72.87 |
| `a46cccc` | Track A: new user-drafted C++ (branch-free width, 64-byte name compare, latency buffer siz | `run-dff4c67a492f` | 0.1866 | 47.3 | 609 | 72.27 |
| `428ae7e` | Track A: new file without the sizing pass over all records (experiment) | `run-bfc9e1bb8e38` | 0.1728 | 47.1 | 609 | 73.13 |
| `8b2268b` | Write-ups for the second A, B and C drafts, with board run IDs | `run-8e894fb86087` | 0.1818 | 50.7 | 609 | 72.27 |
| `f34afab` | Track B: BIG 768 -> 1536 (experiment) | `run-834f45470648` | 0.1748 | 47.3 | 609 | 72.99 |
| `576267e` | Tracks A and C: each thread releases a consumed input piece's pages at once (from the new  | `run-a6aebb16d412` | 0.1737 | 47.2 | 609 | 73.07 |
| `a94d376` | Track B: each thread releases its share of docs.txt after the scan | `run-d58c749305c7` | 0.1674 | 47.2 | 617 | 73.42 |
| `4dde5ce` | Track B: DIAGNOSTIC (answers computed twice) to measure the answer phase on the board; wil | `run-1401c9180e0e` | 0.1919 | 47.2 | 619 | 71.93 |
| `ab00e1a` | Write-ups for the page-release changes and the answer-phase measurement; board run IDs | `run-34580d586e8f` | 0.1756 | 47.6 | 617 | 72.87 |
| `bb07ac5` | Track A: 16 pieces per thread, now that pieces are released as they finish (experiment) | `run-1d25a19ab139` | 0.1734 | 47.2 | 617 | 73.04 |
| `3cbb939` | Track A: prefetch the input 4 KB ahead in the scan loop | `run-4ed70e00b26d` | 0.1708 | 47.5 | 617 | 73.18 |
| `fff546f` | Track B: prefetch the input 2 KB ahead in the token scan | `run-dff88ce2292a` | 0.1921 | 53.9 | 618 | 71.35 |
| `ff9de8e` | PERF: B prefetch experiment (reverted) with its board run ID | `run-c074fc74a66b` | 0.1704 | 47.2 | 617 | 73.23 |
| `c27bda3` | Track A: build with -O2 instead of -O3 (experiment, same source) | `run-eefd4ed9ec73` | 0.1936 | 46.7 | 617 | 71.90 |
| `8218980` | PERF: A -O2 note names its run ID | `run-e5e6a3f8b3e9` | 0.1703 | 47.7 | 617 | 73.20 |
| `7f5e52c` | Track C: one #include <bits/stdc++.h> instead of twelve standard-library includes | `run-a8dc53031a4b` | 0.1683 | 47.6 | 617 | 73.33 |
| `79d68e7` | Track C: region_line also accepts a CRLF region file (fixes a crash in the previous commit | `run-0c3e870b7119` | 0.1733 | 47.3 | 617 | 73.04 |
| `5338276` | Log board runs for the line cuts (A S 306, C S 368) | `run-4ad6f77a0f88` | 0.1816 | 51.8 | 617 | 72.14 |
| `2a7cc01` | Track C: DIAGNOSTIC, record parse disabled to measure the floor (output is wrong; reverted | `run-c1c4dce2a292` | 0.1697 | 47.3 | 617 | 73.27 |
| `4ba1710` | Track C: DIAGNOSTIC, the mark is a plain load instead of the atomic OR (output is wrong; r | `run-0686fd197d2b` | 0.1742 | 47.2 | 617 | 72.99 |
| `67464a6` | Track C: DIAGNOSTIC, no name-table lookup (id taken from the key bits); output wrong; reve | `run-adb1ebea465d` | 0.1690 | 47.2 | 617 | 73.31 |
| `7496859` | Track C: DIAGNOSTIC, the parse only streams the input (one byte per cache line); output wr | `run-190115a381df` | 0.1713 | 47.3 | 617 | 73.16 |
| `0df6a6f` | Track C: decode numbers with pmaddubsw/pmaddwd instead of the scalar multiply chain | `run-289f654f3411` | 0.1736 | 46.8 | 617 | 73.06 |
| `a39d4f6` | Track B: two exact line cuts (610 -> 600 formatted lines) | `run-6711d5829e03` | 0.1724 | 47.3 | 607 | 73.15 |
| `2a7885f` | Track C: find the id's comma with one 16-byte compare instead of the SWAR zero-byte trick | `run-bfcd67fa23ef` | 0.1666 | 47.6 | 607 | 73.49 |
| `37fae72` | Log board run for C's 16-byte comma find (79.90) | `run-1e6abe410603` | 0.1668 | 47.1 | 607 | 73.53 |
| `ba0e248` | README: next-steps paragraph matches the current board times and the C diagnostics | `run-f5407b7d4512` | 0.1941 | 46.7 | 607 | 71.92 |
| `e1bd0df` | Track C: quoted fields that close within 31 bytes on a lone quote skip the 64-byte loop | `run-282dd8057f38` | 0.1676 | 47.5 | 607 | 73.44 |
| `b34456e` | Log board run for C's quoted-field fast path (first reading 79.02) | `run-2c37273b9f1a` | 0.1683 | 47.5 | 607 | 73.39 |
| `351b645` | Track C: parse the two halves of each piece in turn, so two dependent chains overlap | `run-735307b7daa9` | 0.1676 | 47.0 | 607 | 73.48 |
| `800e1ce` | Revert "Track C: quoted fields that close within 31 bytes on a lone quote skip the 64-byte | `run-aec7a53ab3a3` | 0.1674 | 47.5 | 607 | 73.45 |

<!-- board-runs:end -->

## Second draft (`2ce9b21`)

Locally about 17% faster than the first draft in interleaved runs (median about 425 ms against 522 ms) with 46 MB against 50 MB peak. Board: `run-42bec5c66bdc` (`2ce9b21`) 0.1767 s, 49.5 MiB, S 609, 72.87; `run-bfc9e1bb8e38` (`428ae7e`, unchanged code) 0.1728 s, 73.13.

## Releasing the scanned text, and a measurement of the answer phase (`a94d376`, `4dde5ce`)

Each thread drops its share of `docs.txt` after the scan (`MADV_DONTNEED`, clean private mapping, so it cannot change the output). Board: `run-d58c749305c7` (`a94d376`) 0.1674 s, 49.5 MiB, S 617, 73.42, against 0.1737 s and 0.1728 s for the code before it; the gain is small and close to run-to-run noise.

To see how much of B is the answer phase, commit `4dde5ce` temporarily answered every query twice (the first answers overwritten): `run` for that commit read 0.1919 s against 0.1674 s, so the answer phase costs about 25 ms of 167 on the board (about 15%); the scan and the rest is about 140 ms. That commit was reverted straight away (`c127df2`), and it is not part of the final code. The laptop run of the same diagnostic suggested the phase was much larger (about 200 ms of 425), which is why the board number is the one used.

### Prefetch 2 KB ahead in the token scan (tried, reverted)

`_mm_prefetch(p + 2048)` once per 64-byte window, the same trick that helped A. A separate research session measured about 8% less wall on its own 4-vCPU machine. On the board `run-dff88ce2292a` (`fff546f`) gave 0.1921 s, 53.9 MiB, 71.35, against 0.1708 s, 47.5 MiB, 73.18 without it (`3cbb939`). B's board noise is about ±5%, but this was worse on both time and memory, so it was reverted and not re-tried. The scan here already prefetches each token's state, so a second stream of prefetches may only compete with it.
