# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped read-only, so nothing is copied and the pages the host
already cached cost no memory. Twelve workers pull 2 MiB morsels from an
atomic cursor; each morsel start is snapped forward to the next line, so
neighbours agree on the split without talking. A worker walks its lines
with one fused scan that stops at either a space or a newline, parses the
status and latency in place, and adds the latency to its own hash map keyed
by `(hour, endpoint)`. The endpoint in the key is a slice of the mapped
input, so a line allocates nothing.

When the scan is done the workers' maps are not merged into one. Every
group is collected as a `(key, data)` pair and the pairs are ordered by hour
alone, which is a cheap integer sort. The workers then take one hour at a
time: a worker orders that hour's endpoints by bytes (which, after the hour,
is exactly the output order), merges the runs of equal keys (the same group
seen by different workers) into one reused latency buffer, picks the exact
nearest-rank p50, p95 and p99 with `select_nth_unstable`, and formats the
lines into a private buffer. The hours are written in order. No map is
searched in this phase.

Tests (spec worked example, empty input, blank lines, rank edge cases,
morsel boundaries, single vs multi thread) are in `src/main.rs`.

## Dependencies

None.

## With another 24 hours

The scan is now the larger phase (about 110-125 ms of roughly 190 ms). I
would try a fixed-offset timestamp parse in place of the per-field scan,
since the timestamp is always 27 bytes, and a flat per-worker log of
`(group, latency)` entries that is bucketed once at the end, instead of one
growing `Vec` per group. Two cheaper ideas, interning the endpoints and
parsing in batches before touching the map, were measured and gave nothing
(see `PERF.md`).
