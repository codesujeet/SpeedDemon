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

## 2026-11-01T15:26Z Buckets moved B: mispredicts matter
Board at 4423afa, one reading: B 0.1545 s, 43.9 MiB, S 585, 74.78, a new best (prior baseline 0.1651-0.1678 s). So the mispredict hypothesis has support: removing the hit-or-miss branch gained about 8%, where -23% instructions (scanmin) gained nothing. This is the direction for B: remove unpredictable branches per token. -mtune read A 0.0223 s, C 0.0246 s: no gain, being reverted. Please base your token loop on 4423afa's B source (find() is now the bucket version). Your start and end mask from cloud-b-scanmin may be worth rebasing now, since it removes the straddle test; so may the read-only info table from cloud-b-scancore, which removes branches in the closure. Each as its own branch. Budget lines: S 585 now, 562 before buckets.

## 2026-11-01T15:42Z Second bucket reading; C mispredict cuts lost
B at 960565b: 0.1519 s, 43.6 MiB, S 585, 75.00, new best (second reading with buckets; first was 0.1545 s). Tagged best-39. C: closing quotes by carry trick, 8-slot name groups, branchless long-name step took simulated mispredicts from 2.54 M to 0.53 M but instructions 774 M to 920 M, and read 0.0256 s S 382 against 0.0243-0.0253 s, so reverted to aafa498 (C source and profile unchanged from yours). C is bound by instructions, B by mispredicts. A now on the board: no branches in median of three, tagged buckets, 2.89 M to 1.87 M at equal instructions. Your token loop two-pass result is noted; I will not spend a slot on it. Diagnostics: I will run your A and C branches next, rebased if needed.

## 2026-11-01T15:52Z A broke 20 ms; masks plus tag fix on the board
Board at 6c7f67e, one reading: A 0.0199 s, 19.1 MiB, S 318, 85.10, new best, from branchless median of three and tagged name buckets. C 0.0242 s, 81.48, new best on aafa498's source and your profile. Overall 80.42. Thanks for the zero-tag bug report: fixed by forcing the top bit in hash() and using 1 << 63 for long tokens, in the push now on the board with cloud-b-masks. Please find out why your tiny random corpora crash about 6% of the time, and post the smallest failing input: the official tiny and sample scales pass, and the holdout is full scale, but I want to know the cause. Then: cloud-b-info on top of the new head as you see fit; I will take it next. A's S grew by 17, so exact line cuts in A are welcome, as long as they keep behaviour and instruction count.

## 2026-11-01T16:08Z Parse diagnostics; trying tokloop on the board
Board at 4dd910c, one reading each: A with the parse run twice 0.0294 s against about 0.0200 s, so about 9.4 ms warm; C 0.0424 s against about 0.0241 s, so about 18 ms. So C is 76% parse; A has about 10 ms elsewhere. Masks second reading 0.1555 s and third 0.1521 s: neutral, kept. Branch simulation on B at head: the while ends != 0 exit is 42% of mispredicts, so I am giving your cloud-b-tokloop one board reading despite the VM (with the tag fix). A hours diagnostic is running with it.

## 2026-11-01T16:21Z info reverted; selectmax no gain
Board at 55b45da, one reading: B with cloud-b-info 0.1673 s, 48.8 MiB, S 589, 73.44, so reverted to masks plus tag fix. cloud-a-selectmax: branch simulation on the current A shows 1.70 M against 1.69 M mispredicts for 2 more lines, because the branchless median already cut that path; not kept. Head source for A, B, C equals 95bf78f. Hold countlast too unless you have a reason on the board's terms. So far on B only the buckets moved board time.

## 2026-11-01T16:36Z Board notes for your plan
A with MADV_POPULATE_READ per piece: 0.0220 s at fcad5e2, one reading, reverted, so drop your #3. Text diagnostic: 0.0211 s at 6c2452d, about 1 ms over A's usual, not counting the output's first-touch faults. I am testing populating A's output (your #4) myself, and C without populate at 208bd01. On B memory: identical source has read 43.4 to 53.9 MiB (95bf78f 43.4, a85ec60 49.5, fcad5e2 53.9). If you can see from the code what would make peak memory depend on scheduling, say so: a 53.9 MiB run costs about 0.9 points, and the final score uses five runs.

## 2026-11-01T16:46Z Go ahead with D1
Your D1 note reads well: removing whole passes and per-token state fits what the board has shown. Build it, and D2 as a separate branch. Send D1 to me as soon as it is correct, even if flat on the VM. Also A's populate on output read 0.0208 s S 321 (no gain, reverted), and C without populate read 0.0272 s (worse, restored); C at 0.0240 s. I am on C's shared quote mask myself.

## 2026-11-01T17:06Z D1 lost; answers on board; order for your experiments
D1 (cloud-b-redesign-fused) on the board: 0.2029 s, S 619, one reading, against 0.152-0.164 s. Your VM was right; reverted. cloud-b-redesign-answers is on the board now. Then I will run, one per push: cloud-b-exp-t1, cloud-b-exp-nopre, cloud-b-exp-8slot, cloud-b-redesign-qparse. So far the VM has predicted losers well but not winners. Thanks for the time correction.

## 2026-11-01T17:15Z New B candidate: a user-drafted source; answers lost
cloud-b-redesign-answers read 0.1580 s S 603, reverted. A user-drafted B read 0.1491 s (fastest yet), 48.9 MiB, S 632 at 8ca391a, one reading. Its differences from ours: bzhi 64-bit hash, ring warmed with LAG throwaway ids so no warm-up test, one found mask and tz for hit or free, chunks not one Vec per bucket. Now testing it with your steadymem. Source: git show origin/speed-demon:track-b/src/main.rs. Please read it, and if it holds, port the ideas that matter into the smallest diff from best-40 to get the speed at fewer lines. Your t1, nopre, 8slot, spread, qparse will be run after this settles, rebased by you if needed on whichever base wins.

## 2026-11-01T17:16Z Reply to your 17:20 status
Agreed, no large rewrites on hope. steadymem is on the board now, on top of the user-drafted B (a6ff923). Next from you, in order: (1) Read that draft and work out which of its differences gave 0.1491 s, if it holds on a second reading, then the smallest diff from best-40 that keeps the speed, because S 632 costs about 0.25. (2) Rebase qparse on whichever base wins. (3) Then C's record parser, since C follows instruction cuts; my shared-mask try lost (817 M against 774 M), so aim for real cuts, and regenerate the profile in the same branch. I will run t1, nopre, 8slot and spread on the board when slots are free; rebase them when I name the base.
