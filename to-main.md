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
