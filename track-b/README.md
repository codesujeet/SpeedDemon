# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The queries are read first and every distinct term in them (about 76,000)
is interned into a bucketed table that stores each key inline: the first and
last eight bytes of the term plus its length. A lookup is one hash and one
64-byte bucket of three entries, compared without branches (terms longer
than 16 bytes are also compared against the stored bytes).

`docs.txt` is mapped and cut at line boundaries into 1 MB pieces. A pool of
workers (one per core) takes pieces from a shared counter and does everything
for a piece before moving on, so the whole index never exists at once:

1. **Tokenize.** AVX2 builds a 64-bit mask of token bytes (`a-z`, `A-Z`,
   `0-9`; everything else, including every byte of a multi-byte UTF-8
   sequence, separates tokens, as the spec defines), and token starts and
   ends come from bit tricks on that mask. The first token of a line is the
   document id and opens a document. A token's key is built branch-free
   (loads of its first and last 8 bytes, `| 0x20` to fold case, a mask for
   its length) and its bucket is prefetched; the lookups run per document
   over those keys. A token no query uses maps to a reserved id, so
   positions are preserved for phrases.
2. **Postings.** Per document the distinct term ids are collected, then each
   term gets a sorted list of piece-local document numbers (u16, a piece
   holds at most 65,536 documents). Terms present in at least `ndocs/64`
   documents of the piece also get a bitmap.
3. **Queries.** Every query runs on the piece. A presence bitset first rules
   out queries that cannot match the piece. A BOOL query is evaluated
   strictly left to right on sorted lists (a plain merge) and bitmaps (word operations, filters when mixed). A PHRASE
   intersects its terms' lists from the rarest, then scans each surviving
   document's id array for the run around the rarest term, which also handles
   repeated terms.
4. **Accumulate.** Each worker keeps, per query, a match count, an id sum and
   a log of its first ten ids. A worker sees pieces in ascending order, so
   its first ten are its ten smallest; a shared marker per query stops
   workers from logging ids that cannot be among the ten smallest overall.
   The workers' accumulators are merged in parallel at the end.

Peak memory is the workers' piece buffers (a few MB each), the accumulators
and the query table, about 70 MB.

Assumptions taken from the spec: ids are dense and ascending, so only a
piece's first id is parsed; terms are at most 32 bytes; a corpus token longer
than 32 bytes keeps its position and matches nothing; the build targets
x86-64-v3 (AVX2), as `.cargo/config.toml` sets.

Tests (spec worked example, pieces and workers do not change answers, the
document cap of a piece, term table vs `HashMap`, set operations vs naive
versions, random queries vs a brute-force evaluator, repeated-term phrases,
tokenizer and scanner cases) are in `tests/track_b.rs` at the repository
root, compiled into the test build only (`cargo test` in this folder runs
them).

## Dependencies

None.

## With another 24 hours

Every query still runs on every piece (12.5 M presence checks, 3.9 M real
evaluations). Grouping queries by an anchor term and visiting only the
queries whose anchor is present in the piece would remove most of that; the
anchor needs global term counts, which one or two sample pieces could
estimate. The query file is parsed serially before any worker starts (about
20-40 ms), which could be overlapped with mapping the corpus. I would keep
either only if an interleaved A/B shows it (see `PERF.md`).
