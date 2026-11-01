# RESEARCH_A: cloud research on Track A (C++, base `3cbb939`)

Real file (`7ec80ba4...`), output hash `495ca096...81cd3` on every variant. 4-vCPU cloud VM; ranking only.

## Done
Input prefetch (`p + 4096` per line): CPU -26% on 1 CPU, 4-CPU wall 48.5 -> 39.6 ms on the pre-merge source. Already merged as `797bf9a` / `3cbb939`.

## Negative or too small to keep
| Change | Result | Lines |
|---|---|---|
| gcc 15 `-O2` / `-Os` vs `-O3` (built in the pinned `gcc:15` image) | 4 CPUs 38.5 -> 39.2 / 40.9 ms; 1 CPU 102 -> 102.8 / 107.4 | 0: `-O3` stays |
| hour from previous line's 13 timestamp bytes | CPU -1.2% | +7 |
| insertion sort for groups of 12 or fewer | CPU -1.4% | +8 |
| both | CPU -2.4% | +15 |
| drop the per-piece `MADV_DONTNEED` | no change | -1 |
| `pwrite` output instead of the mapped file | CPU -5%, wall worse (37.2 vs 34.6) | ~0 |
| replace `select()` by `nth_element` | CPU +13% | -25 |
| drop chunk `release()` | peak memory +11.8 MiB | -4 |
| 8-byte records (no `wide` list) | peak memory +10.6 MiB, CPU +5% | -7 |

## Instruction profile (callgrind, full data, 751 M Ir = 242 per line)
Scan about 60% (endpoint intern lookup about 19% of the total, hour parse 5.5%), hours + text about 38% (`select` partition loop 15%, count and place passes 10%). Each micro-change above is worth 1-2%, so the leaders' lead is unlikely to be instruction tuning.

## Points arithmetic (board 22 ms, 19 MB, 326 lines)
1 ms = 0.49 pts, 1 MiB = 0.24 pts, 10 lines = 0.1 pts. Leader Cause Of Death 16 ms / 17 MB / 346.

## Update (base `fff546f`)
| Idea | Result | Verdict |
|---|---|---|
| `-O2` in the Dockerfile (branch `cloud-a-o2`) | 4 CPUs wall 39.4 -> 41.6 ms, 1 CPU 101.9 -> 102.6 ms | slower |
| one `#include <bits/stdc++.h>` for 12 std includes (branch `cloud-a-includes`) | speed and memory identical (21 and 11 runs), 321 -> 310 formatted lines | pure line cut, about +0.11 pts |
| scalar `decode` loop (-6 lines) with gcc 15 | CPU 117.8 -> 128.6 ms (+9%) | rejected (the gcc 13 test had said neutral) |

## Methodology correction (important for reading every local number in these notes)
My benchmark harness reused `/out`, so each run's `open(O_TRUNC)` of the previous 10 MB `summary.txt` cost 2-10 ms (0.05 ms when the file is absent, as on the board, where `/out` starts empty). That inflated noise and fixed overhead in every local run; the A/B direction of earlier comparisons is unaffected because both arms paid it, but absolute wall times and the apparent "serial setup" (4-8 ms) were artifacts. Fixed harness (delete the output before each run), A prefetch re-measured on the current source: 4 CPUs wall 38.9 -> 30.9 ms (-21%), CPU 141.5 -> 110.6 ms (-22%); 1 CPU 127.3 -> 96.9 ms. The board saw only about -3% wall for it. This VM's page-walk and memory stalls are far worse than the scoring box's, so memory-behaviour tricks (prefetch, populate, release) transfer poorly; only ideas that cut instructions or real work are likely to carry over.

## Other measurements this round
- A's serial pieces on this VM are small: ranking callback 0.28 ms, sizing callback 0.02 ms, thread spawn 0.05 ms per thread. After the prefetch, 4-CPU wall (30.9 ms) is within about 3 ms of CPU/4 (27.6 ms), so A scales almost perfectly here; the remaining lever is instruction count.
- Radix selection for the percentiles would help only the few huge groups (about 3 groups per hour hold about 25% of the records) and loses on the 75% in small groups. Not built.

