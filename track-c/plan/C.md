# Track C review: new ways to raise the score (base `origin/speed-demon` 8eb3624)

Reviewer: Track C agent. Research only: nothing in the repo was edited, committed or pushed, and nothing touched /data or /out.
Every build is a copy under `/tmp/revc/` with its paths pointed at copies of the inputs under `/tmp/revc/`.
Measurements are callgrind instruction (Ir) and branch-sim counts plus output hashes. No wall-clock timings were taken.
Local compiler: gcc 13.3 (the board uses gcc:15) with the Dockerfile's flags `-O3 -march=x86-64-v3 -mpclmul -mbmi2 -std=c++20 -pthread -static`.
clang 18 was used as a check that the codegen holds up.

Board now (run-b47f848afb8a): T 0.026457 s, M 8,900,608 B, S 373, 80.55 pts.

Derived reference values (not measured): with Tref 14.44 s and Sref 108 (from docs/EXPERIMENTS.md), the board's 80.55 implies Mref ≈ 370 MB.
That puts the memory cap of 50 at about 7.4 MB (7.06 MiB), so memory still has about 0.8 pt of headroom.

## Ranking (expected net points, after line cost)

| # | Idea | Lines (clang-format LLVM, my count) | Expected net | Status |
|---|---|---|---|---|
| 1 | **Parse instruction cut**: six small exact changes in one bundle | 367 -> 365 (−2) | **+0.5 to +1.1** (estimate from a measured −13.6% Ir) | built, hash-checked, fuzzed |
| 1b | Option inside 1: min/max as rarely taken branches | 0 | +0 to +0.07, depends on codegen | A/B it on gcc 15 |
| 2 | Dense name ids (smaller bitsets, M −0.9 MB) | about +8 | +0.2 to +0.3 (estimate) | not built |

All other ideas fell under +0.2 pt or measured worse. They are listed at the end so nobody redoes them.

---

## 1. Parse instruction cut (bundle)

**Ready to apply.** `/tmp/revc/fin_nm.cpp` is the bundle without 1b, and `/tmp/revc/fin.cpp` is the bundle with 1b.
The diff against `track-c/src/main.cpp` is at the end of this file. In it, the paths are already back to /data and /out.

### Measured (callgrind; full seed-1 data copied to /tmp/revc/full; valgrind runs 4 threads)

| Build | Total Ir | parse_records Ir | parse Ir / record | Mispredicts (branch-sim) | sha256 |
|---|---|---|---|---|---|
| base 8eb3624 | 955.7 M | 941.9 M | 283.7 | 2,517,528 | d8088b1c... |
| bundle (fin_nm) | 826.1 M (**−13.6%**) | 812.5 M (−13.7%) | 244.7 | 2,499,700 (−0.7%) | d8088b1c... |
| bundle + 1b (fin) | 819.5 M (−14.3%) | 805.9 M (−14.4%) | 242.7 | 2,509,156 | d8088b1c... |

clang 18 on the 20 MB slice (base 109.04 M Ir): the bundle measures 98.73 M (−9.5%), and the bundle with 1b measures 99.77 M (−8.5%).
So the bundle keeps its sign under a second compiler. 1b does not: it is worse under clang (see 1b).

### Components

Measured on the 20 MB slice (334,183 records; base parse_records Ir 95.82 M). "Alone" means applied to the base by itself; "in chain" means applied in the order listed.

| Component | Alone | In chain | Removes real work, or depends on codegen? |
|---|---|---|---|
| a. Drop the hash finalizer in `NameTable::key` | −3.70 M (−11.1 instr/rec) | −3.70 M | real work |
| b. Amount decoded by maddubs weights (dot gets weight 0), fused with the region into one 2-lane fold | −2.92 M (−8.7/rec) | −5.3 M (money weights −2.67 + fusion −2.59, minus overlap) | real work |
| c. `country_of` sentinel: drop the second `old.region < regions ?` test | −2.47 M (−7.4/rec) | −2.0 M | real work |
| d. Ring starts and ends with placeholder records, so `advance()` has no warm-up or drain tests | (with c) −4.37 M combined | −1.27 M | real work |
| e. `NameTable` / `NamesSeen` passed by value (stops the reloads of `slots_` and `bits` through references) | −0.71 M | −0.71 M | codegen |
| 1b. min/max as `__builtin_expect` branches | −0.65 M | −1.30 M | codegen (worse under clang) |

