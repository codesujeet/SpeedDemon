# Track C: Join (C++)

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

One C++20 file, `src/main.cpp`, built with g++ 15 (`-O3 -march=x86-64-v3`) on the allowlisted gcc image. No dependencies. The design was drafted with ChatGPT by the team and then reviewed and measured by us; the first version of this track was Rust and is kept at git tag `best-18`.

Both files are memory-mapped and every phase runs on one pool of threads, separated by barriers:

1. `regions.csv` is split by lines; each thread flags the country codes it sees. Countries get indices 1..N in code order (0 is `_UNMATCHED`).
2. Each thread fills its slice of a direct table from region id to country index.
3. `records.csv` is split after CRLFs. The spec (2.2) says a CR appears only as the first byte of a CRLF terminator, so the byte after a CRLF always starts a record. Each thread scans 64-byte blocks for quotes, commas and line feeds, masks out the ones inside quotes with a carry-less prefix XOR, and reads each record's commas and terminator off the bit mask. Numbers are decoded eight digits at a time. Sums go to the thread's own per-country totals. Names go through a shared lock-free intern table to dense ids, which are marked in one bitset per country; both steps are prefetched a few records ahead.
4. Each thread counts the set bits of every n-th country.

## Assumptions and risks

- Names are printable ASCII (spec 2.2), so the top bit of a name key is free as a flag.
- A name of up to 16 bytes is its own key. A longer name is keyed by its first 8 bytes and a 63-bit hash of the rest, so two different long names would have to share both to be confused (probability far below one in a million per run, but not zero).
- Table slots are 16 bytes, claimed with a 16-byte compare-and-swap and read with one aligned 16-byte load. Intel documents aligned 128-bit loads as atomic on AVX processors, which the board's CPU is; on other CPUs this would need a different read.
- The cut after a CRLF relies on the spec's "no CR except in a CRLF terminator" guarantee.

## Checks

150 back-to-back full-size runs with the harness flags, all matching the published hash. Locally the C++ build ran about 10% faster than the previous Rust build (median 85 ms against 94.5 ms over 30 interleaved runs each). gcc profile-guided optimization was tried and gave no gain, so it is not used.