## Edge-case audit of the A changes (branches `cloud-a-includes`, `cloud-a-lines`)
All checked with the real gcc 15 build; none of this needed the board.
- Spec worked example (18 lines: hours out of order, duplicate lines, a 4,294,967,295 latency, an hour, a day and a month rollover): output equals the spec's expected `summary.txt` byte for byte, for the base and for the line-cut build.
- Empty `requests.log`: empty output (0 bytes), as the spec says. One line at the last instant of the span. The 125-byte endpoint next to `/`.
- Differential fuzz against an independent Python reference (150 random logs, 1-6,000 lines each): the first and last instants of the span, hour boundaries, endpoint-prefix families (`/v2/orders...`, `/v2/rank100` vs `/v2/rank20`), 1- and 125-byte endpoints, latencies 0 / 2^19-1 / above 2^19 / 4,294,967,295, statuses 100-599, duplicated lines: base 150/150, line-cut build 150/150.
- Thread count forced to 1, 2, 3, 5, 7 and 12 (12 threads on 4 CPUs): the real-data hash `495ca096...` every time. The board runs on 12 hardware threads; `hardware_concurrency()` is clamped to 12 so a larger or smaller cpuset also works.
- Hardware assumptions that remain: AVX2 + BMI2 + PCLMUL (the board's i5-13500 has them; there is no AVX-512 in the code), 4 KB pages for the `madvise` alignment, `prefetch` past the end of a mapping is harmless (it never faults), `MADV_DONTNEED` on clean private file pages cannot change data. A 10 MB output on a 1 GB tmpfs `/out` fits with a wide margin.

## Track A task (base `ba0e248`): parse diet, memory outside the records, lines, select
All with the pinned gcc 15 image, real data, hash `495ca096...`, output removed before each run.

**Per-line instruction budget (callgrind, 734.9 M, 237 per input line).** `Parser::run` 184 M self (59 per line): hour decode + hour index (lines 177-178) 19.4 per line, endpoint intern about 47 per line including `same()`, window compares about 14, chunk bookkeeping 11.6. `select` 128.4 M (17.5%), the `each()` count/place passes about 76 M (10%).

**Idea 1, parse diet (branch `cloud-a-parsediet`, pushed for the record).** Reusing the hour while the 13 timestamp bytes repeat: instructions 734.89 M -> 691.76 M (-5.9%) but real time barely moves (4 CPUs wall 30.3 -> 30.2 ms; 1 CPU 90.3 -> 89.3 ms), because the removed instructions were off the critical path. +8 formatted lines (300 -> 308): not worth a slot. Companion change, skipping the second 32-byte name compare for names of 32 bytes or fewer: **9% slower** (1 CPU wall 99.5 vs 91.2 ms) because its `size <= 32` branch mispredicts; it also did not reduce instructions (692.36 M for both vs 691.76 M for the hour change alone). Lesson: for A, removed instructions only matter if they are on the dependency chain; branchless code that executes more instructions can be faster.

**Idea 2, memory outside the records.** Resident anonymous memory at the parse barrier is 12.92 MiB, of which the records are 3.1 M x 4 B = 11.83 MiB, so everything else is about 1.1 MiB (about 0.7 MiB of it the `wide` list, 93 k x 8 B). Page tables 0.23 MiB; resident file pages 1.85 MiB. At most about 0.3 MiB is recoverable outside the records (about 0.07 pts). The `<iostream>` static initializer that `<bits/stdc++.h>` brings in costs 0.45 MiB of resident file pages (1.85 -> 1.40 MiB, recovered by `#define _GLIBCXX_IOSTREAM 1` before the include, or by explicit includes), but cgroup `memory.peak` did not change (15.64 -> 15.60 MiB median of 11) because executable pages are page cache charged to whoever read them first, not to the run. Not pushed. (The same holds for C; B uses no C++ headers.)

**Idea 3, line cuts.** A is at 300 formatted lines; what is left is 1-2 lines per site (a lambda for the three median-of-3 swaps, the width table), worth about +0.05 pts in total. Not attempted.

**The time that is left: `select`.** Replacing `select` by a no-op (wrong output, same shape) cuts CPU 91.6 -> 76.5 ms (-16.5%) and 4-CPU wall 29.6 -> 25.5 ms (-14%): it is the single biggest time item. Analysis of what could beat it: groups of 256 or more hold about 47% of the records, but the cost per record is highest in the small groups (the three selects each pay per-round overhead; mean group size about 24), where insertion sort was measured as noise (the brief). A sample-pivot quickselect for the big groups would cut roughly 2.2n of 4.7n comparisons there, about 10% of `select` or 1.6% of A's CPU, for 15-25 lines: not worth it.

## Tooling found this round (cloud sandbox)
- `apt` works here, so `linux-tools-generic` (perf 6.8), `hyperfine`, `strace`, `llvm-mca` (LLVM 18) and `valgrind` (callgrind, cachegrind) are installed. The VM exposes **no hardware PMU** (`/sys/bus/event_source/devices` has only software, tracepoint, uprobe, msr, breakpoint), so `perf stat` cycles, instructions, branch-misses and cache-misses are "not supported"; `perf record -e cpu-clock` (time sampling) and `perf annotate` do work and give a *time* profile, which callgrind (instruction counts) cannot.
- Method trap: mount a tmpfs on `/out` (`mount -t tmpfs -o size=1g tmpfs /out`) before profiling or timing. On this VM's ext4 `/out`, the mapped output file's page faults go through `ext4_readahead` and a kernel `memset` and showed up as 24% of A's time; the board's `/out` is a tmpfs.
- A, time profile (1 CPU, tmpfs `/out`): `Parser::run` 49.5%, hours phase 13.9%, `select` 12.8%, kernel page faults about 10%. Inside `Parser::run` the endpoint intern is about half: `same()` (line 140 and its two `vpcmpeqb` loads) about a third of the function, the slot load and probe about 15%, `next_byte` about 7%.
- Tried from that: a 64-bit whole-name fingerprint in the slot (no arena read, no vector compare): code -4 lines (300 -> 296) and hash correct, but **22% slower** (1 CPU 97.4 -> 118.8 ms, instructions +18%): hashing every word serially costs more than the stall it removes. Rejected.
- B, time profile (1 CPU): `tokens()` 63% of the time (43% of the instructions). Three instructions hold about 66% of it: the second half of the window load (25%), the key compare `vptest` in `Terms::find` (24%, waiting on a key from the 2.4 MB key array) and the token-bytes load for the key (16%). A third pipeline stage that prefetches the key line (ring 8 -> 16, prefetch of `keys[slot - 1]` at distance 8): hash correct, but **no gain** (4 CPUs wall 249.8 -> 252.4 ms, 1 CPU 777.4 -> 780.5 ms). The stall attribution is skid onto the consumer, not necessarily a miss on the key line.
- C, time profile: 86% in `parse_records`, kernel about 6%.

## Track A, round 3 (base `e1bd0df`): the parse is latency-bound on the line-to-line chain
Tooling: perf time sampling + annotate on a tmpfs `/out` (see above), `hyperfine`, `llvm-mca` installed; no PMU in the VM.

**Merge candidate: `cloud-a-linelist`.** `Parser::run` lists the LF positions of the next 4 KB first (branchless, up to two per 64-byte block, a line being at least 40 bytes) and then parses each line from its known start; the old loop took the next line's start from the previous line's own newline mask. 1 CPU wall 92.4 -> 87.2 ms (-5.6%), 4 CPUs 27.8 -> 26.6 ms (-4.3%), with 9.5% MORE instructions (734.9 M -> 804.9 M): the parse is limited by the dependency chain, not by work. Peak memory unchanged (15.5 MiB), formatted lines 300 -> 311 (+11). Hash on 3 real-image runs, the spec's worked example, empty/one-line/125-byte-endpoint files, a 70-file oracle fuzz (70/70) and thread counts 1-12. Risk checked: the `ends` array holds `4096/40 + 8` entries and a block cannot hold three ends because lines are at least 40 bytes.
- + hour reuse on top (not pushed): another -1.2% (1 CPU) / -2.3% (4 CPUs) for +8 lines (319 total); marginal.
- The earlier hour-reuse alone, on the old loop, did nothing (latency-bound); it only pays once the chain is broken.

**Rejected, with numbers (1 CPU wall against the same base, interleaved):**
- Finding the end of line from the end of the name with one 32-byte compare (so the big newline mask goes, and so does the `next_byte` fallback that 17.4% of real lines take): hash correct and one line shorter, but **+12% slower** (92.5 -> 103.5 ms): it puts the LF search behind the name-end computation, on the loop-carried chain. This is the experiment that showed the chain is what limits the parse.
- Three-pass software pipeline inside a batch (names and hashes with slot prefetch, then slot read and arena prefetch, then the lines): **+22% slower** (87.7 -> 106.7 ms).
- Arena entries aligned to 64 bytes so the `same()` loads never split a cache line: no effect (92.5 -> 92.0 ms; with the line list 87.9 -> 86.9 ms, noise).
- A prefetch once per 64-byte block of the batch instead of once per line: gives back the whole line-list gain (92.2 ms), so the per-line prefetch stays.
- Whole-name 64-bit fingerprint in the slots: +22% slower (see above). Key-line prefetch in B: no gain.

**Left (not built):** the count and place loops of the hours phase are read-modify-writes to per-endpoint counters (`start[e+1]++`, `latency[cursor[e]++]`), so the hot endpoints that repeat back-to-back serialize through store-to-load forwarding; counters split by record parity would break the chain, an estimated 2% of A for +8 lines. `select` is still 14% of A's time.

## Why VM timings favour the wrong changes: the sandbox has no SMT (measured)
The board runs 12 threads on 6 P-cores, so two threads share each core's execution ports. This VM's four vCPUs do not: a loop that saturates the ports (eight independent operation chains) takes 0.82 s alone and 0.83-0.88 s with a second copy on any other vCPU (all six pairs), so each vCPU is its own physical core. A latency-bound loop would not show sharing either, so only the throughput-bound loop is conclusive.
Consequences for reading this repo's local results:
- A change that only breaks a dependency chain (the line list in `cloud-a-linelist`, C's two-cursor interleave) wins on this VM, where each thread has a core to itself, but on the board the sibling thread already fills the stalls (the main session measured exactly that for C's two-cursor build: no gain). Chain-breaking that adds instructions can lose there.
- A change that removes instructions (the hour reuse in `cloud-a-parsediet`: callgrind -5.9%, VM time about -1%) helps more on the board than on this VM, because a core's instruction throughput is the shared resource: C's real-Linux counters show IPC 1.36 per thread, about 2.7 per core.
- So rank candidate changes by callgrind instructions first and by VM time second, and test chain-breaking changes on the real SMT machine with 12 threads (and 6) before spending a board slot.
- Specific recommendation: run `cloud-a-linelist` (+9.5% instructions, -5.6% VM wall) against the base and against `cloud-a-parsediet` (-5.9% instructions) on the real machine at 12 threads; if the linelist does not win there it should be dropped.
