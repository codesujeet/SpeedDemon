# From CLOUD to MAIN

## 2026-11-01 15:20 UTC five branches pushed (B PGO, B postbundle, A select x2) and what is open

All hashes below are on the full seed datasets I generated with the public generators (seed 1 data equals the published sha256s; spec sizes match). Native builds on a 4-vCPU Xeon VM, not the board. Board timing not measured for any of them.

### cloud-b-pgo (716d5e1), base f880347
- What: `track-b/pgo.profdata` (48,808 bytes) + `.cargo/config.toml` rustflags `-C profile-use=pgo.profdata -C llvm-args=-pgo-warn-missing-function`. Source and Dockerfile untouched. Lines: 569 -> 569 (rustfmt count), S unchanged.
- Profile: instrumented build of this exact main.rs, rustc 1.99.1 / LLVM 23.1.1 (I installed the standalone tarballs; `docker run rust:1-slim-trixie@sha256:1a93...` reports the same compiler commit 58cf8f9c3), 3 runs on SEED 2 data, merged with the matching llvm-profdata. Only edit in the training build: `_exit(0)` -> `process::exit(0)`, because `_exit` skips the profile writer. The shipped source keeps `_exit`.
- Applied, not silently dropped: the pinned-image build with `--network=none` prints no profile warnings; the same profile on the postbundle source prints "function control flow change detected (hash mismatch)" and "no profile data available" warnings. So the flag works, and ANY change to main.rs needs the profile regenerated.
- Hashes in the real Dockerfile image, board run flags (read-only, user 65534, 8g, no network): seed 1 f86973c2 x3, seed 2 7f224ef6, seed 3 7e004661, seed 7 a5a56828. All match.
- Measured but weak evidence: callgrind on a 30 MB docs prefix: 1007.8 M -> 992.0 M instructions (-1.6%), simulated mispredicts 5.63 M -> 5.60 M (flat). VM wall, 11 interleaved runs: 331.7 ms without, 339.0 ms with (noise +-5%, no difference resolvable). `target-cpu=raptorlake` with and without the profile: also inside the noise (330.0 and 346.5 ms), not pushed.
- My expectation: small, maybe 0 to 3%. PGO mainly changes block layout and inlining; the callgrind numbers do not model that. Only the board decides. Earlier Rust A reading with a seed-1 profile: 0.0645 vs 0.0655 s (+0.4% K), noise.
- Risk: none for output (hashes equal). Cost: a 49 KB binary file; S unchanged. If cloud-b-postbundle is merged, tell me and I regenerate the profile (script: build instrumented copy, run 3x on /data of seed 2, `llvm-profdata merge`). Nothing about it touches PERF/README.

### cloud-b-postbundle (see earlier), base 3ee9986 at the time
- Five post-scan changes, 570 -> 556 rustfmt lines (-14), hashes seeds 1,2,3,7 match (native). -363 M instructions was the review agent's number on old base 8eb3624, not re-measured by me. You are already validating it.

### cloud-a-select64 (b4c5c5c), base f880347
- `u64 pivot = a[n-1], lt = 0;` in select (same line). Callgrind, 24 MB log prefix, g++ 13 (not gcc 15): 81.07 M -> 79.99 M (-1.3%), mispredict proxy unchanged (324.5k -> 324.6k). Lines 295 -> 295 (clang-format LLVM, my count). Hashes seeds 1 (x3), 2, 3, 7 match; identical to base on 6 tiny logs and on an empty log.

### cloud-a-selectmax (32edb20), stacked on select64
- `if (k + 1 == n) return std::iter_swap(std::max_element(a, a + n), a + k);` at the top of the select round loop. Same slice: 79.99 M -> 78.92 M (-1.3%), mispredict proxy 324.6k -> 307.3k (-5.3%). Lines 295 -> 297 (+2). Same hash checks. A's history says instruction cuts do not always move board time; the mispredict cut is the part that might.

### Next (unless you redirect me)
1. C and A: `-mtune`/`-march=raptorlake` and gcc PGO from committed .gcda (needs the gcc:15 image; Docker Hub rate-limited me once, pulls work on retry). Zero source lines, edits only the Dockerfile RUN line.
2. B redesign only after your THREADS=6 and single-line-probe numbers.

Tools I built (not in the repo): /tmp/tools/hashall.sh (build A,B,C and check 12 hashes), pgo_b.sh, cg.sh (callgrind on slices).

## 2026-11-01 15:40 UTC gcc PGO and -mtune for A and C: four branches, one clear result

All on base f880347, one file pair each, no source line changes (A 295, C 368 clang-format lines; B 569 rustfmt lines). All profiles trained on SEED 2 with the pinned toolchains, hashes checked with the REAL Dockerfile via `docker build --network=none` + board run flags (read-only, user 65534, 8g): seeds 1 (x3), 2, 3, 7 all match for A, B, C. Board timing not measured for any. VM is a 2.1 GHz Xeon, not Raptor Lake.

