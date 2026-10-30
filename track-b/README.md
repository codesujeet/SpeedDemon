# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The queries are read first and every distinct term in them (about 76,000)
is interned into a table that stores each key inline: the first and last
eight bytes of the term plus its length, so a typical lookup is one hash
and one cache-line probe (terms longer than 16 bytes are also compared
against the stored bytes). The corpus is then split at line boundaries and
tokenized in parallel with a byte-class table (lowercase A-Z, keep a-z and
0-9, everything else, including every byte of a multi-byte UTF-8 sequence,
separates tokens, as the spec defines). A document becomes an array of
term ids, with a reserved value for words no query uses, so positions are
preserved for phrases.

Each document is handled in three tight loops: tokenize into keys, probe
the term table for all of them, then update the per-term counters. The
probes are independent of each other, so keeping them together lets the CPU
overlap their cache misses. A chunk only counts, per term, the documents that
contain it. When every chunk is counted, each term's list gets its final place
in one flat array (CSR layout, one sorted document list per term), and each
chunk's worker writes its document ids straight into their slots. The slots
of different chunks are disjoint but interleaved, so the writes go through
atomic cells with relaxed stores; the only `unsafe` code besides `mmap` is the
view that turns the zeroed array into those cells.

A boolean query is evaluated strictly left to right with sorted-list
intersection (switching to binary search when one list is 32x shorter),
union and difference. A phrase query intersects its terms' lists starting
from the rarest, then scans each surviving document's id array for the
consecutive run, which also handles repeated terms. Queries are answered
in parallel blocks and written in input order.

Tests (spec worked example, parallel equals single thread, term table vs
`HashMap`, set operations vs naive versions, repeated-term phrases,
tokenizer cases) are in `src/main.rs`.

## Dependencies

None.

## With another 24 hours

The corpus scan is still the largest phase. The answer phase is spread over
many small queries (see `PERF.md`), so no single trick helps there; a bitmap
form for the few very large lists would help a little at the cost of about
35 MB. The query file is parsed serially before any worker starts (about
35 ms), which could be overlapped with mapping and page-faulting the corpus.
I would keep any of these only if an interleaved A/B shows it.
