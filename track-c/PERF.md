# Track C: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset,
with the output hash checked against the published SHA-256. Local runs swing
30-50%, so each row is a before -> after median from interleaved or repeated
runs, and rows are only comparable within themselves. The laptop is slower
than the scoring box: the version of steps 1-4 (commit `db4cc76`) measured
**0.135 s, 141 MiB, 543 lines, K 9.75, 49.44 points on the live board**
(run `run-cf24ab5675f5`) and about 0.21 s, 141 MB on this laptop. The **Run
ID** column stays *pending* for steps without their own board run, and
**Points** is only filled from board numbers. Steps 5-11 have not been on the
board yet. Everything that was tried, including the failures, is in
[`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Quote-parity chunking: count quotes per chunk in parallel, prefix-sum, snap each boundary to the first LF outside quotes; each chunk parsed by its own worker straight off the input bytes; dense array for `region_id -> country` | pending | | | pending |
| 2 | Exact `names_distinct`: each country is owned by one worker, parse workers drop `(country, name slice)` into the owner's bucket, owners count distinct slices | pending | about 0.60 -> 0.30 | | pending |
| 3 | `mmap` the inputs | pending | 0.77 -> 0.52 (5 rounds) | 315 -> 119 | pending |
| 4 | Exact distinct names counted by sorting each group's `(hash, name)` pairs and comparing bytes only inside runs of equal hashes, instead of one big hash set per owner | `run-cf24ab5675f5` (commit `db4cc76`) | parse+aggregate phase 234 -> 170 ms (9 interleaved rounds) | 123 -> 141 | 49.44 (board) |
| 5 | A record leaves one `u64` in its country's list (top 32 bits of the hash of the name, offset of the name field) instead of a 12-byte reference plus a 24-byte `(hash, name)` pair; equal-hash runs are compared by re-reading the field. Hashing is done at parse time, when the name is in cache | pending | 0.265 -> 0.208 (11 rounds) | 140 -> 47 | pending |
| 6 | Each parse worker sorts its own per-country lists while they are still in cache, so the final stage merges sorted runs instead of sorting from scratch | pending | 0.207 -> 0.190 (13 interleaved rounds) | 47 -> 48 | pending |
| 7 | Chunk boundaries found by looking a few dozen bytes past each nominal cut for a line feed followed by a digit or a BOM (spec 2.2: names are letters, notes lowercase words), instead of counting every quote in the file first | pending | 0.180 -> 0.169 (15 interleaved rounds) | 48 | pending |
| 8 | No per-record storage for `names_distinct`: all workers share one lock-free open-addressing table that numbers each distinct name (64-bit hash of its raw bytes, CAS to claim a slot) and one bit per (country, name number); a country's distinct count is the popcount of its row. Parse uses a branch-free SWAR digit conversion and an AVX2 search for the closing quote; chunks are 8 per thread, claimed from an atomic counter | pending | about 0.15 -> 0.105 (3 runs each, noisy) | 49 -> 11 | pending |
| 9 | Name sightings are batched (16): each is looked up in the table, and one batch later its bit is set, with prefetches (`prefetcht0` for the slot, `prefetchw` for the bit) issued a batch ahead so cache misses overlap | pending | 0.194 -> 0.157 median, 0.148 -> 0.106 min (11 interleaved rounds) | 12 -> 12 | pending |
| 10 | `regions.csv` parsed in one tight loop into a table sized from the last line, codes mapped through a 15-bit table instead of a `HashMap` | pending | parse regions phase 10 -> 3 ms | 12 | pending |
| 13 | 32 chunks per thread instead of 8 (better balance at the end on P/E cores); the name table is leaked and the process calls `exit(0)` after the output is written, so no destructors or teardown run | pending | 0.125 -> 0.103-0.114 median (11 interleaved rounds; inside the noise) | 11 | pending |
| 11 | Names hashed from one masked AVX2 load (a 64-byte window of constants supplies the mask) instead of four masked scalar loads | pending | 0.111 -> 0.105 median, cpu 1042 -> 991 ms (7 interleaved rounds) | 12 | pending |
| 12 | Region table is one byte per region id (2 MB -> 1 MB, so it stays in L2); the rare group beyond the 254th goes to a spill map | pending | 0.123 -> 0.106 median, cpu 1126 -> 992 ms (13 interleaved rounds) | 11 -> 10 | pending |
| 14 | Each worker `madvise(MADV_DONTNEED)`s the page-aligned interior of a chunk it has finished, so the page-table teardown at exit (counted in T) happens in parallel while parsing | pending | ~85 -> ~74 ms median, 7 of 8 interleaved docker rounds faster (noisy laptop) | 10 | pending |
| 15 | `parse_regions` (serial, before any worker starts): the region id is read with the branch-free SWAR digit run and the common line end is checked first. callgrind on a 8 MB slice: mispredicts 422k -> 224k in total (all 198k of the regions loop gone), instructions 76.5M -> 70.3M. Serial regions phase was ~4 of ~100 ms on the laptop | pending | wall not resolvable on the laptop | 10 | pending |
| 16 | Software prefetch of the input stream (`_MM_HINT_T0`, 1536 bytes ahead) once per record, since the hardware prefetcher stops at page boundaries | pending | laptop ~84 -> ~76 ms median, 6 of 10 interleaved rounds faster (noisy) | 10 | pending |

Step 5 measured the board's way (5 runs, mean of the middle three, highest
memory peak) on the same laptop: **0.139 s and 47 MB, against 0.213 s and
141 MB** for the committed version, and 531 source lines against 552 (the
harness's own count).

## Where the effort went

The risk in this track is correctness, not speed: a BOM is skipped only at
the start of the file or right after a terminator; CRLF and LF end a record
only outside quotes; an LF inside quotes is data; `""` is one quote;
lengths are counted unescaped; money is integer cents and never prints
`-0.00`. Those cases are tests, and the output matches the published full
hash. Parallel parsing is safe because every quote character is either a
delimiter or half of an escaped pair, so the parity of quotes before an
offset says whether that offset is inside a quoted field.

The first distinct-name design interned names per worker and merged the
sets; the merge alone cost 200-280 ms, which is why ownership by country
replaced it (step 2).

## Why steps 4 and 5 worked

The stage profile showed parse about 75-155 ms, the quote-parity bounds 14 ms,
and the distinct-name stage 150-215 ms, the largest. In the full data 3.32 M
records hold 2.98 M distinct (country, name) pairs, so only 10% of inserts
are duplicates and the hash sets grew to millions of entries, with random
inserts into tables of several MB per owner. A group has only about 14 k
names, so its sorted array stays in cache (step 4).

Step 5 shrinks what is kept per record from 36 bytes to 8. Escaping is one to
one (a raw quoted field has every quote in a pair), so equal names have equal
raw bytes and the hash can be taken from the raw field without unescaping.
Only entries with equal hashes can be equal names, so the cold reads of name
bytes happen only for real duplicates (about 10%), and a collision in 32 bits
costs one comparison instead of a wrong answer.

## Tried and dropped

* **A shared dictionary of names with a number per distinct name** (64 and
  then 256 mutex-protected shards, padded to cache lines), so a record only
  stores a `u32`: memory fell to 34 MB, but the time rose from 271 to 471 ms
  because every record locks a shared table. Dropped for step 5.
* **A fingerprint-only distinct count** (not exact): 266 -> 177 ms, only as
  an upper bound for step 4; not kept because it is not exact.
* **Two-stage parse (steps 8-11's parser variant)**: stage 1 finds every `,` and LF outside quotes in 64-byte blocks with a prefix-XOR of the quote mask (carrying the quote count with each position), stage 2 parses each record from its five positions with no search. Output was identical, single-thread parse-only time in a native benchmark was about the same (235 vs 210-245 ms) and the docker A/B was no faster (128 vs 105 ms median, noisy; cpu 889 vs 872 ms min), with more code. Dropped.
* **One cache line per name (hash and 240-bit country set in one table entry, 40 bytes)**: 2^17 entries 119 -> 144 ms median, 2^18 entries (10.5 MB) 129 -> 149 ms (11 interleaved rounds each). Worse: the hash words that were read-mostly now share lines with the written bits, so every bit write invalidates the lookups of other cores. Dropped.
* **Test-before-`fetch_or` on the bitset words** (skip the locked op when the bit is set): 125 -> 132 ms median, cpu 1227 -> 1308 ms. Worse. Dropped.
* **Batch size 8 / 32 instead of 16**: 124 -> 113 / 109 ms median against 16, but the same image measured 106-123 in other runs, so inside the noise. Left at 16.
* **`MAP_POPULATE` on the records mapping**: 121 -> 127 ms median (11 rounds), no gain. Dropped.
* **2 MB-aligned `MADV_HUGEPAGE` tables** for the name table and bitsets: 133 -> 131 ms median (11 rounds), and peak memory rose from 12 to 15-16 MB because of the rounding. Dropped.
* **Single-thread cost split (native, warm RAM, T=1, noisy)**: parse-only about 235 ms of about 335 ms; the name table and bitset about 100 ms; hashing 16 ms with the SWAR version (about 115 ms with four masked scalar loads, before step 11).

## Where the unit tests live

The unit tests are in `tests/track_c.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_c.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 531 to 474.

## Caveats

* Step 7 relies on spec 2.2: a line feed inside a quoted field is followed by a
  letter or a quote, never by a digit or a BOM. The quote-parity scan it
  replaced did not rely on that.

* Steps 8-11 have not run on the scoring box. Expect the board's T to differ.
* `names_distinct` now rests on a 64-bit hash: two different names with the same 64-bit hash (not 3e-10 as first written: the table compares 47 bits along a probe path, so about 5e-5 per run for 100 000 names; a byte-comparing variant was built and measured at +20-30 ms on the laptop and +3 MiB, which is a larger loss than the risk it removes, so it was not shipped) would be counted once. Steps 4-7 compared bytes and were exact whatever the hashes did. If more than about 131 000 distinct names turn up, the table is rebuilt larger and the scan restarts (tested).
* Local timings in steps 8-11 come from a laptop shared with other jobs; differences under about 10% are inside the noise.
* Offsets are kept in 32 bits, so the input must be under 4 GiB (checked).
* Step 1's and the baseline's local timings were not recorded in a form I
  would cite, so those cells are left empty rather than reconstructed.

<!-- board-runs:begin -->
## Board runs

Each row is one scored run of the named commit on the live board, copied from the public board feed (`board.json`). The feed keeps only each track's latest and best run, so runs before these cannot be listed; earlier steps in the table above stay local measurements. Board timing varies by about 1-2 ms between runs of identical code: every push re-runs all four tracks, so a row whose change names another track measured this track's unchanged code and shows that noise.

| Commit | Change | Run ID | T (s) | M (MiB) | S | Points |
|---|---|---|---|---|---|---|
| `32f54ce` | Track A: the writer thread exits the process after the last block (agent-a4 d262378) | `run-6e20c9b0c201` | 0.0482 | 9.6 | 542 | 72.30 |
| `b67a992` | Track A: link the C runtime statically (skips dynamic loading at start-up) | `run-e9f7a7608016` | 0.0483 | 10.2 | 542 | 72.00 |
| `f299e99` | Track B: parse the queries in one SIMD token pass with leaner term interning (agent-b 8537 | `run-3faf0553d35d` | 0.0521 | 10.6 | 542 | 71.01 |
| `33b8455` | Track B: postings fill uses the term offset as its own cursor (agent-b d49c673) | `run-7fc38643a6b0` | 0.0475 | 9.4 | 542 | 72.52 |
| `b208288` | Track A: workers write their own hour block at its final offset; no writer thread (agent-a | `run-020190d2999d` | 0.0482 | 10.3 | 542 | 71.97 |
| `cd4c35e` | Revert "Track A: workers write their own hour block at its final offset; no writer thread  | `run-c4635c6dc06f` | 0.0474 | 10.3 | 542 | 72.16 |
| `e804869` | PERF.md: add a Board runs table with real run IDs from the public feed to every track | `run-870c55ac49de` | 0.0483 | 9.7 | 542 | 72.19 |
| `36f4296` | Tracks A and C: profile-guided optimization from a committed profile (no source or Dockerf | `run-1b9d7bb316d6` | 0.0437 | 9.7 | 542 | 73.27 |
| `158f18f` | Track B: 32-byte token mask (agent-b 0054521); Track A PERF.md header with current board n | `run-c2cca30b2db3` | 0.0433 | 9.7 | 542 | 73.39 |
| `49548fa` | Track B: merge and render in parallel from per-range logs (agent-b a34f4cf) | `run-2c6fc5b6afdd` | 0.0479 | 9.9 | 542 | 72.22 |
| `3286475` | Track C: 111-bit name fingerprint (second independent hash compared on every tag match), d | `run-fb77cbc5db79` | 0.0463 | 10.8 | 556 | 72.13 |
| `280509c` | Track B: vector-compare term lookup, CRC32C hash, fused dedupe (agent-b) | `run-77c892598bb5` | 0.0494 | 11.0 | 556 | 71.35 |
| `892d831` | Track C: regenerate the PGO profile for the 111-bit fingerprint source | `run-50d1b72e7320` | 0.0490 | 11.2 | 556 | 71.35 |
| `2ca5698` | Track C: back to the 36f4296 source and profile (the wider fingerprint cost 3-5 ms on the  | `run-5a68bdb4e25e` | 0.0456 | 10.3 | 542 | 72.58 |
| `b1497a5` | Track B: 2 MB pieces (agent-b c51c3b0) | `run-60441c945919` | 0.0475 | 10.3 | 542 | 72.15 |
| `f00df83` | Revert Track B 2 MB pieces: T -17 ms but M +12 MiB, 65.82 vs 65.94 on the board | `run-9e0e1b6e7e0d` | 0.0412 | 9.9 | 542 | 73.85 |
| `b18c263` | PERF: board run IDs through f00df83 (16 scored runs per track) | `run-e43c88ad604a` | 0.0441 | 10.2 | 542 | 73.01 |
| `44a450f` | Track A: mallopt M_MMAP_THRESHOLD 32 KiB, memory.peak 23.0 -> 20.4 MiB locally (agent-a4 c | `run-2a4dc94fe230` | 0.0441 | 9.6 | 542 | 73.24 |
| `b7bb4d3` | Track A: PGO profile regenerated for the mallopt source (agent-a4 90f380c) | `run-7b8524f7d03b` | 0.0496 | 10.7 | 542 | 71.50 |
| `4c454fe` | Track A: three-phase port with exact output sizes, every thread formats into the mapped ou | `run-7f58a28208fd` | 0.0466 | 9.8 | 542 | 72.57 |
| `5476630` | Re-run board on the Track A port (noise check) | `run-ec0db9e60380` | 0.0467 | 10.1 | 542 | 72.40 |
| `6c9bc1e` | Tracks B and D: user-drafted Rust solutions, reviewed (alt-bd c90b4a8): B 603 lines, D 509 | `run-5c2863a259b0` | 0.0466 | 10.0 | 542 | 72.49 |
| `f47d313` | Track C: user-drafted C++ design ported to Rust, CRLF chunk cut, 440 lines (agent-c3 7827d | `run-106ee1a91ed4` | 0.0496 | 10.5 | 450 | 72.21 |
| `c4832fa` | Re-run board on the Track C port (noise check) | `run-a60a1eb28170` | 0.0490 | 10.5 | 450 | 72.33 |
| `4f9e4bd` | Track D: PAGE 512 -> 256 (experiment) | `run-75bba5fbcfd2` | 0.0439 | 9.7 | 542 | 73.24 |
| `7a3ceb9` | Track D: BLOB 512 -> 1024 (experiment) | `run-aca9f8ea97a4` | 0.0432 | 9.9 | 542 | 73.32 |
| `0b9abd0` | Track D: BLOB back to 512, CHUNK 1024 -> 4096 (experiment) | `run-8a75e21840d3` | 0.0470 | 10.3 | 542 | 72.22 |
| `1a4d82e` | Track D: PAGE 256 -> 128 (experiment) | `run-6883394a1804` | 0.0461 | 10.5 | 542 | 72.37 |
| `a2fa18e` | Track B: persistent thread pool instead of 12 fresh threads per phase (experiment) | `run-3ab736b0b449` | 0.0460 | 10.6 | 542 | 72.34 |
| `b573d30` | Track C: C++ (gcc 15, user-drafted design: pclmul quote mask, dense-id name table); Rust v | `run-e44b534c42b7` | 0.0426 | 10.6 | 388 | 74.28 |
| `17ce1b0` | Track C: static link and _exit(0) after the output (experiment) | `run-b0663152fe1a` | 0.0376 | 10.5 | 389 | 75.68 |
| `9e9667f` | Re-run board on static+_exit Track C (noise check) | `run-21362694e839` | 0.0375 | 10.5 | 389 | 75.71 |
| `308cbff` | Track A: C++ (gcc 15, static, _exit after the output); Rust version stays at tag best-19 | `run-70be1284d180` | 0.0375 | 10.5 | 389 | 75.70 |
| `4d1d805` | Tracks B and D: exit right after the output so the big arrays are not freed one by one | `run-52c4ca2339d6` | 0.0379 | 10.6 | 389 | 75.55 |

<!-- board-runs:end -->

## Language switch to C++

The Rust version (0.0412-0.0496 s, S 542) was replaced by a C++ build of the team's own ChatGPT-drafted design (commit `b573d30`). Local interleaved runs showed it about 10% faster (median 85 ms against 94.5 ms); the first board run measured 0.0426 s, 10.6 MiB, S 388. The gain on the board is mostly the smaller source: time was within the Rust version's noise. A Rust port of the same design (commits 7827d83 and f47d313) was 3-5 ms slower than the older Rust code on two board runs; the C++ build of the same design was not slower on its first board run. gcc PGO: no gain locally, dropped.
