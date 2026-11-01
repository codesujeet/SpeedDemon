# Track C: Join (C++)

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

One C++20 file, `src/main.cpp`, built with g++ 15 (`-O3 -march=x86-64-v3 -static`) on the allowlisted gcc image. No dependencies. The design was drafted with ChatGPT by the team and then reviewed and measured by us. Earlier Rust versions are kept at git tags up to `best-19`.

Both files are memory-mapped, and every phase is cut into pieces that any thread of one pool claims; a phase starts once the one before is done (a thread waits for the work, not for every thread to arrive):

1. `regions.csv` is split by lines; each thread flags the country codes it sees. Countries get indices 1..N in code order (0 is `_UNMATCHED`).
2. Each thread fills its slice of a direct table from region id to country index.
3. `records.csv` is split after CRLFs. The spec (2.2) says a CR appears only as the first byte of a CRLF terminator, so the byte after a CRLF always starts a record. Each record is read field by field: the id's comma is found in one 8-byte word, a quoted field's end by stepping through the bit mask of its quotes (a `""` pair is an escape), so commas and line feeds inside quotes cost nothing; the region's comma and the line feed come from one 32-byte compare each. Numbers are decoded eight digits at a time. Sums go to the thread's own per-country totals. Names go through a shared lock-free table to dense ids, which are marked in one bitset per country; the table lookup, the region read and the bitset word are each prefetched 16 records ahead. A mark that sets a new bit adds one to the country's distinct names, so no pass over the bitsets is needed afterwards.
4. The process writes the output and ends with `_exit(0)`.

## Assumptions and risks

- Names are printable ASCII (spec 2.2). A name's identity is a 64-bit hash of its bytes and length; two different names in the pool of 100,000 would have to collide on all 64 bits to be confused (about 3 in 10^10 per run), which is not zero.
- The cut after a CRLF relies on the spec's "no CR except in a CRLF terminator" guarantee.
- The table holds 8-byte slots probed four at a time with one 32-byte compare, and a slot is claimed with a compare-and-swap.

## Checks

100 back-to-back full-size runs of the final source with the harness flags, all matching the published hash. gcc profile-guided optimization was tried on an earlier version and gave no gain, so it is not used.
