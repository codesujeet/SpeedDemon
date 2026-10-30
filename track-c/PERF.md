# Track C: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements, not
board results: a laptop running Docker with the board's run flags on the full
public dataset, with the output hash checked against the published SHA-256.
Local runs swing 30-50%, so each row is a before -> after median from
interleaved or repeated runs, and rows are only comparable within themselves.
The **Run ID** column stays *pending* until the step has been measured on the
live board, and **Points** is only filled from board numbers. Nothing in this
file has run on the scoring box yet. Everything that was tried, including the
failures, is in [`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Quote-parity chunking: count quotes per chunk in parallel, prefix-sum, snap each boundary to the first LF outside quotes; each chunk parsed by its own worker straight off the input bytes; dense array for `region_id -> country` | pending | | | pending |
| 2 | Exact `names_distinct`: each country is owned by one worker, parse workers drop `(country, name slice)` into the owner's bucket, owners count distinct slices | pending | about 0.60 -> 0.30 | | pending |
| 3 | `mmap` the inputs | pending | 0.77 -> 0.52 (5 rounds) | 315 -> 119 | pending |
| 4 | Exact distinct names counted by sorting each group's `(hash, name)` pairs and comparing bytes only inside runs of equal hashes, instead of one big hash set per owner | pending | parse+aggregate phase 234 -> 170 ms (9 interleaved rounds) | 123 -> 141 | pending |

Current full-data local runs (Linux container, harness replica): about 0.21 s, about 141 MB.

## Where the effort went

The risk in this track is correctness, not speed: a BOM is skipped only at
the start of the file or right after a terminator; CRLF and LF end a record
only outside quotes; an LF inside quotes is data; `""` is one quote;
lengths are counted unescaped; money is integer cents and never prints
`-0.00`. Those cases are tests, and the output matches the published full
hash. Parallel parsing is safe because every quote character is either a
delimiter or half of an escaped pair, so the parity of quotes before an
offset says whether that offset is inside a quoted field.

The first distinct-name design interned names per worker and merged the
sets; the merge alone cost 200-280 ms, which is why ownership by country
replaced it (step 2).

## Why step 4 worked

The stage profile showed parse about 75-155 ms, the quote-parity bounds 14 ms,
and the distinct-name stage 150-215 ms, the largest. In the full data 3.32 M
records hold 2.98 M distinct (country, name) pairs, so only 10% of inserts
are duplicates and the hash sets grew to millions of entries, with random
inserts into tables of several MB per owner. A group has only about 14 k
names, so its `(hash, name)` array stays in cache while it is sorted. A
fingerprint-only set (not exact, so not kept) was the upper bound: 266 -> 177
ms; the exact sort recovers most of that.

## Caveats

* Nothing here has run on the scoring box.
* Step 1's and the baseline's local timings were not recorded in a form I
  would cite, so those cells are left empty rather than reconstructed.
