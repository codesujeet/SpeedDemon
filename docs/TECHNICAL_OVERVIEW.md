# Technical Overview — Speed Demon prep repo

> **Superseded in part (Oct 29):** this overview was written against a
> guessed spec and a synthetic dataset. The current designs and numbers are
> in `PLAYBOOK.md`, `EXPERIMENTS.md` and each track's `PERF.md`; where they
> disagree, those win.


One consolidated account of what this repo is, every decision made and
why, every technique tried (kept, rejected, or found neutral), and every
measured result — pulled together from `PLAN.md`, each track's
`README.md`/`PERF.md`, and `research/*.md`. Read this for the "why,"
and the individual files for full detail/citations on any one point.

---

## 1. What this is and how it's scored

[Speed Demon](https://speeddemon.dev) (Hackathon Raptors, Oct 30 – Nov 2
2026): four fixed tracks, each with a published naive Python reference.

```
K = (Tref/T)^0.50 × (Mref/M)^0.20 × (Sref/S)^0.15 × (Dref/D)^0.15
```

- 65% of the score is this machine-measured `K`; 35% is judged (code
  quality 15%, technique 12%, `PERF.md` 8%).
- Overall "Speed Demon" title = geometric mean of the best 3 of 4 tracks
  — entering all four lets the weakest drop while staying eligible for
  each track's own prize.

**What the formula says to optimize, in priority order** (this ordering
resolved every "is X worth doing" question in this repo without
re-deriving the math each time):

1. **Wall time (T), weight 0.50, uncapped.** The only axis worth more
   than token effort. Priority within T: algorithmic choice >
   avoiding unnecessary allocation/copies > parallelism across the 6
   P-cores (12 threads) > micro-optimization (SIMD, branch tricks).
2. **Peak memory (M), weight 0.20, capped at 50x.** Don't waste it, but
   stop optimizing once comfortably under 8GB and a few x better than
   reference — marginal contribution is small (2x → 1.15, 10x → 1.58).
3. **Source size (S), weight 0.15, capped at 1.0x** (can't earn beyond
   matching the reference's line count, but being larger costs via this
   term). Write clear idiomatic Rust; don't golf it.
4. **Dependencies (D), weight 0.15, no cap, straight cost.** Default to
   **zero runtime dependencies** (`2^-0.15 ≈ 0.90` K-multiplier per
   dependency). This repo ships zero in every track.

**Toolchain decision:** Rust, `std`-only (no `tokio`/`rayon`/`serde`/
`regex`), `std::thread` + manual work-splitting across 12 threads,
hand-written parsers, the exact two-stage `rust:1-slim-trixie` →
`debian:13-slim` Dockerfile pattern the rules' own example shows (digest
-pinned, confirmed building offline with `--network=none` and running
under the harness's exact resource flags — `--cpuset-cpus`,
`--memory=8g --memory-swap=8g`, `--pids-limit`, `--read-only`,
`--tmpfs /tmp`).

**Repo layout:** `track-a/` .. `track-d/` are the four submissions
(real, working code with their own `README.md`/`PERF.md`/`Dockerfile`/
tests — not scaffolding). `prep/` holds rehearsal crates and synthetic
dataset generators (`prep/gen-data`), kept separate so none of it counts
toward any track's source-size score. `research/` holds the methodology
docs cited throughout this file.

**Known unknowns, honestly flagged everywhere they matter:** the real
specs, naive-Python references, output formats, and sample datasets
don't exist until the Oct 28 freeze. Every number in this repo is
measured against a best-guess spec and a synthetic dataset built to
match the rules page's own worked examples and stated sizes as closely
as possible — real, not fabricated, but subject to re-verification the
moment real data lands.

---

## 2. One compiler flag beats every algorithm change in this repo

`-C target-cpu=x86-64-v3`, set in `.cargo/config.toml` in every track
folder. **Measured ~2x** on scanning-heavy code, up to 1.5x elsewhere —
bigger than any single algorithmic change below.

**Why it's safe here specifically:** the rules state build and run
happen in the same Docker build, on the same physical machine (an Intel
i5-13500, Raptor Lake). That's exactly the one case where compiling for
a specific instruction set has no compatibility downside. Raptor Lake's
hybrid P-core/E-core design has AVX-512 fused off entirely — which is
why `v3`, not `v4`, is the right explicit floor (not `native`, since
that risks baking in an assumption about the exact box that isn't
verified).

**This flag can flip which algorithm wins** — the single most important
methodological finding in this repo, stated once here because it recurs
everywhere below: Dial's algorithm (bucket queue) beat a binary heap by
1.24x on Track D's bounded-distance queries *without* this flag.
*With* it, they're statistically tied. A conclusion from one build
configuration silently stopped being true under another. **Nothing
measured on a laptop should be taken as final until re-measured on the
real scoring machine with the real build flags active** — this is why
the binary heap (simpler, ~40 fewer lines) was kept for Track D: with no
measured speed difference, the shorter implementation wins on source
size for free.

