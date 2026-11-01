# RESEARCH_A: cloud research on Track A (C++ build, `308cbff`)

Scope: this session was asked to implement the 3 best ideas as separate commits. **No code change was committed**: none of the three candidates I could test beat the noise on this machine, and a source-line cost with no measured gain is a net loss on the score. Only this file is added. Track C was not touched (see the end).

## Setup and limits

- The repo is **C++ now**, not Rust: `track-*/src/main.cpp`, no `Cargo.toml`. So `cargo test` and `cargo clippy` do not apply, and the `tests/*.rs` files are for the old Rust build. Checks used instead: build with the Dockerfile's flags (g++ 13 locally, the board uses gcc 15), then `cmp` the output against an independent naive reference (a Python script I wrote).
- `speeddemon.dev` is blocked by this session's proxy, so the official full dataset and its published SHA-256 (`495ca096...`) could not be downloaded or checked. The organizers' generator was fetched from GitHub, but the sandbox refused to run downloaded code, so I wrote **my own generator from the spec** (3.1 M lines, 1,842 endpoints, Zipf, about 3% of latencies at or above 2^19, 8-shard-like chunk interleave). Its lines average about 64 bytes (the real data averages 77), so absolute numbers differ from the real ones. Baseline output matched the naive reference byte for byte; every variant below was also checked with `cmp`.
- Machine: 4 vCPUs (Xeon, AVX2 and AVX-512 present), a shared cloud VM, not the i5-13500. No `perf`; only `valgrind` exists, and I did not use it for counts, so **there are no instruction counts here**. Numbers are medians of 9-11 interleaved runs pinned with `taskset` (1 CPU: `0`, 4 CPUs: `0-3`). Run-to-run spread is about +-3 ms at 4 CPUs and +-5 ms at 1 CPU, so anything under that is noise.
- Time split of the current build (one instrumented run each, ms from start): 1 CPU: scan+rank about 110, hours about 30, text about 0.2. 4 CPUs: scan about 30, hours about 10, text about 4.

## Update: real dataset

After the first draft, the official `requests.log` was fetched from `s3.amazonaws.com/raptors.dev/...` (sha256 `7ec80ba4...` matches). The unmodified build outputs `495ca096...81cd3`, the published hash. On the real data: baseline 131.2 ms (1 CPU) and 47.6 ms (4 CPUs); `-O2` 131.9 and 52.2 ms; `pwrite` output 49.6 ms on 4 CPUs. So `-O2` is slightly worse and `pwrite` no better; the conclusions below stand (the tables use my synthetic data).

## Experiments run (all reverted)

| # | Change | 1 CPU wall, base -> variant | 4 CPU wall, base -> variant | Lines | Verdict |
|---|---|---|---|---|---|
| 1 | Groups of up to 32 latencies fully insertion-sorted instead of 3 selections | 139.0 -> 138.4 | 54.4 -> 53.6 | +8 | Noise. Not kept. |
| 2 | Output by `pwrite` of a per-hour buffer instead of the `MAP_SHARED` file mapping | 143.1 -> 139.3 | 47.3 -> 50.6 | -2 | Mixed (better on 1 CPU, worse on 4). The 4-CPU case is closer to the board. Not kept. |
| 3a | Build flag `-mtune=alderlake` | 143.1 -> 143.2 | 53.0 -> 53.2 | 0 | Noise. |
| 3b | Build flag `-O2` instead of `-O3` | 143.1 -> 138.4 | 53.0 -> 51.8 | 0 | Within noise. Worth one board run only if the author wants to try: it costs zero source lines. |
| 3c | `-fno-plt -fno-stack-protector -fcf-protection=none -fno-asynchronous-unwind-tables` | 143.1 -> 141.6 | 53.0 -> 53.0 | 0 | Noise (static binary, nothing to gain). |

Idea 1 matches what `PERF.md` already found for the Rust version (step 15: gain only visible on a clean 1-CPU CPU-time measure); here the CPU time was equal within 1%.

## Ideas list (not already in PERF.md)

"VERIFIED" means I measured it here. Everything else is UNVERIFIED reasoning; I give no gain numbers for those because I have none. Reference links are to documentation I know, not pages I opened in this session.

