# RESEARCH_A: cloud research on Track A (C++, base `3cbb939`)

Real file (`7ec80ba4...`), output hash `495ca096...81cd3` on every variant. 4-vCPU cloud VM; ranking only.

## Done
Input prefetch (`p + 4096` per line): CPU -26% on 1 CPU, 4-CPU wall 48.5 -> 39.6 ms on the pre-merge source. Already merged as `797bf9a` / `3cbb939`.

## Negative or too small to keep
| Change | Result | Lines |
|---|---|---|
| gcc 15 `-O2` / `-Os` vs `-O3` (built in the pinned `gcc:15` image) | 4 CPUs 38.5 -> 39.2 / 40.9 ms; 1 CPU 102 -> 102.8 / 107.4 | 0: `-O3` stays |
| hour from previous line's 13 timestamp bytes | CPU -1.2% | +7 |
| insertion sort for groups of 12 or fewer | CPU -1.4% | +8 |
| both | CPU -2.4% | +15 |
| drop the per-piece `MADV_DONTNEED` | no change | -1 |
| `pwrite` output instead of the mapped file | CPU -5%, wall worse (37.2 vs 34.6) | ~0 |
| replace `select()` by `nth_element` | CPU +13% | -25 |
| drop chunk `release()` | peak memory +11.8 MiB | -4 |
| 8-byte records (no `wide` list) | peak memory +10.6 MiB, CPU +5% | -7 |

## Instruction profile (callgrind, full data, 751 M Ir = 242 per line)
Scan about 60% (endpoint intern lookup about 19% of the total, hour parse 5.5%), hours + text about 38% (`select` partition loop 15%, count and place passes 10%). Each micro-change above is worth 1-2%, so the leaders' lead is unlikely to be instruction tuning.

## Points arithmetic (board 22 ms, 19 MB, 326 lines)
1 ms = 0.49 pts, 1 MiB = 0.24 pts, 10 lines = 0.1 pts. Leader Cause Of Death 16 ms / 17 MB / 346.

## Update (base `fff546f`)
| Idea | Result | Verdict |
|---|---|---|
| `-O2` in the Dockerfile (branch `cloud-a-o2`) | 4 CPUs wall 39.4 -> 41.6 ms, 1 CPU 101.9 -> 102.6 ms | slower |
| one `#include <bits/stdc++.h>` for 12 std includes (branch `cloud-a-includes`) | speed and memory identical (21 and 11 runs), 321 -> 310 formatted lines | pure line cut, about +0.11 pts |
| scalar `decode` loop (-6 lines) with gcc 15 | CPU 117.8 -> 128.6 ms (+9%) | rejected (the gcc 13 test had said neutral) |
