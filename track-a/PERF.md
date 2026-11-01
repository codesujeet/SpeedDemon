# Track A: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset,
with the output hash checked against the published SHA-256. Local runs swing
30-50%, so each row is an interleaved before -> after median over the stated
rounds, and rows are only comparable within themselves. The laptop is
slower than the scoring box. On the live board the first version (steps 1-5,
commit `0bbd5d0`) measured 0.122 s, 79 MB, 537 lines, 57.68 points; the current
version (`36f4296`) measures **0.0274 s, 21.8 MiB, 508 lines, 79.52 points** (best
run so far: 79.88 points, run `run-39a9b4e1b49e`). The **Run ID** column stays
*pending* where a step never had its own board run; the "Board runs" table at the
end lists every run read from the public board feed, with its commit. **Points**
is only filled from board numbers. Everything that was tried, including the
failures, is in [`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Morsel-parallel scan (2 MiB morsels from an atomic cursor, each snapped to a line start), fused space-or-LF SWAR scan per field, per-thread FxHash maps, exact `select_nth_unstable` percentiles; input read into memory | pending | about 1.1 | 332 | pending |
| 2 | `mmap` the input instead of reading it (the host pre-warms `/data`, so mapped pages are not charged to the run) | pending | 1.13 -> 1.05 (5 rounds) | 332 -> 104 | pending |
| 3 | Group keys are slices of the mapped input (no allocation per line); merge, percentiles and formatting done in parallel over blocks of sorted keys | pending | 0.70 -> 0.42 (7 rounds) | 104 -> 83 | pending |
| 4 | Newline search at morsel boundaries is a plain `position` (one call per 2 MiB), deleting a 24-line SWAR function | pending | 0.33 -> 0.30 (7 rounds) | 80 | pending |
| 5 | Merge by hour: the workers' groups are collected as (key, data) pairs and ordered by hour alone (an integer sort); each hour is then sorted by endpoint and merged by one worker, so no map is searched and no pointer-keyed sort of duplicated keys runs serially | board commit `0bbd5d0` (run ID not recorded) | 0.28 -> 0.21 (9 rounds); merge phase 150 -> 50 ms | 78 | 57.68 (board) |
| 6 | No per-worker hash maps of growing vectors. Each 2 MiB morsel is parsed into a small array of packed `u64` lines (hour slot, endpoint number, error flag, latency), which is sorted into one run of latencies per (hour, endpoint). The final merge takes an hour at a time from the few morsels that touch it | pending | 0.194 -> 0.150 (11 rounds) | 77 -> 53 | pending |
| 7 | Each hour's output block is written to the file as soon as all earlier hours are, instead of all blocks being held and then written | pending | 0.191 -> 0.177 (11 rounds) | 53 -> 47 | pending |
| 8 | The morsel's lines are placed by a counting sort over a dense (hour slot x endpoint) table instead of a comparison sort | pending | 0.181 -> 0.160 (11 rounds) | 47 | pending |
| 9 | Line parsing uses the spec's fixed layout: a 27-byte timestamp and a 3-digit status are sliced by offset, so three delimiter scans per line instead of five (the parser no longer tolerates blank or malformed lines, which the spec rules out) | pending | 0.155 -> 0.130 (13 rounds) | 46 | pending |
| 10 | One 64-byte AVX2 window per line finds the method, endpoint, status and line-feed delimiters at once (byte-wise path kept for lines near the end of the data), replacing three byte-wise scans | pending | 0.155 -> 0.134 (15 interleaved rounds) | 45 | pending |
| 11 | No per-morsel results and no merge across morsels. Each worker appends one packed `u64` per line (endpoint number, error flag, latency) to its own list for the line's hour, with a worker-lifetime endpoint table backed by one shared table of numbers; after the scan each hour is counted per endpoint, placed in one run per endpoint, and visited in endpoint-byte order (sorted once). Latency parsed with SWAR, hour found by comparing 13 timestamp bytes, per-hour lists start with room for 8192 lines, the process skips freeing them | pending | 1 CPU (`--cpuset-cpus=2`): 0.42-0.57 -> 0.28 s wall/CPU (3 interleaved runs); 12 threads: no clear change on this loaded laptop (A/B medians 188 -> 185 ms, harness replica 0.132 -> 0.152 s, within the +-20% noise of four agents sharing the machine) | 46 -> 45 | pending |
| 12 | Branchless endpoint check: four masked 32-byte compares replace the length-dependent `bcmp` | pending | 1 CPU, min of 8 interleaved runs: scan 189 -> 168 ms, user+sys 259 -> 235 ms | 45 | pending |
| 13 | Lines are 4 bytes: `error << 31, endpoint << 16, latency in the low 16 bits`, with latencies of 65535 or more stored as 65535 and their real value in a side list per hour list (endpoint numbers must stay below 32,768; the spec's catalogue has 1,842) | pending | 1 CPU: scan 188 -> 187 ms, phase 2 78 -> 71 ms (min of 8); peak 45 -> 38.8 MB | 45 -> 39 | pending |
| 14 | Alternative design (branch agent-a2), rewritten from first principles: one pass, no atomics in the hot loop except the chunk cursor; each stretch of up to ~1000 delimiters is listed as offsets first (AVX2 mask, 8 offsets written unconditionally), then lines are read five offsets at a time so no line is parsed to find the next; `u32` lines (endpoint 11 bits, error, 20-bit latency, longer values followed by a whole word); per-worker endpoint table with a private copy of each name and a two-compare (first/last 32 bytes) exact check; per-worker arena in scan order with a run list per hour change; barrier, then one hour per task | pending | 1 CPU (`--cpuset-cpus=2`), min of 7 CPU user+sys: 285 -> 251 ms (speed-demon HEAD vs this); scan alone, best of 25 in-process repeats: 102 -> 78 ms | 36 -> 25 | pending |
| 15 | Groups of up to 32 latencies are fully sorted instead of selected three times | pending | 1 CPU min CPU 251 -> 235 ms (7-9 runs) | 25 | pending |
| 16 | Each hour gives the pages of the arenas that hold only its lines back to the system (`madvise(MADV_DONTNEED)`) as soon as they are copied into the hour's placement buffer, so the output file's page cache is paid for by input freed | pending | 12 CPUs: 7 runs, CPU unchanged within noise; 1 CPU min CPU 193 -> 171 ms (7 interleaved runs) | cgroup `memory.peak` max of 5-7 runs: 12 CPUs 33.7 -> 24.6 MiB; 1 CPU 24.6 -> 15.6 MiB | pending |
| 17 | Memory per worker: endpoint names are stored back to back (no 64 padding bytes each; the compares already mask what lies outside a name) and the endpoint table has 8,192 slots instead of 16,384 (1,842 names, load 22%) | pending | 1 CPU CPU min (7 interleaved runs) 174 -> 168 ms (noise level); 12 CPUs: no change in CPU | cgroup anon at the end of the scan with 12 workers 17.05 -> 15.75 MiB (step 16 -> here), `memory.peak` max of 7 runs 25.1 -> 24.1 | pending |
| 18 | The endpoint numbers are handed out by a lock-free table (compare-and-swap on a slot, same hash as the private tables) instead of a `Mutex<HashMap>`: every worker takes each of the 1,842 names once, 22,104 lock acquisitions in the first few chunks, all workers waiting for each other (+23 lines after the harness formatter) | pending | 12 CPUs, TSC cycles per worker for the scan (instrumented copies, 4 interleaved A/B rounds of 3-4 runs): lock wait was 18-20 M of 54 M cycles per worker (a third of the scan, about 7 ms) on a warm run; mean per-worker scan 76-94 M -> 33-69 M cycles on the loaded laptop; wall 57-80 -> 38-71 ms | 24 | pending |
| 19 | Each worker gives the page table entries of a twelfth of the mapped input back to the system (`madvise(MADV_DONTNEED)`; pages are read again from the page cache if used) when its scan is done, so the exit does not take apart 58,000 mapped pages on one core | pending | 12 CPUs, `munmap` of the input after the run, TSC: 9-19 M cycles (about 4-7 ms, one core) -> 1 M after the drop; interleaved A/B, 30 runs each: wall median about 55 -> 46 ms, min 36 -> 32 ms (loaded laptop under WSL2, whose page and IPI costs are above the board's); +16 lines after the harness formatter | 23 | pending |
| 20 | The writer thread ends the process as soon as the last block is written, instead of returning through the thread scope (which joins 12 threads, unmaps their stacks and frees their buffers first; +7 lines) | pending | 12 CPUs, 25 interleaved A/B runs with TSC marks (loaded laptop): exec-to-exit median 96 -> 93 ms, time from last worker done to the end of `process` 5.2 -> 4.5 ms; inside the noise of one run, kept because it removes work and costs nothing | 23 | pending |
| 21 | `mallopt(M_MMAP_THRESHOLD, 32 KiB)` before the threads start: the hour buffers (placement buffer, output block) are mapped on their own and unmapped when freed, instead of staying in 12 thread arenas for the rest of the run (at the end of the output phase 8.5 MiB of anonymous memory was still resident in them; +4 lines) | pending | 12 CPUs, cgroup CPU user+system, median of 12 interleaved runs: 566 -> 571 ms (user 378 -> 361 ms, system 189 -> 210 ms): no more work; wall on the loaded laptop is within noise | `memory.peak` median 23.0 -> 20.4 MiB (max 24.1 -> 20.6; 12 runs, tmpfs /out) | pending |

Steps 6-9 together, measured the way the board measures (5 runs, mean of the
middle three, highest memory peak): **0.093 s and 47 MB, against 0.214 s and
78 MB for the committed version** on the same laptop. Source size grows from
546 to 607 code lines (the harness's own count).

## What the time is now

Full data, 3.1 M lines, 239 MB (Linux container, local): on one core the whole
program takes about 0.28 s (scan about 0.2 s, about 58 ns per line; count,
select and format about 0.06 s). Peak memory is 44-47 MB: about 12 MB of
packed lines (4 bytes each since step 13), the endpoint tables and the output file's page
cache (10 MB), plus stacks.

## Tried and dropped


* **Round 3, `pread` of each chunk into a per-worker 1 MiB buffer instead of the shared mapping** (no page faults on the input, names copied out of the buffer): wall min over 7-9 runs, mmap vs pread, on distinct cores: 1 core 161 vs 154 ms, 2 cores 94 vs 96, 4 cores 73 vs 66, 4 cores x 2 threads 73 vs 67; peak memory 24-32 MB vs 25-38 MB. The scan stalls at the same point either way, so page-fault contention on the mapping is not what limits it; reverted (the gain at 4 cores is inside the noise and RSS grows).
* **What the 12-thread scan scaling looks like** (temporary timestamps, local): 1 core scan 124 ms and hour phase 60 ms; 4 cores 60 and 25; 8 threads 55 and 14-20. Windows showed 35-50% processor use from other work during every run, and cpus 0-5 of this machine are 3 physical cores (SMT pairs 0-1, 2-3, 4-5), so local scaling numbers below ideal are not evidence of a shared-resource bottleneck in the program.
* Not built: overlapping the hour phase with the scan. Hours finish in file order only approximately, the arenas would need published lengths and a per-hour pending-chunk count (about 60 more source lines), and the local noise would not let a 5-10 ms effect be measured.

* **Round 2 profile (12 threads, local, temporary per-thread timestamps):** thread start 0.5-4 ms, scan 45-55 ms on every thread (ends within 1 ms of each other, so tail balance is fine), barrier plus `make_plan` 1-2 ms, hour phase 15-24 ms, total 70 ms. Scan wall time scales poorly with threads (1 core 190 ms, 2: 114, 4: 75, 6: 66, 12: 75), not with the per-line cost (about 60 ms on one core in-process). A pre-pass that touches every input page (5-9 ms) or the arena pages did not shorten the scan after it.
* **256 KiB chunks instead of 1 MiB**: A/B medians 77 vs 78 ms (15 rounds), no change; reverted.
* **`mallopt` (no mmap for big buffers, no trim) to stop page-fault churn in the hour phase**: 12-thread medians 70 -> 76 ms, peak 33 -> 43 MB; reverted.
* **One 32-byte compare for names up to 32 bytes**: scan best-of-25 60 -> 61 ms, no change; reverted.
* Not tried: pinning workers to physical cores (the board pins the run to 6 P-cores already; the extra source lines are not justified without a measurable gain).

* **Listing delimiters in a separate pass only (no parse)**: 20 ms for 3.1 M lines on one core, so listing is cheap. By ablation (one core, best of 25 in-process repeats) the rest of the 100 ms scan is: error flag, latency and hour parse 26 ms, endpoint table 43 ms, arena writes 12 ms.
* **MAP_POPULATE and touching all pages first**: no gain in CPU time (page faults are about 25% of the 1-core CPU, but pre-faulting only moves them).
* Comparing against the mapped first occurrence instead of a private copy of each name: the copy was kept, 5 to 10 ms of scan faster (names sit together).
* **Cascading selection** (find p50, then search only the part above it for
  p95, then above that for p99). Correct and tested, but 463 vs 501 ms over
  11 interleaved rounds, so no gain; reverted. The merge phase is not
  dominated by selection.
* **An approximate histogram for the percentiles.** Not an option: the
  output is checked by an exact hash and the rank is nearest-rank.
* **Interning endpoints per worker** (a small hot table for the endpoint,
  then a cheap `(hour, number)` key for the group) while still using big
  per-worker maps: 207 vs 191 ms median over 11 interleaved rounds, no gain.
  Step 6 uses a per-morsel table instead and does not keep the maps.
* **Parsing a batch of 256 lines before the map updates**: 188 -> 179 ms
  median over 11 interleaved rounds, inside the noise; reverted.
* **Morsel size** (1 MiB, 2 MiB, 4 MiB with the step 9 parser): median 131,
  125 and 137 ms, peak 53, 45 and 44 MB. 2 MiB stays.

* **Memory, steps 16-17** (12 CPUs, interleaved runs of 9, cgroup `memory.peak`; median / max): start of this work 33.0 / 33.3 MiB, now 23.3 / 24.1 MiB, CPU unchanged. The peak is now just after the scan: about 12.4 MiB of lines, 1.4 MiB of kernel memory, 2 MiB of tables and stores, then 12 threads allocating their hour buffers (about 0.4 MiB each) before their releases catch up. Pages shared by two hours' slices of one arena are not released (about 700 slices, 3 MiB).
* **Dropped: writing each output block to disk and evicting it from the page cache** (`sync_file_range` + `posix_fadvise(DONTNEED)`): the output's 10 MiB of cache is gone at the end, but the peak is at the start of the output phase, so `memory.peak` only moved 24.6 -> 23.0 MiB; not kept.
* **Dropped: 2 and 4 MiB chunks** (fewer, larger arena slices, so fewer unreleased shared pages): peak median 23.5 -> 22.9 MiB, CPU min 385 -> 408 ms; not kept.
* **Phase 2 by sub-phase** (1 CPU, TSC cycles, best of 3): count 19 M, place 10 M, release 3 M, percentiles 54 M, format 27 M (about 45 ms of 235). The percentile sorts are half of it.

## Where the unit tests live

The unit tests are in `tests/track_a.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_a.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 607 to 434.

* **`madvise(MADV_DONTNEED)` on every morsel after it is parsed**, to drop
  the mapped input from the process: peak 46 vs 45 MB, no effect. The
  pre-warmed `/data` pages are not charged to the run in the first place (the
  run scans a 239 MB mapped file and reports about 44 MiB).

* **Profile-guided build** (a `.profdata` recorded on the public dataset, same
  rustc and LLVM): board 0.0645 s against 0.0655 s, K +0.4%, inside the noise;
  reverted.

## Caveats

* Steps 6-9 have not run on the scoring box. Expect the board's T to differ.
* The step 9 parser and the step 8 table rely on what the spec guarantees:
  well-formed lines, at most 72 hours and about 1,800 endpoints. A morsel with
  more than 2,048 distinct hours stops with an error message.
* Source size is the weak axis: the reference is 56 code lines and this is
  about 600. Every optimisation above had to be worth its lines.

* **Step 11 follow-ups that did nothing measurable.** `madvise(MADV_POPULATE_READ)` on every morsel before the scan (11 interleaved rounds: 115 vs 111 ms median, noise; removed to save source lines). Thread count and morsel size sweeps (5 interleaved rounds, 12 threads vs 8 vs 6; 256 KiB vs 1 MiB vs 4 MiB): medians 131/118/106 ms for 12/8/6 threads, but the machine was shared with other builds, so no setting was adopted; morsel size showed no trend. A bare 12-thread pass over the whole mapped input (touching every page, or summing every 8 bytes) takes 4 and 7-10 ms, so the input mapping is not the bottleneck: the scan is compute-bound at about 58 ns per line on one core.
* **Local measurement caveat for step 11.** With other agents building and benchmarking on the same laptop, the 12-thread wall time cannot resolve a 1.5x change in work (CPU time with 12 threads was 0.9 s user + 0.3-0.7 s sys for the old design, 0.5 s + 0.25 s for the new one, but wall time did not follow). Pinning to one core (`--cpuset-cpus=2`) removes the scheduling noise and shows the real work: 0.42-0.57 s against 0.28 s.

* **Where the wall time goes after step 19 (TSC timestamps per worker, 12 CPUs, laptop under WSL2, so ratios rather than milliseconds).** Thread start: the last worker begins 2-4 ms after `main` on this laptop (serial `spawn`); scan: all workers end within 2-4 M cycles of each other; barrier and plan: 2-3 M cycles of waiting plus a serial `make_plan` of about 2 M cycles, of which the sort of the 1,842 names by their bytes is 1.5 M (names are scattered over the mapped input, so the compares miss the cache and, after step 19, fault again) and the hour map 0.4 M; hour phase: the first and the last worker to finish are 6-13 M cycles apart; after the last worker, 2-9 M cycles until `process` returns (the writer draining the last blocks, joins). Not built, each worth about 0.5 ms at most: sorting the names speculatively on the first worker that reaches the barrier (it waits 2-4 M cycles for the others), splitting the last hours between workers, a tree of `spawn`s.
* **Dropping the input's page table entries after the barrier, by every worker but the first, while worker 0 makes the plan** (so that the plan's name reads are not faulted again): the plan took 4-7 M cycles instead of 2.4-3 because the 11 `madvise` calls' TLB shootdowns interrupt it; A/B of 12 runs total time 124-203 vs 110-157 M cycles; not kept.
* **Dropping each chunk's entries right after it is scanned** (239 `madvise` calls instead of 12): total time no better than the 12-call version on 3 interleaved rounds, more shootdowns; not kept.
* **Branch-free output formatting** (SWAR digits, 64-byte name copy; callgrind 999.8 M -> 974.0 M instructions, 2.90 M -> 2.13 M mispredicts): board 0.0314 s against 0.0307 s with 12 more lines, so the formatting is not on the critical path; reverted.

