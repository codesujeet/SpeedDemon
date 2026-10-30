# Track B: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements, not
board results: a laptop running Docker with the board's run flags on the full
public dataset, with the output hash checked against the published SHA-256.
Local runs swing 30-50%, so each row is a before -> after median from
interleaved or repeated runs, and rows are only comparable within themselves.
The **Run ID** column stays *pending* until the step has been measured on the
live board, and **Points** is only filled from board numbers. Nothing in this
file has run on the scoring box yet. Everything that was tried, including the
failures, is in [`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | Queries read first and their terms interned; the corpus tokenized once, in parallel chunks, into arrays of query-term ids; CSR postings built in two passes; no stored positions; queries answered in parallel blocks | pending | 3-5 -> about 1.4 | | pending |
| 2 | Open-addressing term table with the key inline (first 8 and last 8 bytes plus length) in place of `HashMap<&[u8], u32>` | pending | tokenize 0.73 -> 0.51 | | pending |
| 3 | `mmap` the corpus and the queries | pending | 2.04 -> 1.85 (5 rounds) | 593 -> 349 | pending |
| 4 | The per-document token loop is split into three tight loops (tokenize into keys, probe the term table for all of them, update the counters) so the independent table probes overlap their cache misses; no `unsafe`, no prefetch instruction | pending | 0.97 -> 0.84 (9 interleaved rounds) | 349 | pending |

## What the time is now

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

## Caveats

* Nothing here has run on the scoring box.
* The tokenizer is deliberately byte-based (spec 2.2): anything above 0x7F
  separates tokens. A Unicode-aware tokenizer fails the hash.