**Profile-Guided Optimization — investigated, practically blocked.**
The rules explicitly allow it (build time isn't scored), and typical
gains elsewhere run 5-25%. But `rust:1-slim-trixie` doesn't ship
`llvm-profdata`, and `--network=none` blocks fetching it during the
scored build. Vendoring the binary was attempted and abandoned: its
matching runtime dependency, `libLLVM.so`, is 208MB — over GitHub's
100MB push limit without LFS, with unverified LFS-smudging behavior in
the scoring harness's checkout step not worth betting a correctness gate
on. **Decision: skip PGO.** The target-cpu flag already captures a
bigger, safer win for a fraction of the risk.

---

## 3. The pattern found three times, independently: allocate-on-every-call

The single biggest recurring finding across this entire investigation.
Three different tracks had the same bug — allocate or re-derive fresh
state on every call/iteration instead of reusing it — found
independently, each time by profiling at realistic scale rather than by
pattern-matching from the first instance. **None of these were visible
on toy-sized test data or predicted by a bandwidth-vs-compute roofline
model** (see §9) — only phase-timed profiling at the real dataset's
scale found them.

| track | the bug | the fix | measured win |
|---|---|---|---|
| B | `index.entry(term.to_string())` allocated a `String` on every call, hit or miss | `index.get_mut(term)` (works via `Borrow<str>`), only allocate on an actual miss | ~17% faster `build_index` |
| B | `doc_ids(postings) -> Vec<u32>` cloned a whole term's doc-id array from an interleaved `Vec<(id, positions)>`, every query | struct-of-arrays `Postings { doc_ids, positions }` — doc-id-only queries borrow with zero allocation | 15% faster overall |
| B | `eval_bool` unconditionally allocated `(0..total_docs).collect::<Vec<u32>>()` (1.58M elements) at the top of every call, even though only the rare bare-`NOT` branch needs it | moved the allocation inside the `if first_negated` branch | **3.3x** on query-answering alone — the single biggest fix in Track B |
| C | `for_each_row`'s field handling did `row.push(std::mem::take(&mut field))` — a fresh empty `Vec<u8>` every field, ~16.6M times (5 fields × 3.32M rows) | a reused scratch pool: `std::mem::swap` the built field into a persistent buffer, `clear()` it, so capacity is reused instead of reallocated | **2.5x** on parse+aggregate alone |
| D | `bidirectional_dijkstra` allocated two fresh `vec![u64::MAX; n]` (n=1.74M) distance arrays *per query*, 2000 times | a `Scratch` struct allocated once outside the query loop; first touched-node-list reset (O(touched)), then an epoch/generation counter (O(1)) | **45.8x** on query-answering alone — the single biggest fix in this entire repo |

Also found, and *not* fixed because the measured cost was too small to
matter: Track B's `eval_phrase` did a linear `.find()` scan through
sorted postings instead of `binary_search_by_key` — harmless on a
5-document test corpus, would have been catastrophic at scale; fixed
before it ever shipped.

**The general lesson, as a checklist for any future hot loop:** before
reaching for SIMD or more threads, ask "is something being
allocated/recomputed every call that could be computed once and
reused?" This caught bugs an order of magnitude bigger than any
algorithmic swap in this repo.

---

## 4. Track A — Ingest

**Task:** aggregate per-(endpoint, hour) request counts, exact
p50/p95/p99 latency, and error rate from a space-delimited access log.

**Architecture, in order of what it's worth:**

1. **`-C target-cpu=x86-64-v3`** — see §2.
2. **SWAR 8-byte scanning** instead of byte-by-byte (`find_byte_swar`,
   and later `find_delim_swar`, see below) — ~2x in the `prep/`
   rehearsal.
3. **Morsel-driven parallel dispatch** (Leis, Boncz, Kemper, Neumann,
   SIGMOD 2014): a shared `AtomicUsize` cursor over 2MiB nominal
   morsels, each independently snapped forward to the next line
   boundary so adjacent morsels agree on the split with no coordination
   and no line is split or double-counted. Chosen over a naive
   equal-byte-split specifically because Track A's endpoints follow a
   Zipf distribution — equal byte ranges are not equal work, and a
   thread landing on a dense morsel shouldn't become the bottleneck
   while others sit idle. Each thread writes directly into its own
   persistent `HashMap` (fixed from an earlier version that allocated a
   fresh map per morsel and merge-copied it — removing that extra copy
   layer, though measured as only a small win since this track's ~90
   morsels over 181MB don't generate much merge traffic).
4. **FxHash-style hasher** instead of SipHash — this is a single-process
   batch job with a small, fixed key space, not a public-facing service
   facing adversarial input, so SipHash's DoS-resistance is dead weight.
5. **Exact percentiles via `select_nth_unstable`** (introselect, O(n)
   per call) instead of a full sort or a histogram approximation —
   chosen because an exact-hash correctness gate needs exact values,
   not an estimate.
6. **Hand-rolled integer/date formatting** in the output writer instead
   of `write!`/`Display` — avoids `core::fmt` machinery overhead and a
   `String::from_utf8_lossy` allocation per row.

**Measured (3.1M lines / ~181MB, 1,842 Zipf-distributed endpoints):**

| stage | time |
|---|---|
| read input | ~60-170 ms |
| parse+group (parallel) | ~200-270 ms |
| collect+sort keys | ~20-30 ms |
| percentiles+format | ~85-105 ms |
| write output | ~2-4 ms |
| **total** | **~400-500 ms** |

**Tried and found neutral (kept anyway):** fusing the per-line newline
scan with the per-field space scans into one combined
`find_delim_swar` call. The original touched every line's bytes
*twice* — once to find the newline, again (collectively) to find the 4
space delimiters within it. Fusing them is algebraically a real
reduction in redundant work, but measured **statistically unchanged**
— at this dataset's ~60-byte line lengths, per-line fixed costs
(`HashMap` entry, the endpoint-key allocation) dominate far more than
delimiter-scan redundancy does. Kept anyway because it closes a real
correctness edge case the original didn't have to worry about (a
blank/short line's newline can now be found *during* the combined scan
instead of via a side-channel lookup — handling this correctly needed a
dedicated check and a new test, `blank_lines_are_skipped_without_
corrupting_the_next_line`).

**Checked, not assumed: "12 threads = 12x" is false here.**
A generic claim that morsel dispatch gives perfect linear thread
scaling was checked directly (temporary thread-count override, measured
across 1/2/4/6/12 threads, then reverted — no shipped surface change).
Measured **~1.7-1.9x from 1 to 12 threads**, not 12x: 1 thread ran
727-871ms, 12 threads ran 469-480ms. This is Amdahl's Law, not a bug —
only `parse+group` is parallelized; `read input`, `collect+sort keys`,
`percentiles+format`, and `write output` are all single-threaded and set
a floor no amount of parallelism in the one parallel phase can lower.
6-vs-12 threads was within noise in both directions across repeats — no
consistent hyperthreading effect either way, just not enough remaining
parallel work to fill 12 threads once the serial phases dominate.

**Rejected on paper, with reasoning tied to this repo's own profiling:**
a perfect-hash/flat-array mapping for the 1,842 known endpoints (doesn't
fit the real contest — the endpoint set isn't known ahead of time, and
profiling shows SWAR scanning, not hashing, is what dominates
`parse+group` anyway); `mmap` for the input read (targets a
disk-latency bottleneck this track doesn't have — `read input` is only
~15% of total time, and the rest is compute, not I/O wait);
`#[repr(align(64))]` false-sharing padding (there's no adjacent
small-struct accumulator array in this design for cache lines to bounce
between — each thread's `HashMap` is its own independent heap
allocation).

---

## 5. Track B — Index

**Task:** build an inverted index over a corpus (one document per
line), then answer 50,000 BOOL/PHRASE queries, writing matching doc IDs
per query, in input order.

**Architecture, in order of what it's worth:**

1. **The allocator fixes in §3** — bigger than every other change
   combined.
2. **`-C target-cpu=x86-64-v3`** — §2.
3. **Plain `HashMap<term, postings>`**, not Roaring bitmaps or an FST
   term dictionary. Roaring (and, by the same reasoning, AVX2 bitsets
   for BOOL, reconsidered via external review) was rejected because a
   bitset AND over 1.58M documents costs O(total_docs) *regardless of
   how few documents actually match*, while the sorted-array
   intersection already in place costs O(min(list sizes)) — a decisive
   win for the sparse, Zipf-tailed postings this query mix (terms
   sampled from rank 50+, specifically to avoid stopword-tier
   ubiquity) actually produces. An FST term dictionary was rejected
   because the query grammar only ever does exact-term lookups — no
   prefix/wildcard/range matching — so the extra construction
   complexity solves a problem this track doesn't have.
4. **Selectivity-anchored phrase matching**: `eval_phrase` anchors the
   outer loop on the term with the *fewest* postings, to minimize how
   many documents need checking at all.
5. **`std::mem::forget`** instead of letting ~36M+ individually
   heap-allocated `String`s/`Vec<u32>`s drop at process exit — found by
   profiling to cost ~10 real seconds for cleanup of memory about to be
   reclaimed by process exit anyway, for zero behavioral benefit.

**Measured (1,583,677 documents, ~262MB corpus, ~6,000-word
Zipf-distributed vocabulary, 50,000 queries — 62% BOOL, 38% PHRASE):**

| stage | time |
|---|---|
| read corpus | 154.3 ms |
| tokenize | 2,936.1 ms |
| build_index | 6,270.2 ms |
| read queries | 3.2 ms |
| answer + write 50,000 queries | 22,920.8 ms |
| **total** | **32,294.9 ms** |

Reference: 56.35s. **~1.75x faster. Peak memory: 2.14GB (27% of the
8GB cap) — see §9, fixed down from an initially-measured 4.0GB.**

**Tried and reverted — the clearest negative result in this repo:**
galloping/exponential search (Lucene's `DocIdSetIterator::advance()`,
the PISA/partitioned-index set-intersection literature) for
`eval_phrase`'s per-document lookups, replacing `binary_search` with a
forward-only doubling-probe cursor — correct in principle, since
`anchor.doc_ids` is scanned ascending, so the target doc_id searched for
in every other term's postings is also monotonically increasing.
**Measured ~2x *worse*** (77.8s vs. a 38.3s pre-fix baseline on a
19,000-query PHRASE-only split), confirmed correct (byte-identical
output) but clearly regressed. **Why:** galloping only amortizes its
ramp-up cost when consecutive lookups land close together — but
selectivity-anchoring (point 4 above) deliberately minimizes *how many*
lookups happen at all by anchoring on the rarest term, so each lookup
jumps a large, uncorrelated distance into a larger list. Selectivity-
anchoring and galloping search solve for opposite things. The real fix
kept from this round: hoisting a redundant per-candidate-position
re-search out of the anchor-doc loop (searching each non-anchor term's
postings once per anchor *document*, not once per position candidate
within it), with the lookup buffer allocated once outside the loop — the
first implementation attempt had introduced exactly the
allocate-per-iteration bug this whole investigation has been removing
elsewhere, caught and fixed before shipping.

**A genuine methodology gap, found honestly rather than hidden:** no
clean performance number is published for the hoist-fix above. While
measuring it, this machine's background load (a Chrome process with
~2,700 accumulated CPU-seconds, Windows Defender's real-time scanner
active) produced 36.3s/56.8s/57.0s swings across repeated runs of
*identical* code and data — too large and non-monotonic to be the code.
This exposed a real blind spot in the control-benchmark methodology used
throughout this repo (`log-scanner`, used to validate every other
result in this file, generates its data in-memory and never touches
disk — so it's blind to exactly this kind of disk-I/O/antivirus
contention, which hits Track B hardest since it's the most
file-I/O-heavy of the four tracks). The fix is correctness-safe and
architecturally sound regardless; a trustworthy percentage needs a
quieter machine, not more retries on this one.