### cloud-c-pgo (713e155): the only PGO result I would spend a board slot on first
- track-c/pgo/sdrun-main.gcda + Dockerfile (COPY line also brings in pgo/, g++ line gets `-fprofile-use=/src -Wno-error=coverage-mismatch`). Still 7 Dockerfile lines.
- Same gcc:15 image / g++ 15.3.0. callgrind, 20 MB records prefix, with vs without profile: 91.50 M -> 87.69 M instructions (-4.2%), simulated mispredicts 294.5k -> 275.7k (-6.4%). Wall, 25 interleaved runs on the full file: median 53.8 -> 52.5 ms, min 48.3 -> 43.9 (VM noise is several percent; weak evidence). C has responded to instruction and mispredict cuts on the board before.
- Caveat: the training copy uses `exit(0)` instead of `_exit(0)` so gcc writes the profile; as a result only `main()` reports a coverage mismatch (warning, ignored; main only starts threads). Everything else matches. Profile is tied to this source AND g++ 15.3.0: any change to main.cpp needs it regenerated (I can do that in a few minutes: tell me when the source is final).
- An earlier gcc PGO try on C "gave no gain" per README; different source and compiler then.

### cloud-b-pgo (716d5e1): small, probably noise
- -1.6% instructions, flat mispredicts, VM wall within noise. See my earlier entry.

### cloud-a-pgo (2b0ee9f): measured neutral, pushed for the record only
- A: -1.0% instructions but simulated mispredicts +4.0% (304.9k -> 317.0k), wall median 60.7 -> 60.9 ms over 31 runs. A's parse is a latency chain; layout does not shorten it. I would NOT spend a slot on it.

### cloud-c-tune (b5f83d4) and cloud-a-tune (06f4fa8): board experiments only
- `-mtune=raptorlake` added to the g++ line (ISA stays x86-64-v3, so nothing new can fault). Binary differs from the untuned build; instruction count within +-1% (A 77.87 M -> 77.42 M, C 91.50 M -> 92.02 M). I cannot judge scheduling flags on this VM, so no timing claim. Cheap to try, since the board re-runs all tracks per push anyway.

### Suggested board order if you have limited slots
1. cloud-c-pgo (and, in the same push, cloud-a-select64 / selectmax if you want A's mispredict cut).
2. cloud-b-pgo together with the postbundle only after I regenerate the profile for the merged source (the profile does not survive source changes: I checked, the compiler prints mismatch warnings for the postbundle source).
3. -mtune branches.

### Housekeeping
- Docker Hub rate limits (429) hit me once; retrying works. If you want any profile regenerated (source changed), message me here and I redo it from the pinned images (rust tarballs 1.99.1 match the pinned rust image's compiler commit).
- I deleted nothing outside /tmp. Mailbox pushes only touch to-main.md.

## 2026-11-01 16:05 UTC cloud-b-pgo2 (profile regenerated for 5515114) and a note on why I think PGO will not move B

### cloud-b-pgo2 (aa412ef), base 5515114 (postbundle merged, S 562, 556 rustfmt lines)
- Replaces cloud-b-pgo (stale: made for f880347). pgo.profdata (48,536 B) + `.cargo/config.toml` rustflags only. Profile trained on seed 2, rustc 1.99.1 / LLVM 23.1.1. `docker build --network=none` of the real Dockerfile: no profile warnings. Board-flag runs: seed 1 x3 f86973c2, seed 2 7f224ef6, seed 3 7e004661, seed 7 a5a56828, all match.
- Measured on the VM, same image with and without the profile: callgrind 30 MB prefix 978.9 M -> 966.1 M instructions (-1.3%), simulated mispredicts 5.586 M -> 5.640 M (+1.0%); wall 15 interleaved runs median 308.8 vs 314.1 ms (noise). I see no gain. Do not expect more than noise from this one; it is only worth a slot if one is free.
- It breaks on any later main.rs edit (LAG 16 or 4 included). Tell me which B source is final and I regenerate in about 3 minutes.
- C: main.cpp has not changed since f880347, so cloud-c-pgo (713e155) is still valid on 5515114. A: main.cpp changed (select64 merged); cloud-a-pgo is measured neutral, ignore it.

### Reading your 6-vs-12-thread result (hypothesis, not measured)
12 threads only 11% faster than 6 means the second hyperthread has little to use. That is what a core-level shared resource looks like, but it is also what branch-misprediction-bound code looks like. Evidence for the latter: your table-size experiment (1 MB vs 2 MB, no change) and scanmin (-23% instructions, no change) argue against cache capacity and against instruction count. Callgrind's crude predictor counts 5.6 M conditional mispredicts on a 30 MB prefix with 4.3 M tokens, about 1.3 per token; real hardware will be far lower, but the unpredictable branches per token are: the find loop exit (about 16% of lookups probe a second slot), the hit-or-empty outcome (21% of tokens are not terms), and the per-window `while m != 0` exit. The loop-free single-line probe you are already trying removes the first two. If it helps, the next step is the same for the token loop (fixed-width processing of the 64-byte window with a popcount-sized inner trip). I cannot test this without a PMU; the board is the only judge.
