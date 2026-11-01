# B_post: Track B outside the token scan (review of origin/speed-demon 8eb3624)

Scope: startup, query parse, sample/class/renumber, allocation and faults, Units -> post scatter, docs(), apply(),
phrase lookup, line()/write, threading, memory peak. Research only: nothing in /home/user/SpeedDemon was edited,
/data and /out were not touched, and no timing runs were made. All work is in /tmp/claude-0/bpost/:
- copies of the code that read /tmp/claude-0/bpost/data/ (a copy of the real full data) and write
  /tmp/claude-0/bpost/out/results.txt
- callgrind runs, about 19 s each, single-threaded under valgrind
- a differential fuzz under `valgrind --tool=none`
- Python analysis of the real queries and docs (read-only)

**Bottom line.** One low-risk bundle of five small changes:
- output byte-identical:
  - hash f86973c2 on the real data
  - differential fuzz: 27 of 28 small corpora identical to the old binary. The 28th is a pre-existing panic that both binaries hit identically (section 5).
- **-14 formatted lines** (S 576 -> 562)
- **-363 M instructions (-5.2 %)**, all of it outside the token scan
- no new allocations
- **expected net +0.25 to +0.40 pt** on B; the time part is an estimate
- full diff: /tmp/claude-0/bpost/bundle.diff (patched file: /tmp/claude-0/bpost/bundle_main.rs)

The bundle clears the +0.2 bar only as a whole. Only U+S (item 1) comes close on its own (+0.16 to +0.21); every other part is below the bar alone.

**Memory is at the cap.** It cannot pay more than +0.17 pt in total, so no memory idea is ranked.

## 0. Conversion constants (derived; every estimate below uses them)

- **Memory cap.** I fitted every B run in `real-spec/board-runs.jsonl` with Tref 56.35 s and Sref 91. Every run gives
  Mref = 2200.2 MB, to within 0.1 MB.
  - At eee525f, M = 45,760,512 B = 43.64 MiB, so Mref/M = 48.08. The cap of 50 is reached at 41.97 MiB.
  - **All memory-peak work together is worth at most 10·log10(50/48.08) = +0.17 pt.**
  - Memory is therefore a cost for time ideas: each MiB added costs about 0.098 pt.
- **One formatted line on B** = 7.5·log10(577/576) ≈ **0.0057 pt**. The brief's 0.009 is the A/C value.
  - Line counts here use default rustfmt, then count non-blank, non-comment lines. This reproduces the board's
    S = 576 exactly: 569 lines of main.rs plus 7 lines of Dockerfile.
- **Time.** 1 ms of the 167.9 ms board wall = 25·log10(167.9/166.9) ≈ **0.065 pt**, so the +0.2 bar is about 3.1 ms.
- **Answers phase.** RESEARCH measured 355 Mcycles single-thread for a phase the board shows as about 25 ms, so about
  **0.07 ms of board wall per Mcycle**.
  - I use this as the upper end of each answers estimate.
  - The lower end assumes pure compute: 4.5 GHz over about 7.5 effective threads, about 0.03 ms per Mcycle.

## 1. Where the post-scan instructions go (callgrind, real full data, b0 = 8eb3624 with paths changed)

Total 6,937 M Ir. Inclusive counts:
- Answers closure: 1,463 M (21 %)
  - `apply`: 1,039 M. Of this, 315 M is memset and 92 M is memcpy, and valgrind counts these per byte: 10,091 bitmap builds and about 2,900 Cow clones of 31 KB each.
  - `docs()`: 119 M over 86,769 calls.
  - `bit_sums`: **189 M over 3,387 calls**, about 55.7 k instructions each.
  - core::fmt: about 40 M in total.
- Scatter closure: 473 M (7 %), or **43 instructions per posting** over 10.94 M postings.
  - About 150 M of this is `Flatten` overhead (`option.rs`, `into_iter.rs`, `non_null.rs`).
- Sample: 106 M.
- Serial `main` work: about 45 M, plus about 8 M of sort.

RESEARCH said the answers phase spends 65 Mcycles (18 %) in `line()` and called bitmap results rare. That is no longer true.
- 3,387 of the 31,000 BOOL answers are bitmaps (median 8.5 k ids, p10 2.3 k).
- So `line()` is mostly `bit_sums`, not formatting. llvm-mca on alderlake gives the old loop 17.35 cycles per 32-byte chunk, which is about 57 Mcycles for 3,387 × 977 chunks.

