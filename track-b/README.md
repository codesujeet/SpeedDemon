# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

**Provenance.** This `src/main.rs` replaced the earlier piece-based version.
It was drafted with ChatGPT by the project owner and then reviewed and
tested here (hash checks on four generated full-size datasets, repeated
runs, the edge-case suite, `cargo clippy -D warnings`); two lines were
changed to satisfy clippy. The earlier implementation, its unit tests
(`tests/track_b.rs`, which target that implementation and are no longer
compiled) and its PERF history are in git history.

The queries are read first and only their terms are looked for. Each term
(at most 32 bytes) is a 32-byte lowercase, zero-padded key in an
open-addressing table. `docs.txt` is mapped over a spare zero page so a
32-byte load past any byte of it stays inside the mapping, and it is cut at
line boundaries into 12 shares, one per thread.

1. **Sample.** Each thread counts, for every query term, the documents that
   hold it in the first 1/32 of its share. A BOOL term in more than 1/32 of
   the documents is kept as a bitmap; each phrase's rarest term (by that
   sample) becomes its anchor.
2. **Scan.** Terms are renumbered by role (bitmaps, lists, anchors, terms
   only phrases read) so a token's role is read off its id. Tokens are found
   from AVX2 class masks of 64-byte windows, with the hash-table slot of each
   token prefetched eight tokens ahead. Every phrase is checked wherever its
   anchor occurs, while the text is in cache, by comparing the neighbouring
   tokens. A BOOL term in a document sets a bit in a bitmap or files a 16-bit
   unit under a bucket of its term; a thread's buckets are then scattered
   into one run of 16-bit document offsets per term.
3. **Answer.** BOOL queries run strictly left to right on sorted lists and
   bitmaps (merge, binary search when one list is much longer, word
   operations for bitmaps); phrase answers come from the matches found in the
   scan. 64 queries at a time go to each thread, and the output is joined in
   input order.

Assumptions: a document id is at the start of each line and ids are dense
and ascending; terms are at most 32 bytes (a longer corpus token never
matches); at most 32,767 documents per thread share (the full corpus of
250,000 documents gives about 20,800) and at most 131,072 distinct query
terms (the public data has about 76,000); both are asserted. The build
targets x86-64-v3 (AVX2), as `.cargo/config.toml` sets. The thread count is
fixed at 12. Inputs outside the spec (an empty query file, a corpus of a few
bytes, more than 393,000 documents) abort instead of answering.

There are no unit tests for this implementation; it is checked against the
published hash, generated datasets and the edge-case suite (see `PERF.md`).

## Dependencies

None.

## Dependencies

None.

## Latest version

The current `src/main.rs` is a second team draft (ChatGPT, reviewed and measured by us): tokens go into an id stream first and phrases are matched from it, lists over 768 documents meet other lists as bitmaps, terms are hashed from the first 16 bytes with the CRC32 instruction (the key comparison tells apart any that collide), and the threads are started once and reused. The process ends with `exit(0)` after the output. Output matched the previous version on seeds 1, 2, 3 and 7, and 40 back-to-back full-size runs matched the published hash.
