# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

`regions.csv` becomes a dense array from `region_id` to country group;
any id outside it is unmatched (one byte per id; the rare group beyond the 254th is kept in a small map). `records.csv` is mapped read-only and cut
into many more chunks than threads (8 per thread); workers claim chunks from
an atomic counter, so fast and slow cores (P and E cores) finish together. A
chunk cut must not land inside a quoted field. The spec makes names letters
and notes lowercase words, so a line feed inside a quoted field is followed
by a letter or a quote, while one that ends a record is followed by the next
id's digits or a BOM; the first such line feed after each nominal cut is a
record boundary, found by looking at a few dozen bytes. The last 8 KB of the
file is copied behind zero padding and parsed last, so the AVX2 loads never
run past the mapping.

Each worker parses records straight from the input bytes (BOM only at the
start of the file or after a terminator, CRLF or LF outside quotes, `""` as
one quote): digit runs are converted with a branch-free SWAR routine, the
closing quote of a name or note is found with a 32-byte AVX2 compare (a `"`
followed by another `"` is an escaped pair). Per country it updates the
count, id sum, integer-cent amount sum, min and max, and the name and note
byte lengths after unescaping.

`names_distinct` keeps nothing per record. All workers share one lock-free
table, filled by compare-and-swap, that gives every distinct name a number:
the key is a 64-bit hash of the name's raw bytes (escaping is one to one, so
equal names have equal raw bytes and nothing is unescaped; an empty unquoted
and an empty quoted name are both the empty string). A second shared array
holds one bit per (country, name number), set with `fetch_or`; a country's
distinct count is the popcount of its row. Lookups are batched 16 at a time
and the bit is set one batch later, with prefetches issued a batch ahead, so
the cache misses of the table and the bitset overlap. The table starts
sized for 131 000 names (the spec's pool is 100 000); if there are more, the
scan restarts with a larger one, so the result does not depend on that
guess. Output is sorted by country bytes with `_UNMATCHED` last; money is
printed from integer cents.

Assumptions beyond spec section 2.1, all from section 2.2: names are letters
and spaces (so an LF followed by a digit or a BOM is a record boundary),
ids, regions and amounts are unquoted, each region code is three letters
`A`-`Z`, and `regions.csv` has LF line endings. Equality of names rests on
a 64-bit hash: two different names would be merged only if their 64-bit
hashes were equal (about 3e-10 for 100 000 names).

Tests (spec worked example, many thread counts, a large repeated file that
makes real chunk cuts, a file with more distinct names than the first table
holds, cents formatting, quoted fields that start or end with a quote) are
in `tests/track_c.rs` at the repository root, compiled into the test build
only (`cargo test` in this folder runs them).

## Dependencies

None.

## With another 24 hours

The parse is about two thirds of the work and the name table and bitset the
rest. A two-stage parse (find separators with a prefix-XOR of the quote mask,
then parse records from known positions) was tried and was no faster in this
form (see `PERF.md`). The next thing to try is an exact (key-comparing) variant of the name table
to remove the 64-bit-hash assumption.