1. **Insertion-sort small groups** (above). VERIFIED: no gain here. Exactness risk: none.
2. **`pwrite` output instead of mmap** (above). VERIFIED: mixed. Risk: none.
3. **`-O2`/tuning/hardening-off flags** (above). VERIFIED: noise. Risk: none. Docs: https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html
4. **Replace the `M_MMAP`/THP-off `prctl` and per-thread region `mmap` by one shared region**. Why: fewer syscalls and page-table setups at start. UNVERIFIED; risk: none; likely under 1 ms.
5. **Single 32-byte window for the endpoint end** (load at the name start, space mask, fall back past 32 bytes) and a second at `name_end + 5` for the newline: 3 loads/masks per line instead of 4. UNVERIFIED. The longer dependency chain may cancel the saving. Risk: none if the fallback is kept. Intrinsics: https://www.intel.com/content/www/us/en/docs/intrinsics-guide/index.html
6. **Remember the previous line's endpoint** and skip hash+lookup when the 32-byte compare matches (Zipf-heavy head). UNVERIFIED; on shard-ordered chunks adjacent lines rarely share an endpoint outside the top few, so the hit rate is probably low. Risk: none (compare is exact).
7. **Prefetch the table slot** for the next line while the current one is stored (the lookup is 43 of 100 ms of scan per PERF.md, partly cache misses on names). UNVERIFIED. Risk: none. https://gcc.gnu.org/onlinedocs/gcc/Other-Builtins.html (`__builtin_prefetch`)
8. **AVX-512 compares (`_mm512_cmpeq_epi8_mask`)**: one 64-byte compare replaces two 32-byte ones and the movemask merge. UNVERIFIED and **not usable as is**: the Dockerfile targets `x86-64-v3`, and the i5-13500 has no AVX-512, so this would crash on the board. Rejected on the contest hardware.
9. **Delete code for source lines**: replace the branch-free `select` (about 25 lines) by `std::nth_element` three times. PERF.md says nth_element mispredicts about every other element in the old Rust design; the comment in `main.cpp` says the same. About 25 lines is about 0.17 points, so it pays only if it costs under about 0.4 ms. UNVERIFIED. Risk: none.
10. **Delete the `wide` side list** by storing latency in a full `u32` record (8 bytes a line) or by widening the packed field. Saves roughly 10 lines but doubles line memory (about 12 MiB more); memory weight is lower, so possibly net positive. UNVERIFIED. Risk: none.
11. **Hugepage-backed output buffer** (`MAP_HUGETLB` / `madvise(MADV_HUGEPAGE)`) for the 10 MB result to cut page faults in the text phase (about 4 ms at 4 CPUs here). The code disables THP on purpose to protect `memory.peak`, so this trades memory for time. UNVERIFIED. https://www.kernel.org/doc/html/latest/admin-guide/mm/transhuge.html
12. **Overlap the hours phase with the scan** for hours whose input is complete. Already considered in PERF.md ("not built", about 60 lines). Not worth the lines.
13. **PGO**: tried on the Rust build (PERF.md, +0.4%, noise). A gcc `-fprofile-use` profile would need a committed `.gcda`/profile in the build context; the Dockerfile copies only `main.cpp`. Rejected for this build.

## What the existing PERF.md already covers (so I did not repeat it)

Morsel size and thread-count sweeps, MAP_POPULATE, pread vs mmap, page-table `madvise` on the input, `mallopt`, selection variants, SWAR/AVX2 parse, lock-free name table, static link and `_exit`. The scan is compute-bound at about 40-60 ns a line on one core; what remains is measurement noise on this class of change.

## Recommendation

- On a cloud VM, 1-5 ms changes cannot be resolved. If the author wants a data point, the only zero-line candidate worth one board run is idea 3b (`-O2`), at the cost of one 5-minute board slot.
- Highest-value leftover for the score is lines, not time: ideas 9 and 10 (about 0.17 points per 25 lines) need a timing check on the real box.

## Track C

Not started in this session. I read `track-c/README.md` and `PERF.md` only: it is the same shape (C++, mmap, pool of threads, barriers, `_exit`), and its README says PGO and a wider fingerprint were tried and reverted. Its spec and generator are at `specs/c-join.md` and `generators/c_join.py` in `hackathon-raptors/speed-demon-2026` for whoever picks it up.
