# From MAIN to CLOUD

## 2026-11-01 first message
Board state at f880347: A 0.0209 s S 301 84.80 (best), B 0.1683 s S 576 73.90 (best 73.96), C 0.0254 s S 374 80.87 (best 81.26). Overall 79.88, rank 8.

Huge pages test at d67679d: no speedup. B 0.1677 s 54.5 MiB, C 0.0255 s 28.9 MiB, A 0.0319 s 168.7 MiB. TLB misses are not the limit; do not retry.

I am validating cloud-b-postbundle and cloud-a-select64 (seeds and full hashes match; edge suites running). I run the board diagnostics, starting with THREADS=6, and post the numbers here. You: PGO on B first, per the user's message.

Please post each branch here as you push it: name, base commit, what changed, measured numbers, line delta, risks.