**Bug-class history (fixed earlier, see §3 for the general pattern):**
`index.entry(term.to_string())`'s redundant allocation, the struct-of-
arrays `Postings` rewrite, the lazy `(0..total_docs)` allocation in
`eval_bool`, and a linear `.find()` scan replaced with
`binary_search_by_key` in `eval_phrase` (harmless on a toy test corpus,
would have been catastrophic at scale).

**Dataset-design lesson, not a code bug:** the first corpus generator
used a 24-word vocabulary, making every word "common" and defeating the
phrase-matching anchor-on-rarest-term optimization entirely (every term
had a huge postings list). Fixed by regenerating with ~6,000 Zipf-
distributed words — but even then, query terms sampled from the *same*
Zipf distribution kept landing on near-ubiquitous head words. Second
fix: sample query terms uniformly from ranks 50+ specifically, the
standard IR practice of excluding stopword-tier terms from query
matching, applied to the generator instead of the index.

**Research grounding found to explain the bugs, after the fact, not
before:** SPIMI (Stanford IR textbook) and Lucene's immutable-segment,
separate-codec-file architecture were researched *after* profiling found
the allocator bugs, specifically to check whether they were known,
named problems — they were. That's a useful sanity check that the bugs
were real engineering problems and not noise, but not a substitute for
the profiling that actually found them.

