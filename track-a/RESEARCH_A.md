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

## Methodology correction (important for reading every local number in these notes)
My benchmark harness reused `/out`, so each run's `open(O_TRUNC)` of the previous 10 MB `summary.txt` cost 2-10 ms (0.05 ms when the file is absent, as on the board, where `/out` starts empty). That inflated noise and fixed overhead in every local run; the A/B direction of earlier comparisons is unaffected because both arms paid it, but absolute wall times and the apparent "serial setup" (4-8 ms) were artifacts. Fixed harness (delete the output before each run), A prefetch re-measured on the current source: 4 CPUs wall 38.9 -> 30.9 ms (-21%), CPU 141.5 -> 110.6 ms (-22%); 1 CPU 127.3 -> 96.9 ms. The board saw only about -3% wall for it. This VM's page-walk and memory stalls are far worse than the scoring box's, so memory-behaviour tricks (prefetch, populate, release) transfer poorly; only ideas that cut instructions or real work are likely to carry over.

## Other measurements this round
- A's serial pieces on this VM are small: ranking callback 0.28 ms, sizing callback 0.02 ms, thread spawn 0.05 ms per thread. After the prefetch, 4-CPU wall (30.9 ms) is within about 3 ms of CPU/4 (27.6 ms), so A scales almost perfectly here; the remaining lever is instruction count.
- Radix selection for the percentiles would help only the few huge groups (about 3 groups per hour hold about 25% of the records) and loses on the 75% in small groups. Not built.

## Edge-case audit of the A changes (branches `cloud-a-includes`, `cloud-a-lines`)
All checked with the real gcc 15 build; none of this needed the board.
- Spec worked example (18 lines: hours out of order, duplicate lines, a 4,294,967,295 latency, an hour, a day and a month rollover): output equals the spec's expected `summary.txt` byte for byte, for the base and for the line-cut build.
- Empty `requests.log`: empty output (0 bytes), as the spec says. One line at the last instant of the span. The 125-byte endpoint next to `/`.
- Differential fuzz against an independent Python reference (150 random logs, 1-6,000 lines each): the first and last instants of the span, hour boundaries, endpoint-prefix families (`/v2/orders...`, `/v2/rank100` vs `/v2/rank20`), 1- and 125-byte endpoints, latencies 0 / 2^19-1 / above 2^19 / 4,294,967,295, statuses 100-599, duplicated lines: base 150/150, line-cut build 150/150.
- Thread count forced to 1, 2, 3, 5, 7 and 12 (12 threads on 4 CPUs): the real-data hash `495ca096...` every time. The board runs on 12 hardware threads; `hardware_concurrency()` is clamped to 12 so a larger or smaller cpuset also works.
- Hardware assumptions that remain: AVX2 + BMI2 + PCLMUL (the board's i5-13500 has them; there is no AVX-512 in the code), 4 KB pages for the `madvise` alignment, `prefetch` past the end of a mapping is harmless (it never faults), `MADV_DONTNEED` on clean private file pages cannot change data. A 10 MB output on a 1 GB tmpfs `/out` fits with a wide margin.