Other measured facts from the syscall trace (`valgrind --trace-syscalls`) and the Python simulation:
- Units: exactly **384 chunks** (one per thread-bucket, at most 29,594 of 32,768 units used), so the scatter does 384 munmaps of 68 KB.
- Only 13 gap units of 15 or more exist in the whole corpus.
- The output takes **198 `write` syscalls** of about 7.4 KB each, because BufWriter flushes every 8 KB.
- Term classes: 377 bitmap terms (10.14 M postings), 49,833 + 3,234 list terms (10.94 M postings), 14,736 anchor-only and 7,567 other terms.
- The simulated arm counts match RESEARCH exactly: bsearch 7,755, BB 762, OR merge 12,595.
- `docs()` builds **50.1 M list elements**. 11.1 M of them belong to the large side of a binary-search AND/NOT (7,755 calls), used for only 143 k probes.

## 2. Ranked: the bundle (one board slot)

Each item was measured separately, by callgrind delta and rustfmt line count. Every variant and the combined build print hash
f86973c2 on the real data. The combined build (`bF`) also matches the old binary on 27 of 28 random small corpora; the 28th panics identically in both (section 5):
1 to 30,000 docs, upper case, non-ASCII, empty and token-less docs, phrase windows, `and`/`or`/`not` terms, absent
terms, and 50 to 3,000 queries.

| # | Change | Lines | Ir delta | Expected time (estimate) | Net pt (estimate) |
|---|---|---|---|---|---|
| U+S | Units as one `Vec<u16>` per bucket, and the scatter loop as `for_each` | **-18** | -187 M (scatter 473 -> 311 M, scan -13 M) | -0.9 to -1.7 ms | **+0.16 to +0.21** |
| B | New `bit_sums`: nibble-LUT count and index sums, 20 instead of 57 instructions per chunk | +4 | -128 M | -1.1 to -2.5 ms | **+0.05 to +0.14** |
| W | chunk count from `spans.chunks(64)` (the `fs::write` half was dropped, see section 3) | -1 | 0 | 0 | +0.006 |
| F | Term sample 1/64 again (was 1/64 in 88d70d5; eee525f went back to 1/32) | 0 | -46 M (sample 106 -> 49 M) | about -0.7 ms | +0.04 |
| E | Empty-list cur OR t returns `docs(t)` directly | +1 | -16 M (memset/memcpy-inflated) | about -0.15 ms | about +0.005 (optional) |
| **Total** | | **-14 (S 562)** | **-363 M (-5.2 %)** (final build: 6,937.4 -> 6,574.0 M) | **-2.8 to -5.0 ms** | **+0.25 to +0.40** |

None of the five allocates anything new, so memory should not change. The Units vectors are the same 64 KB mmaps at the same points. The peak was not measured here.

### U+S. Units as a plain `Vec<u16>`, scatter with `for_each` (main.rs 316-354, 449, 499, 522-534)

Sketch:

```rust
struct Units { v: Vec<u16>, prev: u32 }
impl Units {
    fn add(&mut self, offset: u32, doc: u32) {
        let gap = doc - std::mem::replace(&mut self.prev, doc);
        self.v.push((offset << 4 | gap.min(GAP)) as u16);
        if gap >= GAP { self.v.push(gap as u16); }
    }
}
// scan:    (0..BUCKETS).map(|_| Units { v: Vec::with_capacity(1 << 15), prev: 0 })
//          and drop the `buckets.iter_mut().for_each(Units::finish)` line
// scatter: units.v.into_iter().for_each(|x| { ...same body, `continue` -> `return`... });
```

- **Why identical.** The unit stream is the same, so `seg`, `start` and `post` are the same.
- **Memory is the same.**
  - `with_capacity(1 << 15)` of u16 is the same 64 KB mmap per bucket, because M_MMAP_THRESHOLD is 64 KB.
  - The vector is freed when the bucket has been scattered, so 384 munmaps happen, as before.
  - If a holdout bucket ever exceeds 32 k units, glibc grows the mmapped block with mremap, which copies nothing and touches no extra pages. The old code started a second chunk instead.
- **Measured.** -18 lines, -24.7 M Ir from the Vec change. -151 M Ir from `for_each` alone, which is 0 lines: the flattened `for` loop is 43 instructions per posting, `for_each` is 29.
  - A plain `for x in units.v` is **worse**: 386 M against 311 M. Keep `for_each`.
- **Time.** Scatter is 37 ms of CPU on 1 CPU (brief). Taking 35 to 50 % of the 34 % instruction cut is 4 to 6 ms of CPU, which is about 0.6 to 0.9 ms of board wall at about 7.5 effective threads. The scan saves another 13 M Ir.
  - Lines: 18 × 0.0057 = +0.10.
  - Net about +0.16 to +0.21.
