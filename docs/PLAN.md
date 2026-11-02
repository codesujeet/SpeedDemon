# Speed Demon prep plan (solo, Rust, all 4 tracks)

## Context

Speed Demon is a 72-hour performance hackathon (Oct 30 18:00 UTC – Nov 2
18:00 UTC) run by Hackathon Raptors. Four fixed tracks (A Ingest, B Index,
C Join, D Traverse), each with a published naive Python reference. Score =
65% machine-measured efficiency coefficient `K = (Tref/T)^0.50 ×
(Mref/M)^0.20 × (Sref/S)^0.15 × (Dref/D)^0.15` + 35% judge score (code
quality 15%, technique 12%, PERF.md 8%). Overall "Speed Demon" title =
geometric mean of best 3 of 4 track totals, so entering all 4 lets the
weakest get dropped while still being eligible for the per-track $50 "highest
K" prizes on all four.

You're going solo, targeting Rust across all four tracks, with several days
of open prep time before the Oct 28 18:00 UTC freeze (when real specs, the
naive Python references, and the exact scoring harness drop). This plan
exists because: (a) nothing is buildable yet — specs aren't published — so
prep time has to go into things that transfer regardless of exact spec
wording, and (b) solo + 4 tracks means time budgeting across 72 hours is the
single biggest risk, bigger than any individual algorithmic trick.

**Intended outcome:** a working, correct, reasonably fast Rust submission in
all 4 tracks by code freeze, a PERF.md with real run IDs for each, and a
write-up submitted for the side-quest prize — structured so time is spent
where K actually pays (T has 0.50 weight and no cap; M, S, D are secondary
and two of them only cost).

## What the formula says to optimize, in priority order

1. **Wall time (T), weight 0.50, uncapped.** This is the only axis worth
   spending more than token effort on. A 10x speedup is worth more than
   perfecting memory or shrinking source. Budget the majority of hours here,
   in this order of expected payoff: algorithmic choice > avoiding
   unnecessary allocation/copies > parallelism across the 6 P-cores (12
   threads) > micro-optimization (SIMD, branch-free tricks).
2. **Peak memory (M), weight 0.20, capped at 50x.** Don't actively waste
   memory, but don't burn hours shaving MB once comfortably under the
   container's 8 GB limit and a few x better than the reference — the
   marginal contribution is small (2x → 1.15, 10x → 1.58).
3. **Source size (S), weight 0.15, capped at 1.0x (can't earn beyond
   reference).** Write clear idiomatic Rust; don't golf it. Being ~1.5–2x
   the reference's line count is fine and expected (see worked example:
   Nightshade wrote 95% more code and still won on T).
4. **Dependencies (D), weight 0.15, no cap, straight cost.** Default to
   **zero runtime dependencies** unless a crate saves meaningfully more time
   than the ~10% K penalty it costs (`2^-0.15 ≈ 0.90` per dependency,
   multiplicative). One well-chosen dependency is fine if it buys a large T
   win; three is probably not.

This ordering should resolve every "is X worth doing" question during the
event without re-deriving the math each time.

## Why Rust, and the default toolchain stance

Rust is already the recommended choice and matches the allowlisted
`rust:1-slim-trixie` build image + `debian:13-slim` runtime image shown in
the rules' example Dockerfile — use exactly that two-stage pattern (static
or minimal-dynamic binary, slim runtime, no dev toolchain shipped at
runtime). Default to **std-only** (no `tokio`, no `rayon`, no `serde`, no
`regex`) and reach for `std::thread` + manual work-splitting across the 12
available hardware threads, memory-mapped I/O (`mmap`, which rules say
works, unlike `io_uring` which is blocked), and hand-written parsers. Each
crate you consider pulling in should be weighed explicitly against the ~10%
per-dependency K cost.

## Pre-freeze prep (now → Oct 28 18:00 UTC) — several days available

Nothing track-specific can be finalized (no real spec yet), so prep goes
into things that transfer:

1. **Environment, once:**
   - ~~Install Rust~~, ~~Docker Desktop~~, ~~verify an offline multi-stage
     build~~ — **done 2026-10-25.** Rust/cargo were already present.
     Docker Desktop installed via `winget install Docker.DockerDesktop`
     (WSL2 backend; `wsl --status` already showed Default Version 2).
     Pulled `rust:1-slim-trixie` and `debian:13-slim` — their digests
     matched the rules page's own example exactly, so those are now baked
     into all four `track-*/Dockerfile`s instead of left as placeholders.
     Build-tested all four end to end with `docker build --network=none`
     (succeeds fully offline once the base images are cached) and ran the
     result with the harness's exact `docker run` flags (`--cpuset-cpus`,
     `--memory=8g --memory-swap=8g`, `--pids-limit`, `--read-only`,
     `--tmpfs /tmp`) — correct exit-code propagation confirmed. Image size
     ~118MB disk / 30MB content, comfortably under the 2GB cap. Re-check
     `speeddemon.dev/allowlist.json` at freeze in case either image gets
     re-tagged before Oct 28 — unlikely, but free to verify.
   - Set up a public GitHub repo now (empty is fine), with `track-a/`
     through `track-d/` folders, so installing the Speed Demon Board GitHub
     App and wiring the live board is a 2-minute task once registration
     closes (Oct 26) and the board opens.
   - Register and join the Discord before Oct 26 (team formation close) —
     irrelevant for team size but some logistics/announcements may only
     land there.
