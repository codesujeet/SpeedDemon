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

## 2026-11-01 16:45 UTC cloud-b-tokloop (45b5a50): token loop as two passes, measured SLOWER on the VM; do not spend a slot unless you want the negative on the board

Base 4423afa. Hashes seeds 1, 2, 3, 7 match (native build, rustc 1.99.1); 16-slot version also matched all four.

What it is: pass 1 per window writes 10 token words (address plus length above bit 56) with no branch on the token count (tzcnt and blsr on the start and end masks, count advanced by popcount; a loop only for the rare window with more than 10 tokens). The token touching the window's last byte is left for the next window using lzcnt instead of a branch. Pass 2 is one flat loop over the document's tokens (hash, prefetch, look up the token LAG places back). Lines 579 -> 601 (rustfmt count, +22).

Measured on the 4-vCPU VM only (not the board), vs 4423afa, same toolchain:
- callgrind, 30 MB corpus prefix: instructions 1024.6 M -> 1066.9 M (+4.1%); simulated conditional mispredicts 3.83 M -> 3.43 M (-10.6%).
- wall, 15 interleaved runs, full file: median 311.5 -> 347.4 ms (+11.5% slower).
- 16-slot variant: +8.9% instructions, -12.3% mispredicts, wall 320.9 -> 373.3 ms (+16% slower).

Reading: the window-level branches (while-exit, straddle test) are not what limits the scan. The old loop interleaves extraction with hashing and lookups, so out-of-order execution overlaps them; staging the positions first adds stores and reloads and serializes that overlap, which costs more than the removed mispredicts save. The VM is a Skylake-class Xeon, not Raptor Lake, so the sign could differ on the board, but I would not bet on it. Everything points at the lookups, not the tokenizer, which fits your bucket test.

Next, in your order: (1) A diagnostics, then C diagnostics, as correct-output branches, each commit marked DIAGNOSTIC; (2) B #2 (count and last in one u32 array) as its own branch on top of the current speed-demon.

## 2026-11-01 17:30 UTC A and C diagnostic branches (six, all marked DIAGNOSTIC, correct output, base aafa498)

Rule: each runs ONE phase twice and keeps the output correct, so T(diag) - T(head) is the cost of that phase on the board. Do not merge. Hashes match on seeds 1, 2, 3, 7 for all six (A native build with the Dockerfile flags; C built with the real Dockerfile and the committed gcc profile, run with the board flags). VM numbers are medians of 15-21 interleaved runs on a 4-vCPU Xeon, shown only as a rough guide; they are NOT the board.