**a. Drop the finalizer** (lines 177-178 of `key`: `h = (h ^ h >> 31) * ...; h ^= h >> 32;`).
- Why the output stays identical: the finalizer is a bijection on u64 (xorshift, multiply by an odd constant, xorshift). So two names have equal keys with it exactly when they have equal keys without it. The collision odds of the 64-bit key are unchanged, and `h + (h == 0)` stays.
- What could change is only the slot spread. I checked it on all 100,520 distinct raw names of the full data, inserted in first-seen order, with the table weighted by each name's occurrences:

  | Hash | Groups probed per lookup | Lookups needing a second group | Longest probe |
  |---|---|---|---|
  | with the finalizer | 1.279 | 13.3% | 32 |
  | without it | 1.270 | 13.0% | 29 |

  The index is the top 17 bits of `lo*K1 ^ len + hi*K2`, and the top bits of a 64-bit multiply mix every input bit. The probe count on the slice is also unchanged (1.166 groups per lookup with the finalizer, 1.160 without).

**b. The money fold.**
- What changes: `fold` now decodes two numbers in one xmm. Lane 0 is the region's digits, lane 1 is the amount's 8 bytes. maddubs gets per-byte weights (10,1)(10,1)(1,0)(10,1) in lane 1, so the dot at byte 5 drops out with weight 0. The final madd uses (1000,1) instead of (10000,1), which gives cents = int*100 + frac directly.
- What it replaces: the scalar `(w & 0xFFFFFFFFFF) << 8 | (w & 0xFFFF000000000000)` shuffle (9 instructions per record by callgrind) and a second and/maddubs/madd/packus/madd chain.
- `digits()` (id, `region_line`) calls `fold(s, e, e, e).first`. KEEP[0] = 0 zeroes lane 1.
- Why it is correct: spec 2.1 says `amount = -?DIGIT+ "." 2DIGIT`, and 2.2 says the range is −999.99..99,999.99. So after the sign there are at most 8 bytes and the dot is always third from the end. The old `money()` relied on exactly that already. Region ids are at most 7 digits (2.2).
- Tested: amounts 0.00, −0.05, 99999.99 and −999.99 all appear in the fuzz below.

**c + d. Sentinel and placeholders.**
- `country_of` is allocated `nregions + 2`. `country_of[nregions]` stays 0, so an unknown region still clamps to `regions` and maps to _UNMATCHED as before. That means the lookup in `advance()` is just `country_of[old.region]`.
- `country_of[nregions + 1] = 255` is a placeholder country no output line ever reads. The output loop only visits 0..codes.size(), and the country limit tightens from 255 to 254 (the spec has 240, section 2.3).
- `pending[]` and `marks[]` start filled with placeholders: region `regions + 1`, name key 1, marks {255, 0}. The drain pushes 2·LAG placeholders, so `advance()` has no `n >= LAG` or `n - LAG < last` tests and no `last` variable.
- Their sums go to `totals[255]` and their marks to bitset row 255. Both are allocated (256 rows) and never printed.
- Costs:
  - Key 1 takes one name-table slot (100,521 of 131,072).
  - Row 255 is not in the populated range, so it faults in lazily: about 4-8 KB.
  - About 4.6k extra atomic ORs in all (96 pieces × 48 placeholders), against 3.32 M real ones.

**e.** Passing two one-pointer structs by value stops gcc from reloading `slots_` and `bits` through the references after every store into `totals` or the ring. It costs 0 lines.

### Correctness checks done (all on inputs I created under /tmp/revc)