2. **Per-track skill rehearsal** (using the hints already published on the
   page, which are final in spirit even if exact spec numbers aren't):
   - **A (Ingest):** practice a hand-rolled line/byte scanner over a large
     synthetic log file (avoid `BufRead::lines()` + `String` allocation per
     line; work over `&[u8]` slices). Practice a custom RFC3339
     microsecond-timestamp parser (no `chrono`) and a streaming / selection-
     based percentile approach (e.g. a fixed-bucket histogram or
     P²/t-digest-style estimator) instead of collecting-then-sorting full
     latency vectors per endpoint. Rehearse against any large log-like
     dataset.
   - **B (Index):** build a toy inverted index (postings lists + positions
     for phrase queries) over a public text corpus, try delta + varint
     postings compression, and test whether sorting posting lists by
     selectivity before AND-intersecting beats naive left-to-right
     intersection. This is the two-phase (build vs. query) trade-off track —
     rehearse timing both phases separately on a toy corpus.
   - **C (Join):** write a from-scratch RFC4180-ish CSV tokenizer handling
     quoted commas, escaped quotes (`""`), embedded newlines, mixed
     CRLF/LF, and a BOM that can appear mid-file at chunk boundaries (the
     worked example in the page is the exact test case — use it as a unit
     test now). Decide hash-join vs. sort-merge ahead of time (hash join is
     almost certainly right at this record count on 8 GB RAM) and rehearse
     emitting sorted aggregate output deterministically.
   - **D (Traverse):** practice reading a large edge list fast (avoid
     per-line `String`/`Vec` churn — parse `u32` triples directly off a byte
     buffer), build a CSR (compressed-sparse-row) adjacency layout, and
     rehearse bidirectional Dijkstra or A* for point-to-point shortest paths
     at this scale — the hints say contraction hierarchies likely don't pay
     off given the query mix, so don't over-invest there.
3. **Harness dry-run mentality:** once the real harness drops at freeze,
   immediately read its exact run command and replicate the resource caps
   locally (`--cpuset-cpus`, `--memory=8g --memory-swap=8g`, `--read-only`,
   `--tmpfs /tmp`) so your own local timings aren't misleading you about
   what the scoring machine will see.
4. **PERF.md habit, starting now:** keep a running log of every optimization
   idea tried, even in prep, with before/after numbers on your own machine.
   This becomes the skeleton of the real PERF.md once live-board run IDs
   exist to cite — judges reward documented failed experiments too.

## Freeze → kickoff (Oct 28 18:00 UTC → Oct 30 18:00 UTC, ~48h)

- Read the 4 real specs and the one naive Python reference per track
  immediately; diff them against the prep assumptions above and adjust.
- Port your rehearsed approach per track into the real spec's exact I/O
  contract (`/data` filenames, `/out` output format, hash-sensitive
  ordering/float formatting rules) before any datasets exist — get the
  Dockerfile, output format, and harness contract 100% right so sample-data
  time (next phase) is pure verification, not scrambling.
- When the 10% sample datasets + expected hashes land (Oct 30 06:00 UTC,
  T-12h), run all 4 tracks end-to-end locally against them immediately and
  confirm SHA-256 matches before kickoff. Resist tuning further against the
  sample's specific bytes (explicitly warned against in the rules) — the
  12-hour window is for verification, not overfitting.

## During the 72 hours (Oct 30 18:00 UTC → Nov 2 18:00 UTC)

Rough solo time budget across 4 tracks (adjust once real difficulty is
known from the specs):

- **Hours 0–4:** full datasets land; get a correct-but-slow Rust baseline
  compiling and passing the hash check in all 4 tracks first, before
  optimizing any one of them. A correct slow submission in all 4 beats a
  fast submission in 1.
- **Hours 4–40:** rotate across tracks in priority order of expected K
  payoff (probably A and C first — parsing-dominated, most directly
  rehearsed; then D; then B, the most structurally different). After each
  meaningful change, push to the submission branch, get a live-board run,
  and log the before/after + run ID into that track's PERF.md immediately
  (not retroactively).
- **Hours 40–60:** second pass — parallelism across tracks where idle
  CPU time exists (e.g., Track A's per-hour bucketing is naturally
  shardable across the 12 threads/6 P-cores), then revisit any track still
  weak on K.
- **Hours 60–68:** freeze feature work per track one at a time; finalize
  README.md, `.speed-demon.toml`, verify Dockerfile builds offline from a
  clean checkout, re-verify hashes against full dataset output one more
  time.
- **Hours 68–72:** write/polish the write-up for the $300 side quest
  (2 × $100), final live-board sanity run per track (respecting the 1
  run/15min rate limit), code freeze submission.

## Submission checklist (per track, before freeze)

- `Dockerfile` — two-stage, `FROM` pinned by digest from
  `speeddemon.dev/allowlist.json`, builds with `--network=none`, reads only
  `/data`, writes exactly the one spec-named file to `/out`, exits 0.
- `PERF.md` — naive → final narrative, each step citing a real live-board
  run ID, including failed experiments.
- `README.md` — plain-prose approach, dependency list (ideally empty),
  "what I'd do with another 24h."
- `.speed-demon.toml` — track letter, team name, language (`rust`), declared
  dependency count.
- `src/` — public, OSI-licensed at freeze.
- Verify locally against the 10%/full-dataset expected hash before every
  push to the live board.

## Risks to actively guard against

- **Hash mismatches** from float formatting / ordering edge cases — pin
  these down from the spec text, not from "looks right" on a sample.
- **OOM kill** from the 8 GB cap with swap disabled — watch peak RSS locally
  under the same `--memory=8g --memory-swap=8g` constraint before trusting a
  local run.
- **Nondeterminism** (e.g., unordered hash-map iteration leaking into
  output order, thread-scheduling-dependent tie-breaks) — the rules call
  this out explicitly as a correctness bug, not a flake; use deterministic
  structures/iteration order anywhere it touches output.
- **Build-time network calls** — no `cargo` fetches at build time; vendor
  or vendor-free (std-only) is the safe default given the offline
  `--network=none` build.
- **Over-threading** — don't spawn more than ~12 worker threads; the box is
  pinned to `0-11` and extra workers cost via contention, not help.

## Verification plan

- Local: replicate the harness's exact `docker run` flags (cpuset, memory
  caps, read-only root, tmpfs `/tmp`, no network) from the published command
  once the harness drops at freeze, and confirm each track's container
  builds offline, runs, exits 0, and produces a hash-matching output on both
  the 10% sample and (once released) the full dataset.
- Live: use the Speed Demon Board (1 run per 15 min per team) as the source
  of truth for real timing/memory once kickoff starts, and cite every run
  ID used in a PERF.md claim — judges check that cited runs exist and moved
  in the claimed direction.
