# RESEARCH_B: cloud research on Track B (Rust, base `3cbb939`)

Real full dataset (docs.txt `6cd46167...`, queries.txt `77ef7ffc...`), results hash `f86973c2...` checked on every build. 4-vCPU cloud VM, interleaved runs: a ranking of ideas, not board numbers.

## Result: input prefetch (branch `cloud-b-prefetch`, +1 code line)
`_mm_prefetch(p + 2048)` per 64-byte window in `tokens()`. 4 CPUs: wall 272.5 -> 251.3 ms (-8%), CPU 872 -> 790 ms (-9.5%). 1 CPU: 852 -> 822 ms (-3.6%). 4096 ahead is the same. Docker image (pinned rust image) builds and passes the hash. Smaller than on A (-26% CPU) because B does far more work per input byte.

## Where the time goes (instrumented copy, 4 CPUs, wall ms from start of main)
| Phase | ms |
|---|---|
| queries parsed (single thread, 263 k words, 50,000 queries) | 10.5 |
| sample (1/32 of docs, includes the one-time spawn of 11 threads) | 10 |
| renumber / class sort / bitmap alloc (single thread) | 6 |
| scan (12 static shares) | ~170 |
| postings scatter | 30 |
| answers | 31 |
| write | 2 |

- The serial prefix (queries + renumber, about 16 ms plus the spawn) is pure single-thread CPU: the query parse costs 9.5 ms even on 1 CPU. It is about 10% of this run and a larger share of a 12-thread run, so it is the best structural target left on B. Ideas, all UNVERIFIED: parse the queries in parallel (ids only need to be consistent, the later renumbering sorts by class, then rarity, then id), or overlap the parse with the sample by splitting the query file.
- Per-thread scan CPU time is balanced: 48.5-59.6 ms, mostly 49-54 (one thread +18%). Perfect dynamic balancing could win at most about 5% of the scan, and would need per-piece `count`/`last`/bucket arrays (memory) and about 40 lines (about -0.2 pts at S 617). Not worth it on this evidence.
- Postings + answers are about 24% of the local wall time and both are parallel already.

## Points arithmetic for B (board 167 ms, 50 MB, 617 lines)
1 ms = 0.065 pts, 10% of T = +1.15 pts, 1 MiB = 0.09 pts, 10 lines = 0.05 pts. Leaders: another ello 74 ms, Cause Of Death 100 ms.
