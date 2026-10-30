# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

`regions.csv` becomes a dense array from `region_id` to country group;
any id outside it is unmatched. `records.csv` is mapped read-only and cut
into chunks that parse in parallel. Chunk cuts must not land inside a
quoted field, and they can be found without parsing: every quote is either
a delimiter or half of an escaped `""`, so the parity of quotes before an
offset says whether it is inside quotes. Workers count quotes per chunk, a
prefix sum gives each chunk's starting state, and each boundary is snapped
to the first line feed outside quotes.

Each worker parses records straight from the input bytes (BOM only at the
start of the file or after a terminator, CRLF or LF outside quotes, `""` as
one quote), updates its per-country count, id sum, integer-cent amount sum,
min and max, and the name and note byte lengths after unescaping.

The exact distinct-name count keeps eight bytes per record. While the name
is still in cache, the worker hashes its raw bytes and appends one `u64` to
its country's list: the top 32 bits of the hash and the offset of the name
field in the input. (Escaping is one to one, so equal names have equal raw
bytes, and nothing needs to be unescaped.) Afterwards each country is taken
by one worker, which gathers that country's lists from every parse worker
and sorts them. Only entries with equal hashes can be equal names, so only
those runs are compared, byte for byte, by reading the fields again; the
count is exact whatever the hashes do. Output is sorted by country bytes
with `_UNMATCHED` last; money is printed from integer cents.

Tests (spec worked example, many thread counts, cents formatting, quoted
fields that start or end with a quote) are in `src/main.rs`.

## Dependencies

None.

## With another 24 hours

The memory that is left is the eight bytes per record (about 26 MB) plus the
growth slack of the per-country lists; sizing them from a first pass over the
chunk, or writing each chunk's entries to one flat array, would trim that.
The parse itself is near its floor (removing the region lookup saves about
17 ms of roughly 90). A shared dictionary of names, so a record would keep
only a number, was tried and was slower because every record locks a shared
table (see `PERF.md`).
