# Track A: Ingest (C++)

Reads `/data/requests.log` (one line per request: timestamp, method, endpoint, status, latency) and writes `/out/summary.txt`: for every hour and endpoint, the count, the 50th, 95th and 99th percentile latency and the 5xx rate. The exact task and output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md). Percentiles are exact (nearest rank), because the output is checked by hash.

## Approach

One C++20 file, `src/main.cpp`, built with g++ 15 (`-O3 -march=x86-64-v3 -static`) on the allowlisted gcc image. One pool of at most 12 threads (`hardware_concurrency`, capped) does everything in three phases:

1. **Parse.** The log is memory-mapped over a spare zero page and cut at line ends into 8 pieces per thread, which threads claim in order. A line's hour index comes from the fixed 27-byte timestamp (day and hour digits only, 72 hours). The line ends of each 4 KB are listed first, then the method, endpoint and latency are read with 16- and 32-byte compares. Endpoints are interned in one lock-free 4,096-slot table (CRC32 hash, four slots per bucket compared by 16-bit tag, then the name). Each line files a 4-byte record (13-bit key: 12-bit endpoint id plus the 5xx flag; latency above it) in a 16 KB chunk of its hour. A thread unmaps a piece's input pages as soon as it is parsed.
2. **Hours.** The last thread to arrive at a barrier ranks the endpoint names in byte order. Threads then claim hours: an hour's latencies are counting-sorted by endpoint rank into scratch and three selections per group give p50, p95 and p99. Each group's numbers are kept together with the exact size its text line will take, since a number's width is known without formatting it. The hour's chunks go back to the system in one `madvise`.
3. **Text.** The last thread into the next barrier turns the sizes into offsets, sizes the output file and maps it; then every thread formats its hours straight into the file. The process ends with `_exit(0)`.

## Assumptions and known limits

Guaranteed by the spec, and relied on:

- Timestamps are 27 bytes inside a fixed 72-hour span (`2026-10-30T18:00Z` to `2026-11-02T17:59:59Z`); lines are well formed and at least 40 bytes.
- The method is at most 7 bytes and `" SSS latency"` at most 15 bytes (16-byte compares).

Limits of this implementation, none reachable with the published data (a seed has 1,842 endpoints):

- **Endpoint table: 4,096 slots**, ids are 12 bits. More than 4,096 distinct endpoints would not fit, and the names arena (512 KiB) overflows with `_exit(2)`.
- Latencies of 2^19 us or more (about 3% of lines) do not fit in a record and go on a per-hour side list with their key.
- The scans read up to 64 bytes past a line; this relies on the zero page mapped after the file.
- Behaviour on malformed input is not defined (no checks, to save source lines).

## Dependencies

None.

## Checks

The first C++ version ran 100 back-to-back full-size runs with the harness flags, all matching the published hash, and the later drafts 80 or more. Each change was also compared against the Rust build on seeds 1, 2, 3 and 7. The earlier Rust version (0.026-0.029 s, S 508-510) is kept at git tag `best-19`. The optimisation history, with run IDs, is in [`PERF.md`](PERF.md).
