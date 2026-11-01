# Handoff prompt for the next cloud session (paste everything below the line)

---

You are continuing Speed Demon 2026 work for the solo team "Microsecond" (repo codesujeet/SpeedDemon). Read this whole
note before acting.

## Where things are
- Live code: branch `speed-demon`. NEVER push to it unless the user says so for a specific change. No tags. Do not edit
  PERF.md, README.md, real-spec/board-runs.jsonl or other tracks' files.
- Research notes and tools: branch `cloud-research-notes`:
  - `track-a/RESEARCH_A.md`, `track-b/RESEARCH_B.md`, `track-c/RESEARCH_C.md`: every measurement and negative
    result so far. Read them; never re-propose a dead idea.
  - `track-b/handoff/BRIEF.md`: the scoring formula, board hardware, current scores and leader gaps, the dead-idea
    list and the B profile on 8eb3624. The four review agents were given this brief.
  - `track-b/handoff/tools/`: helper scripts. Each has the old scratchpad path hard-coded in it
    (`/tmp/claude-0/-home-user-SpeedDemon/41f9d852-.../scratchpad`). Edit that to your own scratch dir first.
    - bench.py `reps cpus bin...`: interleaved wall/CPU medians. Mount tmpfs on /out first; it deletes the outputs before each run.
    - cgpeak.sh `cpus bin`: peak memory (cgroup v1).
    - rlines.py (rustfmt, max_width=100) and slines.py (clang-format LLVM): formatted code-line counts.
    - fuzz_bd.py `base new [cases]`: differential fuzz for B. Terms are 1-32 bytes, per the spec's 32-byte maximum.
    - fuzz_a.py, fuzz_c.py, gen_a.py, ref_a.py: generator, oracle and fuzzers for A and C.
    - g15build.sh: builds C++ with the board's gcc-15 image; it also restarts dockerd if it died.
- Unmerged experiment branches: `cloud-b-compactkeys` is obsolete (written for the old key-comparing lookup), and the
  other `cloud-*` branches are already merged or recorded as dead in the notes.

## Data (not in git; download again)
- Seed 1: `https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/data/track-{a,b,c}/full-seed1/...`, listed in
  `https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/data/SHA256SUMS.txt`. B: docs.txt and queries.txt.
  The programs read `/data/...` and write `/out/...`. Symlink /data to the track's data dir.
- Seeds 2, 3 and 7 are NOT public. Ask the user (their main session has them) or have them run the hashes.
- Expected output hashes (sha256 prefixes): seed 1 A 495ca096, B f86973c21022, C d8088b1c.
  A 2:fe3469ff10 3:5207056c6c 7:b384b9ddf5; B 2:7f224ef6 3:7e004661 7:a5a56828; C 2:f99e6f9133 3:4fb4eeb21b 7:da8b48aef3.
- The live board feed is `https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json` (team "Microsecond").

## Environment notes
- apt works. perf: use `/usr/lib/linux-tools-6.8.0-147/perf` with `-e cpu-clock` (there is no PMU). valgrind/callgrind is the
  instruction counter; `--branch-sim=yes` gives a mispredict proxy. Run valgrind on the binary directly, not via taskset.
- Docker: start dockerd yourself (see g15build.sh). Verify with the real Dockerfile, `docker build --network=none`, then
  `docker run --rm --network=none --read-only --user 65534 --memory 8g -v DATA:/data:ro --tmpfs /tmp -v OUT:/out IMAGE`,
  and compare the output hash on 3 or more runs. Build from a clean copy of the track dir, so no target/ and no Cargo.lock go in.
- The VM has 4 vCPUs and no SMT. The board is an i5-13500 with 6 P-cores and SMT, running 12 threads. Gains from
  latency tricks shrink on the board; cuts to instructions and mispredicts carry over. Delete Cargo.lock before
  committing (it is untracked, and the stop hook complains about it).

## State at handoff (Nov 1 ~13:00 UTC; freeze Nov 2 15:00 UTC; stop pushing code ~14:00 UTC)
- Board: overall about 79.6, rank 8/96. A about 84.2 (0.0218 s, S 317). B 73.96 (0.1679 s, 43.6 MiB, S 576; hash-only
  Terms::find, head 8eb3624). C about 80.5. Leaders: B #1 81.05 (0.0744 s, S 960).
- Four Opus review agents were started (research only): A, C, B-scan, B-post-scan. Their outputs were planned in the old
  scratchpad `plan/{A,C,B_scan,B_post}.md`. If they are not in this branch they were lost. Re-run that review with
  BRIEF.md if the user wants it.
- Planned next step: merge the agents' findings into `FORWARD_PLAN.md` (ranked by net points after line cost, cross-checked
  against the notes). Then implement each idea on its own branch `cloud-<track>-<idea>`, based on the latest
  origin/speed-demon. Verify each with Docker and its hash, measure callgrind instructions, interleaved 1 and 4 CPU timing,
  peak memory and the line delta. Commit one idea per commit, with a message that matches the diff, gives the numbers and
  says "board timing not measured". Push, and report each idea to the user: branch, gain, line delta, risk.