---

## 6. Track C — Join

**Task:** parse a hostile CSV (`records.csv`: BOM reappearing mid-file,
quoted commas, escaped quotes, embedded newlines, mixed CRLF/LF), join
each record's `region_id` against a lookup table, emit sorted per-region
aggregates (count, total amount).

**Architecture, in order of what it's worth:**

1. **The scratch-pool fix in §3** — found by profiling, bigger than
   every other change here.
2. **`-C target-cpu=x86-64-v3`** — §2.
3. **Correctness first, on the track the rules explicitly warn about**:
   `for_each_row`'s SWAR bulk-copy tokenizer, restructured to emit one
   row at a time via callback instead of collecting all 3.3M rows into
   memory simultaneously. Cross-checked against the exact worked
   example from the rules page as a unit test.
4. **Fixed-point integer cents** instead of floats for the `amount`
   field — faster to parse (pure integer arithmetic, no rounding mode,
   no exponent handling) and immune to float re-serialization
   mismatches against an exact-hash gate (e.g. `0.1 + 0.2 != 0.3` in
   `f64`, trivially, against a Python reference that likely used
   `Decimal` or string arithmetic).
5. **A single FxHash join table, not radix partitioning** — see
   negative result below.

**Measured (3,320,000 records / ~168MB, matching the rules page's exact
hostile-CSV feature set, joined against a 40-row lookup table):**

