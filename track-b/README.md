# Track B: Index (Rust)

Reads `/data/docs.txt` (one document per line, starting with its numeric id and a tab) and `/data/queries.txt` (boolean and phrase queries) and writes `/out/results.txt`, one answer line per query. The exact task and output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

One Rust file, `src/main.rs`, std only (no crates). Built for x86-64-v3 (AVX2) through `.cargo/config.toml`; the term hash also needs the AES instructions. Twelve threads are started once and reused for every parallel phase (a barrier gate hands each phase to them). The corpus is never indexed in full: only the terms that occur in the queries are looked for.

1. **Read the queries first.** `queries.txt` is tokenized with AVX2 class masks. Upper-case words are operators, lower-case words are terms. Each term (a token of at most 32 bytes, lower-cased) is hashed with three AES rounds over its 32 bytes (padded with zeros) and interned in an open-addressing table of 4-slot buckets that live in 32 bytes; one compare settles a hit or a miss with no branch.
2. **Sample.** `docs.txt` is mapped over a spare zero page and cut at line ends into 12 equal shares. Each thread counts, per query term, the documents holding it in the first 1/64 of its share. A term used in a BOOL query that is in more than 1/32 of the sampled documents becomes a bitmap; each phrase's rarest term (by the sample) becomes its anchor. Terms are then renumbered by role (bitmaps, lists, anchors, terms only phrases read) with a counting sort, so a token's role is read off its id; the phrases are grouped by anchor with a second counting sort.
3. **Scan.** Each thread tokenizes its share window by window (64 bytes), looks up each token's id through a ring of four (the slot is prefetched when the token is hashed, the lookup happens four tokens later; the ring starts full of the hash of no term so the first lookups need no test), and per document, after a SIMD compaction that splits the ids into bitmap ids and phrase-anchor candidates without branches: sets bits for bitmap terms, files a 16-bit unit (term, document gap) into one of 32 buckets for list terms (both `u16` of a unit are always written and the cursor moves by one or two, so there is no branch on the gap), and checks every phrase whose anchor is in the document by comparing the neighbouring token ids. Each thread then drops its share of the file from memory. The buckets are scattered into one run of 16-bit document offsets per term and thread; threads are kept at most two buckets apart so the peak does not depend on scheduling.
4. **Answer.** BOOL queries run strictly left to right on sorted lists and bitmaps (merge, bitmap word operations; lists above 768 documents meet other lists as bitmaps). Phrase answers come from the matches found in the scan. Threads take 64 queries at a time and the output is joined in input order. The process ends with `_exit(0)`.

## Assumptions and known limits

Within the published data and the spec; these are not checked at run time (nothing asserts, to save source lines), so inputs outside them give wrong output rather than an error.

- **Terms are identified by a hash alone.** A term is the 45 bits of its hash that the table keeps (46 bits above the 18 id bits, with the top bit forced to 1), with no comparison of the string. A corpus token whose kept hash bits equal a query term's would be counted as that term, and two query terms with equal bits would be merged. The odds are small (a 45-bit identity against a vocabulary of the order of 10^5 terms) but not zero, and we did not compute them exactly; the output matched the published hash on every full-size run. Tokens longer than 32 bytes never match.
- **Term ids are 18 bits** (id + 1 below 262,144): at most 262,143 distinct query terms (the public data has about 76,000).
- **Document offsets are `u16` per thread.** Each thread's share must hold fewer than 65,536 documents (the full corpus of 250,000 documents gives about 20,800 per share), and no term may occur in 65,536 or more documents of one share.
- **At most 131,072 list terms** (the unit packs the term index divided by 32 into 12 bits).
- Document ids at the start of each line are dense and ascending, so the id of a line is also its position.
- The thread count is fixed at 12 and the build assumes AVX2 and AES-NI (the scoring CPU has both).
- Inputs outside the spec (an empty query file, a corpus of a few bytes) are not handled.

## Dependencies

None.

## Verification

Needs Docker; the datasets are generated with the contest's generator and are not in the repository. From the repository root:

```
python3 check.py b --data <directory with the docs.txt and queries.txt> --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927
.\real-spec\dock.ps1 -Track b -Scale full -Runs 20
```

The first builds the track image offline and compares the SHA-256 of the output with the published hash of the seed-1 data; the second replicates the board run (pre-warmed volume, board flags, `memory.peak`) and prints MATCH or MISMATCH. Outputs matched the reference on generated seeds 1, 2, 3, 7, 11 to 16 and 21 to 23 at full scale; 40 back-to-back full-size runs matched the published hash for the second draft. The edge suite has one known out-of-spec failure, `06_long_tokens` (tokens over 32 bytes never match). `cargo clippy -D warnings` was clean for the first draft. There are no unit tests for this implementation (`tests/track_b.rs` targets the earlier one and is not compiled). The history with run IDs is in [`PERF.md`](PERF.md); the whole-repo architecture is in [`../docs/OVERVIEW.md`](../docs/OVERVIEW.md).

## Provenance

This source started as a draft written with ChatGPT by the team, then reviewed, tested and tuned here with Claude Code. The earlier piece-based implementation and its history are in git history.
