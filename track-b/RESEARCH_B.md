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

## Query parse, measured (not branched)
The 9.4 ms single-thread parse is 4.8 ms of byte-at-a-time slice splitting (`split` closures over 1.8 MB, 263 k words) plus 4.0 ms of table inserts. Prefetching the insert slots 8 words ahead saved only 0.7 ms (9.4 -> 8.8 ms, hash unchanged), so the inserts are not miss-bound. Splitting with the SIMD class masks could save about 4 ms of 167 ms on B: +0.28 pts on B, about +0.07 overall, for about 15 lines. Not worth a branch.

## B campaign, step 1 (branch `cloud-b-diet`) and a method warning
- **`kept`/`anchors` grow-only** (+2/-2 lines): callgrind instructions 7,165.95 M -> 6,951.99 M (-3.0%), but the real saving is about 1% CPU (4 CPUs, 15 runs: 840.5 -> 832.3 ms; 1 CPU flat). Valgrind counts every byte of a `rep stosb` memset as an instruction, so for B instruction counts overstate memset-heavy changes; use interleaved real time. Hash `f86973c2...` on 3 real-image runs. Not worth a board slot alone.
- **Phase ablations do not work for B** (they do for C): deleting the term lookup, the state update, the operator application or the postings scatter produces wrong postings, which makes later phases process far more data (the deleted builds ran 2-4x slower, not faster). Only before/after pairs with identical output are valid.
- **Per-token budget (callgrind, full data, 197 instr/token):** `tokens()` 84.5, scan closure about 44, answers about 39, postings scatter 12, other about 17. Hottest lines: the masked bitmap update `pending[..] |=` (12 per token), the lookup compare (7.8), the lag-ring pop (7.4), the id store (5.0). `bit_sums` is 0.3 M instructions (bitmap results are rare). The micro-diet candidates (window batching instead of the ring, fusing the id store, a cheaper masked update) are each worth about 1-2% real time; together at most 10%, with B's board noise at +-5%.

## B serial prefix after the sample, timed (4 CPUs, ms)
From `sampled` (about 20.6 ms after main starts): building the `packed` keys +1.7, `sort_unstable` +0.9, `order` +0.2, `terms.renumber` +2.35, `anchored` sort +0.55, bitmap allocation +0.4: about 6 ms of single-thread work, about 3.6% of a 167 ms run if the board's single-thread speed matched, and at best half of it is recoverable (a counting sort in place of the sort, renumbering without cloning the 2.4 MB key array). Worth at most about +0.25 pts on B; not attempted.

## B campaign (goal: highest B score), branches and results
All against `origin/speed-demon` 79d68e7, real data, hash `f86973c2...`, output removed before each run, 3 real-image runs per branch.

| Branch | Change | Result | Lines | Verdict |
|---|---|---|---|---|
| `cloud-b-diet` | `kept`/`anchors` grow-only | CPU -1% (instructions -3%, memset-inflated) | +0 | tiny |
| `cloud-b-queryparse` | queries tokenized with the SIMD class masks | parse step 9.19 -> 6.70 ms (-2.5 ms); equal to the old parse on the real queries and on a 60-file fuzz (empty lines, double LF, no final LF, 1- and 32-byte terms, 8-term phrases across windows) | +20 | net +0.06 pts alone |
| **`cloud-b-combo`** | queryparse + grow-only + sample 1/64 | wall -1.2% (4 CPUs), -2.5% (2 CPUs), -1.8% (1 CPU); first round -3.4%; peak memory 54.5 -> 52.5 MiB | +20 | **best B candidate: about +0.3 pts for time and memory against -0.1 for lines; merge as one slot** |
| (not pushed) per-line batch in `tokens()` instead of the lag ring | prefetch every token as it is made, look all up after the line | **slower**: 4 CPUs wall +4.6%, CPU +8%; 1 CPU +9% | -1 | rejected: the continuous ring hides latency better |

Notes from the work:
- A pre-existing sharp edge, not changed: the term table is sized from the query file's byte length (`len / 8` slots). A small query file with many distinct short terms can fill it and `Terms::insert` then loops forever (my first fuzz file hung this way). The real data has a 29% load; the spec's tiny scale has the same ratio. Not a risk for the board.
- The parse change is serial CPU work (9.2 -> 6.7 ms), so unlike the prefetch it should carry over to the board nearly 1:1. Together the branch is worth about 2% of B, below B's +-5% run-to-run board noise.

## B memory timeline and the bitmap cutoff (negative)
Anonymous memory, MiB, 4 CPUs, `cloud-b-combo`: queries parsed 5.9, sample done 9.6, scan done 38.9 (postings buffers in `Units` about 22, bitmaps about 11.8 = 377 terms x 31 KB, per-thread `count`/`last` about 3.3, term tables about 4.4), postings done 39.0 (the `Units` chunks are freed as the 20 MB `post` fills), answers done 41.1. The board's 47-48 MiB is that plus about 6 MiB of kernel and page-table memory.
The cutoff for keeping a term as a bitmap (`df * 32 <= sampled` makes it a list) was tried at 16 and 8, and the opposite direction at 64: cutoff 64 adds about 4-5 MiB; cutoff 16 leaves the peak unchanged (52.4 vs 52.2 MiB median of 7) and costs +9% wall; cutoff 8 raises the peak to 55.3 MiB and costs +13% wall. The existing 1/32 is at the optimum. Rejected.