| stage | time |
|---|---|
| read regions | ~0.2 ms |
| parse regions | ~0.05 ms |
| read records | ~65-90 ms |
| parse+aggregate records | ~400-480 ms |
| collect+sort keys | ~0 ms (≤40 output rows) |
| format output | ~0 ms |
| write output | ~0.4-0.6 ms |
| **total** | **~500-550 ms** |

Reference: 14.44s. **~26x faster, single-threaded** (up from an earlier
~9.4x estimate taken before the scratch-pool fix was found).

**Tried and measured negative, kept anyway for the real database
literature behind it:** radix-partitioned hash join vs. a naive single
hash table (`prep/radix-hash-join`). The CWI/MonetDB lineage says radix
partitioning should win once a hash table exceeds L2/L3 — but **the
naive join won at every tested size, by 1.7x-7.2x**, likely because
Rust's `HashMap` is already SwissTable/open-addressing (not the
pointer-chasing chained tables the original research was partitioning
around) and this CPU's cache is simply bigger than the problem.
**Decision: don't radix-partition.** The opposite of what the database
theory predicts in the abstract — exactly why it got measured instead of
assumed.

**Tried and measured neutral:** direct array indexing for the
`region_id` join key (a star-schema dimension key bounded by the 40-row
lookup table itself), replacing the FxHash table, with an overflow
HashMap fallback so correctness doesn't depend on the real spec's key
space actually being dense. **Statistically indistinguishable** from the
hash version — 40 distinct keys is small enough to already sit in L1,
so hashing was never the bottleneck at this cardinality (this technique
pays off at thousands-to-millions of distinct dimension values, not 40).
Kept anyway: simpler invariants, no collision handling, and safer by
construction for an unknown real key space.

**Rejected on paper:** a 1GB mmap'd output buffer with an atomic-cursor
parallel write — solves a write-bandwidth problem this track doesn't
have (at most 40 output rows; `format output` and `write output` already
measure at ~0ms).