Track A (cloud-a-diag-parse 06bd22a, cloud-a-diag-hours d0c13d0, cloud-a-diag-text 334d2d8): one-file change in track-a/src/main.cpp each.
- parse: each worker runs a second Parser over every piece before the real one. The throwaway parser does the whole parse and endpoint interning but writes its records into one reused 16 KB scratch chunk (so it skips per-hour chunk carving only). VM: head 49.4 ms -> 63.5.
- hours: each hour's counting sort, select and line sizing run twice; the first result goes to a throwaway vector and does not release the hour's records. This includes select. VM: 49.4 -> 60.7.
- text: each hour's lines are formatted into the mapped output twice (same bytes). The first pass takes the output page faults. VM: 49.4 -> 53.3.
- Caveat: the second pass of parse is warm (the first took the file's page faults), so it under-reads a cold parse a little. A's hours phase and text phase are claimed per hour by all threads with barriers between, so the extra work lands partly on the critical path and partly in idle time; read the delta as an upper-ish bound on the phase's contribution.

Track C (cloud-c-diag-parse 6dc01a8, cloud-c-diag-regions 2b68de3, cloud-c-diag-threads a8e2c70): one-file change in track-c/src/main.cpp each; parse_records is untouched in all three, so its gcc profile still matches (only the thread lambda in main changes, which the profile never covered well).
- parse: after the real parse of a piece, parse_records runs again over the same piece with its own throwaway Totals (names and bitset marks are already set, so real output cannot change). VM: head 47.1 ms -> 83.8.
- regions: after the real flag and fill phases, two more phases redo the same idempotent stores (present[] flags, country_of[] table), no populate. VM: 47.1 -> 50.3.
- threads: twelve empty threads are created and joined before the real pool. VM: 47.1 -> 47.5.
- Set-up apart from the parse, as I read your request: T(head) - delta(parse) is roughly everything else (mmap, populate, release, exit); delta(regions) and delta(threads) split two of its pieces. If you wanted a literal "populate twice", it is meaningless (second populate hits resident pages), so I did not build it.

Next: B #2 (count and last in one u32 array) on top of the current speed-demon, as its own branch.

## 2026-11-01 18:40 UTC three B branches on aafa498: countlast, masks, info; plus a bug note and a fuzz fix

All three based on aafa498 (buckets kept). Real Dockerfile, `docker build --network=none`, board flags, seeds 1 (x3), 2, 3, 7: all hashes match for each. VM numbers (4-vCPU Xeon, not the board) are one run of 21 interleaved wall runs and one callgrind pass on a 30 MB corpus prefix; the VM has not predicted the board for B so far, so treat them as sanity checks only. Board timing not measured for any of these.

### cloud-b-masks (af446d2): ported from scanmin, the straddle test per token is gone
- Token starts and ends come from two masks; ends excludes bit 63, so a token reaching the window's last byte has no end and its start is the left-over where the next window begins. One `push` closure for both paths. Lines 579 -> 575 (-4). Instructions -1.7% (1025.4 M -> 1008.4 M), simulated mispredicts -0.7%, wall median 338.0 -> 328.6 ms (inside noise). Expect a small effect; it is the branch your note asked for, and it is also fewer lines.

### cloud-b-info (f7223da): ported from scancore, read-only info table
- find() returns NONE (u32::MAX) on a miss, one u16 per term says which pending word to OR and whether the term is anchor and list; per-token dedup moved to the kept-list loop. Lines 579 -> 583 (+4). Instructions -6.0% (1025.4 M -> 964.1 M); simulated mispredicts +6.2% (3.82 M -> 4.05 M); wall 309.7 -> 312.2 ms (no difference). So it repeats scanmin's pattern on the VM: fewer instructions, same time. Only the board can say if the shorter closure helps now that find() is cheaper. Independent of cloud-b-masks (touches different lines); they should merge together without conflict, but I have not built the combination (one idea per branch).

### cloud-b-countlast (3b60c85): B #2, mark and count in one u32 per term
- Lines 579 -> 587 (+8). Instructions +2.2%, simulated mispredicts +4.1%, wall 319.7 -> 322.5 ms (no difference). The idea only pays if the second cache line per list token was a real stall; the VM cannot show it. Mixed expectation, mild: I would put it last in the queue. Note: it changes pre to *const u32, so it conflicts with cloud-b-info in tokens()/the sample pass; they are alternatives for the same arrays, not additive without a rebase.

### cloud-b-tokloop (45b5a50, base 4423afa): already posted, measured slower on the VM (+11.5%); skip.

### Two things I found while testing, for your attention
1. My old differential fuzzer was passing vacuously: the base binary itself panics on tokens longer than 32 bytes. Cause: the bucket find() matches a FREE slot when h = 0 (tokens over 32 bytes are hashed to 0, tag 0 equals an empty slot's tag), so it returns id 0 - 1 = u32::MAX and the sample pass indexes out of bounds. It is out of spec (spec 2.2: no generated token is longer than 32 bytes, longest 17), and your edge-suite B 06_long_tokens was already known out of spec; before the bucket change such a token returned 'none' instead of panicking. If you want it safe at no cost: set the hash to a value whose tag bits are non-zero for len > 32, e.g. `h = 1 << 63`... but I have not built or tested it.
2. I fixed the fuzzer (tokens at most 32 bytes, base failures reported); it detects a deliberately broken bit-63 rule in 27 of 30 cases and finds 0 differences for masks, info and countlast (180 / 180 / 120 cases). The base crashes by itself on about 6% of the tiny random corpora (cases with few documents); those are excluded and my builds never fail where the base succeeds.

Tools are not in the repo (/tmp/tools/fuzz_b.py in my container). Ask and I will paste the script into this mailbox.

### Order I would put on the board
1. cloud-b-masks (fewer lines, removes a per-token branch), 2. cloud-b-info, 3. cloud-b-countlast. They are three single readings of at most a few percent each, so the board will not resolve them individually; I would test masks+info together if you can afford only one slot.

## 2026-11-01 16:50 UTC Cause of the tiny-corpus crash: docs.txt of 1 to 11 bytes (out of spec, unreachable by the generator)

- Smallest failing input: docs.txt = `0<TAB><LF>` (3 bytes, one empty document), any queries.txt. Head 6c2452d panics at main.rs:379 `index out of bounds: the len is 3 but the index is 18446744073709551615`.
- Cause: the share cuts. `cut[t] = text.len() * t / THREADS`; when docs.txt is shorter than 12 bytes, cut[1] (and maybe more) is 0, and `while *c < text.len() && text[*c - 1] != b'\n'` reads text[0 - 1].
- Exact range, measured: crashes for every docs.txt of 1 to 11 bytes (3, 4, 5, 6, 8, 10, 11 bytes tried; also 2 docs in 10 bytes); passes for 0 bytes (the `*c < text.len()` test short-circuits) and for 12 bytes and more (cut[1] >= 1). All 7 fuzz failures of seed 9 were 3-byte files.
- Out of spec in practice: every official scale is far above 12 bytes (tiny: 1,000 documents, 1,000,131 bytes; the holdout is full scale). Nothing else in the fuzz failed.
- Zero-line fix if you want it: `while 0 < *c && *c < text.len() && text[*c - 1] != b'\n' {` (a cut of 0 stays 0, which is a line start). NOT verified beyond compiling: I have not run hashes or a reference comparison on it. Tell me if you want it as a branch with checks.

## 2026-11-01 17:05 UTC B redesign: design note (before building)

Small item done first: cloud-b-smallfix (a001e0b, base 208bd01): `while 0 < *c && ...` in the share cut, 0 added lines, seeds 1/2/3/7 hashes match (native). Not checked: real image, tiny/sample, reference comparison on the 1-11 byte inputs, fuzzer.

### What the board numbers imply (my reading, not measured further)
- The bucket change gained about 8%. Its comment says the removed branch mispredicted about 1 token in 3. At ~17 cycles per mispredict that is ~5.6 cycles per token, and B's scan is roughly 70-80 cycles per token per thread (about 100 ms, 3 M tokens per thread, 2.5 GHz). So the gain matches a simple "cycles per token" model.
- In that model the rest of the per-token cost is the work itself: ~115 instructions per token at about 1.5 IPC per thread, ~3 per core with both hyperthreads busy. That also fits 12 threads being only 11% faster than 6 (the core is already full).
- It does not fit scanmin (-23% instructions, no gain). So I do not trust instruction counts alone. What I trust: removing whole passes and whole per-token stores and loads, not shaving them.

### D1 (build first): one fused pass per token, no per-document id list (your a + b + c)
- Per token: classify, hash, prefetch, ring (as now) -> bucket lookup (as now) -> act on the id right there, by id range (ids are already grouped by class after the renumber):
  - bitmap term: OR the doc bit into `pending` (L1);
  - list term: dedupe and emit the (term, doc) unit, with a branch-free choice of a scratch target for non-list tokens;
  - anchor (about 2% of tokens, a predictable branch): verify that anchor's phrases on the TEXT around the token, by hashing the neighbouring tokens and comparing with the phrase terms' hashes (the same 64-bit hash the term table trusts, collision odds about 2^-63 per compare);
  - everything else (non-terms, phrase-only terms: about 22% plus the bitmap and list tokens' extra state) does nothing more.
- Goes away: the id store per token (36.5 M), the per-document closure pass that re-reads every id, the kept and anchors arrays, the per-token `last[]` read-modify-write for non-list tokens, and the id-list phrase compare.
- Why it might help on the board: your +100 ms (tokens + lookups + ring + ids) against +27 ms (classify + hash) says the cost is everything after the hash. D1 deletes a whole second pass over 36.5 M ids and most per-token state. It is not a shave of the same pass.
- Risk: phrase verification on text (backward and forward tokenisation inside the document, stopping at the TAB and LF) must exactly match the id-list semantics. The fuzzer covers phrases, upper case, non-ASCII separators and window edges, and I will add cases.

### D2 (separate branch, low risk): the serial prefix (your e)
- The VM's query parse is about 10 ms on one thread: byte-by-byte `split` closures plus a hash insert per term. The class sort (9.3 M instructions), the anchored and found sorts and the seg loop add a few ms more, all on one thread while 11 wait.
- Plan: tokenise queries.txt with the same 64-byte masks as the corpus (no byte loops), and move what can be moved into the existing on_threads phases. Expected a few ms; the VM can measure single-thread serial time reasonably.

### D3 (separate branch): the answer phase and memory spikes (your f, and your 16:36 question)
- Why peak memory depends on scheduling, from the code: at the answer phase `mallopt(-3, 64 << 20)` raises the mmap threshold, so every bitmap clone (31 KB), every docs() Vec and every OR result comes from per-thread malloc arenas. Freed blocks stay resident, and each thread's high water depends on which 64-query chunks it happened to claim. bits (~11.5 MB) and post (~22 MB) are still alive then. A run where one or two threads draw the heaviest OR chains keeps more arena memory resident: that is a plausible source of 43 to 54 MiB on identical code. I have not measured it.
- Plan: one reusable scratch bitmap and list buffers per thread (no allocation per operation), which should give a steady peak and fewer page faults. Then specialised loops per operator.

### Not now
- Radix-partitioned lookups (each partition's slice of the table in L1): the biggest possible lookup win, but too big a rewrite for the time left.
- Hyperthread producer/consumer split (your d): too risky to get right and correct by 10:00.

Order: D1 now. I will post as soon as it is correct, then D2 and D3.

## 2026-11-01 18:15 UTC D1 is correct but slower on the VM; an answer-phase branch; where B's mispredicts are

### cloud-b-redesign-fused (D1), base 208bd01: correct; VM says slower, so one reading at most
- Checked: real image + board flags, seeds 1 (x3) / 2 / 3 / 7 hashes match; tiny equals the reference (dcdad7cedfc0); sample equals the spec (eeeec1e9319d); differential fuzz with phrases cut from real document windows: 0 differences in 186 valid cases (it catches a deliberately wrong phrase rule in 21 of 30). Lines 579 -> 613 (+34).
- VM only: instructions +34% (1013 M -> 1360 M on the 30 MB prefix), simulated mispredicts +25%, wall median 306.6 -> 386.0 ms (+26%).
- Why my premise was wrong: the per-document "second pass" I wanted to delete is a loop over an L1-resident array, which is cheap. To keep the action branch-free for every token (the board's lesson), every token has to write a scratch unit, a dedup mark and a count. That costs more than the loop it replaces. You asked for it even if flat; it is not flat, but here it is if you want one reading. The VM was right on tokloop.

### cloud-b-redesign-answers (e2970db), base 208bd01: low risk, the answer phase without two kinds of branch
- `keep` replaces Vec::retain in the list-with-bitmap AND/NOT and in the skewed list AND/NOT, so keep-or-drop is arithmetic, not a branch. `put` writes numbers instead of write!/core::fmt, which calls through function pointers per number.
- Checked: real image + board flags, seeds 1 (x3) / 2 / 3 / 7; tiny and sample as above; phrase-heavy fuzz 0 differences in 93 valid cases. Lines +18 (597).
- VM only: 30 MB prefix, instructions -3.2%, simulated conditional mispredicts -10% (3.85 M -> 3.46 M), indirect 1.20 M -> 0.64 M. Wall within noise. On the full data the answer phase is a smaller share, so expect at most about 1-2% of B. Probably not resolvable in one reading.

### Where B's mispredicts are now (callgrind branch simulation, full seed-1 data, head 208bd01): 11.96 M in all, about 0.33 per token
- tokens(): 5.29 M, of which `while ends != 0` 4.40 M (one per 64-byte window)
- answer phase: the docs() list-building closure 1.67 M (its loop over 12 thread segments per term use), apply 0.75 M (retain 0.74 M, fixed in the branch above)
- scan closure per-document loops 0.92 M; query parse iterators 0.43 M; memcmp (phrase compare) 0.31 M; malloc 0.24 M; bucket loop in find 0.42 M
- Reading: the bucket change removed roughly this many mispredicts again and gained about 8%. Everything left is worth maybe another 8-10% if it could all be removed for free, which it cannot. I do not see a path to 0.110 s through branches alone.

### Next
- D2: the serial query parse with the corpus's 64-byte masks (VM: about 10 ms on one thread, about 28 instructions per byte of queries.txt in byte-wise split closures). This is single-thread work, where instruction cuts should show.
- Then the docs() segment loop (1.67 M mispredicts): build each term's list without a loop per thread segment.

## 2026-11-01 18:40 UTC D2 part 1: cloud-b-redesign-qparse (c1bf425), base 208bd01

- The serial query parse no longer splits bytes one at a time: each word's length is one trailing-zero count of the classes() letter-or-digit mask after it; the byte after the word (space, quote or LF) says whether the line goes on. Operator vs term by the first byte's case (spec 2.3 grammar), not by position.
- Checked: real image + board flags, seeds 1 (x3) / 2 / 3 / 7 match; tiny equals the reference, sample equals the spec (both contain the op_words queries, lowercase and/or/not as terms); phrase-heavy fuzz 0 differences in 96 valid cases. Lines +5 (584).
- VM only: on the full queries.txt with a one-document corpus, main's own instructions 55.9 M -> 34.1 M (-39%). This is single-thread work before the scan, so my estimate is about 3-4 ms on the board if it is instruction-bound there (not measured). One reading may not resolve it against +-5% noise; it is independent of cloud-b-redesign-answers, and both merge on 208bd01 without touching the same lines (not built together).
- Next: the rest of the serial prefix (sorts, renumber, seg loop), then the docs() per-segment loop.

## 2026-11-01 19:10 UTC cloud-b-redesign-qparse updated (27bb471): the query parse was waiting on memory

- Phase timers in a VM copy: the serial query parse took 9-10 ms, and most of it was 165,000 inserts into the cold 2 MB term table, each a miss after the previous one (about 55 ns each). The parse now only hashes; the inserts follow in the same order (ids unchanged), each bucket prefetched 16 terms ahead.
- VM, 5 runs each: main start to end of the query parse 9-10 ms -> 7.1-7.9 ms (table zeroing about 1 ms, parse and hash about 4.5 ms, inserts about 1.9 ms). I tried the parse on all 12 threads: 7.0-8.0 ms, no better (joining the pieces cost about 1.4 ms), so it is not in.
- Checked as before (real image + board flags, 4 seeds, tiny, sample, phrase fuzz 0 differences in 96 cases). Branch total +18 lines over 208bd01.
- Expected on the board: about 2-3 ms of serial time, if it behaves like the VM (not measured). Still worth one reading together with cloud-b-redesign-answers if you want to save slots: they touch different lines.
- Next: the remaining serial steps after the sample (class sort about 1.5-2.4 ms, renumber about 2-3 ms, seg loop about 2 ms on the VM), then the token loop's window exit with 128-byte windows (VM test first, since the VM was right about tokloop).

## 2026-11-01 17:25 UTC (real time) three cheap board experiments on the lookup path, one negative

Correction first: my headings since "17:05 UTC" were wrong (my clock guess). The real times were about 16:35 to 17:00. This one is real.

Since the board, not the VM, decides B, these are cheap one-idea builds for readings. All base 208bd01; native build hashes for seeds 1/2/3/7 match on each; none changes output by construction except 8slot, which also passed tiny, sample and the phrase fuzz.
- cloud-b-exp-t1 (hint change only): the bucket prefetch and the last[id] prefetch use T1 (into L2) instead of T0. Question: are L1 fill requests per token (two per token, both hyperthreads of a core sharing the fill buffers) the limit? Your lag 16 reading (0.2136 s, much worse) fits that idea: more prefetches in flight did harm. 0 lines.
- cloud-b-exp-nopre: no last[id] prefetch at all (one fewer L1 fill request per token). -1 line. Same question from the other side.
- cloud-b-exp-8slot (7b4f192): term table buckets of eight slots in one aligned 64-byte line (two 32-byte compares), same 2 MB. A full bucket (next-bucket probe, 0.42 M of 12.0 M simulated mispredicts) gets rarer, and no bucket straddles two lines. +5 lines.
- Negative, not pushed: 128-byte token windows (to halve the `while ends != 0` exits). VM: +13.5% instructions (128-bit mask operations), simulated mispredicts -8%, wall +9% (21 interleaved runs, median 338.6 -> 369.1 ms). Dropped, as with tokloop.

Branch summary for B (all correct on 4 seeds, tiny, sample; none board-measured):
1. cloud-b-redesign-qparse (27bb471): serial query parse, VM -2 to -3 ms of serial time. +18 lines.
2. cloud-b-redesign-answers (e2970db): answer phase without retain's branch and without core::fmt. +18 lines.
3. cloud-b-exp-t1, cloud-b-exp-nopre, cloud-b-exp-8slot: lookup-path questions for the board.
4. cloud-b-redesign-fused: correct, VM +26% slower; only if you want the reading.
1 and 2 can go in one push (different lines).

## 17:09 UTC (from `date`) B memory: the peak swing is the scatter; cloud-b-redesign-steadymem (23a681f) fixes it on the VM

Time correction again: my last heading said 17:25, but `date` read 17:04 then. From now on the times are from `date`.

Your 16:36 question, answered with measurements (cgroup v1 on my VM, /data warmed from outside the group, fresh group per run):
- Anonymous memory at phase ends is the same in every run: 37.2 MiB after the scan, 37.4 after the scatter, 39.3 after the answers. So the swing is a transient inside a phase.
- A 0.5 ms sampling trace of the group's usage: about 41 MiB at the end of the scan (bitmaps 11.5 MiB plus 384 units vectors, 24 MiB capacity / 21 MiB used), up to 49-50 MiB during the scatter, about 40 MiB after it (bitmaps plus post, 21 MiB). Peak runs land in the scatter. The answer phase's mallopt is not the cause: removing it changed nothing (peaks 47.8-51.4 vs 47.5-51.0).
- Cause: every thread writes its own segments into every bucket's region of post, so a thread that runs ahead makes post's pages resident while the others still hold their units. How far ahead the fastest thread gets is up to the scheduler, which is the run-to-run swing.

cloud-b-redesign-steadymem (23a681f), base 208bd01:
- No thread starts bucket b until all 12 have finished bucket b - 2 (yield while waiting; only waits on lower buckets, so it cannot deadlock). +4 lines.
- VM, 8 runs each: head 45.1-50.6 MiB; this 41.8-42.3 MiB. The score stops rewarding memory at about 42 MiB (your number), so this sits right at it, with almost no spread. VM wall 21 interleaved runs: 314.9 vs 296.9 ms median (no slowdown seen; on the VM 12 threads share 4 CPUs, so the waits should be worse here than on the board).
- Checked: real image + board flags, seeds 1 (x3) / 2 / 3 / 7 match; tiny and sample match; phrase fuzz 0 differences in 39 cases.
- Expected on the board: peak about 42-44 MiB every run instead of 43.4-53.9. That is worth up to about 0.9 points on a bad run, and it removes a risk in the five final runs. Board memory not measured. This is the B change I am most confident in so far.

## 17:16 UTC two more single-idea B experiments (both base 208bd01, both output-neutral by construction)

- cloud-b-exp-spread (0e482bf): in the scan closure, every non-bitmap token (~55%) read-modify-wrote the same spare word pending[nbits], and every token without kept/anchor state (~22%) the same last[nkeep], so runs of such tokens wait on each other through one address. They now hit one of eight spare words picked by token position (cmov), and the bit is ORed without the multiply. Full-data callgrind put that one OR line at 363 M instructions (10 per token). Checks: 4 seeds native, tiny, sample. +3 lines. VM: +2.5% instructions, wall no difference. Low expectation, cheap question.
- cloud-b-exp-nta (4cb09bf): the input stream's 2 KB-ahead prefetch is NTA instead of T0, asking whether the streamed text evicts the shared term table from L2. VM: median 306.6 -> 328.5 ms, min 269.8 -> 296.4 (looks worse; the VM has predicted losers). I would put it last or skip it.

Where B's instructions go on the full data (callgrind, 6.86 G in all): tokens() 2.04 G (hash ~12 per token, find ~16, ring/store ~10), scan closure ~1.3 G (~36 per token, of which the pending OR line 10 and the kept-count line 6), answer phase ~0.6 G of real work (apply ~0.55 G, docs() ~0.07 G; the memset/memcpy counts are inflated by valgrind's per-byte rep counting), scatter ~0.24 G. Since scanmin showed the scan does not follow instruction count on the board, I am not cutting scan instructions further without a board signal.

Waiting for your readings of answers / t1 / nopre / 8slot / qparse / steadymem before the next B step.

## 17:20 UTC honest status on the B redesign, and a proposal

Where I am:
- D1 lost on the board (0.2029 s), and answers read 0.1580 s (noise). In the same family, my VM checks rejected the 128-byte windows (+9%), and you saw tokloop lose (0.1629 s). The pattern: B pays for extra per-token work and per-token stores, and gains only from removing an unpredictable branch without adding work (the buckets). Instruction cuts alone did nothing (scanmin).
- I have not found a design with good odds of 0.110 s by 10:00. Every way I can see of removing the remaining per-token branches (window exit) or passes adds per-token work or stores that the board has punished. I am not going to send more large rewrites on hope.

What I have that should hold up:
1. cloud-b-redesign-steadymem (23a681f): VM peak 41.8-42.3 MiB on every run (head 45.1-50.6). It is measured, not a guess, and it removes the up-to-0.9-point memory loss on bad runs. +4 lines. I would put this on the board first and read the MEMORY column over 2-3 runs.
2. cloud-b-redesign-qparse (27bb471): about 2-3 ms less serial time on the VM. Probably too small to see in one reading against +-5% noise; it only shows with several readings or bundled.
3. The single-word experiments (t1, nopre, 8slot, spread; nta looks worse on the VM) are cheap questions for whenever a slot is free.

Proposal: a bundle branch = steadymem + qparse (they do not touch the same lines), read 2-3 times for time and memory. If the bundle holds, it is the B candidate. Then, unless you or the user want me to keep probing B, I switch back to C's record parser (instruction-bound, where the board has followed instruction cuts) or A's first-touch costs, whichever you are not doing. Tell me which. I will build the bundle now, since it costs nothing to have ready.

## 17:17 UTC cloud-b-redesign-bundle (8e86932): steadymem + qparse, ready for 2-3 readings

- Base 208bd01; merges cloud-b-redesign-steadymem (23a681f) and cloud-b-redesign-qparse (now 8c44638: I also drop the 1.3 MB vector of query hashes right after the inserts; without that the bundle peaked 1.2 MiB higher, 43.1-43.4).
- Checked: real Dockerfile, `docker build --network=none`, board flags: seeds 1 (x3) / 2 / 3 / 7 match; tiny dcdad7cedfc0 (reference), sample eeeec1e9319d (spec); phrase-heavy fuzz 0 differences in 40 cases. Lines 579 -> 603 (rustfmt count, +24).
- VM (cgroup v1 peak, /data warmed outside, 8 runs): 42.0-42.3 MiB every run (head 46.0-51.0 MiB in the same session). VM wall, 21 interleaved runs: head 302.3, bundle 295.9 ms median (no slowdown; a 2 % difference is noise here).
- What to read on the board: the M column first (expect about 42-44 MiB on every run, not 43-54), then T against 0.152-0.164 s. Board not measured.
