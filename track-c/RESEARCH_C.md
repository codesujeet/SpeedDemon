# RESEARCH_C: cloud research on Track C (C++, base `3cbb939`)

Real files (records.csv `36166f4a...`, regions.csv `414c57c4...`), hash prefix `d8088b1c...` on every variant. 4-vCPU cloud VM; ranking only.

## Negative results (nothing was branched)
| Change | 4 CPUs wall | 1 CPU wall | Lines | Verdict |
|---|---|---|---|---|
| remove `MADV_POPULATE_READ` per piece | 41.8 -> 43.3 | 134.8 -> 140.9 | -2 | worse |
| remove the `p + 2048` input prefetch | 41.8 -> 67.5 | 134.8 -> 228 | -1 | far worse (+60%) |
| prefetch distance 1024 / 4096 / 8192 | flat | flat | 0 | 2048 is optimal |
| remove table prefetches (names, bitsets) | 39.3 -> 62.6 | 134 -> 167 | -2 | far worse |
| remove `populate_share` | 39.3 -> 42.4 | flat | -6 | worse (grows with threads) |
| parallel `MADV_DONTNEED` of bitsets, name table and region table after the parse | 39.9 -> 41.0 | 133.1 -> 132.9 | +7 | no gain; peak memory unchanged (6.9 MiB) |
| gcc 15 `-O2` / `-Os` instead of `-O3` | 41.7 -> 43.8 / 42.1 | 137.7 -> 138.7 / 139.6 | 0 | `-O3` stays |

## Instruction profile (callgrind, full data, 1,140 M Ir = 343 per record)
`parse_records` is 83%. Digit decoding (`fold` chain, three numbers per record) is about 20%, quoted-field scanning about 12%, the bitset word address about 4%. Candidate (UNVERIFIED, about 8% of instructions at best, about +0.5 pts): SIMD digit decode like A's `decode`, and a cheaper way to build `id_sum`. IPC is about 3, so time follows instruction count.

## Gap to the leaders
NickTrapp 18 ms / 13 MB / 303 lines vs ours 29 ms / 9 MB / 400: time is 5.2 pts of the 4.5-pt gap, memory costs them 1.6, lines are +0.9 for them. 1 ms = 0.37 pts, 1 MiB = 0.48 pts.

## Update (base `fff546f`)
| Idea | Result | Verdict |
|---|---|---|
| parallel release of tables (branch `cloud-c-release`) | 4 CPUs wall 38.8 -> 39.8 ms, 1 CPU flat, memory same, +9 lines | no gain |
| one `#include <bits/stdc++.h>` for 12 std includes (branch `cloud-c-includes`) | speed and memory identical, 394 -> 383 formatted lines | pure line cut, about +0.09 pts |

## Edge-case audit of the C changes (branches `cloud-c-includes`, `cloud-c-lines`)
- A bug I introduced and then caught: the first version of the line cut ended `region_line()` with `p = comma + 5`, which assumes an LF-only `regions.csv` (spec 2.3 says it is). On a CRLF copy of the real `regions.csv` that build **segfaulted** while the original did not. Final version: `p = comma + 5 + (comma[4] == '\r')`, same instruction count, correct on LF and CRLF. A memchr-based variant also worked but cost +0.7% instructions, so it was not used.
- Spec worked example (BOMs at the start and after terminators, LF inside quotes, `""` escapes, empty quoted and unquoted names, trailing spaces, a region below the table's range that is missing, a region above it, negative cents): output equals the spec's expected `aggregates.txt` byte for byte for the base and the final build, with LF and with CRLF region files.
- Differential fuzz against an independent Python oracle written from spec 2.1 (300 random hostile files: mixed CRLF/LF terminators, mid-file BOMs, quoted LFs, escaped quotes, 0-6 trailing spaces, amounts from `0.00` to `99999.99`, negatives, regions inside, missing from and above the table; every third case with CRLF regions): base 300/300, final build 300/300, the first version of the line cut 200/300 (every CRLF-regions case crashed).
- Thread count forced to 1, 2, 3, 5, 7 and 12: the real-data hash `d8088b1c...` every time.
- Assumptions that remain, all from the spec: names and notes are printable ASCII (the top bit of a name key is a flag); the name table is keyed by a 64-bit hash (collision odds around 5e-5 per run per PERF.md; a byte-comparing table was rejected for its cost); a record starts after every CRLF (no CR inside quotes, spec 2.2); `MADV_POPULATE_READ` needs Linux 5.14 and is simply ignored by `madvise` on an older kernel.

## Ablation of C's per-record components (callgrind, full data, base `cloud-c-lines`: 1,115 M instructions)
Each copy deletes one component (output wrong, instruction count only); several deletions let the compiler also drop the work that fed them, so the numbers are upper bounds on what each piece could ever save.

| Component deleted | Instructions | Saved |
|---|---|---|
| none (base) | 1,115.4 M | |
| totals bookkeeping (7 field updates per record; also frees the money, id and length work that only fed it) | 749.1 M | 366 M (33%) |
| distinct logic (name-table probe + bit mark + prefetch) | 948.5 M | 167 M (15%) |
| money parse | 1,003.6 M | 112 M (10%) |
| id decode | 1,053.8 M | 62 M (5.5%) |
| region decode | 1,058.4 M | 57 M (5.1%) |
| name hash | 1,073.2 M | 42 M (3.8%) |

Conclusion: no single piece is above about 17% of the work once its feeders are separated out (pure totals updates are about 57 instructions per record, distinct about 50), and each is already near its floor. Reaching the leaders' C (about 30% less time) would need a different algorithm for all of them at once, not a better version of one. Packing `name_bytes` and `note_bytes` into one add would save about 3 instructions per record (1%).
