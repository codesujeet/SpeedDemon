# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped read-only, so nothing is copied and the pages the host
already cached cost no memory. Twelve workers pull 2 MiB morsels from an
atomic cursor; each morsel start is snapped forward to the next line, so
neighbours agree on the split without talking.

A worker turns a morsel into a small, cache-sized array of packed `u64`
values. The spec fixes the line layout, so the 27-byte timestamp and the
3-digit status are read by offset and only the method, endpoint and latency
need a delimiter scan. Each line becomes one value holding its hour slot
(which of the few hours the morsel touches), its endpoint number (looked up
in a small per-morsel table), an error flag and its latency. A counting sort
over (hour slot, endpoint) then places the latencies of each group in one
run, and the morsel keeps just those runs and a short list of groups. There
is no large hash map and no per-group vector.

The log is close to time order, so a given (hour, endpoint) appears in only
a couple of morsels. The final phase takes one hour at a time: it collects
that hour's groups from the morsels that touch it, orders them by endpoint
bytes (after the hour, exactly the output order), merges equal endpoints into
one reused latency buffer, picks the exact nearest-rank p50, p95 and p99 with
`select_nth_unstable`, and formats the lines. Hours are written to the output
in order as soon as they and all earlier hours are done, so the output is
never held in memory in full.

Tests (spec worked example, empty input, rank edge cases, morsel boundaries,
single vs multi thread) are in `src/main.rs`.

## Assumptions

The parser relies on what the spec guarantees: every line is well formed, the
timestamp is 27 bytes, the status is three digits, and a run covers 72 hours
and about 1,800 endpoints. The packed format holds up to 2,048 hours and about
a million endpoints per morsel and stops with an error message beyond that.

## Dependencies

None.

## With another 24 hours

The scan is still the larger phase. I would look at replacing the per-field
delimiter scan with one pass that finds all the spaces of a line at once, and
at the memory that is left: the latencies themselves (12 MB), the group
records (6 MB) and the output file's page cache. Cheaper ideas, interning the
endpoints under the old design and parsing in batches before touching a map,
were measured and gave nothing (see `PERF.md`).
