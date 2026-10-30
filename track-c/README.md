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

The exact distinct-name count uses ownership and sorting. Each country
belongs to one worker, and the parse workers hand `(country, name slice)`
to the owner. The owner collects a country's names as `(hash, name)` pairs
and sorts them by hash: a country has only a few thousand names, so the
array stays in cache, where one large hash set over millions of entries did
not. Names with equal hashes are then compared byte for byte, so the count
is exact. Output is sorted by country bytes with `_UNMATCHED` last; money
is printed from integer cents.

Tests (spec worked example, many thread counts, cents formatting, quoted
fields that start or end with a quote) are in `src/main.rs`.

## Dependencies

None.

## With another 24 hours

The stage profile is: quote-parity bounds about 14 ms, parallel parse about
70-90 ms, distinct names about 80 ms. The parse is near its floor (removing
the name hand-off saves about 13 ms, removing the region lookup about
17 ms). Hashing each name during the parse, while its bytes are still in
cache, would avoid cold reads in the distinct pass, but it adds about 40 MB
of memory, which roughly cancels the gain; I would try a smaller entry
layout first.
