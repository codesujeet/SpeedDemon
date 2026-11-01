# Track A review: new ways to raise the score (base `origin/speed-demon` 8eb3624)

Reviewer: Track A agent. Research only: nothing in the repo was edited, run or pushed, and no timing runs were made.

## How the numbers here were made
- **Instructions.** I built copies in `/tmp/revA/` with Ubuntu **gcc 13.3** and, as a second data point, **gcc 14.2**. Both used the Dockerfile flags plus `-fno-stack-protector -fcf-protection=none -U_FORTIFY_SOURCE`, so the hardening defaults match the plain `gcc:15` image. Only the two file paths were changed.
  - Instructions were counted with `callgrind` on a copy of the real seed-1 log (4 threads).
  - There is no Docker daemon here, so **gcc 15 itself was not built**. gcc 13 and gcc 14 disagree on the register-allocation item (1b), so gcc 15 must be checked before a board slot is spent.
- **Branch mispredicts** come from `callgrind --branch-sim=yes`, which uses a simple simulated predictor. Read them as direction only.
- **Line counts** are clang-format 18 (LLVM style) without comment and blank lines. That gives 311 for the base, and the board says 317, so the board count runs a constant 6 higher. All deltas use my count.
- **Base:** 839.3 M instructions with gcc 13 and 839.2 M with gcc 14 (271 per input line). Parse 65%, `select` 16%, count/place passes about 10%, text about 5%.
- **Points.**
  - 1% of T ≈ 0.11 pt, one line ≈ 0.009 pt, 1% of M ≈ 0.043 pt.
  - Every time number is an **estimate**. I convert an instruction cut into a time cut at **0.3 to 0.8 ×**. The low end reflects the linelist lesson: on the board, +11.6% instructions were time-neutral because they broke a dependency chain. The high end reflects SMT, where instruction throughput is the shared resource.
- **Board noise:** the unchanged pre-linelist code scored 0.02135 to 0.02625 s over 15 runs (median 0.02186). A 3% change needs about 4 runs per arm to show.

## Ranked findings

### 1. Instruction diet: parse and `select` (+5 lines) — est. +0.3 to +0.75 pt
Measured as one bundle on the current code:
- gcc 13: 839.3 → 755.8 M (**−9.9%**)
- gcc 14: 839.2 → 777.5 M (**−7.3%**)
- 311 → 316 lines
- simulated mispredicts 3.23 → 3.18 M
- hash correct

Each part, measured alone on the base:

| Part | gcc 13 | gcc 14 | Lines |
|---|---|---|---|
| 1a block scan of the line list | −28.5 M | −33.1 M | 0 |
| 1b keep calls out of the intern loop | −28.9 M | **−11.4 M** | +5 |
| 1c hour index in one expression | −11.4 M | −11.8 M | −2 |
| 1d 64-bit partition index in `select` | −10.9 M | −10.5 M | 0 |
| 1e maximum without partitioning | −8.2 M | −8.2 M | +2 |

**1a. The block scan (lines 178-183).**
- Line 180, `if (end - scan < 64) m &= …`, runs for every one of the 3.73 M 64-byte blocks (about 6 instructions each), though it only matters at a piece's end.
- Delete it. Drop listed ends at or past `end` once per 4 KB batch instead: `while (n && ends[n - 1] >= end) --n;` before the line loop.
- Store the first and last LF of a block and advance by popcount, replacing the two `n += m != 0, m &= m - 1` lines:
  `ends[n] = scan + _tzcnt_u64(m), ends[n + 1] = scan + 63 - _lzcnt_u64(m), n += _mm_popcnt_u64(m);`
- **Why the output is the same.** Lines are at least 40 bytes (grammar: 27 + 1 + 3 + 1 + 1 + 1 + 3 + 1 + 1 + LF). So any 64-byte window holds at most 2 LFs, even one that reaches past `end`.
  - Ends come out in increasing order, so the ones past `end` are the last listed, and the filter removes exactly those.
  - With one LF in a block, first equals last and slot n+1 is overwritten. With none, both slots are garbage and n does not move.
  - `ends` already has 8 spare entries.

