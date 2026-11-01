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

## Update (base `fff546f`): ideas from the brief
| Idea | Result | Verdict |
|---|---|---|
| parallel release of `post` (20 MB) and `bits` (11 MB) before the write (branch `cloud-b-release`) | 4 CPUs wall 243.9 -> 246.7 ms, 1 CPU 815 -> 817 ms, memory same, +14 formatted lines | no gain |
| `MADV_POPULATE_READ` of each thread's share before its scan | 4 CPUs wall 245.8 -> 250.7 ms, 1 CPU 793 -> 777 ms (flat) | no gain here (the board's contention cannot be reproduced on 4 vCPUs) |
| balance the cut points | see below | cannot help |

**Balancing cut points cannot help.** Tokens per share: 3,020,245-3,026,589 (+-0.2%); phrase hits 1,556-1,685; scan CPU per share on 1 CPU 45.6-47.2 ms (+-1.7%). The shares are already equal in work. The 140 ms board scan against about 46 ms of CPU per share means the board threads are stalled (shared-core hyper-threads, page-fault or memory contention, staggered start), not unbalanced; only dynamic claiming would absorb that, and it is out of scope.

**Instruction profile (callgrind, full data): 7.17 G Ir, about 197 per token over 36.3 M tokens.** `tokens()` 44% (84 per token), the scan closure 27%, answers (`apply`, closures) about 20%, memset 5.5%. No line above 6%. 77.5% of all tokens are query terms and 45% fall on only 377 bitmap terms, so an early "is it a term" filter would not skip much. Per-line leaders: the bitmap `pending[...] |=` line 6.1%, the term-slot compare 4.0%, the lag ring `look` 3.7%, the id store 2.6%. Any real gain here needs a different per-token structure, not a tweak.

## Lesson from the board (after `fff546f`)
The 2 KB prefetch (`cloud-b-prefetch`, -8% wall and -9.5% CPU on the 4-vCPU VM) scored WORSE on the board: 0.1921 s, 53.9 MiB, 71.35 against 0.1708 s, 47.5 MiB, 73.18 without it, and was reverted. B's scan already prefetches each token's slot and state, so a second prefetch stream most likely competes with those. A's prefetch (-26% CPU locally) turned into about -6% wall on the board. So a local 4-vCPU gain overstates the board effect, and for B it even reversed it: only ideas that cut real work, not ones that only change memory behaviour, should be trusted for B. The board also measured B's answer phase at about 25 ms of 167 ms (15%), so the scan and everything before it is about 140 ms.

## What is left, with honest size estimates (none built)
- Cheaper query parse (9.5 ms single thread, 263 k words) and renumber (6 ms): at most about 10 ms of 167 ms, so at most about +0.65 pts on B (+0.2 overall), and about 20 lines.
- C: decoding `id` from its last two digits (ids rise by 1-11, spec 2.2) saves about 2% of C's instructions; not worth the lines.
- No remaining small idea is worth more than about +0.3 overall. Reaching the leaders (B 77-81, C 83.8) needs a different per-record / per-token structure, not tuning of this one.