**Deliberately not done yet: parallelism.** Track A's morsel-driven
dispatch (snapping chunk boundaries to the next `\n`) doesn't transfer
safely here — a `\n` can appear *inside* a quoted field (the worked
example's `note` field does exactly this), so naively snapping a
boundary to "the next newline" can land inside a multi-line quoted field
and corrupt whichever thread gets the back half. The correct fix is a
two-phase design (a cheap sequential quote-parity-aware boundary scan,
then N threads each parsing their now-guaranteed-safe chunk) —
deliberately deferred, since single-threaded already beats the
reference by 26x and this is the track most sensitive to correctness
bugs.

---

## 7. Track D — Traverse

**Task:** load a weighted directed graph into an adjacency structure,
answer point-to-point shortest-path queries in input order.

**Architecture, in order of what it's worth:**

1. **The reused-`Scratch` fix in §3** — found by profiling, bigger than
   every other change in this repo by a wide margin.
2. **`-C target-cpu=x86-64-v3`** — §2.
3. **Byte-level parsing straight into a CSR (compressed-sparse-row)
   adjacency layout** — no intermediate per-node `Vec<Vec<_>>`
   adjacency list. Matters specifically because Dijkstra's access
   pattern is latency-bound pointer-chasing (follow a random edge to a
   random node, read its row-offset, follow another edge — each step
   depends on the last), not bandwidth-bound streaming; CSR turns
   "chase a pointer to a heap-allocated per-node struct" into "index
   into one flat array," which doesn't reduce the *number* of random
   accesses but makes each one as cheap as a random access can be.
4. **Bidirectional Dijkstra, with a custom 4-ary `DHeap` instead of
   `std::collections::BinaryHeap` (always 2-ary)** — more comparisons
   per `sift_down` (up to 3 instead of 1) in exchange for the 4 children
   of index `i` landing in the same or an adjacent 64-byte cache line
   far more often than a 2-ary heap's 2 children do. Not Dial's
   algorithm (bucket queue) either — §2's flagship example of why a
   laptop benchmark under one build flag isn't the final answer.

**Measured (1,740,512 nodes / 5,000,000 edges / ~92.5MB, matching the
rules page's stated size almost exactly, 2,000 bounded-distance
queries):**

| stage | before | after |
|---|---|---|
| read graph | ~82 ms | ~35-55 ms |
| parse graph | ~142 ms | ~127-189 ms |
| build CSR fwd | ~208 ms | ~179-224 ms |
| build CSR rev | ~194 ms | ~185-225 ms |
| read+parse queries | ~1 ms | ~1 ms |
| **answer 2000 queries** | **26,851.9 ms** | **~511-535 ms (round 3)** |
| write output | ~1 ms | ~1 ms |
| **total** | **27,498.0 ms** | **~1,100-1,190 ms** |

Reference: 72.69s. **~61-64x faster** (up from an earlier ~3.0x estimate
taken before the `Scratch` fix — see §3 for the fix itself: a standalone
microbenchmark isolated 22.18s of the original 26.85s as pure
per-query allocate-and-zero of two 1.74M-element arrays).

**Round 2, tried as a refinement: epoch/generation counter instead of a
touched-node reset list.** Instead of recording which nodes were
touched and resetting only those (O(touched) per query), `Scratch` keeps
an `epoch: u32` counter and `epoch_f`/`epoch_b` arrays — a node's
distance is only valid when its epoch entry matches the current query's,
so starting a new query is a single increment, O(1) instead of
O(touched). **Measured statistically identical to round 1** (~570-605ms
either way) — round 1's O(touched) reset was never the bottleneck, since
those nodes were already paid for by the search itself. Kept anyway as
the textbook-correct version of the pattern (fewer invariants to track:
one check, "does epoch match," instead of separate touched-list
bookkeeping), reported honestly as a simplification, not a further
measured speedup.

**Round 3, a real further win: a 4-ary heap.** An external review
(from routing-engine practice — Neo4j, OSRM) suggested replacing
`BinaryHeap` with a 4-ary (d-ary) heap once the allocator bug was no
longer the dominant cost — exactly the right order to have tried it in,
since no heap variant would have shown a measurable difference while
that bug still dominated. Implemented a custom `DHeap` with the same
`push`/`pop`/`peek`/`clear` interface (drop-in, no change to the
surrounding search logic), with two new unit tests checking the heap
directly. **Measured ~511-535ms vs. round 2's ~570-605ms — a real,
consistent ~10-12% improvement**, confirmed via byte-identical output
and a stable control benchmark.

**Honest caveat that still stands:** the ~61-64x number answers "how much
did removing the allocator bug help on *this* synthetic graph" — it
doesn't answer "what will the real submission score." Uniform-random
edges are the exact pathological case flagged in `prep/graph-csr/
NOTES.md`: they don't produce the short-diameter, locality-rich
structure a real dataset almost certainly has (the rules page's own
hint — "most searches are short" — says so directly). The scratch-buffer
fix helps *any* graph shape equally (it's pure allocator overhead,
independent of search-frontier size); the *remaining* ~580ms for 2000
queries could still be dominated by frontier size on a real graph with
different structure, and that part of the number won't transfer. Rerun
against the real sample graph the moment it's released before trusting
these figures for anything beyond "the allocator bug is fixed."

**Researched, deliberately not applied:** graph reordering for cache
locality (BOBA, Degree-Based Grouping) — a real, measured technique, but
research is explicit it doesn't always help and depends on the real
graph's degree distribution, unknown until the real dataset lands, and
the reordering pass itself costs time inside the scored run. Contraction
hierarchies / ALT — confirmed not worth it: CH needs expensive
preprocessing to pay off across *many* repeated queries on the same
graph, and this track's query volume (2,000, preprocessing counted
inside the timed run) likely doesn't repay that cost.

---

## 8. Cross-cutting ideas checked and rejected (the "generic advice" audit)

