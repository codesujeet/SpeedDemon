> **Status note (2026-11-01).** This log is the rehearsal-era record: its numbers were measured on a laptop before any board run, and the sentence below that says no run IDs exist was true then. The board runs with real run IDs are in the "Board runs" table of each track's `PERF.md`, which is the source for scoring claims. Since this log was written, tracks A and C were rewritten in C++ (see their READMEs) and tracks B and D were replaced by newer drafts; the Rust variants it describes for A and C are kept at git tags up to `best-19`. Treat the entries here as history, not as a description of the current code.

# Experiment log (rehearsal repo, real specs, full-scale data)

Every number below was measured on a laptop, inside Docker (Linux VM, 12
vCPUs), with the harness's run flags (`--memory=8g`, `--read-only`, 12
cpuset, input volume pre-warmed from a separate container) and `memory.peak`
read inside the container. **Laptop numbers are for choosing between
variants, not for scoring**: the hybrid CPU and background load swing a
single run by 30-50%, so every comparison is an *interleaved* A/B
(`real-spec/ab.ps1`: A, B, A, B, ... one run each, median reported).
Board run IDs: none yet, this repo never touched the board.

Reference times on the scoring box: A 32.05 s, B 56.35 s, C 14.44 s,
D 72.69 s. Every kept change below still matches the published SHA-256 of
the full-scale output.

## Kept