- **Risk.** Low. RESEARCH says "bucket-add cuts" are dead because `put` was already one compare and one store. This change cuts lines, not instructions, and callgrind shows the scan slightly *cheaper*.
- **Verify** with an interleaved local A/B of scan wall time and one board run.

### B. Cheaper `bit_sums` (main.rs 251-272)

Per 32-byte chunk:
- split each byte into nibbles l and h
- per-byte count c = cnt[l] + cnt[h], and per-byte index sum s = lo[l] + hi[h], where hi = lo + 4·cnt
- n = sad(c) gives the 4 word counts
- Σ 8·j·c (j = byte in word) = madd(maddubs(c, [0,8,..,56]), 1)
- accumulators: count Σn, 64·Σ n·word, and Σ s plus the byte term. The byte term sits in the same u64 lanes, so the result is the low half plus the high half.

- **Why identical.** It returns the same (count, sum) pair. It was checked against brute force on 65 bitmaps of 3,908 words at densities from 0 to 100 %, and against the real output hash.
- **No overflow.** maddubs pairs are at most 832. The per-lane i32 sums stay under 1.7 M, and the total low-half sum stays under 2^32 at N = 250 k.
- **Measured.**
  - 19.5 k instead of 56.5 k Ir per call; -128 M Ir on the full run.
  - llvm-mca, alderlake: 6.35 against 17.35 cycles per chunk, about -36 Mcycles in total. Skylake model: 7.0 against 20.3.
  - +4 lines.
- **Time.** 36 Mcycles × 0.03 to 0.07 ms = 1.1 to 2.5 ms. It cuts port-1/5 shuffle and SAD work, which SMT shares, so it should carry over to the board. Net +0.05 to +0.14.
- **Risk.** Low. Pure AVX2 (x86-64-v3).

### W. Chunk count from `spans.chunks(64)` (main.rs 556-557)

`let chunks: Vec<Mutex<String>> = spans.chunks(64).map(|_| Mutex::default()).collect();` replaces the `n` line and the `(0..n)` map.
- **Why identical.** It gives ceil(n/64) chunks, the same as `div_ceil`, including n = 0.
- -1 line.

### F. Sample 1/64 (main.rs 396)

0 lines. This undoes an unintended revert: 88d70d5 set 1/64, then eee525f, a draft built on older code, went back to 1/32.

- **Why identical.** The sample only steers anchor choice, class and order. Any choice gives the same answers (spec §3: anchor position j is checked exactly; bitmap and list hold the same S(t)).
- **Python simulation on the real data.**
  - 1/32: 377 bitmaps, 10.943 M list postings, anchor df sum 972 k.
  - 1/64: 378 bitmaps, 10.938 M list postings, anchor df sum 1,031 k. That is +6 % phrase candidates, about 60 k checks.
  - 1/128 would raise the anchor df sum by +16 %. Do not go further than 1/64.
- **Measured.** The sample drops from 106 to 49 M Ir across 12 threads. The phase is a barrier on the critical path, so the estimated saving is about -0.7 ms on the board. Net about +0.04.

### E. `∅ OR t = S(t)` without a merge (fold in answers, line 573)

Add the arm `Docs::List(v) if v.is_empty() && op == b'O' => docs(id),` before the existing empty arm (+1 line).

- 231 bitmap results and 167 bitmap clones came from an empty current list ORed with a large term. The cases are 1,848 with a list term and 167 with a bitmap term; those with |b| > 2048 went through a 31 KB bitmap build and then `bit_sums`.
- **Why identical.** ∅ ∪ S = S. `docs()` returns the same sorted list, or the same bitmap borrowed.
- -16 M Ir, but most of that is memset bytes; the real saving is about 3 Mcycles. Net ≈ 0. Optional; keep it only if the line is acceptable.

## 3. Measured and rejected (below the bar after line cost)

- **Probe postings directly for binary-search AND/NOT** (`member(i, d)` over the u16 segments, no materialization of the large list). It also deletes the `apply` bsearch arm.
  - Avoids building 11.1 M list elements.
  - Ir only -1.1 M: the probes cost what the copies cost. +15 lines.
  - The cycle gain (copies are memory-bound, about 1.6 cycles per element, so at most about 18 Mcycles, about 0.5 to 1.3 ms) is not worth -0.085 pt of lines. Rejected.
