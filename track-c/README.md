# Track C: Join (C++)

Reads `/data/regions.csv` (region id to country code) and `/data/records.csv` (CRLF line endings, a BOM and a header, quoted name and note fields) and writes `/out/aggregates.txt`: per country the record count, sums, minimum and maximum amount and the number of distinct names, with records of unknown regions under `_UNMATCHED`. The exact task and output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

One C++20 file, `src/main.cpp`, built with g++ 15 (`-O3 -march=x86-64-v3 -static`, section garbage collection, and a committed gcc profile, see below). Both files are memory-mapped over a spare zero page. At most 12 threads (`hardware_concurrency`, capped) form one pool; every phase is cut into pieces that any thread claims, and a phase starts once the one before is done (a thread waits for work, not for every thread to arrive).

1. **Countries.** `regions.csv` is split by lines; each thread flags the country codes it sees (3 letters, in a 15-bit table). Countries get indices 1..N in code order (0 is `_UNMATCHED`).
2. **Region table.** Each thread fills its slice of a direct table, one byte per region id, from region id to country index.
3. **Records.** `records.csv` is split after CRLFs into 8 pieces per thread. The spec (section 2.2) says a CR appears only as the first byte of a CRLF terminator, so the byte after a CRLF always starts a record. Each record is read field by field: the id's comma from one 8-byte word, a quoted field's end by stepping through the bit mask of its quotes (a `""` pair is an escape), so commas and line feeds inside quotes cost nothing; the region's comma and the line feed come from one 32-byte compare each. Numbers are decoded eight digits at a time. Sums go to the thread's own per-country totals. Names go through a shared lock-free table to dense ids, which are marked in one bitset per country; the region read, the table lookup and the bitset word are each prefetched 16 records ahead. A mark that sets a new bit adds one to the country's distinct names, so no pass over the bitsets is needed afterwards. A thread returns the pages of a finished piece to the system.
4. **Output.** The totals of the threads are summed, the text is written, and the process ends with `_exit(0)`.

The `Dockerfile` builds with `-fprofile-use` and the profile `pgo/sdrun-main.gcda`, recorded on seed 2. It must be regenerated whenever the source changes (`-Wno-error=coverage-mismatch` keeps the build from failing on a mismatch).

## Assumptions and known limits

- **Name table: 131,072 slots** (8 bytes each, 1 MB), probed four at a time and claimed with a compare-and-swap. The pool of the public data has 100,000 names. There is no overflow check: with more than 131,072 distinct names the probe would not terminate.
- **A name's identity is a 64-bit hash of its bytes and length**, with no string comparison. Two different names in a pool of 100,000 would have to collide on all 64 bits to be confused (about 3 in 10^10 per run), which is not zero. Names are printable ASCII (spec 2.2).
- **Fewer than 255 countries** (the process exits with "too many countries"); region ids are ascending, the last line of `regions.csv` holds the largest id, and the table is one byte per id.
- **Known out-of-spec failure:** a CR inside a quoted note. The cut after a CRLF relies on the spec's guarantee that a CR appears only in a CRLF terminator; if a CR followed by LF occurred inside quotes, a piece could start in the middle of a record and the output would be wrong.
- Reads up to 64 bytes past the end of each file; this relies on the zero page mapped after it.

## Dependencies

None.

## Checks

100 back-to-back full-size runs of the C++ source with the harness flags matched the published hash, and outputs matched the Rust build on seeds 1, 2, 3 and 7. gcc profile-guided optimization gave no gain on the first C++ version and was dropped then; it was adopted later, once a profile from a separate run was committed (about 0.0250 s to 0.0243 s on the board). The earlier Rust versions are at git tags up to `best-19`. The optimisation history, with run IDs, is in [`PERF.md`](PERF.md).