A recurring theme in later research rounds: taking generic
high-performance-systems advice (the kind that sounds right in the
abstract) and checking it against this repo's *own measured data shapes*
before adopting it. About half of it didn't survive the check — which is
itself the more valuable finding than any one speedup number, because it
demonstrates the methodology (measure, don't assume) rather than just
its results.

| idea | verdict | why |
|---|---|---|
| AVX2 bitsets for Track B's BOOL queries | rejected | bitset AND costs O(total_docs) regardless of match count; sorted-array intersection costs O(min(list sizes)) — wins decisively for this track's sparse, rank-50+-sampled terms |
| Galloping search for Track B's PHRASE queries | tried, measured ~2x worse, reverted | amortizes only when lookups cluster; selectivity-anchoring deliberately minimizes lookup count, the opposite precondition |
| Perfect hash / flat array for Track A's endpoints | rejected | the endpoint set isn't known ahead of time in the real contest; profiling shows SWAR scanning, not hashing, is the actual bottleneck |
| mmap for Track A/C/D's file reads | not yet done, bounded upside | profiling shows `read input` is ~15% of total time; the rest is compute, not I/O wait — mmap can't mask a latency that isn't there |
| `#[repr(align(64))]` false-sharing padding | rejected, not applied | no adjacent small-struct accumulator array exists in this design for cache lines to bounce between — would be decorative, not a fix for a real problem |
| Direct array indexing for Track C's `region_id` | tried, measured neutral, kept | 40-entry table already fits in L1; technique's real payoff is at much larger cardinalities |
| Epoch counter for Track D's `Scratch` | tried, measured neutral vs. round 1, kept | round 1's O(touched) reset was already cheap; kept as the simpler/more-correct pattern anyway |
| Fusing Track A's newline/space scans | tried, measured neutral, kept | per-line fixed costs dominate more than delimiter-scan redundancy at this line length; kept for a correctness edge case it closes |
| 1GB mmap'd output buffer for Track C | rejected | at most 40 output rows — no write-bandwidth problem exists to solve |
| "12 threads = 12x" (perfect linear scaling) | checked, measured false | ~1.7-1.9x actual, explained by Amdahl's Law once you account for which phases are and aren't parallelized |
| Radix-partitioned hash join (Track C) | tried, measured 1.7-7.2x worse, kept the naive join | textbook database theory predicted a win; this CPU's cache is bigger than the problem, and Rust's `HashMap` isn't the chained-table design radix partitioning was built around |
| Dial's algorithm vs. binary heap (Track D) | tied under the real build flag | won by 1.24x without `-C target-cpu=x86-64-v3`; the flag erased the difference, so the simpler implementation was kept |
| Stable vs. unstable sort correctness risk (Track C) | not applicable | `aggregates` is keyed by unique `region_id` — no two output rows ever share a key, so there's nothing for sort stability to affect |
| 4-ary heap vs. binary heap (Track D) | tried, measured ~10-12% faster, kept | targets `sift_down` cache locality, a cost that only became the bottleneck once the allocator bug (§3) was already fixed — tried in the right order |
| SIMD vectorized set intersection for Track B's BOOL AND/OR (`intersect_sorted`/`union_sorted`) | not implemented, correctly scoped | unlike PHRASE's selectivity-anchored lookups (where galloping failed), these are plain two-pointer merges over whole operand lists — a real candidate for vectorization, not ruled out by the galloping failure, but not attempted given unsafe-intrinsic risk the day before freeze |
| Projection pushdown for Track A (skip timestamp/status parsing) | not applicable | the output contract (hour bucketing, error rate) actually needs every field — there's nothing unused to skip |
| String interning / flat-array grouping for Track A's endpoints | not pursued | doesn't address the measured bottleneck — profiling shows the SWAR scan dominates `parse+group`, not the `HashMap` operations interning would speed up |

---

## 9. Memory, finally measured — one real OOM-risk bug found and fixed

Every number in this file through §8 is wall-clock time. Peak memory
(M, 20% of K, capped at 50x — but more importantly, the harness enforces
`--memory=8g --memory-swap=8g` with **no swap**, so an OOM kill is an
instant zero on that track) had never been directly measured on any
track until this pass. Measured via Windows `Process.PeakWorkingSet64`,
polled every 10ms across a full real-dataset run:

| track | peak RSS | % of 8GB cap |
|---|---|---|
| A | ~250-253 MB | ~3% |
| B | **4,000.6 MB → 2,138.3 MB after a fix** | 50% → 27% |
| C | ~164 MB | ~2% |
| D | ~280 MB | ~3.5% |

Three tracks were already comfortably low. **Track B was not** — at
4GB, it was using half the container's entire memory budget, with no
margin if the real contest corpus turns out larger than this repo's
synthetic one. Cause: `run()` tokenized the *entire* corpus into one
`docs: Vec<Vec<String>>` structure (millions of individually
heap-allocated `String`s, one per token occurrence) before `build_index`
ran, and that structure was never used again afterward but also never
dropped — it sat at full size through the entire 50,000-query
answering phase for zero benefit, alongside the 262MB `corpus_text` it
was built from. Fixed by fusing tokenizing and indexing into one
streaming pass over `corpus_text.lines()` (at most one document's
tokens alive at a time, instead of the whole corpus's) and explicitly
dropping `corpus_text` once indexing finishes. Verified correctness
first — all tests pass, full-dataset output byte-identical to the
pre-fix baseline — before trusting the new memory number: **4.0GB →
2.14GB, a ~1.87x reduction**, now 27% of the cap instead of 50%. This
cost nothing in speed; it's a pure, free reduction in real OOM risk,
and it was invisible until memory was actually measured rather than
assumed safe because wall-clock numbers looked fine.

Also confirmed while auditing this axis: all four tracks declare zero
runtime dependencies (`[dependencies]` sections in every `Cargo.toml`
are empty) — the straight `D=0` cost this repo has targeted from the
start — and every track's release profile (`opt-level = 3, lto = true,
codegen-units = 1, panic = "abort"`) is already set correctly, confirmed
at both the workspace level (local dev-loop convenience) and
independently inside each track's own `Cargo.toml` (what actually
applies in the real standalone Docker build, which never sees the
workspace root). `cargo clippy --all-targets` runs clean (zero warnings)
on all four scored tracks after a couple of small style fixes
(`needless_range_loop`, `useless_vec` in test code) — relevant to the
judged 15% code-quality score, not the K-score itself.

## 10. Methodology: how results in this repo get trusted

**The roofline check, done once before reaching for any specific
technique** (`research/capacity-planning.md`): for each track's hot
loop, compute the memory-bandwidth floor (bytes touched / ~30GB/s
sustained) and compare to the measured time. All four tracks measured
**nowhere near** that floor (Track A: 59x the floor; Track C: 226x) —
meaning the bottleneck everywhere is compute, allocator overhead, or
(Track D specifically) memory *latency* from pointer-chasing, not
memory *bandwidth*. This one calculation ruled out an entire category of
optimization (reducing bytes touched, columnar compression) as
premature for every track, and correctly predicted that Track D's
problem (latency) needs a different fix (CSR, fewer dependent hops) than
Tracks A/C's problem (compute/allocator overhead, fixed by removing
unnecessary work).

**The control-benchmark discipline**, used to validate nearly every
number in this file: run `prep/log-scanner` (unchanged code) in the same
window as any suspicious timing change. If it stays flat while the
measurement under test varies, the variance is real; if both move
together, it's environmental noise, not a code change. This caught a
real episode where three consecutive "regressions" (97s → 119s → 148s →
215s on Track B) were entirely session-accumulated noise — a clean
rebuild reproduced the original ~99.7s almost exactly, and the
underlying fixes being tested the whole time were correct, just masked.

**The gap this discipline had, found honestly rather than papered
over**: `log-scanner` generates its data in-memory and never touches
disk, so it was blind to disk-I/O/antivirus contention — exactly the
kind of noise that made Track B's galloping-search measurement
untrustworthy (§5). Every number in this repo that depends on file I/O
is more exposed to that blind spot than the control benchmark itself
is; this is disclosed rather than hidden, and no number was published
where the noise was large enough to make it misleading.

**Correctness is checked before performance, every time**: every fix in
this repo was verified via (a) the existing test suite passing, (b) new
tests targeting the specific edge case the change could have broken,
and (c) a byte-for-byte `diff` of full-scale output against the pre-fix
run, before any speed number was trusted or written down. This caught a
real self-inflicted bug (an allocation introduced inside the Track B
phrase-matching hot loop during the galloping-search attempt) before it
shipped.

---

## 11. What's still open, honestly listed

- **The hackathon rules may have changed.** A separately pasted listing
  described 5 tracks and a different K formula/weighting than the
  detailed page this repo builds against. Judged as superseded (the
  detailed page's own changelog frames it that way), but not
  independently confirmed — verify at speeddemon.dev directly before
  the freeze.
- **Every output format/filename/column set in all four tracks is a
  best-effort guess.** The processing engines (parsing, aggregation,
  indexing, traversal) are the real, reusable work; the I/O surface
  needs re-checking against the real spec text the moment it's
  published (Oct 28 18:00 UTC).
- **Track D's ~61-64x figure is explicitly not a score prediction** — see
  §7's caveat. Rerun against the real sample graph before trusting it.
- **Track B's hoist-fix has no trustworthy performance number** — see
  §5. Needs re-measurement on a quieter machine, not more retries on
  this one.
- **Track C's parallelism is deliberately deferred** — the two-phase
  quote-aware boundary-scan design is specified in `track-c/PERF.md`
  but not implemented, given correctness risk on the track most likely
  to fail the hash gate.
- **mmap for file reads** remains a legitimate, unexplored idea with
  bounded (not dramatic) upside per §8 — worth doing for the mechanical-
  sympathy narrative in judged scoring, not because profiling says it's
  where the time is.
