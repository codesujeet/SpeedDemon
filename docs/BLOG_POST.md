# 72 hours, four programs, and a scoreboard that kept disagreeing with my laptop

*Speed Demon 2026 write-up, team Microsecond (solo). Built for the Hackathon Raptors Speed Demon contest. Code, logs and every board run ID: https://github.com/codesujeet/SpeedDemon*

Speed Demon is a 72-hour contest with four small batch programs: ingest a log and print hourly percentiles (A), index a text corpus and answer boolean and phrase queries (B), join two CSV files and aggregate (C), and shortest paths on a road-like graph (D). Each is scored on wall time, peak memory and source size by a live board that runs your code in a locked-down container. The reference is a deliberately slow Python program: 32 s for A and 56 s for B on the board's data. My final programs run in 0.0195 s (A) and 0.1124 s (B).

This is not a tour of the final code (the repo has that). It is the list of things I got wrong, because those taught me more.

## 1. The scoring formula decides what is worth doing

Score is `50 * log10(K)` with `K = (Tref/T)^0.5 * min(Mref/M, 50)^0.2 * min(Sref/S, 1)^0.15 * (Dref/D)^0.15`. Three consequences I only appreciated late:

- **1% of time is about 0.11 points, and one source line is about 0.002 to 0.005 points.** A 60-line idea has to save about 2% of the run to break even. I priced every idea this way before building it, and it killed many.
- **Memory is capped.** The memory term stops improving once M is below `Mref / 50`. For B that cap is about 44 MB and we sat at 43.4 MiB, so a rewrite to halve its memory was worth almost nothing. For C the cap is about 7 MB and we were at 8.9 MiB, so each MiB was worth about half a point, but no cheap cut existed (measured below).
- **The log scale flattens everything.** Going from 0.020 s to 0.0195 s on A is a quarter of a point. At the top, the board is decided by noise, which leads to the next section.

## 2. The board is both judge and profiler, and it disagrees with your laptop

Docker Desktop on a Windows laptop has a lot of noise, and the board machine is a different CPU. Repeated readings of identical code on the board varied by about 2% for A, 3.5% for C and 4% for D, and B swung between 0.113 and 0.133 s. Several of my best-looking local wins were not wins on the board:

- **A, streaming hours.** The log is in roughly clock order, so an hour's records can be sorted, formatted and freed while the rest of the log is still parsed. This cut memory by 3 MiB locally with equal wall time. On the board the same change made A *slower*: 0.0228 s against 0.0200 s (`run-70e34892adfd`), a 14% loss that dwarfed the memory gain. I reverted it. Later a differently structured version of the same idea (a single pass with early hour close, a redo path if a line arrives late, hours' text written once all are done) came out at 0.0195 s and 14.4 MiB (`run-c858924f52ea`, 85.73). The idea was right; my first implementation had threads waiting on each other.
- **B, "worth 8% locally".** A round of three small changes (start the worker pool first, a 16-byte two-round hash, a branch-free filter) cut the *minimum* of 40 interleaved local runs by about 8% and the median by nothing. On the board it read 0.1116 s against 0.1124 s: noise. Reverted.
- **B, SIMD intersection.** An AVX2 8x8 block intersection was 9% faster on one CPU and 7% *slower* on 12 CPUs. The permutes land on a port that the hyperthread sibling also uses.

The rule I ended with: a local result is a hypothesis, and only a repeated board reading, with a note of the run IDs, is evidence. Everything in `PERF.md` cites the board run that measured it.

## 3. Two-line (well, three-line) wins that actually paid

- **A, three lines of `mallopt`.** Setting `M_MMAP_THRESHOLD` to 32 KiB and reserving the per-hour vectors exactly made the large buffers separate mappings that are returned to the kernel as threads finish with them. Peak memory fell from 19.3 to 18.2 MiB on the board for three lines of code.
- **B, replace a hash with one AES round.** Hashing query terms and document tokens with a single `aesenc` per 16 bytes (the same instruction for both sides, so a fixed key is fine) gave a cheap, well-mixed hash, which then made four-slot SIMD buckets for the term table practical. Time went from 0.1382 s to 0.1124 s over a series of such steps.
- **B, branch-free `Units::add`.** The last step. A branch on the gap between two values became an arithmetic advance of one or two units, so there is nothing left to mispredict.

## 4. Profiling that was honest about its limits

I used callgrind (instruction counts and simulated branch mispredicts) for ranking changes, and then confirmed on the board. Examples:

- In C, about 23% of the parse loop's cycles went to one `lock bts` (marking a country-name bit in a shared bitset). I found no exact alternative, so it stayed. This is the "profiler reading you cannot act on" case, and it is why the C code is mostly a pile of small cuts: 223 instructions per record, 48 of them text fields, 15 totals, 12 bit marks.
- In A, callgrind said half of the simulated mispredicts were in the median selection. Replacing it with a min/max version that has no branch dropped simulated mispredicts from 2.89 M to 1.69 M at the same instruction count and was visible on the board (0.0199 s).
- Some things *looked* promising and were not: gcc PGO on A saved 3.9% of instructions with no measurable timing change in 12 interleaved runs; a 64-byte bucket layout for B saved 5 instructions per token and was 4 to 7% slower in wall time (more cache lines).

## 5. Using AI tools, honestly

Parts of the code were drafted with Claude Code and with OpenAI's Codex, which the rules allow, and I used several sub-agents to try ideas in parallel. The useful discipline was not letting any of it in without checking:

- A Codex-written one-pass A (the version above) was adopted only after matching hashes on seeds 1, 2, 3 and 7, a full-scale fresh seed against the reference, and the edge suite, and then confirmed on the board.
- A Codex-written B was 20 to 27% faster locally but failed 7 of my edge cases (a hash fold sent periodic tokens to a single value, a long-token hash cancelled itself on a 33-byte run, a 16-bit phase assert and an 18-bit document-count assert). I had it fixed until the whole edge suite passed. On the board it read 0.0979 s and 0.1022 s: after paying for 439 extra source lines, the same points as the version I already had, with a narrower hash. I kept the old one.

The edge suites were the most valuable thing I built. A failed run on the final commit zeroes the track, so each track has a directory of out-of-the-ordinary inputs (for example few lines, tokens at the 16/32/64-byte boundaries, many digits in ids, very many documents) checked against the reference.

## 6. What I would do with another day

- B: delta-coded postings, to cut memory under the cap *and* time; and a full-width term hash with a byte check only on collision candidates.
- C: a rank-based region table (saves about 0.6 MiB, estimated cost 3.5% in instructions, not worth it yet) and the digit-decode-in-one-vector idea from my Codex draft with a fresh profile.
- All tracks: a CI-style script that runs the board's exact `docker run` flags and prints median/min/CPU time over 20 interleaved runs, which I wrote halfway through and should have written first.

If you read one thing: the final PERF.md of each track lists every experiment, including the ones that lost, with the board run behind each number: https://github.com/codesujeet/SpeedDemon

*Thanks to Hackathon Raptors for a well-built contest, and especially for the public board feed that made "measure on the real machine" possible.*