**1b. Keep calls out of the hot intern loop (lines 116-141).**
- **What the base binary does.** Every line spills four or five registers before the probe and reloads them after it. It also rebuilds the `' '` and `'0'` broadcast constants on every line, because the loop body contains two calls: the new-name `memcpy`, and `memcmp` (`x == s`) for names over 64 bytes (6% of lines, 5.5 M instructions in memcmp).
- **Fix part 1.** Move the insert into `[[gnu::noinline, gnu::cold]] u64 insert(std::atomic<u64>& slot, std::string_view s)`. It returns 0 if this thread won the CAS, else the winner's slot value. Call it as `if (v == 0 && (v = insert(slot[i % SLOTS], s)) == 0) return i % SLOTS;`.
- **Fix part 2.** `same()` compares 64 bytes at a time:
  `if (x.size() != s.size()) return false; u64 diff = ~(eq(0) | u64(eq(32)) << 32); if (s.size() <= 64) return (diff & mask) == 0; return diff == 0 && same(x.substr(64), s.substr(64));`
- **Why the output is the same.** Endpoints are at most 125 bytes (grammar `"/" *124echar`), so at most two steps run.
  - Both 64-byte reads stay in bounds. The log side is followed by the rest of the line and the spare page. The arena side ends at or before `offset + 128 ≤ offset + len + 63 < ARENA + 64` because len > 64.
  - The CAS semantics are unchanged.
- **Compiler caveat.**
  - With gcc 13 only the two halves together pay: alone each gives about −2.5 M, and `[[unlikely]]` alone gives −5.6 M.
  - **gcc 14 gives only −11.4 M** (−1.4%). This part is register-allocation dependent and is the most likely to shrink on gcc 15.
  - Its 5 lines cost 0.045 pt. On gcc 14 it is still about +0.05 to +0.1; if gcc 15 looks like gcc 14 or worse, drop it.

**1c. The hour index (lines 190-191).** `int h = (p[6] + p[9]) * 24 + p[11] * 10 + p[12] - 2850;`. It also removes the `p[6] == '0'` branch.
- **Why the output is the same.** The span is fixed for every seed (spec §2), and the current code already relies on that.
  - The month's last digit plus the day's last digit is 0, 1, 2, 3 for Oct 30, Oct 31, Nov 01, Nov 02.
  - Check: `2026-10-30T18` gives 24·96 + 490 + 56 − 2850 = 0. `2026-11-02T17` gives 24·99 + 490 + 55 − 2850 = 71.

**1d. 64-bit partition index.**
- `u64 pivot = a[n - 1], lt = 0;` (line 85). The inner loop loses a `mov` + `lea` zero-extension: 10 → 8 instructions per element.
- Mispredicts unchanged (3.144 vs 3.149 M). The output is identical: same comparisons, same swaps.

**1e. The maximum without partitioning.**
- `if (k + 1 == n) return std::swap(*std::max_element(a, a + n), a[n - 1]);` at the top of the round loop.
- r99 = n−1 for every n ≤ 100 (97% of groups, 42% of records), and r95 = n−1 for n ≤ 20. Those selections become one scan.
- Simulated mispredicts −5% (3.23 → 3.05 M).
- **Why the output is the same.** a[k] ends up holding the k-th smallest and nothing follows it, so the contract of `select` holds.

**Estimate.** −7.3% to −9.9% instructions → T −2.2% to −7.9% → +0.24 to +0.87 pt, less 5 lines (−0.045). Net **+0.3 to +0.75**. It needs 3 or more board runs.

### 2. One stretch of chunks per hour, released the moment the hour is copied out (−21 lines) — est. +0.25 to +0.4 pt
- **Change.** Replace the per-thread chunk regions and the front-run `release()` with one `MAP_NORESERVE` mapping holding a 16 KB-chunk stretch per hour, shared by all threads through an atomic bump counter `carved[h]`.
  - The Chunk header is just `n`, since a fresh chunk's count is a zero page. `CAP = 4095`.
  - Parse: `if (!c || c->n == Chunk::CAP) c = chunks[h] + carved[h]++;`
  - `each(h, f)` walks the hour's chunks contiguously, then every parser's `wide[h]`.
  - After the place pass, one `madvise(chunks[h], carved[h] * sizeof(Chunk), MADV_DONTNEED)`.
