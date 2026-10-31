# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped read-only, so nothing is copied and the pages the host
already cached cost no memory (a zero page is mapped after the file so the
vector loads of the last line stay inside the mapping). Twelve workers take 1 MiB
chunks from an atomic cursor; each chunk start is snapped forward to the next
line, so neighbours agree on the split without talking. This cursor is the only
shared state in the scan, apart from a lock-free table of endpoint numbers that
a worker reads once per endpoint name.

Per chunk, a worker first lists the offsets of all delimiters (spaces and line
feeds) with AVX2 compares and bit scans, about four per 64 bytes. The spec gives
every line exactly five, so the lines are then read five offsets at a time and no
line has to be parsed to find the next one. The 27-byte timestamp and the 3-digit
status are read by offset. Each line becomes one `u32`: endpoint number (11 bits,
the catalogue has 1,842), error flag (first status digit is 5) and a 20-bit
latency (SWAR parse); a latency of 2^20-1 or more is stored as the mask followed by
one whole word. The words go to the worker's own arena in scan order, and a short
run list records where the hour (13 timestamp bytes `YYYY-MM-DDTHH`, compared with
the previous line's) changes. The endpoint is found in a worker-private open
addressing table keyed by a hash of the length and the first and last 8 bytes; a
hit is accepted only after comparing the first and last 32 bytes with a private
copy of the name (and the blocks in between for names over 64 bytes), and a miss
asks the shared table for the name's number. Collisions are therefore always
resolved exactly and nothing depends on the names.

After a barrier, workers take one hour at a time. The hour's runs are in the
arenas of the (few) workers whose chunks overlap it. Lines are counted per
endpoint number, the latencies placed in one run per endpoint, and the endpoints
visited in byte order (sorted once). Groups of up to 32 latencies are sorted,
larger ones use `select_nth_unstable` (largest rank first, the others inside what
remains below it). Blocks are written to the file in hour order as soon as they
and all earlier hours are done, and the process exits without freeing anything.

Tests (spec worked example, empty input, rank edge cases, morsel boundaries,
a full catalogue of colliding names, names that differ in one byte, against a plain reference
implementation) are in `tests/track_a.rs` at the repository root, compiled into
the test build only (`cargo test` in this folder runs them).

## Assumptions

The parser relies on what the spec guarantees: every line is well formed and
ends with a line feed, the timestamp is 27 bytes, the status is three digits,
and the latency has at most 10 digits. Hours are kept by their timestamp text,
so any number of hours and endpoints works.

## Dependencies

None.

## With another 24 hours

Memory is the arenas (4 bytes a line, 12 MB) and the output file's page cache;
2-byte latencies with an escape would halve the first. The per-line cost is now the
endpoint table lookup and the hour/latency work; a static split into line-aligned
ranges (so each hour is touched by one or two workers) would also make the output
phase read less scattered data.