## Rules (from the user)
- Byte-identical output. One failed run zeroes a track.
- Zero dependencies, no Cargo.lock, no data files, no third-party packages or skills in the repo. Keep the Dockerfile allowlisted and pinned.
- Never invent numbers; say plainly what was and was not measured.
- Points: 1% faster is about +0.11 pt; one formatted line is about -0.009 pt; gains under about 0.2 pt are not worth a board slot.
- Only push to speed-demon itself if the user allows it. Then all four seed hashes must match, run `git pull --rebase` first,
  touch only that track, need a 3% or better measured gain, and check the board twice afterwards, reverting if it got worse.

## Update, Nov 1 ~14:00 UTC: review-agent results (all saved under track-{a,b,c}/plan/ on this branch)
Pushed and verified (seed-1 hash in the real Docker image, 3 runs; seeds 2/3/7 NOT checked; board timing not measured):
- cloud-c-parsecut (8d7fe32): C callgrind instructions -8.9%, -2 lines. Low risk. Try first for C.
- cloud-c-minmaxbranch (58ea37c, stacked): instructions -1.3%, +2 lines, about +0.1 pt. Keep only if the board confirms.
- cloud-b-scancore (a1b1fcb): B instructions -19.6%, +22 lines. VM 1 CPU 691.7 -> 585.0 ms, 4 CPU 302.6 -> 265.9 ms (noisy).
- cloud-b-scanmin (28a2cd1, stacked on scancore): -23.3% instructions vs base, +19 lines. VM 1 CPU 566.4 ms, 4 CPU 258.9 ms.
  The differential fuzz against base matched on 150/150 cases. Try this on the board first.
Not yet implemented:
- B post-scan bundle: track-b/plan/B_post.md and B_post_bundle.diff. Five changes: Units as a Vec<u16> per bucket, a faster
  bit_sums, the sample back to 1/64, chunks(64), and the empty-OR shortcut. -14 lines, -363 M instructions, est. +0.25..0.40 pt.
  It was written against 8eb3624 and must be rebased onto scancore/scanmin if those merge.
  Note from that agent: on B one line is worth 0.0057 pt, and memory is near its cap (worth at most +0.17 pt).
- Track A: track-a/plan/A.md, A_armL.patch and A_armP.patch. The A agent was implementing cloud-a-parsecuts,
  cloud-a-hourchunks and cloud-a-droplist against the new A base (86570b0 + 68912bc). Check origin for those branches;
  any half-finished state is in scratchpad plan/A_context_*.md, which may be lost.
- Track A pushed (callgrind on the gcc 15 image, hash 495ca096 on 3 Docker runs; seeds 2/3/7 not checked; base 41d7902 = 699.97 M, 340 lines):
  cloud-a-parsecuts d5df850: -4.5% instructions, +4 lines. Also fixes a base crash: the piece cut reads log[-1] on tiny logs at 8+ threads.
  cloud-a-hourchunks 9ca493d: +0.3% instructions, -14 lines, peak about the same. Its commit message wrongly says a one-line log
  at 12 threads "still crashes as on the base". Not corrected yet.
  cloud-a-droplist bb0136d (stacked): -12.9% instructions vs base, -19 lines overall. Needs a board A/B; the line-chain risk is back.

## Final state (Nov 1, end of session)
All remote branches are up to date. Local-only branches hold no unpushed commits. Everything from the scratchpad
worth keeping is on this branch:
- track-*/plan/*.md: the four agents' findings. A_context_cloud-a-parsecuts.md is the A agent's state file.
- track-a/plan/files/: main_armL.cpp, main_armP.cpp, armL.patch, armP.patch (A agent phase-1 sources; superseded by
  the cloud-a-* branches).
- track-b/plan/files/: B_scan_core.rs, B_scan_min.rs, B_scan_fused.rs (scan agent; the paths are patched to /tmp/bscan,
  so use the pushed branches instead); bundle_main.rs, the B_post_bundle.diff source, fuzz.py, fuzz_one.py and post_an*.py
  (post-scan agent: bundle source written against 8eb3624, fuzzer, analysis scripts); stash_key16_wip.patch (dead
  16-byte-key experiment, kept for reference only).

Branches to test on the board, best first (seed-1 hash checked in the real image; seeds 2/3/7 NOT checked):
1. cloud-b-scanmin (or cloud-b-scancore). 2. cloud-c-parsecut. 3. cloud-a-parsecuts. 4. cloud-a-hourchunks.
5. cloud-a-droplist (A/B test). 6. cloud-c-minmaxbranch (marginal).
Not yet a branch: the B post-scan bundle (track-b/plan/B_post_bundle.diff). It needs rebasing onto scanmin.
Open items: cloud-a-hourchunks 9ca493d has a wrong sentence in its commit message (the user decides: correction
commit or leave it). The empty dirs /w and /w/out in the old container (irrelevant in a new one).

Estimated gains (from callgrind and VM timings, assuming 50-80% carries over to the board; NOT board-measured):
B +1.1..+1.9 (to about 75-76), C +0.35..+0.7 (about 81.0-81.4), A +0.3..+0.6 (about 85.1-85.4);
overall about 79.7 -> 80.2-80.8. Reaching 81+ needs a larger B redesign after the scan (serial setup, scatter, answers).
