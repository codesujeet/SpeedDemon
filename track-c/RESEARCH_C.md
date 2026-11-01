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
