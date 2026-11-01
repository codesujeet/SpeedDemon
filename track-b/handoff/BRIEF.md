# Shared brief for the Speed Demon 2026 review agents (team "Microsecond")

## The contest
- Repo: /home/user/SpeedDemon. The live code is branch `origin/speed-demon` (head 8eb3624). Read it with
  `git -C /home/user/SpeedDemon show origin/speed-demon:track-X/src/main.*`. The working tree may hold other branches; do not edit it.
- Tracks: A log-ingest (C++, track-a/src/main.cpp), B inverted index (Rust, track-b/src/main.rs), C CSV join (C++, track-c/src/main.cpp). D does not count.
- Specs: /tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/spec/{a-ingest.md,b-index.md,c-join.md,CONVENTIONS.md,scoring.py}. Read the spec for your track fully before proposing anything; output must stay byte-identical.
- Score per track: K = (Tref/T)^0.5 · min(Mref/M,50)^0.2 · min(Sref/S,1)^0.15 · (Dref/D)^0.15, points = 50·log10 K.
  T = wall time (middle 3 of 5 runs), M = cgroup memory.peak, S = source lines after clang-format (LLVM) / rustfmt, D = dependencies.
  Rules of thumb: 1% faster ≈ +0.11 pt; each extra formatted line ≈ −0.009 pt; 1% less peak memory ≈ +0.04 pt.
  Overall = geometric mean of the best 3 tracks.
- Board (scoring box): Intel i5-13500, 6 P-cores with SMT, the program runs 12 threads, 8 GB memory limit, no network, read-only root, /tmp and /out mounts.
  Instruction throughput per core is the shared resource under SMT, so cutting instructions and mispredicts carries over; latency-hiding tricks often do not.
- Current board: A ≈ 84.2 (0.0218 s, S 317); B 73.96 (0.1679 s, 43.6 MiB, S 576); C ≈ 80.5. Leaders: B #1 81.05 (0.0744 s, S 960, C++), #2 80.54 (0.0779 s, S 1039, Rust). Gaps: A 3.2, B 7, C 3.2.

## Past findings: do NOT re-propose these
Read all three notes files first:
`git -C /home/user/SpeedDemon show origin/cloud-research-notes:track-a/RESEARCH_A.md` (same for track-b/RESEARCH_B.md and track-c/RESEARCH_C.md).
Also dead on B: SIMD bucket probe in find, branchless combined find test, bigger or smaller slot tables, unchecked OR-merge writes,
term-key prefetch, 2 KB input prefetch, per-thread page release, phrase first-id reject, straddle-branch removal, bucket-add cuts,
two-pass tokens (hash a block, then look them up), parallel query parse (+33 lines for −2.6%), MADV_POPULATE on the docs share or the postings chunks,
16-byte keys / u32 slots (obsolete: the lookup is now hash-only with a 46-bit tag; never weaken the tag below 40 bits).
Dead on A: hour reuse on top of linelist, fingerprint intern, 3-pass intern, aligned arena, eol-from-name-end, second-compare skip, select replacement. linelist is merged.
Dead on C: block-mask field-end pass (mispredicts −25%, instructions +33%, cycle-neutral).

## Measured B profile on 8eb3624 (4 vCPU VM, no SMT)
Queries parse+insert 8-9 ms serial; sample+sort 14-19 ms; scan ~140 ms; scatter ~25 ms; answers ~31 ms.
1 CPU: scan 485, scatter 37, answers 118 ms. perf: tokens() 44%, scan closure 21%, apply 8%, scatter 4%, docs() 4%, memcmp in the phrase check 3.5%, page faults ~5%.
callgrind: tokens() ~82 instructions/token (hash ~20, find ~8 at 1.16 probes, ring/stores ~10). 79% of tokens are query terms.

## Rules for you
- RESEARCH ONLY. Do not edit, commit or push anything in /home/user/SpeedDemon. Do not change the /data symlink and do not write /out (other agents share them).
- Do not run the track binaries or timing benchmarks: four agents share one 4-vCPU VM, so timings would be noise.
  You MAY build copies of the code in your own /tmp/<yourname>/ directory and use objdump, llvm-mca and valgrind/callgrind ONLY on inputs you create under /tmp/<yourname>/.
  The binaries read /data/... paths, so to run one you must copy the source and change the paths to your own directory first. Read-only Python analysis of
  the real data in /tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/data/{a_real,b_real,c_real} is fine.
- No third-party packages, crates or skills. Zero dependencies is part of the score.
- Read your track's code line by line. For every idea, give: the exact lines or functions it changes, a sketch of the new code, why the output stays
  byte-identical (cite the spec), the expected effect with arithmetic (instructions/token, ms, formatted-line delta, net points), the risk, and how to verify it.
  Mark anything you did not measure as an estimate. Never invent measurements. Rank by expected net points after line cost; drop ideas under +0.2 pt.
- Write your findings to /tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad/plan/<your file name>.md and finish with a 10-line summary in your final message.