- **Full seed-1 data:** sha256 `d8088b1c...` for base, the bundle and the bundle with 1b.
- **Spec worked example (section 4):** byte-identical for base and bundle.
- **Differential fuzz against a Python oracle written from spec 2.1: 60 files, 100 runs.** The generated files had:
  - mixed CRLF and LF terminators and mid-file BOMs
  - quoted LFs, `""` escapes and a lone `"` in notes
  - 0 to 6 trailing spaces on names, and both forms of empty name and empty note
  - amounts at both range ends (0.00, −0.05, 99999.99, −999.99)
  - regions that hit, fall in holes, or lie above the table
  - CRLF `regions.csv` in every third case
  - files of 1 to 5,000 records, so with 12 threads many of the 96 pieces are empty

  Result: seeds 1-40 passed for base and bundle at the VM's default of 4 threads (40/40 each). Seeds 41-60 passed for the bundle with the thread count forced to 1 and to 12 (40/40 runs).
- **Thread count forced to 1, 2, 7 and 12 on the 20 MB slice:** output identical to base each time.

### Expected effect (estimate, not measured on any clock)

- **Time.** The board diagnostics in track-c/PERF.md split the run as: set-up 5.5 ms, streaming 6 ms, name table and marks 4 ms, field parse 14 ms. So about 18 ms is instruction-bound, and research measured IPC ≈ 3, with time following instruction count.
  - If it converts fully: −13.6% of 18 ms ≈ 2.4 ms, so T goes 26.5 → 24.0 ms, which is 25·log10(26.5/24.0) = **+1.07 pt**.
  - If only half converts under SMT: **+0.53 pt**.
- **Lines.** −2 formatted lines, about +0.02 pt.
- **Memory.** Unchanged, apart from about 8 KB for row 255.
- **Risk.** Low for a to d, which are exact and checked above. gcc 15 might already have done some of e by itself.
- **How to verify.** Build with gcc:15 in the Dockerfile image, check the sample and full hashes, run `perf stat -e instructions,cycles,branch-misses` on the laptop against base (interleaved), then do one board run.

### 1b. min/max as branches (optional, decide by A/B on gcc 15)

The change:

```
-        t.amount_min = std::min(t.amount_min, old.cents);
-        t.amount_max = std::max(t.amount_max, old.cents);
+        if (__builtin_expect(old.cents < t.amount_min, 0)) t.amount_min = old.cents;
+        if (__builtin_expect(old.cents > t.amount_max, 0)) t.amount_max = old.cents;
```

- **gcc 13:** −0.65 M Ir alone, −1.3 M in the chain, and −6.6 M on the full data (−0.7%).
- **clang 18:** +1.0% Ir.
- **Mispredicts.** A new running minimum or maximum per (thread, country) occurs about H(n) times, with n ≈ 1,150 records per thread per country on full data. That gives 2·7.6·241·12 ≈ 44k events in all, or 0.013 per record.
  - Callgrind's slice count (+12k) matches that model.
  - On the full data, the mispredict total of the bundle with 1b (2.509 M) is still below base.
- **Net if gcc 15 behaves like gcc 13:** +0.03 to +0.07 pt, 0 lines. Keep it only if gcc 15's `perf stat` shows fewer cycles.

---

## 2. Dense name ids so the bitsets shrink by 0.9 MB (estimate only, not built)

**Where the memory goes now (estimate from allocation sizes, not measured in a cgroup).**

| Item | Size |
|---|---|
| Bitsets, 241 rows × 16 KB, all populated | 3.95 MB |
| Name table | 1.05 MB |
| `country_of` | 1.0 MB |
| Per-thread totals | 0.2 MB |
| Page tables, stacks, binary and kernel | the rest of the 8.9 MB |

Name ids are slot indices spread over 0..131,071, but only 100,520 names exist. So every row's four pages are touched.

**The change.**
- **Slot layout.** A slot holds `(key & ~0x1FFFF) | id`. The id is dense, starts at 1 (so a slot is never 0), and comes from a per-thread block of 64 taken by `fetch_add(64)` on one global counter. That is about 1,600 contended operations in all, instead of 100k.
- **Lookup.** Compare `v & ~MASK` against `k & ~MASK`. On a hit, return `slots_[j] & MASK`.
- **Bitset layout.** Word-major: `bits[(id >> 6) * NC + c]`, with NC = codes.size() + 1. Ids below about 101.3k (100,520 plus 12 × 64 of block slack) then touch only the first 1,583 × 241 words, which is 3.05 MB.
- **Populate.** Populate only that range, using the spec's planning figure of 100,000 plus slack. Anything beyond it faults in lazily, so correctness never depends on the pool size.