- **Fusing the term-list iteration into retain/setbits/tobits** (16 M elements). It needs the same post-segment helper plus three new arms (+15 to 20 lines) for well under 1 ms. Rejected without building.
- **`line()` with one ids iterator** (Box<dyn Iterator>, bits via `successors`). -4 lines, but +16 M Ir in the answers phase from dyn calls and boxes. Net ≈ 0. Rejected.
  - The formatting itself (core::fmt, about 40 M Ir) is too small for a hand-written itoa to pay for its lines.
- **Single-pass query split** (split on bytes ≤ 0x20, dispatch on the first byte). +2 lines, -5.6 M Ir. The parse is bound by insert misses and word-end mispredicts, not by byte loops. About +0.02 pt. Rejected.
- **AND with the smaller side second** (b·16 < a). This happens 3,511 times (1,961 bitmap builds over 5.1 M elements, 1,550 merges). A probe of the small side costs about as much as the current bitmap retain. Not pursued.
- **`std::fs::write` of the collected chunks in place of BufWriter.**
  - Measured: -4 lines, and it would save 197 of the 198 `write` syscalls.
  - Rejected for memory: `collect::<String>()` builds a new 1.46 MB buffer, plus realloc copies, at the end of the run. RESEARCH's timeline puts the anonymous-memory peak there ("answers done" 41.1 MiB, above "scan done" 38.9).
  - The chunk strings live in the worker arenas, and freeing them does not return pages to the OS.
  - Likely +1.4 to +2 MiB at the peak, which is -0.14 to -0.2 pt against about +0.05.
  - `write_all` per chunk would mean 782 syscalls. Keep BufWriter.
- **Deleting `mallopt(-3, 64 << 20)`.** Not a no-op: glibc 2.39 accepts it (returns 1). It keeps any answer allocation of 64 KB or more, for example a class-2 list (BOOL term that is also a phrase anchor, which can have any df), on the heap instead of mmap/munmap per call. Keep.
- **Memory.** Capped (section 0). For example, sizing `count` to `lists` instead of `nkeep + 1` saves nothing: the untouched calloc pages are never charged.

## 4. Hypotheses for the board only (not ranked; the board punished memory-behaviour tweaks before)

1. **Scatter scales badly.** The VM shows 37 ms on 1 CPU and 25 ms on 4 CPUs. The CPU part is about 9 ms on 4 CPUs, so roughly 14 ms is non-CPU. Candidates, all with measured counts:
   - 384 munmaps of 68 KB with 11-thread TLB shootdowns
   - about 5.3 k first-touch faults on the 21.9 MB `post` (PR_SET_THP_DISABLE is on; the VM runs THP=madvise, the board probably `always`)
   - serial `seg`/`start` fill: 641 k strided reads of 12 count arrays, about 3 M Ir
   Check on the board with `perf stat -e page-faults,tlb:tlb_flush` or by timing the phase. Batching releases is on the dead list ("per-thread page release"), so no fix is proposed.
2. **Answers phase: 25 ms on the board against about 10 ms of compute.** Likely memory traffic: 100 MB of `post` reads for 50 M list elements, plus about 0.5 GB of bitmap memset/copy/scan, against a 24 MB L3 that holds neither post (21.9 MB) plus bits (11.8 MB). Item B cuts the scan part. A real fix means not materializing term lists, which costs lines (section 3).
3. **Barriers.** on_threads is crossed 4 times, 8 barriers in all, each a Condvar wake of 11 threads, about 50 µs. Under 0.5 ms in total.

## 5. Notes

- **Pre-existing edge case, not a regression.** A docs.txt shorter than 12 bytes panics in the cut loop (`text[*c - 1]` with c = 0), in the old and new code alike. The fuzz caught it with a single empty document "0\t\n". The spec's sizes make it irrelevant.
- **How to verify the bundle.**
  1. Apply /tmp/claude-0/bpost/bundle.diff.
  2. Check the hash f86973c2 on full seed 1, and seeds 2, 3 and 7 if available.
  3. Run an interleaved A/B on the VM (scatter and answers phases), then one board run.
  4. Expect S 562 and M at about 43.6 MiB (no new allocations; not measured here).
  5. If the board shows no time gain, the 14 lines alone are still worth +0.08 pt.
- **Fuzz details.** 28 random corpora of 1 to 30,000 docs, seeds 1 and 2 of /tmp/claude-0/bpost/fuzz.py, run under valgrind on the final build. 27 match the old binary byte for byte. Case 4 of seed 2 is the single-empty-document file, where both panic identically.