- **What it deletes:** `Chunk::prev` and `hour`, `Parser::next/kept/map/release/each`, `finished`, `releasing`, the release block in the hours loop, and the release line in the `sized` barrier.
- **Measured.**
  - Alone: 311 → 290 lines (−21). Instructions −1.6% with gcc 13 and −1.0% with gcc 14.
  - On top of item 1: about 0 to −0.5% instructions, still −21 lines.
- **Why the output is the same.**
  - Each chunk is filled only by the thread that carved it, and both passes read the hour's records in the same order.
  - Room per hour is `size/40/CAP + 1 + nt` chunks: every line in one hour, plus one part-full chunk per thread. That holds for any input, since no disorder bound is assumed (spec §2).
  - Released chunks are never read again.
  - An adversarial log was run through both final builds at 1 and 12 threads and matched the oracle: 300,000 lines all in hour 18:00, 80% of them the minimum 40-byte line, with 2^19+ latencies mixed in. That exercises the room bound, many chunks per hour, and two LFs in nearly every 64-byte block (item 1a).
- **Memory (estimate, from a schedule model of the hours phase).** The real data's hour sizes are 14,484 to 73,244 lines (diurnal), and the place pass is about 40% of an hour's work.
  - The peak is early in the hours phase: all records, plus 12 latency scratches, plus lines.
  - With the front-run release, the 69.5 k-line hour 0 blocks every chunk behind it during round 1, and the model gives a peak 2.4 MiB above the records.
  - Releasing per hour gives 1.6 MiB.
  - Against that, the 72 stretches touch about 84 page-table pages instead of about 18: +0.26 MiB, which is charged and not freed by `MADV_DONTNEED`.
  - Net about **−0.5 MiB** (−2.7% M) → +0.11 pt.
  - This agrees with the old note that larger release batches "left about 1 MB more resident at the peak".
  - Also, 72 `madvise` calls replace the batched front runs, so there are fewer TLB shootdowns.
- **Risks.**
  - 1.7 GB of virtual space for the 239 MB log. This is fine with `MAP_NORESERVE` under overcommit modes 0 and 1. Add a `MAP_FAILED` check (the current code has none either).
  - **Measure memory.peak locally** (cgroup, `/out` on tmpfs) before the board. If page tables cost more than estimated, round `room` up to a 2 MB multiple.
- **Estimate.** Lines +0.19 (certain), memory +0.05 to +0.15, time 0 to +0.05. Net **+0.25 to +0.4**.

### 3. Decision to make: keep or drop the line list (`09cbdd1`) once items 1 and 2 are in — est. −0.2 to +0.2 pt, needs an A/B
- **Board record.** The line list scored 0.02184 and 0.02193 s (2 runs), against a 15-run median of 0.02186 s before it. So its +11.6% instructions (gcc 13: 752 → 839 M) were time-neutral, and it costs 11 lines.
- **With items 1, 2 and 5 on both bases:**
  - arm L (keep the list): gcc 13 754.8 M / gcc 14 775.5 M, 297 lines;
  - arm P (drop it): 696.7 M / 720.4 M, 286 lines.
  - The list now costs +7.6 to +8.3% instructions instead of +11.6%.
- **Arithmetic.** If its chain-breaking was worth about 11.6% of instructions on the board, arm L is now about 3% instruction-equivalent ahead (T 1-2.5%, +0.1 to +0.27 pt). Arm P is 11 lines shorter (+0.10).
- **Verdict:** a wash on current evidence.
- **Recommendation.** Ship arm L (the merged code path). Then spend one or two board runs on arm P, because the "neutral" reading rests on 2 runs.

## Board diagnostic plan (suggested, as C did)
- **Why.** Nobody knows how A's 21.8 ms divide. Three wrong-output diagnostic runs, reverted afterwards, would locate it.
- **The three runs:**
  - (a) `_exit(0)` right after the `ranked` barrier: setup plus parse.
  - (b) `select` replaced by nothing: what the selection costs on SMT.
  - (c) the text loop skipped, with the file still sized: text plus output page faults.
