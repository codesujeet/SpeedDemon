# B scan review (Track B, `origin/speed-demon` 8eb3624): classes / hash / find / tokens / walk / scan closure / sample

Scope: the corpus scan only. Scatter and answers are covered by the other agent. The interfaces they use are unchanged
(`count`, `buckets`, `found`, `ids`, `seg`/`start` meaning; see "Interface" below).

## TL;DR

- I built every idea in a private copy (`/tmp/bscan/*`, paths moved to `/tmp/bscan/data`), checked that the output was byte-identical
  against the unmodified code, and measured instructions with callgrind. No timing runs.
- **Primary board slot = `core`** (`plan/B_scan_core.rs`): ideas #1 + #2 + #3 below, with the original boundary loop.
  - Scan-closure inclusive Ir 574.6 M → **414.2 M (−27.9%)** on a 30 MB real-data prefix (4.32 M tokens): **133 → 96 per token**.
  - +21 formatted lines (rustfmt, non-blank: 575 → 596; the base count of 575 matches the board's S 576).
  - Estimate: 0.5–0.8× of the Ir cut shows up as time, on a board scan of about 100–115 ms. That is −14 to −26 ms of 167.9 ms,
    **+0.75 to +1.6 pts net**.
- **Follow-up A/B on top of `core` = #4** (start/end-mask boundaries). The all-in reference is `min` (`plan/B_scan_min.rs`).
  - 385.3 M (−33% vs base, **89 per token**; −6.7 per token vs core), and −3 lines vs core (593).
  - Estimate +0.25 to +0.45 more, but its cousin "straddle-branch removal" lost wall time, so measure it alone.
- Memory (M axis) is effectively unchanged. Per thread, `count` + `last` shrink from (nkeep+1)×4 B to lists×4 B: about −60 KB per
  thread, −0.7 MB in all. Add 150 KB for the shared `info` table and +4 B per anchored row.
- Both `core` and `min` were byte-identical to the base build (md5) on the 30 MB and 130 MB real-data prefixes with the full real
  `queries.txt`, and on 12 synthetic fuzz seeds. The fuzz covered tokens of 1–70 bytes including ≥64-byte runs, upper case,
  non-ASCII separators, empty and token-less docs, and phrases across windows. The implementer must still confirm
  `f86973c2...` on the full data in the real image.
- **2x is not reachable from the scan alone with this design.** About 30–35% fewer scan instructions is the realistic ceiling. What
  remains per token is mostly inherent: the hash → find → state dependency chain, the find-loop branch, and the loop-exit
  mispredict once per 64-byte window. The leaders' 74 ms also needs the serial prefix, the scatter and the answers to shrink.
- Outputs were identical (md5 against the base build) on the 30 MB and 130 MB real-data prefixes, with the full real
  `queries.txt`, and on 12 synthetic fuzz seeds: tokens of 1–70 bytes, upper case, non-ASCII separators, empty and token-less
  docs, phrases across windows. The implementer must still confirm `f86973c2...` on the full data in the real image.

## Measured ladder (cumulative, scan-closure inclusive Ir, 30 MB prefix of the real docs + full real queries)

| step (cumulative) | scan Ir | Δ per token | program Ir |
|---|---|---|---|
| base 8eb3624 | 574.6 M (133.0/token) | | 1007.8 M |
| 1. 16-byte masked mum hash | 505.0 M | −16.1 | 934.1 M |
| 2. start/end-mask token boundaries | 481.3 M | −5.5 | 909.6 M |
| 3. `find` returns `id-1` with none = `u32::MAX` (no `min`); closure state rework (intermediate u32 state word) | 433.1 M | −11.2 | 865.3 M |
| 4. read-only `info: Vec<u16>` replaces per-token `last`; list dedup moved to the kept loop | 408.0 M | −5.8 | 840.8 M |
| 5. hoist raw pointers out of the closure loop (it was reloading `info`/`kept`/`anchors` from the stack) | 395.0 M | −3.0 | 830.1 M |
| (linear hash buffer instead of the ring: rejected) | 395.7 M | +0.2 | 828.3 M |
| 6. inline phrase neighbour reject + 17–32-byte path folded into the same hash | 391.4 M | −0.9 | 825.6 M |
| **`core`** = `min` without step 2 (original boundary loop) | **414.2 M (95.9/token)** | **−37.1 total** | 851.1 M |
| **`min`** = 1+2+3(final form)+4+5+6, ring kept, sample marks back to u16 | **385.3 M (89.2/token)** | **−43.8 total** | 819.8 M |
| (fused per-token action into `tokens()`: rejected, see end) | 373.4 M | −2.8 vs min | 807.3 M |

Method notes:
- These are scan-phase numbers, not program totals, because valgrind counts each `rep stosb` byte. The sample briefly used u32
  marks, and that alone moved the program total by about 16 M. `min` uses u16 again.
- The 30 MB prefix exaggerates fixed per-thread costs. On full data the base scan is about 126 per token (82 `tokens()` + 44
  closure, per the brief), so expect about 126 → 82–85 there. Instruction counts per token are size-independent; time is not.

## Ranked ideas

All of them modify `tokens()`, `hash()`, `Terms::find` and the scan closure. Land #1 + #2 + #3 together as `core` (one board slot),
then A/B #4 on top of it (`min`). I list them by expected net points. For the time→points conversion: board scan ≈ 100–115 ms; time effect = 0.5–0.8
of the Ir cut (discounted, because removing the state prefetch earlier cut Ir 1.6% and cost 2–3% of time); 1% of T ≈ +0.11 pt;
1 formatted line ≈ −0.009 pt.

### 1. Read-only per-term `info` table; dedup only list terms, in the kept loop (steps 3–5): about +0.4 to +0.8 pt

**What changes.** `Terms::find` (line 92), the sample closure (lines 397–404), the `info` build after the renumber (after line 440),
the scan-closure setup (lines 447–459) and the token loop (lines 466–481), the kept loop (lines 493–496).

**Why.** The current closure loop is about 38 instructions per token in the asm. It has 8 stack reloads, since 14+ live values do
not fit in registers. Most of the work is useless for most tokens:
- `last[i]` is read and written for **every** token, although only list terms (29.7 + 0.8% of tokens) need dedup;
- the `min`/range compares on `nbits`, `a0`, `lists`, `nkeep` are recomputed per token.

Python over the real data: bitmap terms are 45.4% of tokens, list terms 29.7%, bool+anchor 0.8%, anchor-only 1.5%, phrase-only 1.7%,
non-terms 20.8%. List terms almost never repeat inside a document (10.78 M tokens vs 10.70 M pairs), so the moved dedup branch is
nearly always predicted.

**Sketch** (as built in `plan/B_scan_min.rs`):
```rust
// find: return id-1, empty slot -> u32::MAX (drops cmp+cmov)
return ((s & ID_MASK) as u32).wrapping_sub(1);
// after renumber: one u16 per term, index 0 = "not a term"
let info = |r: usize| match r {
    r if r < nbits => r as u16 + 8,                // pending index (8.. = bitmaps)
    r if r < a0 => 1 << 15 | (r & 7) as u16,        // list
    r if r < lists => 3 << 14 | (r & 7) as u16,     // list + anchor
    r if r < nkeep => 1 << 14 | (r & 7) as u16,     // anchor only
    _ => (r & 7) as u16,                            // phrase-only
};
let info: Vec<u16> = [0].into_iter().chain((0..nterms).map(info)).collect();
// scan: pending has 8 dummy words in front; flush iterates pending[8..]
let (mut count, mut last) = (vec![0u16; lists], vec![0u16; lists]);
let (share, pre) = (&text[cut[t]..cut[t + 1]], info.as_ptr());   // tokens() prefetches info[id+1]
...
let (pending, info) = (pending.as_mut_ptr(), info.as_ptr());
let (ap, kp) = (anchors.as_mut_ptr(), kept.as_mut_ptr());
for (at, &id) in toks.iter().enumerate() {
    let v = *info.add(id.wrapping_add(1) as usize);
    *pending.add((v & 0x3FFF) as usize) |= bit;
    (*ap.add(na), na) = (at, na + (v >> 14 & 1) as usize);
    (*kp.add(n), n) = (id, n + (v >> 15) as usize);
}
...
for &i in &kept[..n] {
    if std::mem::replace(&mut last[i as usize], mark) != mark {
        buckets[i as usize % BUCKETS].add(i / BUCKETS as u32, off);
        count[i as usize] += 1;
    }
}
```
The token loop is now about 14 instructions per token (two-way unrolled by rustc), with no stack reloads. Non-bitmap tokens OR into
one of 8 dummy words (`r & 7`), so their read-modify-write chain is spread over 8 addresses instead of one. Before, every one of
them hit `pending[nbits]`. The sample indexes `df`/`last` by `id + 1` (u16 marks; about 650 sample docs per thread) and returns
`df.split_off(1)`.

**Why the output is identical.** S(t) counts documents (spec §3), so deduplicating a list term at kept time, once per (term, doc),
gives the same postings as deduplicating per token. Bitmap OR is idempotent. Phrase-only and non-terms touch only dummies.
`toks` still holds the exact term ids, with none = MAX, which never equals a phrase id, so the phrase compare is unchanged.

**Interface for the scatter/answer agent.** `count` is now sized `lists`. The scatter reads `c.0[i]` only for `i < lists`.
`buckets`, `found` and `ids` keep their meaning. `nkeep` stays in use only for building `info` and `astart`.

Cleanup when landing: `nkeep` is no longer used after the block, so drop it from the destructuring tuple (rustc warns).

**Risks.**
- (a) Per-thread `last` is no longer prefetched. The prefetch moved to the shared read-only `info`, so the kept loop's `last[i]`
  loads (106 KB per thread) are demand loads. They are independent, so OoO should overlap them.
- (b) The bitmap pending index must stay below 16384 − 8. Today `nbits` = 369; by construction `nbits ≤ 32 × distinct tokens per
  sampled doc`, about 4 k. A `debug_assert!` would compile out in release, so either rely on that bound or pay one line for
  `assert!(nbits < 16376)`.
- (c) Same u16 mark-wrap assumption as today (about 20.8 k docs per share).

**Estimated effect.** −20 instructions per token (−15% of scan Ir) → 7.5–12% of scan time → +0.5 to +0.9 pt; about +10 lines →
−0.09; **net +0.4 to +0.8**.

**Verify.** Full-data hash `f86973c2`; perf stat cycles/instructions on the board image, A/B against base.

### 2. 16-byte masked multiply-fold hash (step 1, folded in step 6): about +0.3 to +0.65 pt

**What changes.** `hash()` (lines 70–79) and the `len <= 32` test in `tokens()` (line 163). `Terms::insert` uses the same function,
so query terms and corpus tokens stay consistent.

**Why.** The current hash is 27 instructions in the asm: a 32-byte load and mask, four lane extracts, two rotates, three
multiplies and three `movabs` constants that are re-materialised per token. Python over the real data: 79.9% of tokens are
≤ 8 bytes, 99.99% are ≤ 16, and only 3,969 of 36.3 M are 17 bytes (the longest). Spec §2.2 says no generated token is longer
than 32 bytes. So a 16-byte, one-multiply hash covers nearly every token, and 17–32 gets a second part behind a branch that is
never taken.

```rust
const K1: u64 = 0x9E37_79B9_7F4A_7C01; // low byte 0x01: lo ^ K1 != 0 (byte 0 is always a token char)
const K2: u64 = 0x01B2_AE3D_27D4_EB4F; // high byte 0x01: hi ^ K2 != 0 (byte 15 is 0 or a token char)
unsafe fn hash(p: *const u8, len: usize) -> u64 {
    static KEEP: [u128; 2] = [!0, 0];
    let part = |q: *const u8, n: usize, k: u64| {
        let mask = _mm_loadu_si128(KEEP.as_ptr().cast::<u8>().add(16 - n).cast());
        let v = _mm_and_si128(_mm_or_si128(_mm_loadu_si128(q.cast()), _mm_set1_epi8(0x20)), mask);
        let m = (_mm_cvtsi128_si64(v) as u64 ^ k) as u128 * (_mm_extract_epi64::<1>(v) as u64 ^ K2) as u128;
        m as u64 ^ (m >> 64) as u64
    };
    match len {
        0..=16 => part(p, len, K1),
        17..=32 => part(p, 16, K1) ^ part(p.add(16), len - 16, K1 ^ 1 << 40).rotate_left(32),
        _ => 0, // > 32 bytes cannot equal any term (spec §2.3: term = 1*32 chars)
    }
}
```
The asm is now `lea/sub` for the mask address, `vpor`, `vpand`, `vpxor`, `vmovq`, `vpextrq`, `mulx`, `xor`: 9 instructions,
against 27.

**Safety.**
- Both factors are provably non-zero, so the 128-bit product is never 0.
- All 64 bits are used: the slot index is the low 18 bits, the tag the high 46. The tag stays at 46 bits, above the 40-bit floor.
- The 16-byte load reads at most as far as today's 32-byte load. `map()` already over-maps by 4096 bytes.
- Python collision check (an exact re-implementation): see "Hash check" below.

**Estimated effect.** −16.1 instructions per token (−12% of scan Ir); the sample pass and the 263 k query inserts also get the
cheaper hash. 6–10% of scan time → +0.4 to +0.73 pt; about +8 lines → −0.07; **net +0.3 to +0.65**.

**Verify.** Full-data hash plus the Python collision check on the holdout's distribution (synthetic stand-in below).

### 3. Inline phrase-neighbour reject (part of step 6): about +0.1 to +0.3 pt (estimate, marginal; land it with #1)

**What changes.** The `anchored` build (line 417), its renumber (line 436) and the phrase loop (lines 482–492).

```rust
let nb = if j + 1 < to { j + 1 } else { j - 1 };
anchored.push((a, q as u32, j - from, nb - from, ids[nb as usize]));
... anchored.iter_mut().for_each(|a| (a.0, a.4) = (new[a.0 as usize], new[a.4 as usize]));
... for &(_, q, j, nb, id) in &anchored[astart[k]..astart[k + 1]] {
        if toks.get((at + nb as usize).wrapping_sub(j as usize)) != Some(&id) { continue; }
        /* old spans/ids slice compare */
```

**How this differs from the dead "phrase first-id reject".** That one still had to load `spans[q]` and `ids[...]` (600 KB and
1 MB tables, random access) to get the first id. This one stores the neighbour id inside the 20-byte `anchored` row, so the 96% of
candidates that fail (RESEARCH_B: 987 k of 1.03 M fail on one id) never touch `spans`/`ids`.

**Evidence.** Callgrind cache simulation (`--D1=49152,12,64 --LL=1310720,10,64`, i.e. a 1.25 MB L2 standing in for LL;
single-threaded simulation; prefetches ignored) on the 30 MB prefix: simulated L2 read misses 4.047 M → 3.700 M (−351 k, −8.6%).
Instructions −0.8 per token. The time effect is an estimate: about 3 M misses at full scale, at about 60 cycles each where they do
not overlap, is at most about 5 ms of board wall time; I would bank 1.5–3 ms.

**Why the output is identical.** The check is a necessary condition for the slice compare that follows.

**Risk.** The previous attempt measured 0% on 4 CPUs. Bundle this with #1 rather than spending a board slot on it alone.

### 4. Start/end-mask boundary loop (step 2): about +0.1 to +0.25 pt, separate A/B only

**What changes.** The `while m != 0` loop and the straddle test in `tokens()` (lines 147–169).

```rust
let w = word & nl.wrapping_sub(1) & !nl;                       // bytes before the first LF
let (mut starts, mut ends) = (w & !(w << 1), w & !(w >> 1 | 1 << 63));  // bit 63 is never a known end
while ends != 0 {
    let s = starts.trailing_zeros();
    let len = ends.trailing_zeros() + 1 - s;
    (starts, ends) = (starts & starts.wrapping_sub(1), ends & ends.wrapping_sub(1));
    push(hash(p.add(s as usize), len as usize));
}
let mut next = starts.trailing_zeros().min(64) as usize;      // the leftover start is the straddler
if next == 0 {                                                // >= 64-byte token: cold
    next = (0..).find(|&i| !(*p.add(i)).is_ascii_alphanumeric()).unwrap();
    push(hash(p, next));
}
```

**How this differs from the dead "straddle-branch removal".** That idea still tested every token (`len == 64`) and cut the window
mask with lzcnt. Here there is **no per-token straddle test**: a token touching bit 63 simply has no end bit, so the loop ends one
token early, and the leftover start is resolved once per window. The per-token path is `tzcnt, tzcnt, sub, inc, blsr, blsr,
test/jne`.

**Why the output is identical.** Tokens are maximal runs (spec §2.2), and a window always starts at a separator or at a token start.

**Evidence.** Measured: −5.5 instructions per token on top of the hash change alone, and −6.7 per token (414.2 → 385.3 M) on top of
`core`. It also saves 3 formatted lines (596 → 593). Because the earlier cousin lost wall time on the VM while cutting instructions,
treat the time effect as unknown until an A/B of `min` against `core` shows it.

### Dropped (below the +0.2 bar after line cost, or measured worse)

- **Fused per-token action** (do the `info`/pending/kept work inside `tokens()` at lookup time, with no second pass over `toks`):
  −2.8 instructions per token over `min` (−3.1% of scan Ir) for +49 formatted lines. It also needs unchecked `kept`/`anchors`
  writes before the doc length is known (the real max is in the hash-check section below). Net about +0.1–0.2 − 0.44 → **negative**.
- **Linear hash buffer instead of the LAG ring**: +0.7 M scan Ir (worse than the ring once the rest is in place).
- The u32 state word with a narrow mark store: rustc turned the mark write into a 16-bit store over a 32-bit field, a
  store-forwarding hazard on repeated terms. The read-only `info` design (#1) avoids it and is fewer instructions anyway.
- **Not tried, too small:** a pshufb-LUT `classes()` (about −6 instructions per 64-byte window ≈ −0.7 per token); removing the
  `n >= LAG` branch by pre-filling the ring (+8 junk lookups per doc eat most of the gain).

## What is left per token in `min` (≈ 89 Ir/token on the prefix; full data ≈ 82–85)

From the asm and the callgrind lines:
- **`tokens()` per token, about 44:** boundaries 8 (two of them are rustc's `xor` dependency breakers before `tzcnt`), hash 9 + 2
  (length test), slot prefetch and ring 9, find about 10 at 1.16 probes, id store and `info` prefetch 4.
- **Per 64-byte window, about 6 per token:** `classes()` 26 instructions, plus bookkeeping and `reserve`.
- **Closure loop:** 14.
- **Kept loop:** dedup + `Units::add` + `count`, about 20 per kept token, about 6 per token.
- **Phrase:** about 1.5 Ir per token, plus the remaining `anchored`-row misses: 191 k simulated L2 misses on the prefix, from the
  `astart` → row chain.
- **Per doc / flush:** about 1.5.

Inherent costs: the find loop's empty/mismatch branches (21% of tokens are non-terms, about 16% of lookups probe more than once:
unpredictable), one loop-exit mispredict per window, and the hash → slot → id → info dependency chain that the LAG-8 ring hides.
The dead list already covers the obvious attacks on these (SIMD bucket probe, branchless find, table size).

## Hash check

This is an exact Python re-implementation of the hash in #2 (OR 0x20, zero padding, the K1/K2 XOR, a 128-bit product folded hi ^ lo,
and the rotated second part for 17–32 bytes). It asserts that both factors are non-zero for every input. Source:
`/tmp/bscan/py/hashcheck.py`; output: `/tmp/bscan/py/hashcheck.txt`.

| set | items | full-64-bit collisions | non-terms whose 46-bit tag equals some term's tag |
|---|---|---|---|
| real: all distinct corpus tokens + all 75,747 query terms | 609,889 | 0 | 0 |
| synthetic holdout stand-in: 2.63 M random 3–12-letter words, 80 k of them as "terms" | 2,632,994 | 0 | 0 |
| every 1–4-char [a-z0-9] string + 0..10^6 decimal + zero-padded numbers | 2,860,462 | 0 | 0 |
| random 13–32-byte [a-z0-9] tokens (the 17–32 path) | 500,000 | 0 | 0 |

The tag test is stricter than the table needs, since it ignores probe position. Also from the same pass: max tokens per doc = 1,076,
max line = 7,616 bytes, max token = 17 bytes, max query term = 17 bytes. `min` keeps the existing `kept`/`anchors` growth
(`resize` to `toks.len()`) and the per-window `reserve(33 + LAG)`. A 64-byte window holds at most 32 complete tokens plus one
straddler, and the ring adds at most LAG, so it has no new bounds assumptions. Only the dropped fused variant would have needed
a fixed bound.

## Files

- `plan/B_scan_core.rs`: the primary slot, #1 + #2 + #3 (paths point to `/tmp/bscan/data` and `/tmp/bscan/out`; restore
  `/data/...` and `/out/...`).
- `plan/B_scan_min.rs`: `core` + #4, the all-in reference for the follow-up A/B.
- `plan/B_scan_fused.rs`: the rejected fused variant, for reference.
- Private build dirs: `/tmp/bscan/{base,min,new,new3..new7}`; callgrind outputs `/tmp/bscan/cg_*.out`; fuzz generator
  `/tmp/bscan/py/fuzz.py`; token/class statistics `/tmp/bscan/py/stats_full.txt`.
