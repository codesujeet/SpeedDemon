# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped read-only, so nothing is copied and the pages the host
already cached cost no memory. Twelve workers pull 1 MiB morsels from an
atomic cursor; each morsel start is snapped forward to the next line, so
neighbours agree on the split without talking. The last few hundred bytes are
scanned from a zero-padded copy so the 64-byte vector loads stay inside the data.

The spec fixes the line layout, so the 27-byte timestamp and the 3-digit
status are read by offset and only the method, endpoint and latency need a
delimiter scan (one 64-byte AVX2 window finds the four delimiters of almost
every line). Each line becomes one packed `u32` (endpoint number, error flag,
latency, parsed with SWAR) that is appended to the worker's own list for the
line's hour, found by comparing the 13 timestamp bytes `YYYY-MM-DDTHH`. A
worker keeps one endpoint table for the whole run (open addressing, names
compared byte for byte, so collisions are always resolved); the first time it
meets a name it asks one shared table for the name's number.

After the scan, workers take one hour at a time. Only that hour's lists (one
per worker) are read: lines are counted per endpoint number, the latencies are
placed in one run per endpoint, and the endpoints are visited in order of their
bytes (sorted once, about 1,800 names), so nothing is sorted or merged per
hour. p50, p95 and p99 come from `select_nth_unstable` on each run (largest
rank first, the others inside what remains below it). Blocks are written to the
file in hour order as soon as they and all earlier hours are done.

Tests (spec worked example, empty input, rank edge cases, morsel boundaries,
more endpoints than the first table holds, against a plain reference
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

Memory is now mostly the packed lines (12 MB) and the output file's page
cache; 6-byte lines or 2-byte latencies with an escape would shrink the first.
The scan itself would gain from finding all delimiters of a whole 64-byte
block at once instead of line by line.
