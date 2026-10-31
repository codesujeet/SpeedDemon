# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped read-only, so nothing is copied and the pages the host
already cached cost no memory (a zero page is mapped after the file so the
vector loads of the last line stay inside the mapping). Twelve workers take 1 MiB
chunks from an atomic cursor; each chunk start is snapped forward to the next
line, so neighbours agree on the split without talking.

**Phase 1, scan.** After the 27-byte timestamp every line has four more
delimiters (the spaces after method, endpoint and status, and the line feed).
One 64-byte AVX2 compare finds all of them for most lines (a loop takes the next
64 bytes for the rest), the status is three digits, and the latency is parsed
with SWAR. The hour (one of the spec's 72) comes from the month, day and hour
digits. A line becomes one `u32` (endpoint number 11 bits, error flag, 20-bit
latency) appended to the worker's vector for that hour; the 1% of lines with a
latency of 2^20 or more go to a second vector as a `u64`. The endpoint is found
in a worker-private open-addressing table keyed by a hash of the length and the
first and last 8 bytes; a hit is accepted only after comparing the first and last
32 bytes with a private copy of the name (and the blocks in between for names
over 64 bytes), and a miss takes the endpoint's number from one table shared by
all workers: a lock-free open-addressing table whose slots are claimed with a
compare-and-swap and which refers to the first occurrence of each name in the
input (a mutexed map made all workers wait for each other while they inserted
the 1,842 names). Collisions are always resolved exactly. Each per-hour vector
starts with a capacity of 128 KiB, which makes the allocator hand out its own
mapping, so its pages can be returned with `madvise(MADV_DONTNEED)` (unmapping
it stalls the other threads). When a worker has finished scanning it also drops
the page table entries of a twelfth of the mapped input, which the exit would
otherwise take apart on one core.

**Phase 2, hours.** The endpoints are sorted by name once. Workers then take one
hour at a time: its vectors from all scan workers are counted per endpoint (in
name order) and the latencies placed in one run per endpoint, the vectors are
freed, and each run gives its p50, p95 and p99 (a run of up to 32 is sorted,
a larger one is selected, each rank inside what lies above the one before). The
block is formatted and written to the file in hour order as soon as it and all
earlier hours are done.

Tests (spec worked example, empty input, rank edge cases, chunk boundaries,
a full catalogue of colliding names, names that differ in one byte, against a plain reference
implementation) are in `tests/track_a.rs` at the repository root, compiled into
the test build only (`cargo test` in this folder runs them).

## Assumptions

The parser relies on what the spec guarantees: every line is well formed and
ends with a line feed, the timestamp is 27 bytes, the status is three digits,
the latency has at most 10 digits, all timestamps lie in the 72 hours from
2026-10-30T18 to 2026-11-02T17, and there are at most 2,048 endpoints (the
catalogue has 1,842).

## Dependencies

None.

## With another 24 hours

Memory is the per-hour vectors (4 bytes a line, 12 MB) and the output file's page
cache. The phase 2 per-hour work (count, place, select, format) is about a third
of the CPU time and could be fused with the end of the scan.