| Track | Change | Before -> after (median, rounds) | Peak MB | Commit |
|---|---|---|---|---|
| A, B, C | `mmap` the inputs instead of `fs::read` (the host pre-warms `/data` into page cache, so mapped pages are not charged to the run's cgroup) | A 1129 -> 1051 ms, B 2040 -> 1846 ms, C 773 -> 517 ms (5 rounds each) | A 332 -> 104, B 593 -> 349, C 315 -> 119 | `9a5c7aa` |
| B | specialised open-addressing term table with inline 8+8-byte key | tokenize ~730 -> ~510 ms | | `0c439e5` |
| C | count distinct names per owning worker instead of one global set | ~600 -> ~300 ms | | `5975708` |
| D | expand the queue with fewer entries, not the smaller key | 3934 -> 2580 ms (interleaved), 3x fewer heap pops | ~505 | `190760c` |
| D | parallel line-aligned parse; owner-range parallel CSR builds (each worker owns a node range, scans all edges twice, writes only its own slice) | 2353 -> 1494 ms (5 rounds) | ~520 | `8d8dcfd` |
| A | group keys are `&[u8]` slices of the mapped input (no allocation per line); merge, percentiles and formatting run in parallel over blocks of sorted keys | 695 -> 423 ms (7 rounds) | 104 -> 83 | `fec5872` |
| D | one interleaved `[fwd, bwd]` label per node (stored as dist+1 so 0 = unreached), packed `(target, weight)` adjacency, one search loop for both directions, wide searches cleared by a sweep | 1033 -> 1103 ms (9 rounds, about 7% slower, inside the noise) | 515 -> 381 | `1912b9f` |
| A | `find_byte_swar` replaced by `position` (one call per 2 MiB morsel) | 331 -> 298 ms (7 rounds, neutral) | 80 | `baaaceb` |
| D | reverse CSR built on the idle cores during the serial BFS; both CSRs renamed into BFS order by a row copy instead of relabelling edges and building twice | 0.90-1.01 -> 0.84-0.96 s total (Linux container, 3 runs each) | 380 | `d8a3114` |
| D | skip relaxations with `nd >= best` | query phase 436 -> 420 ms, inside the noise; sound and 3 lines | 377 | `see git log` |
| A | merge by hour: collect (key, data) pairs, integer-sort by hour, then one worker per hour sorts its endpoints and merges runs of equal keys (no map lookups, no serial pointer-keyed sort) | 282 -> 208 ms median (9 interleaved rounds); merge phase 150 -> 50 ms | 78 | `e7696f5` |
| C | exact distinct names by sorting each group's `(hash, name)` pairs (bytes compared only inside equal-hash runs) instead of per-owner hash sets | parse+aggregate 234 -> 170 ms median (9 interleaved rounds); total about 0.33 -> 0.21 s | 123 -> 141 | `c1cb434` |
| B | per-document token loop split into keys / probes / counters so the term-table probes overlap | 968 -> 838 ms median (9 interleaved rounds); harness replica about 1.17 -> 0.82 s | 349 | `870fbcc` |
| D | edges filed under their owning worker before the CSR is counted and placed (each worker scans 1/12 of them per pass) | original CSR phase 81 -> 56 ms; total 837 -> 780 ms median (9 interleaved rounds); harness replica 0.91 -> 0.78 s | 380 | `be9a026` |

## Tried and reverted (no measurable gain)

| Track | Change | Result |
|---|---|---|
| A | parse 256 lines into (key, status, latency) records, then update the map in a second loop | 188 -> 179 ms median over 11 interleaved rounds (inside the noise), +8 lines |
| B | query dictionary pre-sized to 2^17 entries (the serial `parse queries` step, about 35 ms) | median 36.7 vs 37.1 ms over 9 interleaved runs: no change |
| B | branchless intersect / union / difference | answer phase 255 vs 236 ms median over 9 interleaved runs: slightly worse |
| A | endpoint interning per worker (hot table, then `(hour, number)` group key) | 207 vs 191 ms median over 11 interleaved rounds: no gain |
| A | cascading `select_nth_unstable` (p95 searches only above p50's slot, p99 above p95's) | 463 vs 501 ms over 11 rounds: no gain. The 196 ms merge phase is not dominated by selection. Tests passed, correctness fine. |
| B | `last` and `count` merged into one `(u32, u32)` array per term | 1165 vs 1153 ms over 9 rounds: noise |
| D | heap entry packed into one `u64` (`dist << 21 \| node`) | query phase median 430 vs 436 ms over 10 interleaved runs (about 2-3% of the total); about 8 lines plus an overflow assert |
| D | first version of the interleaved labels, per-node reset only (an intermediate step, not a revert: superseded by the kept change in `1912b9f`) | 1065 vs 1216 ms: 14% slower; the sweep-clear added there recovered most of it |

## Measured, not applied

| Track | Experiment | Result |
|---|---|---|
| B | term table at 1.5x terms (3.1 MB) vs 2x (6.3 MB) | 976 vs 960 ms: no difference |
| B | term table at 8x terms (about 25 MB) | 1055 vs 914 ms: 15% slower, so the lookup is only mildly cache-bound |
| B | skip the dictionary lookup entirely (output wrong by design, timing only) | 245 vs 933 ms: tokenizing and pushing are about 150 ms; the lookup-and-count chain is most of the rest |

## Rejected after profiling

* **AVX2 intersection for B's BOOL/PHRASE queries.** Per-phase profile on
  Linux (three runs, total 0.83-1.11 s): tokenize 0.55-0.85 s, answer
  queries 0.19-0.25 s (20-25%), parse queries 0.04 s. Even a 2x on the
  query phase saves about 0.1 s and adds `unsafe` lines that count against S.
  (An earlier profile on a more heavily loaded machine read 1.2 s / 0.35 s.)
* **Dropping B's token-id array and re-tokenizing phrase candidates on
  demand.** The 19,000 phrase queries have 214,226 candidate documents
  covering 62,986,655 tokens, about 1.7x the corpus (36,276,657 tokens).
  Re-scanning would be slower than keeping the ids.
* **A prefilter that rejects non-query words before the term-table probe
  (B).** 79.2% of the 36.3 M tokens are query terms (75,747 distinct terms
  in the queries), so there is little to reject.
* **An approximate histogram for A's percentiles.** The output is checked
  by an exact hash, so percentiles must be exact nearest-rank.

## Rejected without measuring

* **Parallel BFS for D's node relabelling.** The serial BFS is about
  0.25 s; a level-synchronous parallel BFS pays a barrier per level on a
  lattice-like graph with long diameter.

## Probe data worth remembering

* C, full data: 3,320,000 records, 2,980,424 distinct (country, name) pairs
  (10.2% duplicate inserts), names up to 25 bytes (94% at most 16, 99.9% at
  most 20). Stage split before the change: bounds 14 ms, parallel parse
  75-155 ms, distinct names 150-215 ms. A fingerprint-only distinct set (not
  exact) ran the phase in 177 ms against 266 ms.
* C parse stage (bounds 14 ms included): median 88 ms; without the names
  push 75 ms, without the region lookup 71 ms, fastest run 60 ms. Moving
  the name hashing into the parse would avoid cold reads later but add
  about 40 MB, about break-even in K.
* B answer phase CPU by kind (12 workers summed): BOOL 1.5-1.8 s, PHRASE
  1.1-1.3 s; 100-140 BOOL and about 60 PHRASE queries over 2 ms make up
  30-40% of it.
* B, full data: 250,000 docs, 36,276,657 tokens, 75,747 query terms,
  31,000 boolean and 19,000 phrase queries, phrase candidate max 1,054
  docs per query.
* B per-thread (12 threads, about 3.02 M tokens each): scan 453-490 ms on
  8 threads and about 630 ms on the 4 slower cores; pass B and prefix sums
  33-92 ms; term-range join 34 ms.
* D, full data: 1,740,512 nodes, 5,000,000 edges, 2,000 queries. Phases
  after the kept changes: BFS reorder about 250 ms (serial),
  CSR builds about 80-100 ms each, queries about 430-490 ms, parse about
  45 ms (three runs, total 0.90-1.01 s).
* A, full data: 3,100,000 lines, 238,836,234 bytes. After the kept changes:
  parse and group about 110-125 ms, merge+percentiles+format about 50 ms,
  write about 7 ms (the merge phase was about 150-196 ms before the
  per-hour merge).
* Harness replica (`real-spec/local_harness.py`: allowlist gate, offline
  build, `sd-timer`, per-run cgroup parent, `docker diff`, 5 runs, mean of
  the middle three, max peak), laptop, full data, all hashes MATCH, after
  this round's changes (earlier figures in parentheses):
  A 0.20 s / 80 MB (0.48 s / 87 MB), B 0.80 s / 350 MB (1.17 s / 349 MB),
  C 0.22 s / 141 MB (0.33 s / 125 MB), D 0.78 s / 385 MB (1.15 s / 385 MB; 0.91 s before the owner buckets).
* S, measured with the harness's own `static_axes.py` (scc after rustfmt,
  tests and the 7-line Dockerfile included): A 537, B 905, C 543, D 736
  against reference lines of 56 / 91 / 108 / 43; D (dependencies) is 1.
  With the official references that is roughly K 11.2 / 8.6 / 7.7 / 7.4,
  about 52 / 47 / 44 / 43.5 machine points (they were 42.5 / 42.7 / 40.4 /
  39.7). Laptop times, not scoring-box times.