**Effect (estimate).**
- M: 8.90 → about 8.00 MB (−10.1%), which is 0.2·50·log10(1/0.899) = **+0.46**.
- Time: +4 instr per record at lookup (vpand, and, reload, and), +1.6% Ir, so **−0.09 to −0.17**.
- Lines: about +8, so −0.07.
- Net: **+0.2 to +0.3**.

**Risks.**
- The key comparison drops from 64 to 47 bits. Collision odds go from about 3e-10 to about 3.5e-5 per run (100k names). That is still above the brief's 40-bit floor for B, but it is a real weakening.
- Board M swings by about ±1 MiB between runs of identical code (8.3 to 10.8 MiB in the PERF table), so one board run cannot confirm a 0.9 MB gain. Look at the min over several runs.
- The 100,000 used for populate is a "planning, not a guarantee" figure from spec 2.2. Going past it only costs speed.

Ranked second because it is the only memory lever I found above about 0.3 MB, and memory is still 0.8 pt below its cap.

---

## Investigated and dropped (do not redo)

| Idea | Finding |
|---|---|
| Three-lane ymm fold (id + region + amount in one 256-bit pass) | +0.54 M Ir against the 2-lane fold, because the inserts and upper-lane extracts cost more than the saved chain. Use 2 lanes. |
| Name and note byte counts packed into one u64 accumulator (exact: each half stays below 2^31 per spec 3) | +2.3 M Ir in the chain (worse codegen). |
| `-fno-tree-slp-vectorize` (gcc packs count, id_sum and the byte counts into a ymm add) | −0.5 M on the base, but +1.0 M on top of the bundle. |
| 8-slot (full cache line) name-table probe to cut the second-group mispredict | Second-group rate 13.5% → 6.7% by simulation, for +4-5 instr per lookup: break-even. |
| Probe order that keeps the second group in the prefetched line | Saves an L2 access on about 6.7% of lookups: below 0.1 pt. |
| `munmap` each consumed piece instead of `MADV_DONTNEED`, to free page tables (about 0.4 MB) | The strips kept mapped at piece boundaries keep almost every 2 MB page-table page alive. A newer kernel may free empty page tables on DONTNEED anyway (not checked; the board's kernel is unknown). Under 0.15 pt and uncertain. |
| Startup: static libgcc frame registration (`__register_frame_info` → `classify_object_over_fdes`, 0.6 M Ir) and `ios_base::Init` (pulled in by bits/stdc++.h) | About 0.05-0.1 ms serial. Worth under 0.1 pt; `-static-pie` or dropping iostream would cost lines or relocation work. |
| `number_countries` (32k atomic loads, 0.36 M Ir, serial inside call_once) | About 0.05-0.1 ms. Under 0.1 pt. |
| Rank-bitmap `country_of` (1 MB → about 0.34 MB) | +6-8 instr and a second dependent load per record. Its memory gain (+0.3) and time loss (−0.3) cancel. |
| id from the previous id plus a 2-digit delta (spec 2.2: steps of 1 to 11) | The comma search is still needed, and the fold is already about 6 instructions. No net gain. |
| Collect the distinct counts from row popcounts after the parse (plain `lock or`, no setb or add) | Saves about 4-5 instr per record but adds a serial or partitioned pass over 494k words, plus lines. About break-even. |

## Appendix: diff of the bundle (fin_nm) against origin/speed-demon:track-c/src/main.cpp

```diff
--- b.cpp	2026-11-01 13:03:24.989688481 +0000
+++ fin_nm.cpp	2026-11-01 13:18:58.999108345 +0000
@@ -74,32 +74,27 @@
 }
 
 
-// The value of the last n (1 to 8) of the eight digit bytes in v (the first
-// byte is the lowest): reduced to digit values, the bytes before them masked
-// to leading zeros, then folded in pairs, quads and halves.
-u64 fold(u64 v, size_t n) {
+// Two numbers, each from the eight bytes that end where it does (the first
+// byte is the lowest), reduced to digit values with the bytes before it masked
+// to leading zeros, then folded in pairs, quads and halves: lane 0 the 1 to 8
+// digits [s0, e0), lane 1 an amount [s1, e1) in cents, whose dot (third from
+// the end, at most 99999.99) is given weight 0.
+std::pair<u32, u32> fold(const char* s0, const char* e0, const char* s1, const char* e1) {
     static constexpr auto KEEP = [] {
         std::array<u64, 9> a{};
         for (int k = 1; k <= 8; ++k) a[k] = 0x0F0F0F0F0F0F0F0F & ~u64(0) << 8 * (8 - k);
         return a;
     }();
-    __m128i x = _mm_and_si128(_mm_cvtsi64_si128(v), _mm_cvtsi64_si128(KEEP[n]));
-    x = _mm_maddubs_epi16(x, _mm_set1_epi16(0x010A));
+    __m128i x = _mm_and_si128(_mm_set_epi64x(load8(e1 - 8), load8(e0 - 8)), _mm_set_epi64x(KEEP[e1 - s1], KEEP[e0 - s0]));
+    x = _mm_maddubs_epi16(x, _mm_setr_epi16(0x010A, 0x010A, 0x010A, 0x010A, 0x010A, 0x010A, 1, 0x010A));
     x = _mm_madd_epi16(x, _mm_set1_epi32(0x00010064));
     x = _mm_packus_epi32(x, x);
-    return _mm_cvtsi128_si32(_mm_madd_epi16(x, _mm_set1_epi32(0x00012710)));
+    x = _mm_madd_epi16(x, _mm_setr_epi16(10000, 1, 1000, 1, 0, 0, 0, 0));
+    return {_mm_cvtsi128_si32(x), _mm_extract_epi32(x, 1)};
 }
 
 // The value of the 1 to 8 decimal digits in [s, e).
-u64 digits(const char* s, const char* e) { return fold(load8(e - 8), e - s); }
-
-// An amount's value in cents, from its digits [s, e) with the dot third from
-// the end: at most 7 digits (99999.99), so the eight bytes ending at e, with
-// the dot's byte dropped, are one number.
-u64 money(const char* s, const char* e) {
-    u64 w = load8(e - 8);
-    return fold((w & 0xFFFFFFFFFF) << 8 | (w & 0xFFFF000000000000), e - s - 1);
-}
+u64 digits(const char* s, const char* e) { return fold(s, e, e, e).first; }
 
 // The first c at or after p (one is known to follow). 64 bytes at a time:
 // a note is often longer than 32, and the loop's exit is then predictable.
@@ -174,8 +169,6 @@
             if (len - i < 8) w &= (u64(1) << 8 * (len - i)) - 1;
             h = (h ^ w) * 0x94D049BB133111EBull;
         }
-        h = (h ^ h >> 31) * 0xD6E8FEB86659FD93ull;
-        h ^= h >> 32;
         return h + (h == 0);  // 0 marks a free slot
     }
 
@@ -239,7 +232,7 @@
 // word prefetched; LAG records after that the name is marked. So each of the
 // three random reads (region table, name table, bitsets) has a steady lead.
 void parse_records(const char* p, const char* end, const char* file_end, const u8* country_of,
-                   size_t regions, NameTable& names, NamesSeen& seen, Totals* totals) {
+                   size_t regions, NameTable names, NamesSeen seen, Totals* totals) {
     constexpr u64 LAG = 16;
     struct Pending {
         u64 name;
@@ -249,25 +242,24 @@
     struct Mark {
         u32 country, id;
     } marks[LAG];
-    u64 n = 0, last = UINT64_MAX;  // records started; all of them once known
+    u64 n = 0;  // records started; the ring starts and ends with placeholders
+    for (u32 k = 0; k < LAG; ++k) pending[k] = {1, u32(regions) + 1}, marks[k] = {255, 0};
     // Retires ring slot n % LAG.
     auto advance = [&]() __attribute__((always_inline)) {
         u32 k = n % LAG;
-        if (n >= 2 * LAG && n - 2 * LAG < last) totals[marks[k].country].distinct += seen.mark(marks[k].country, marks[k].id);
-        if (n >= LAG && n - LAG < last) {
-            const Pending& old = pending[k];
-            u32 c = old.region < regions ? country_of[old.region] : 0;
-            Totals& t = totals[c];
-            ++t.count;
-            t.id_sum += old.id;
-            t.amount_sum += old.cents;
-            t.amount_min = std::min(t.amount_min, old.cents);
-            t.amount_max = std::max(t.amount_max, old.cents);
-            t.name_bytes += old.name_bytes;
-            t.note_bytes += old.note_bytes;
-            marks[k] = {c, names.id(old.name)};
-            seen.prefetch(c, marks[k].id);
-        }
+        totals[marks[k].country].distinct += seen.mark(marks[k].country, marks[k].id);
+        const Pending& old = pending[k];
+        u32 c = country_of[old.region];
+        Totals& t = totals[c];
+        ++t.count;
+        t.id_sum += old.id;
+        t.amount_sum += old.cents;
+        t.amount_min = std::min(t.amount_min, old.cents);
+        t.amount_max = std::max(t.amount_max, old.cents);
+        t.name_bytes += old.name_bytes;
+        t.note_bytes += old.note_bytes;
+        marks[k] = {c, names.id(old.name)};
+        seen.prefetch(c, marks[k].id);
     };
 
     while (p < end) {
@@ -284,12 +276,10 @@
         // LF, if any.
         u32 lf = match32(p, '\n');
         const char* f4 = p + __builtin_ctz(match32(p, ','));
-        u64 region = digits(p, f4);
-        const char* amount = f4 + 1;
         const char* eol = lf ? p + __builtin_ctz(lf) : file_end;
-        const char* amount_end = eol - (eol[-1] == '\r');
-        bool negative = *amount == '-';
-        i64 cents = (negative ? -1 : 1) * i64(money(amount + negative, amount_end));
+        bool negative = f4[1] == '-';
+        auto [region, money] = fold(p, f4, f4 + 1 + negative, eol - (eol[-1] == '\r'));
+        i64 cents = (negative ? -1 : 1) * i64(money);
 
         // Region ids are below 10^7 (fit u32); one past the table means none.
         u32 r = region < regions ? u32(region) : u32(regions);
@@ -300,7 +290,7 @@
         names.prefetch(key);
         p = eol + 1;
     }
-    for (last = n; n < last + 2 * LAG; ++n) advance();
+    for (u32 i = 0; i < 2 * LAG; ++i) advance(), pending[n++ % LAG] = {1, u32(regions) + 1};
 }
 
 // 15-bit code of a three-letter country; numeric order is alphabetical order.
@@ -364,7 +354,8 @@
     // the last line holds the largest id.
     const char* last = static_cast<const char*>(memrchr(regions, '\n', rsize - 1)) + 1;
     const size_t nregions = atoll(last) + 1;
-    u8* country_of = zeroed<u8>(nregions);  // 0 = no such region
+    u8* country_of = zeroed<u8>(nregions + 2);  // 0 = no such region
+    country_of[nregions + 1] = 255;  // the ring's placeholders: a country no line shows
     std::vector<size_t> rcut = split(regions, strchr(regions, '\n') - regions + 1, rsize, nthreads, "\n");
     std::vector<std::atomic<u8>> present(1 << 15);
     std::array<u8, 1 << 15> index_of{};
@@ -373,7 +364,7 @@
     auto number_countries = [&] {
         for (u32 k = 0; k < present.size(); ++k)
             if (present[k]) codes.push_back(k), index_of[k] = codes.size();
-        if (codes.size() > 255) fputs("too many countries\n", stderr), _exit(1);
+        if (codes.size() > 254) fputs("too many countries\n", stderr), _exit(1);
     };
 
     // records.csv: skip the BOM and header, then split after CRLFs into eight
```
