# Kickoff playbook — what to type, in what order, and how to check it

Written Oct 29 from the real specs (`real-spec/speed-demon-2026/specs/`)
and this repo's validated rehearsal code. Every design below produced
**byte-identical output to the organisers' published full-scale hashes**
and passed `check.py` (offline Docker build + the board's sandboxed run).

This repo is rehearsal only. Rule 05: all solution code is written during
the 72 hours, in the submission repo. Keep this file and `track-*/src/main.rs`
open as a reference and write each track fresh — don't paste.

## 0. Before kickoff (setup, not solution code)

- Submission repo from the starter template ("Use this template"), private
  until freeze. Default branch `speed-demon` (the board watches it).
- Install the Speed Demon Board app on it; submit the Tally form; set
  `team` in each `.speed-demon.toml`.
- Kickoff **Oct 30 15:00 UTC**, freeze **Nov 2 15:00 UTC** (moved 3h earlier).
- Board: 1 run per team per 15 min, full public dataset (seed 1), capped at
  3x the reference time. Final scoring: holdout seed, 5 runs, mean of the
  middle 3, capped at 10x. Cite board run IDs in every PERF.md step.

## 1. Per-track skeleton (identical for all four)

`track-x/Cargo.toml` — zero dependencies, binary named after the track:

```toml
[package]
name = "track-x"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "track-x"
path = "src/main.rs"

[dependencies]

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
panic = "abort"
```

`track-x/.cargo/config.toml`: `[build] rustflags = ["-C", "target-cpu=x86-64-v3"]`
(the box is an i5-13500: AVX2, no AVX-512; images build on a separate
machine, so name the level instead of `native`).

`track-x/Dockerfile` (validated with check.py on all four tracks):

```dockerfile
FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
COPY . /src
WORKDIR /src
RUN cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/track-x /app/run
ENTRYPOINT ["/app/run"]
```

Give each package its own binary name. Four packages all producing
`run.exe` in one workspace let cargo leave another track's binary in place
during local testing (it happened here).

## 2. S (source size) is expensive — know the rule

`harness/static_axes.py`: S = **code lines** (scc: no blank lines, no
comments) after **rustfmt with default settings** (your rustfmt.toml is
ignored), over **every file in the track folder** (the harness copies the
track directory and counts only the copy; Markdown, TOML, YAML and
lockfiles are "not code" and free, so PERF.md and README.md cost nothing).
Reference S is tiny: A=56, B=91, C=108, D=43. Our rehearsal code after
rustfmt is A 530, B 898, C 536, D 729 lines plus 7 for the Dockerfile (the
harness's own `static_axes.py` agrees; tests are about 180, 234, 61, 176 of
those), so S costs roughly 20-35% of K on every track. A 10% cut in lines
is worth only about +1.5% K, so delete code that earns nothing, but do not
trade speed for lines. Consequences:

- Doc comments are free. Code lines are not; every optimisation must beat
  its own line cost (`K ∝ S^-0.15`: +10% lines ≈ -1.4% K).
- **Tests inside `src/main.rs` count.** The rule counts "everything in the
  track's folder", so tests kept at the repo root, outside `track-x/`, are
  not counted: `#[cfg(test)] #[path = "../../tests/track_x.rs"] mod tests;`
  at the bottom of `main.rs` (verified: `cargo test` compiles them, and a
  release build of the track folder alone succeeds without them). Worth
  2-6% of K per track (180 / 234 / 61 / 176 test lines for A / B / C / D).
  This follows the rule's literal wording, not a stated endorsement, and
  the judges read the repo — so keep the tests committed and mention where
  they live in README.md. Your call whether you're comfortable with it; if
  not, keep them inline and pay the S cost.
- Skip debug-only code in the submission (the `TRACK_X_TIMING` phase
  macro): time phases on a local branch instead.
- Measure locally: `rustfmt --edition 2021 --config-path <empty.toml> f.rs`,
  then count non-blank, non-`//` lines.

## 2b. Memory (M) — what the counter charges

M is the container cgroup's `memory.peak`. The host pre-warms `/data` into
page cache before the run, so **`mmap`ing an input adds nothing to M**,
while `fs::read` copies it into charged heap. Mapping the inputs cut peak
memory 2-3x on A, B and C (A 332 -> 104 MB, B 593 -> 349, C 315 -> 119) at
equal or better time. Heap and the output you build in memory are charged;
dropping big temporaries before the next big allocation lowers the peak.
M's credit is capped at 50x the reference, with weight 0.20: halving M is
worth about +15% K, halving T about +41% K.

## 3. Track A — Ingest (reference 32.05s, rehearsal ~0.3s, ~80 MB)

Contract: `/data/requests.log` → `/out/summary.txt`. Line:
`ts method endpoint status latency`, space-separated, LF, ts always 27
bytes. Output one line per (hour, endpoint):
`YYYY-MM-DDTHH:00:00Z endpoint count p50 p95 p99 errors error_ppm`,
sorted by hour then endpoint as unsigned bytes. No header. Empty input →
empty file.

- Percentile rank = `(p * n).div_ceil(100)` (nearest rank, integer only),
  value = `select_nth_unstable(rank - 1)` on the group's latencies.
- errors = 500..=599 (499 is not). error_ppm = `errors * 1_000_000 / count`.
- Group key `(hour_key, &[u8] endpoint)`: the endpoint is a slice of the
  mapped input, so a line allocates nothing (a `to_vec()` per line was the
  worst cost here). `Ord` on that tuple is exactly the required order.
- Parallel: morsels from an atomic cursor, each snapped to the next LF;
  per-thread `HashMap` (FxHash). Fused SWAR scan for space-or-LF per field.
- **Do not merge the per-thread maps serially.** Return the maps, sort the
  union of keys once, and let workers take blocks of 8 consecutive keys
  from an atomic cursor: each gathers that group's latencies from every map
  into one reused buffer, runs the three `select_nth_unstable` calls, and
  formats into its own `Vec<u8>`; the blocks are written in key order.
  695 -> 423 ms and 104 -> 83 MB. Cascading the selects (p95 only above
  p50's slot) measured no gain; an approximate histogram is out, the hash
  gate needs exact nearest-rank.
- Full-scale phases (laptop Docker): parse+group ~166 ms, merge+percentiles
  +format ~196 ms, write ~19 ms.
- Input is not time-sorted (8 interleaved shards) — irrelevant to a
  full group-by, just don't assume order.
- Reference: `track-a/src/main.rs`. Worked example from spec §4 is a test.

## 4. Track B — Index (reference 56.35s, rehearsal ~1.0s)

Contract: `/data/docs.txt` (`id<TAB>text`, ids dense 0..N-1) +
`/data/queries.txt` → `/out/results.txt`, one line per query in input
order: `count sum id1 .. id10` (m = min(10, count); `0 0` for none).

- **Tokenizer is byte-based ASCII** (spec §2.2): lowercase only A-Z; a
  token is a maximal run of `[a-z0-9]`; every other byte separates,
  including every UTF-8 byte ≥ 0x80. A `char`/Unicode tokenizer fails the
  hash. Use a 256-entry class table.
- BOOL is strictly left to right, NOT is binary (`a NOT b` = a \ b). No
  precedence. Single-term BOOL is valid.
- Design that won: read queries **first**; intern every distinct query
  term (75,747 at full); tokenize the corpus once, in parallel chunks,
  storing each document as an array of query-term ids (`NONE` for other
  tokens, which still hold a position) and building postings only for
  query terms, CSR-style in two passes (count with small dense arrays,
  then fill a flat array). No positions stored; free the text after.
- PHRASE: intersect the terms' doc lists rarest first, then scan each
  candidate doc's id array for the exact consecutive run (handles
  `"x x x"`). BOOL: borrow the first list; AND switches to binary search
  over the remaining suffix when one list is 32x shorter; NOT is a sorted
  merge.
- Corpus-scan lookups are the hot spot: a custom open-addressing table
  with the key inline (first 8 + last 8 bytes as u64 + len) beat
  `HashMap<&[u8], u32>` (~130ns/lookup) clearly.
- Answer queries in parallel blocks from an atomic counter.
- Phase split at full (laptop Docker): parse queries 0.04s, tokenize
  corpus ~0.55-0.85s, answer ~0.2s. Peak 349 MB (reference 2,200 MB).
- Where the tokenize time goes (measured): byte classification plus
  pushing ids is ~150 ms of it; the lookup-and-count chain is most of the
  rest. 79% of the 36.3M tokens are query terms, so an early reject does
  nothing. A 1.5x smaller or 8x larger term table moves time by 0 / +15%.
  Merging `last` and `count` arrays: no change. AVX2 for the set
  operations: not worth it, queries are ~20-25% of the run.
- Do not drop the id array to save memory: phrase candidates (214k docs,
  63M tokens) are 1.7x the corpus, re-tokenizing them would be slower.

## 5. Track C — Join (reference 14.44s, rehearsal ~0.25s, ~125 MB)

Contract: `/data/records.csv` + `/data/regions.csv` → `/out/aggregates.txt`:
`country count id_sum amount_sum amount_min amount_max names_distinct name_bytes note_bytes`,
sorted by country bytes, `_UNMATCHED` last (unknown region_id). Countries
with no records print nothing. Money is integer cents printed `-?int.dd`,
never `-0.00`.

- Parse per spec §2.1 exactly: BOM (EF BB BF) skipped only at file start
  or right after a terminator; terminators CRLF or LF outside quotes; LF
  inside quotes is data; `""` is one `"`. Empty quoted and unquoted are the
  same value. id/region/amount always unquoted (§2.2).
- name_bytes / note_bytes = unescaped length = raw content length minus
  the number of `""` pairs.
- region_id → country via a dense array indexed by id (ids ≤ 999,999;
  records may reference up to 9,999,999 → bounds check → unmatched).
- **Parallel parse is safe via quote parity**: every `"` is a delimiter or
  half of `""`, so parity of quotes before an offset = inside/outside a
  quoted field. Count quotes per chunk in parallel, prefix-sum, snap each
  boundary to the first LF outside quotes. If a previous boundary already
  passed the nominal offset, resume there with `inside = false`.
- names_distinct (exact): each country is owned by one worker
  (`group % workers`). The parse writes `(group, name location)` into the
  owner's bucket; owners count distinct names per country with a set of
  byte slices. Don't intern per worker and merge — that merge alone was
  200-280ms.
- Sums exceed 2^32 (u64 / i64).

## 6. Track D — Traverse (reference 72.69s, rehearsal ~1.0s, ~380 MB)

Contract: `/data/graph.txt` (`n m`, then m lines `u v w`) +
`/data/queries.txt` (`s t`) → `/out/distances.txt`: one line per query,
the distance or `-1`. Nothing else.

- Weights are full u32 including 0; sums need u64. Parallel edges,
  duplicate lines, self-loops, isolated nodes, nodes with no in/out edges,
  islands — all present in every dataset.
- Bidirectional Dijkstra over forward + reverse CSR, 4-ary heap. **Stop as
  soon as either heap is empty** — otherwise a target with no in-edges makes
  the forward search settle the whole 1.74M-node main body.
- **Expand the side with the smaller queue, not the smaller key.** Any
  alternation is correct under `tf + tb >= best`, but balancing by key lets
  a source whose first edges weigh ~2^32 starve its own side while the
  other settles the whole graph: 3x fewer heap pops, 3934 -> 2580 ms.
- Per-worker state: one `Vec<[u64; 2]>` of labels (forward, backward) per
  node, stored as distance + 1 so a calloc'd array is already "unreached";
  a `touched` list clears the previous query (a sweep `fill` instead when
  more than 1/16 of the nodes were touched). The two sides of a node share
  a cache line, which is what a relaxation reads. One loop serves both
  directions (`side` 0/1, `graphs[side]`, `heaps[side]`).
- Adjacency is one `Vec<(u32 neighbour, u32 weight)>` per CSR.
- Queries in parallel (the spec allows any order), one scratch per worker,
  next query from an atomic counter. 12 scratches were most of the memory:
  515 -> 381 MB with the label layout above.
- **Relabel nodes in BFS order before building the CSRs.** The spec's ids
  are a random relabelling; BFS order put road-lattice neighbours close
  in memory: query phase 2.4s -> 0.83s, and less memory. BFS stays serial
  (~0.25 s); a level-synchronous parallel BFS pays a barrier per level.
- **Parallel preprocessing** (2353 -> 1494 ms): split the edge text at LF
  into one slice per thread and parse them concurrently into per-thread
  `Vec<(u32, u32, u32)>`; build each CSR with every worker owning a
  contiguous node range: it scans all edges once to count its nodes'
  degrees, the main thread prefix-sums, then each scans again and writes
  only its own `split_at_mut` slice. No atomics. The same function builds
  the original-id CSR for BFS, then the relabelled forward and reverse ones.
- What's left at full (laptop Docker): BFS reorder ~250 ms, CSR builds
  ~80-100 ms each, queries ~430-490 ms, parse ~45 ms (total ~0.9-1.0 s).
- Our earlier ~61x on a synthetic random graph did not survive the real
  road network (it was 3.7x before these changes). Trust only real data.

## 7. Verify every change (rehearsal tooling in this repo)

- `real-spec/` (gitignored) holds the organisers' repos and generated
  data. Regenerate with `py generators/<track>.py --seed 1 --scale
  {tiny,sample,full} --out ...`; the reference gives the right answer for
  tiny/sample; full hashes are published in each spec.
- `real-spec/verify.ps1` runs all tracks at all scales and compares
  hashes; `real-spec/peak.ps1 -Track x` gives wall time and peak memory.
- `check.py` (from the starter) builds offline and runs like the board:
  `py check.py x --data <dir> --expected <sha256>`.
- This laptop is a noisy hybrid CPU; repeat timings and trust the board.
- `real-spec/dock.ps1 -Track x [-Timing] [-Runs n]` rebuilds the track's
  image, runs it with the board's flags against a pre-warmed docker volume,
  and prints wall ms, `memory.peak` and MATCH/MISMATCH. `real-spec/ab.ps1`
  alternates two prebuilt images (`buildrev.ps1` builds one from any git
  revision) and prints medians: a single run differs by 30-50%, so only
  interleaved medians decide. Build both images explicitly; a script that
  reuses a tag can silently measure a stale image (it happened here).
- `EXPERIMENTS.md` logs every kept, reverted and rejected experiment with
  its numbers; read it before trying an idea a second time.

## 8. Order of work in the 72 hours

1. Hour 0-1: skeletons, Dockerfiles, `.speed-demon.toml`, push, confirm the
   board builds and runs the unchanged reference.
2. Then one track at a time, correctness first (check.py against the
   published hash), push, record the board run ID, then the next track:
   C (most failure-prone, simplest fast path), A, B, D.
3. Second pass: performance work, one change per push, each with a board
   run ID in PERF.md (failed experiments included).
4. Last 6 hours: freeze features, re-verify all hashes, PERF.md / README,
   make the repo public at freeze.
