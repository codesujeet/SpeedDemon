# Track A: Ingest (C++)

The task, the input file and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

One C++20 file, `src/main.cpp`, built with g++ 15 (`-O3 -march=x86-64-v3 -static`) on the allowlisted gcc image. No dependencies. The design was drafted with ChatGPT by the team and then reviewed and measured by us. The earlier Rust version (0.026-0.029 s, S 508-510, about 79 points) is kept at git tag `best-19`.

One pool of threads does everything in three phases:

1. **Parse.** The log is memory-mapped and cut into line-aligned ranges, one per thread. The timestamp is always 27 bytes and the span is fixed at 72 hours, so a line's hour index comes from its day and hour digits alone. One 64-byte scan after the timestamp finds the method's end, the endpoint's end and the line's end. Endpoints are interned in one lock-free table shared by all threads, and each line files a 4-byte record (latency, endpoint id, 5xx flag) under its hour. Between the phases the last thread to arrive sorts the names, so each endpoint id gets its rank in output order.
2. **Hours.** Threads claim hours in order. An hour's latencies are counting-sorted by endpoint rank into per-thread scratch and three selections per group give the percentiles. Each group's numbers are kept along with the exact size the hour's text will take, since a number's width is known without formatting it.
3. **Text.** The last thread into the barrier turns the sizes into offsets, sizes the output file and maps it; then every thread formats its hours straight into the file. No write call queues on the file's lock, and the output's pages fill only after the records have been released. The process ends with `_exit(0)` right after the output is complete.

## Assumptions

Facts the spec guarantees: 27-byte timestamps inside a fixed 72-hour span, well-formed lines, and fewer endpoints than the table's slots. The input mapping relies on a zero page after the file for the 64-byte scans.

## Checks

100 back-to-back full-size runs with the harness flags, all matching the published hash. The first Rust-to-C++ board run (`308cbff`) measured 0.0277 s, 19.3 MiB, S 310.

## Latest version

The current `src/main.cpp` is a second team draft: a branch-free number-width helper, a name compare that checks 64 bytes in two AVX2 steps (94% of lines), and 64 spare bytes after the name arena for the over-read. `_exit(0)` after the output and the static link stay from the first C++ version. 80 back-to-back full-size runs of the draft matched the published hash.
