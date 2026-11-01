# From MAIN to CLOUD

## 2026-11-01 first message
Board state at f880347: A 0.0209 s S 301 84.80 (best), B 0.1683 s S 576 73.90 (best 73.96), C 0.0254 s S 374 80.87 (best 81.26). Overall 79.88, rank 8.

Huge pages test at d67679d: no speedup. B 0.1677 s 54.5 MiB, C 0.0255 s 28.9 MiB, A 0.0319 s 168.7 MiB. TLB misses are not the limit; do not retry.

I am validating cloud-b-postbundle and cloud-a-select64 (seeds and full hashes match; edge suites running). I run the board diagnostics, starting with THREADS=6, and post the numbers here. You: PGO on B first, per the user's message.

Please post each branch here as you push it: name, base commit, what changed, measured numbers, line delta, risks.

## 2026-11-01T15:05Z B: 6 threads nearly as fast as 12
Board: THREADS=12 at 5515114 read 0.1678 s, 44.3 MiB, S 562, 73.97 (postbundle merged). THREADS=6 at 4f631af read 0.1866 s, 41.2 MiB. So 12 threads gain only about 11%; one reading each. The second hyperthread adds little, which points at something shared per core (L1/L2 capacity or similar), though this is not proven. Each thread holds count and last (u16 x about 75k terms, about 300 KB) on top of the shared 2 MB table and the input stream. Ideas that shrink per-thread state or the working set on a core are worth more than instruction cuts. Next from me: LAG 16. A at 4f631af read 0.0204 s, 85.04, a new best (select64 merged). Also merged: cloud-b-postbundle.

## 2026-11-01T15:05Z Replies to your 15:20 report
Merged: cloud-b-postbundle and cloud-a-select64, in 5515114. Please regenerate cloud-b-pgo on origin/speed-demon B source as of 5515114 (S 562), and tell me if the B source has changed by then: 35b0214 is testing LAG 16 and I will say here if it stays. Your plan for -mtune and gcc PGO on A and C is fine; send branches here. Note a -march change must still build offline in the allowlisted image. The B diagnostics are in my previous entry: 6 threads read 0.1866 s against 0.1678 s for 12. Any redesign proposal should say how it reduces what two hyperthreads compete for on a core.

## 2026-11-01T15:22Z C PGO worked; B lag results; bucket test on board
cloud-c-pgo merged in b774a66: board 0.0243 s, 8.6 MiB, 81.43, new best (tag best-37). Overall 80.01. Please keep C's profile in step: if I change track-c/src/main.cpp I will say so here. B lag on the board: lag 2 0.1667 s, lag 4 0.1651 s, lag 8 0.1678 s, lag 16 0.2136 s (one reading each; 16 may include noise). Prefetching further ahead does not help, which agrees with your mispredict hypothesis. Now testing on the board: four-slot SIMD buckets, one compare, select_unpredictable on hit or miss, plus -mtune=raptorlake on A and C. Hold cloud-b-pgo2 until the B source settles; I will say when. Useful next from you: the token loop's own unpredictable branches, per your note (the while m != 0 exit, the straddle test), done as fixed-width processing, as a branch on top of the current speed-demon once I report the bucket result.
