# Track C: performance log

Every step from the reference to the current version.

**How to read the numbers.** The T and M columns are local measurements: a
laptop running Docker with the board's run flags on the full public dataset,
with the output hash checked against the published SHA-256. Local runs swing
30-50%, so each row is a before -> after median from interleaved or repeated
runs, and rows are only comparable within themselves. The laptop is slower
than the scoring box: the version of steps 1-4 (commit `db4cc76`) measured
**0.135 s, 141 MiB, 543 lines, K 9.75, 49.44 points on the live board**
(run `run-cf24ab5675f5`) and about 0.21 s, 141 MB on this laptop. The **Run
ID** column stays *pending* for steps without their own board run, and
**Points** is only filled from board numbers. Step 5 has not been on the
board yet. Everything that was tried, including the failures, is in
[`EXPERIMENTS.md`](../docs/EXPERIMENTS.md).

| Step | Change | Run ID | T (s), local | M (MB), local | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Quote-parity chunking: count quotes per chunk in parallel, prefix-sum, snap each boundary to the first LF outside quotes; each chunk parsed by its own worker straight off the input bytes; dense array for `region_id -> country` | pending | | | pending |
| 2 | Exact `names_distinct`: each country is owned by one worker, parse workers drop `(country, name slice)` into the owner's bucket, owners count distinct slices | pending | about 0.60 -> 0.30 | | pending |
| 3 | `mmap` the inputs | pending | 0.77 -> 0.52 (5 rounds) | 315 -> 119 | pending |
| 4 | Exact distinct names counted by sorting each group's `(hash, name)` pairs and comparing bytes only inside runs of equal hashes, instead of one big hash set per owner | `run-cf24ab5675f5` (commit `db4cc76`) | parse+aggregate phase 234 -> 170 ms (9 interleaved rounds) | 123 -> 141 | 49.44 (board) |
| 5 | A record leaves one `u64` in its country's list (top 32 bits of the hash of the name, offset of the name field) instead of a 12-byte reference plus a 24-byte `(hash, name)` pair; equal-hash runs are compared by re-reading the field. Hashing is done at parse time, when the name is in cache | pending | 0.265 -> 0.208 (11 rounds) | 140 -> 47 | pending |

Step 5 measured the board's way (5 runs, mean of the middle three, highest
memory peak) on the same laptop: **0.139 s and 47 MB, against 0.213 s and
141 MB** for the committed version, and 531 source lines against 552 (the
harness's own count).

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

## Why steps 4 and 5 worked

The stage profile showed parse about 75-155 ms, the quote-parity bounds 14 ms,
and the distinct-name stage 150-215 ms, the largest. In the full data 3.32 M
records hold 2.98 M distinct (country, name) pairs, so only 10% of inserts
are duplicates and the hash sets grew to millions of entries, with random
inserts into tables of several MB per owner. A group has only about 14 k
names, so its sorted array stays in cache (step 4).

Step 5 shrinks what is kept per record from 36 bytes to 8. Escaping is one to
one (a raw quoted field has every quote in a pair), so equal names have equal
raw bytes and the hash can be taken from the raw field without unescaping.
Only entries with equal hashes can be equal names, so the cold reads of name
bytes happen only for real duplicates (about 10%), and a collision in 32 bits
costs one comparison instead of a wrong answer.

## Tried and dropped

* **A shared dictionary of names with a number per distinct name** (64 and
  then 256 mutex-protected shards, padded to cache lines), so a record only
  stores a `u32`: memory fell to 34 MB, but the time rose from 271 to 471 ms
  because every record locks a shared table. Dropped for step 5.
* **A fingerprint-only distinct count** (not exact): 266 -> 177 ms, only as
  an upper bound for step 4; not kept because it is not exact.

## Where the unit tests live

The unit tests are in `tests/track_c.rs` at the repository root, included
with `#[cfg(test)] #[path = "../../tests/track_c.rs"] mod tests;`, so the
release build (which only sees this folder) never compiles them and the
source-size count of this folder does not include them. `cargo test` in this
folder still runs them. By the harness's own count this changed the source
lines from 531 to 474.

## Caveats

* Step 5 has not run on the scoring box. Expect the board's T to differ.
* Offsets are kept in 32 bits, so the input must be under 4 GiB (checked).
* Step 1's and the baseline's local timings were not recorded in a form I
  would cite, so those cells are left empty rather than reconstructed.