- **What it decides.** If (a) is close to 21 ms, only parse instructions matter (items 1 and 3).

## If the bundle disappoints on the board
- **M up:** look at item 2 (page tables, and when the scratch is touched against when records are released).
- **T up or flat:** look at 1b first (gcc 15 register allocation), then 1e (max_element's branch on real hardware).
- Bisect with callgrind on the gcc 15 image before spending board slots.

## Measured and rejected or under the +0.2 bar (new; do not re-propose)
| Idea | Result |
|---|---|
| Startup: each claimer finds its own piece boundaries (`cut` lambda), and main runs `work(0)` instead of idling in `join` | +2 lines, 0 instructions. Removes 95 serial file faults in `main` (est. 0.1-0.3 ms) and one idle thread; est. +0.03 to +0.12. **It is in both arm files: strip the `cut` lambda / `work(0)` hunk if you want only items 1-2.** |
| `#pragma GCC unroll 2/4` on the partition loop | −1.7% / −2.1% instructions, but simulated mispredicts +5% / +7% (the remainder loop on small groups) |
| Digits written backwards with the known width instead of `to_chars` | +1.7% instructions |
| Counting by slot id (no `rank[]` load), ranking as a claimed task, order mapped at text time | +0.25% instructions, +4 lines; the serial ranking is only about 1-2 M instructions |
| `[[unlikely]]` on the insert branch only | −0.7% |
| LPT order for the hours (largest first) | model: hours-phase idle 9.4% → 0.3% (in-order ends on the biggest hours, 63-73 k lines), but peak +1.7 MiB from round-1 scratch. Net negative. |
| "In order, last 12 hours largest-first" (with item 2) | model: hours tail −7%, memory unchanged; est +0.05 pt for +3 lines |
| Splitting the latency scratch by endpoint range | an extra record pass (+3-4% instructions) for about −1 MiB: a wash |

## Verification done
- **Arm L** (`main_armL.cpp`; the same code as `/tmp/revA/v/c8.cpp`):
  - real-data hash at 12, 7 and 1 threads;
  - the spec's worked example, byte for byte;
  - an empty file (0-byte output) and a one-line file;
  - **140** fuzz logs (1-5,000 lines) against an independent Python oracle at 1, 3 and 12 threads;
  - the adversarial single-hour log at 1 and 12 threads.
- **Arm P** (`main_armP.cpp`; the same code as `pp6.cpp`):
  - real-data hash at 12, 7 and 1 threads;
  - the worked example and the empty file;
  - **120** fuzz logs at 1, 3 and 12 threads;
  - the adversarial log at 1 and 12 threads.
- **What the fuzz covers:** hour, day and month boundaries, the first and last instants, 1- to 125-byte endpoints including 64/65-byte names and prefix families, latencies 0, 2^19-1, 2^19 and 4,294,967,295, statuses 100 to 599, and duplicate lines.
- **Single items alone:** every one gave the real-data hash.
- All runs were under `valgrind --tool=none` with the thread count forced, on inputs in `/tmp/revA/`.

## Files
- `/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/plan/A_files/main_armL.cpp` and `armL.patch`: items 1, 2 and the startup rider on the current code. gcc 13 754.8 M (−10.1%), gcc 14 775.5 M (−7.6%), S 297 (board ≈ 303).
- `/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/plan/A_files/main_armP.cpp` and `armP.patch`: the same without the line list. 696.7 M / 720.4 M, S 286 (board ≈ 292).
- Oracle: `/tmp/revA/ref.py`. Generator: `/tmp/revA/gen.py`. Callgrind outputs: `/tmp/revA/v/`.

**Expected total for arm L: est. +0.5 to +1.15 pt.** That is time +0.25 to +0.85, lines +0.13 and memory +0.05 to +0.15. Verify it with:
1. callgrind on the gcc 15 image;
2. a local memory.peak check;
3. 3 or more board runs against the 0.02186 s median.