## Real-time breakdown of B's answers phase (valid: thread 0 alone, TSC, `speed-demon` 4ba1710)
Method note: timing the answers with all 12 threads on one CPU is wrong (each thread's timestamp deltas include the other threads' time); only thread 0 may work while the others return. Total about 355 M cycles, about 16% of B's CPU (the board measured the phase at 25 ms of 167).

| Part | Mcycles | Share |
|---|---|---|
| `apply` (55,769 calls, about 3.3 k cycles each) | 183-191 | 52% |
| `docs()` building lists (first term 30, later terms 52) | 82 | 23% |
| `line()` formatting results | 65 | 18% |
| phrase sets | 5 | 1.5% |

`apply` by arm: list OR list 39.1 (12,595 calls), bitmap AND list 38.9 (10,477), list to bitmap build 30.2 (10,091), list merge 26.6 (14,971), bitmap set from list 20.2 (6,180), binary-search AND/NOT 10.8 (7,755), list NOT bitmap 9.4 (3,029), bitmap-bitmap 4.8 (762). Flat: no arm above 21% of `apply`, and the whole phase is 16% of B, so a 25% speed-up of everything in it is about 4% of B (+0.4 pts). Not attempted.

## Position and the arithmetic of 80 (board, 2026-11-01 ~10:00 UTC)
Microsecond is #8 overall (79.44): A 84.6, B 73.5 (166.8 ms, 47.1 MiB, S 607, K 29.55), C 80.7. Best B entries: 81.05 (74.4 ms, 44.4 MiB, S 960), 80.54 (77.9 ms, 27.2 MiB, S 1039), 78.55 (100.4 ms, 40.6 MiB, S 823), 77.65 (113.7 ms, 36.9 MiB, S 716).
80 points means K = 39.8, i.e. x1.35 over ours. Through time alone that is T x0.55 (167 -> 92 ms); with memory at 40 MiB it is 98 ms, at 30 MiB it is 110 ms (K ~ T^-0.5 M^-0.2 S^-0.15). The two entries at 80.5-81 are 2.2x faster than ours and pay 960-1039 lines; one of them also holds 27 MiB, 20 MiB less than ours.
Where a 45% cut would have to come from, with every measured lever for this design:
- serial prefix (query parse about 9 ms, renumber about 6 ms, spawn): about 12 ms recoverable at most (7%), +0.7 pts, about 20-40 lines
- per-token instructions: `tokens()` 85 + scan closure 44 + answers 39 of 197 instructions per token; the cheap trims are gone (closure `look` already in a register: 0 change; removing the state prefetch: -1.6% instructions but +2.3% to +3.3% time; ring depth, key prefetch, batch lookups: no gain or slower)
- answers phase (25 ms on the board): flat, no arm above 21% of `apply`
- memory: anonymous peak is Units about 22 + bitmaps 11.8 + `count`/`last` 3.3 + tables; the bitmap cutoff sweep gave nothing
Sum of the measured levers: about 10-15% of T, about +1.5 to +2 pts, i.e. B to about 75. Reaching 80 needs a different architecture (the 27 MiB entry stores far less than 2 bytes per posting plus bitmaps), and nothing in the profiles shows a path to it from this one.

## Session addendum (Nov 1): B scan ladder, bucket probe, phrase reject

- 1-CPU scan ladder (ms of ~605): lookup ~38%, scan closure ~34% (phrase matching ~60 ms, bucket adds ~34 ms), token iteration ~15%, classify ~9%.
- Phrase checks: 1,032,600 candidates, 987,267 (96%) fail on a single id compare, 19,427 match.
- SIMD 8-slot bucket probe (tag[8]+id[8], movemask): hash correct after fixes, mispredicts -16% in callgrind branch-sim, but +11% instructions and ~10% slower wall. Abandoned.
- Phrase first-id reject before the slice compare (+3 formatted lines): 1 CPU 746.3 -> 737.6 ms wall (-1.2%), 4 CPU 241.9 -> 242.0 ms (0%), hash f86973c2 unchanged. Below the 0.2-pt bar after the line cost; not pushed. Board timing not measured.

- Straddle-branch removal (edge = lzcnt of !word, window mask cut to the token start, per-token test reduced to `len == 64`; also a BMI2 `_bzhi_u64` variant), on ca9b733: hash f86973c2 unchanged, -1 formatted line, but slower: 4 CPU wall 245.7 -> 260.9 ms (shift/min version) and 239.7 -> 245.8 ms (bzhi version); 1 CPU 777.5 -> 800.9 ms. Not pushed. Board timing not measured (VM has no SMT; mispredict counts not checked).
- Bucket-add (`Units::add`/`put`) is already one store, one compare; no cut found. Closure reads as branch-free already; no restructure candidate with a measured gain.