<!-- board-runs:begin -->
## Board runs

Each row is one scored run of the named commit on the live board, copied from the public board feed (`board.json`). The feed keeps only each track's latest and best run, so runs before these cannot be listed; earlier steps in the table above stay local measurements. Board timing varies by about 1-2 ms between runs of identical code: every push re-runs all four tracks, so a row whose change names another track measured this track's unchanged code and shows that noise.

| Commit | Change | Run ID | T (s) | M (MiB) | S | Points |
|---|---|---|---|---|---|---|
| `32f54ce` | Track A: the writer thread exits the process after the last block (agent-a4 d262378) | `run-39a9b4e1b49e` | 0.0259 | 23.1 | 508 | 79.88 |
| `b67a992` | Track A: link the C runtime statically (skips dynamic loading at start-up) | `run-15fd92907080` | 0.0281 | 23.4 | 508 | 78.92 |
| `f299e99` | Track B: parse the queries in one SIMD token pass with leaner term interning (agent-b 8537 | `run-f4e75da5bc9d` | 0.0271 | 27.7 | 508 | 78.57 |
| `33b8455` | Track B: postings fill uses the term offset as its own cursor (agent-b d49c673) | `run-aba44205a125` | 0.0289 | 26.8 | 508 | 78.02 |
| `b208288` | Track A: workers write their own hour block at its final offset; no writer thread (agent-a | `run-f6f7bcc42a1c` | 0.0254 | 27.4 | 516 | 79.29 |
| `cd4c35e` | Revert "Track A: workers write their own hour block at its final offset; no writer thread  | `run-5156e7ee3367` | 0.0272 | 23.7 | 508 | 79.22 |
| `e804869` | PERF.md: add a Board runs table with real run IDs from the public feed to every track | `run-35c07889b928` | 0.0278 | 24.7 | 508 | 78.81 |
| `36f4296` | Tracks A and C: profile-guided optimization from a committed profile (no source or Dockerf | `run-bac39780b486` | 0.0274 | 21.8 | 508 | 79.52 |
| `158f18f` | Track B: 32-byte token mask (agent-b 0054521); Track A PERF.md header with current board n | `run-7cf129aecf02` | 0.0292 | 27.0 | 508 | 77.90 |
| `49548fa` | Track B: merge and render in parallel from per-range logs (agent-b a34f4cf) | `run-f1548a29e949` | 0.0302 | 30.1 | 508 | 77.04 |
| `3286475` | Track C: 111-bit name fingerprint (second independent hash compared on every tag match), d | `run-fdef798466b5` | 0.0314 | 27.7 | 508 | 76.97 |
| `280509c` | Track B: vector-compare term lookup, CRC32C hash, fused dedupe (agent-b) | `run-861ceb90812d` | 0.0316 | 22.9 | 508 | 77.73 |
| `892d831` | Track C: regenerate the PGO profile for the 111-bit fingerprint source | `run-44baf3d9c9ef` | 0.0287 | 26.9 | 508 | 78.08 |
| `2ca5698` | Track C: back to the 36f4296 source and profile (the wider fingerprint cost 3-5 ms on the  | `run-021bc02d8c03` | 0.0260 | 26.2 | 508 | 79.29 |
| `b1497a5` | Track B: 2 MB pieces (agent-b c51c3b0) | `run-474389647c4a` | 0.0282 | 27.9 | 508 | 78.11 |
| `f00df83` | Revert Track B 2 MB pieces: T -17 ms but M +12 MiB, 65.82 vs 65.94 on the board | `run-b9b556122cc2` | 0.0285 | 24.8 | 508 | 78.52 |
| `b18c263` | PERF: board run IDs through f00df83 (16 scored runs per track) | `run-6702f437271f` | 0.0270 | 29.2 | 508 | 78.42 |
| `44a450f` | Track A: mallopt M_MMAP_THRESHOLD 32 KiB, memory.peak 23.0 -> 20.4 MiB locally (agent-a4 c | `run-d32c96fcb132` | 0.0293 | 22.3 | 510 | 78.66 |
| `b7bb4d3` | Track A: PGO profile regenerated for the mallopt source (agent-a4 90f380c) | `run-b3e9f1f07f68` | 0.0308 | 25.9 | 510 | 77.49 |
| `4c454fe` | Track A: three-phase port with exact output sizes, every thread formats into the mapped ou | `run-6530313fd395` | 0.0299 | 18.5 | 621 | 78.62 |
| `5476630` | Re-run board on the Track A port (noise check) | `run-fed7c25d2dd7` | 0.0312 | 18.6 | 621 | 78.14 |
| `6c9bc1e` | Tracks B and D: user-drafted Rust solutions, reviewed (alt-bd c90b4a8): B 603 lines, D 509 | `run-fc26d4ecbd5c` | 0.0296 | 24.6 | 510 | 78.13 |
| `f47d313` | Track C: user-drafted C++ design ported to Rust, CRLF chunk cut, 440 lines (agent-c3 7827d | `run-86a31ad638b8` | 0.0297 | 20.4 | 510 | 78.90 |
| `c4832fa` | Re-run board on the Track C port (noise check) | `run-8d7db8d67590` | 0.0305 | 26.0 | 510 | 77.58 |
| `4f9e4bd` | Track D: PAGE 512 -> 256 (experiment) | `run-9a5f54804ca5` | 0.0286 | 22.4 | 510 | 78.89 |
| `7a3ceb9` | Track D: BLOB 512 -> 1024 (experiment) | `run-da00b4b0506a` | 0.0274 | 20.9 | 510 | 79.69 |
| `0b9abd0` | Track D: BLOB back to 512, CHUNK 1024 -> 4096 (experiment) | `run-8cb139320e99` | 0.0302 | 20.6 | 510 | 78.68 |
| `1a4d82e` | Track D: PAGE 256 -> 128 (experiment) | `run-f1bf9309754d` | 0.0276 | 27.7 | 510 | 78.37 |
| `a2fa18e` | Track B: persistent thread pool instead of 12 fresh threads per phase (experiment) | `run-60a438196df7` | 0.0284 | 26.5 | 510 | 78.28 |
| `b573d30` | Track C: C++ (gcc 15, user-drafted design: pclmul quote mask, dense-id name table); Rust v | `run-2b86ebff8137` | 0.0279 | 22.3 | 510 | 79.20 |
| `17ce1b0` | Track C: static link and _exit(0) after the output (experiment) | `run-9e8eecf0dcd9` | 0.0267 | 25.9 | 510 | 79.01 |
| `9e9667f` | Re-run board on static+_exit Track C (noise check) | `run-a2dc253315df` | 0.0352 | 24.4 | 510 | 76.29 |
| `308cbff` | Track A: C++ (gcc 15, static, _exit after the output); Rust version stays at tag best-19 | `run-ccae47851733` | 0.0277 | 18.4 | 310 | 81.71 |
| `4d1d805` | Tracks B and D: exit right after the output so the big arrays are not freed one by one | `run-f676f02c28ef` | 0.0277 | 18.4 | 310 | 81.72 |
| `d68c42e` | Untrack Cargo.lock files added by mistake | `run-675fa046c3d5` | 0.0277 | 18.6 | 310 | 81.68 |
| `23561b4` | Track C: LAG 16 -> 32 (experiment) | `run-7b877d515593` | 0.0283 | 18.9 | 310 | 81.38 |
| `a302a61` | Track C: LAG back to 16 (32 scored 75.61, no gain) | `run-60f2532bf9bd` | 0.0299 | 18.7 | 310 | 80.84 |
| `70fee14` | Track A: 16 pieces per thread (experiment) | `run-020e3f03e219` | 0.0283 | 19.4 | 310 | 81.27 |
| `8d367d9` | Track C: new user-drafted C++ (64-bit hash key, field-by-field parse, distinct counts from | `run-a0ac1d68073a` | 0.0281 | 18.4 | 310 | 81.56 |
| `2ce9b21` | Track B: new user-drafted Rust (token id stream, CRC hash, bitmap for big lists, persisten | `run-b87352798d2d` | 0.0333 | 19.6 | 310 | 79.46 |
| `a46cccc` | Track A: new user-drafted C++ (branch-free width, 64-byte name compare, latency buffer siz | `run-08c55254bc3e` | 0.0270 | 18.3 | 329 | 81.84 |
| `428ae7e` | Track A: new file without the sizing pass over all records (experiment) | `run-e4130a512def` | 0.0267 | 19.1 | 321 | 81.84 |
| `8b2268b` | Write-ups for the second A, B and C drafts, with board run IDs | `run-6221fb7f6371` | 0.0289 | 20.2 | 321 | 80.76 |
| `f34afab` | Track B: BIG 768 -> 1536 (experiment) | `run-9b0793be0739` | 0.0264 | 18.6 | 321 | 82.10 |
| `576267e` | Tracks A and C: each thread releases a consumed input piece's pages at once (from the new  | `run-442bba74c314` | 0.0220 | 18.5 | 326 | 84.02 |
| `a94d376` | Track B: each thread releases its share of docs.txt after the scan | `run-9934a3ba4185` | 0.0229 | 18.8 | 326 | 83.54 |
| `4dde5ce` | Track B: DIAGNOSTIC (answers computed twice) to measure the answer phase on the board; wil | `run-6b8564b1b12d` | 0.0229 | 18.9 | 326 | 83.51 |
| `ab00e1a` | Write-ups for the page-release changes and the answer-phase measurement; board run IDs | `run-1332852988a0` | 0.0235 | 22.0 | 326 | 82.56 |
| `bb07ac5` | Track A: 16 pieces per thread, now that pieces are released as they finish (experiment) | `run-9e02e3230cc7` | 0.0228 | 19.2 | 326 | 83.52 |
| `3cbb939` | Track A: prefetch the input 4 KB ahead in the scan loop | `run-9edb429d5aae` | 0.0214 | 18.4 | 327 | 84.34 |
| `fff546f` | Track B: prefetch the input 2 KB ahead in the token scan | `run-10d571793035` | 0.0215 | 18.7 | 327 | 84.23 |
| `ff9de8e` | PERF: B prefetch experiment (reverted) with its board run ID | `run-d15d1ee1519b` | 0.0218 | 18.9 | 327 | 84.03 |
| `c27bda3` | Track A: build with -O2 instead of -O3 (experiment, same source) | `run-dbba4c78aeb2` | 0.0234 | 18.7 | 327 | 83.34 |
| `8218980` | PERF: A -O2 note names its run ID | `run-7ece1db1a78d` | 0.0268 | 19.3 | 327 | 81.72 |
| `7f5e52c` | Track C: one #include <bits/stdc++.h> instead of twelve standard-library includes | `run-6326d94020ea` | 0.0221 | 18.5 | 316 | 84.10 |
| `79d68e7` | Track C: region_line also accepts a CRLF region file (fixes a crash in the previous commit | `run-c85b60f6b888` | 0.0218 | 18.3 | 306 | 84.39 |
| `5338276` | Log board runs for the line cuts (A S 306, C S 368) | `run-7396a8bb5432` | 0.0240 | 20.6 | 306 | 82.85 |
| `2a7cc01` | Track C: DIAGNOSTIC, record parse disabled to measure the floor (output is wrong; reverted | `run-65ee72074e87` | 0.0218 | 18.6 | 306 | 84.31 |
| `4ba1710` | Track C: DIAGNOSTIC, the mark is a plain load instead of the atomic OR (output is wrong; r | `run-09b013307362` | 0.0225 | 18.6 | 306 | 84.02 |
| `67464a6` | Track C: DIAGNOSTIC, no name-table lookup (id taken from the key bits); output wrong; reve | `run-63ba78fa6b18` | 0.0214 | 18.7 | 306 | 84.52 |
| `7496859` | Track C: DIAGNOSTIC, the parse only streams the input (one byte per cache line); output wr | `run-9718ba47d387` | 0.0216 | 19.2 | 306 | 84.32 |
| `0df6a6f` | Track C: decode numbers with pmaddubsw/pmaddwd instead of the scalar multiply chain | `run-1ea57f17feb5` | 0.0232 | 18.8 | 306 | 83.58 |
| `a39d4f6` | Track B: two exact line cuts (610 -> 600 formatted lines) | `run-40f714fa9d43` | 0.0243 | 18.3 | 306 | 83.21 |

<!-- board-runs:end -->

## Language switch to C++

The Rust version (0.026-0.029 s, 22-26 MiB, S 508-510) was replaced by a C++ build of the team's own ChatGPT-drafted design. Board run `run-ccae47851733` (commit `308cbff`): 0.0277 s, 19.3 MiB, S 310, 81.71 points; a second run `run-f676f02c28ef` (`4d1d805`) gave 0.0277 s, 19.3 MiB, S 310, 81.72. The gain is mostly the smaller source (S 310 against 508) and about 4 MiB less memory; time is about the same as the Rust version. A Rust port of this same design (commit `4c454fe`) had scored 78.62 and 78.14 and was reverted, so the C++ build, not only the design, matters. The static link and the `_exit(0)` after the output were added in the same change and were not measured separately on A.

## Second C++ version

Locally the new draft was 6-8% faster in interleaved runs (median 74-75 ms against 80 ms). I also tried its extra pass that counts the records of every hour before sizing the latency buffer; it adds 8 source lines and ran no faster, so it is not in the final file. Board: `run-08c55254bc3e` (`a46cccc`, with the counting pass) 0.0270 s, S 329, 81.84; `run-e4130a512def` (`428ae7e`, without it) 0.0267 s, S 321, 81.84. 16 work pieces per thread instead of 8 gave 81.27 (`70fee14`), so 8 stays.

## Releasing consumed input pages in the parse threads (`576267e`)

Each thread now drops a piece's input pages (`MADV_DONTNEED` on the page-aligned part of the piece) as soon as it has parsed it, instead of one thread unmapping all of the log's pages at process exit. On a private, unmodified file mapping this only drops pages, so a later read would fault them in again and cannot change the output. Idea taken from the team's newer draft (which also carried the record-counting pass I had already dropped). Locally there was no measurable difference (the laptop does not show the exit cost), but on the board: `run-442bba74c314` (`576267e`) 0.0220 s, 19.4 MiB, S 326, 84.02, against 0.0267 s before; the re-run `run-9934a3ba4185` (`a94d376`, unchanged A code) gave 0.0229 s, 83.54. 120 local runs matched the hash and outputs matched the Rust build on seeds 1, 2, 3 and 7.

### Prefetch the input 4 KB ahead

One line in the scan loop: `_mm_prefetch(p + 4096, _MM_HINT_T0)`. The hardware prefetcher stops at each 4 KB page boundary and the file's pages are scattered, so this reaches into the next page early. Idea from a separate research session that measured about 17% less CPU on its own 4-vCPU machine; on the board `run-9edb429d5aae` (`3cbb939`) gave 0.0214 s, 18.4 MiB, S 327, 84.34, against 0.0228 s for the same code without it (`run-9e02e3230cc7`). Single runs, so the gain is real but its size is uncertain. Hash matched on every local run.

### `-O2` instead of `-O3` (tried, reverted)

Same source, one build flag. Board `run-dbba4c78aeb2` (`c27bda3`): 0.0234 s, 18.7 MiB, 83.34, against 0.0214–0.0218 s with `-O3`. That board run was slow for every track (C on unchanged code went from 0.0296 s to 0.0382 s), so the size of the loss is not reliable; there was no sign of a gain, so `-O3` stays.
