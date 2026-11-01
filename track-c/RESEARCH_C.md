# RESEARCH_C: cloud research on Track C (C++ build)

**No code change was committed.** The baseline is already at a local optimum for every cheap idea I tested.

## Setup

- Real files fetched from `s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/data/track-c/full-seed1/`; both sha256 match `SHA256SUMS.txt`. The unmodified build outputs `d8088b1cd42ba133...` (the published prefix; I did not have the full 64 hex characters of the expected hash, so I compared the prefix and byte-compared every variant with the baseline output).
- 4 vCPU shared cloud VM, g++ 13 (board: gcc 15), Dockerfile flags. No `perf`. Wall and CPU time, medians of 7 (1 CPU) and 11 (4 CPUs) interleaved runs; spread about +-2 ms (4 CPUs) and +-3 ms (1 CPU). Not the scoring box; ranking only.

## Experiments (all reverted; all outputs byte-identical to baseline)

| Change | 1 CPU wall (ms) | 4 CPU wall (ms) | Lines | Verdict |
|---|---|---|---|---|
| Baseline | 134.8 | 41.8 | 0 | |
| Remove `madvise(MADV_POPULATE_READ)` per piece | 140.9 | 43.3 | -2 | Worse (about 4%); the call earns its lines. Net of lines (0.07 per 10) it is still a loss. |
| Remove the `_mm_prefetch(p + 2048)` input prefetch | 228.2 | 67.5 | -1 | Much worse (about +60%). On this VM the page cache pages are scattered and the hardware prefetcher stops at page edges, as the comment says. |
| Prefetch distance 1024 / 4096 / 8192 bytes | 135.3 / 135.6 / 136.9 vs 133.6 | 41.5 / 41.6 / 42.2 vs 41.2 | 0 | Flat; 2048 is at the optimum. |

## Ideas not run (UNVERIFIED; no numbers)

1. Software-prefetch two pages ahead for the first touch of each new page, in addition to the 2048 prefetch. Risk: none.
2. `MADV_SEQUENTIAL`/`MADV_WILLNEED` on the records mapping instead of `POPULATE_READ`: fewer lines, may lose the page-map saving.
3. Replace the `std::string`/`to_string` output building by a fixed char buffer: output is under a few KB, so the gain is below noise; only worth it if it removes lines.
4. Delete the BOM and `\r` special cases: not allowed, the spec requires them.
5. Trade `LAG` per level (country read, name probe, mark) separately; PERF.md already swept a single LAG (8/16/32) without a clear result.
6. Atomic `fetch_or` per thread-private bitsets merged at the end: more memory (256 x 16 KB x threads) and PERF.md shows false sharing issues; likely worse on M.
7. Table sizing: `NameTable` has 131,072 slots for 100,000 names (load 76%); 2^18 slots lowers probes but doubles the table out of L2. PERF.md measured a nearby variant as worse.

The Rust PGO and wider-fingerprint attempts are already documented in `track-c/PERF.md`; I did not repeat them.
