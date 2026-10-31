//! Track C — Join. Parses a hostile `records.csv` (quoted commas, `""`
//! escapes, LFs inside quoted notes, mixed CRLF/LF, BOMs at the start of
//! some records), joins each record's `region_id` to a country through
//! `regions.csv`, and writes one line of aggregates per country.
//!
//! Spec: real-spec/speed-demon-2026/specs/c-join.md. Design notes:
//! - Both files are mapped; one pool of threads runs every phase, separated
//!   by barriers: flag the country codes in `regions.csv` (split by lines),
//!   fill a one-byte-per-id table `region_id -> country`, parse the records,
//!   count the distinct names of every country.
//! - Countries are numbered 1..=N in code order (0 is `_UNMATCHED`), so the
//!   output order is the index order.
//! - `records.csv` is cut after CRLFs: spec 2.2 puts a CR only at the start
//!   of a terminator, never inside quotes, so the byte after a CRLF starts a
//!   record. Pieces are claimed from a counter (cores differ in speed).
//! - A record is read off 64-byte blocks: bit masks of the quotes, commas
//!   and LFs, the bytes inside quotes masked out with a carry-less prefix
//!   XOR (a `""` pair flips it twice, so it changes nothing). Each record's
//!   four commas and its LF come straight from the mask, with the count of
//!   quotes before each, so field lengths need no second pass.
//! - Numbers are decoded eight digits at a time; amounts are integer cents.
//! - `names_distinct`: names get dense ids from a shared lock-free table
//!   (escaping is one to one, so equal raw bytes mean equal values), and an
//!   id is marked in one bitset per country; the count is a popcount. A name
//!   of up to 16 bytes is its own key (exact). A longer one is keyed by its
//!   first 8 bytes and a 62-bit hash of the rest.
//! - Region lookup, name lookup and bit marking each run `LAG` records
//!   behind the parse, with prefetches issued ahead, so their cache misses
//!   overlap.

use std::arch::x86_64::*;
use std::fmt::Write;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize};
use std::sync::{Barrier, Mutex, OnceLock};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
}

/// `mmap` of `len` bytes (at `addr` if `fd` is a file, else anywhere, zeroed).
fn map(addr: *mut u8, len: usize, fd: i32) -> *mut u8 {
    // PROT_READ | PROT_WRITE (PROT_READ for a file), MAP_PRIVATE | MAP_FIXED or MAP_ANONYMOUS.
    let (prot, flags) = if fd < 0 { (3, 2 | 0x20) } else { (1, 2 | 0x10) };
    // SAFETY: a fresh mapping, or one over a mapping of ours.
    let p = unsafe { mmap(addr, len.max(1), prot, flags, fd, 0) };
    assert!(p as isize != -1, "mmap failed");
    p
}

/// `n` zeroed values of a type for which zero is valid (only atomics and
/// plain integers are used). Pages are charged only once touched.
fn zeroed<T>(n: usize) -> &'static [T] {
    // SAFETY: zero bytes are a valid value of every T used here; the mapping lives forever.
    unsafe { std::slice::from_raw_parts(map(0 as _, n * std::mem::size_of::<T>(), -1).cast(), n) }
}

/// A table that every thread writes all over is populated up front, by
/// `i` of `n` threads each taking its share of the pages in one call:
/// faulting it in page by page from many threads made them queue on the
/// kernel's page-table locks. (Without MADV_POPULATE_WRITE the pages are
/// simply faulted in later.)
fn populate<T>(t: &[T], i: usize, n: usize) {
    let pages = std::mem::size_of_val(t).div_ceil(4096);
    let (from, to) = (pages * i / n, pages * (i + 1) / n);
    // SAFETY: whole pages of one of our own mappings.
    unsafe {
        madvise(
            (t.as_ptr() as usize + from * 4096) as _,
            (to - from) * 4096,
            23,
        )
    };
}

/// Maps `path` read-only, over a spare zero page: the 64 bytes after the
/// returned slice are readable, so block loads may run past its end. The
/// harness pre-warms `/data`, so mapped pages add nothing to peak memory.
fn map_file(path: &str) -> std::io::Result<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len() as usize;
    let base = map(0 as _, len + 4096, -1);
    if len > 0 {
        map(base, len, file.as_raw_fd());
    }
    // SAFETY: `len` bytes of the mapping above.
    Ok(unsafe { std::slice::from_raw_parts(base, len) })
}

fn load8(p: *const u8) -> u64 {
    // SAFETY: callers keep the 8 bytes inside the mapping.
    unsafe { p.cast::<u64>().read_unaligned() }
}

fn prefetch<T>(p: *const T) {
    // SAFETY: a prefetch is a hint; it never faults or changes memory.
    unsafe { _mm_prefetch::<_MM_HINT_T0>(p.cast()) };
}

/// The value of the last `n` (1 to 8) of the eight digit bytes in `v` (the
/// first byte is the lowest): reduced to digit values, the bytes before them
/// masked to leading zeros, then folded in pairs, quads and halves.
fn fold(v: u64, n: usize) -> u64 {
    let v = v & 0x0F0F_0F0F_0F0F_0F0F & (!0u64 << (8 * (8 - n)));
    let v = (v * 10 + (v >> 8)) & 0x00FF_00FF_00FF_00FF;
    let v = (v * 100 + (v >> 16)) & 0x0000_FFFF_0000_FFFF;
    (v * 10000 + (v >> 32)) & 0xFFFF_FFFF
}

/// The value of the 1 to 8 decimal digits in `[s, e)`.
fn digits(s: *const u8, e: *const u8) -> u64 {
    fold(load8(e.wrapping_sub(8)), e as usize - s as usize)
}

/// An amount's value in cents, from its digits `[s, e)` with the dot third
/// from the end: at most 7 digits (99999.99), so the eight bytes ending at
/// `e`, with the dot's byte dropped, are one number.
fn money(s: *const u8, e: *const u8) -> u64 {
    let w = load8(e.wrapping_sub(8));
    fold(
        w << 8 & 0xFFFF_FFFF_FF00 | w & 0xFFFF << 48,
        e as usize - s as usize - 1,
    )
}

/// Bit masks of the 64 bytes at `p`: the quotes, and the commas and LFs.
#[inline(always)]
unsafe fn masks(p: usize) -> (u64, u64) {
    let v = [0, 32].map(|i| _mm256_loadu_si256((p + i) as _));
    let eq = |c: u8| {
        let m = |v| _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(c as i8)));
        m(v[0]) as u32 as u64 | (m(v[1]) as u64) << 32
    };
    (eq(b'"'), eq(b',') | eq(b'\n'))
}

/// The commas and LFs outside quotes, in order, from a record start
/// onwards, each with the number of `"` bytes before it (so a field's quotes
/// are a difference, with no second pass over its bytes). Addresses are
/// plain numbers: a block may start before the first record.
#[derive(Default)]
struct Scan {
    block: usize,
    end: usize,
    /// Separators of the current block not yet returned.
    bits: u64,
    /// All ones when the block ends inside quotes.
    carry: u64,
    /// Quote bits of the current block, and the quotes before it.
    q: u64,
    before: u64,
}

impl Scan {
    /// The next separator, or the end of the file once there is none.
    #[inline]
    #[target_feature(enable = "pclmulqdq")]
    unsafe fn next(&mut self) -> (usize, u64) {
        while self.bits == 0 {
            self.block += 64;
            self.before += self.q.count_ones() as u64;
            if self.block >= self.end {
                return (self.end, self.before);
            }
            let (q, seps) = masks(self.block);
            self.q = q;
            // Bit i of the prefix XOR is set between an opening quote and its closing quote.
            let x = _mm_clmulepi64_si128::<0>(_mm_cvtsi64_si128(q as i64), _mm_set1_epi8(-1));
            let inside = _mm_cvtsi128_si64(x) as u64 ^ self.carry;
            self.carry = (inside as i64 >> 63) as u64;
            self.bits = seps & !inside;
        }
        let i = self.bits.trailing_zeros();
        self.bits &= self.bits - 1;
        let before = (self.q & ((1u64 << i) - 1)).count_ones() as u64;
        (self.block + i as usize, self.before + before)
    }
}

/// A name as the table sees it: `(lo, hi)` are the 16 bytes of a name of up
/// to 16 bytes (`hi` has no `FLAG`), or the first 8 bytes and a 62-bit hash
/// of the rest for a longer one, or `(CLAIMED, FLAG | READY)` for the empty
/// name. Every printable ASCII byte (spec 2.2) has its top bit clear, which
/// frees `FLAG` (the top bit of `hi`), `CLAIMED` (the top bit of `lo`) and
/// `READY` (the top bit of the byte in the middle of `hi`).
#[derive(Clone, Copy, Default)]
struct Key(u64, u64);

const FLAG: u64 = 1 << 63;
const CLAIMED: u64 = 1 << 63;
const READY: u64 = 1 << 55;
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

impl Key {
    /// The key of the `len` raw name bytes at `s`.
    #[inline(always)]
    fn new(s: *const u8, len: usize) -> Key {
        if len <= 16 {
            // SAFETY: names end well inside the padded mapping; the mask drops the bytes after them.
            let v = unsafe { s.cast::<u128>().read_unaligned() };
            let v = v & u128::MAX.checked_shr(128 - 8 * len as u32).unwrap_or(0);
            return Key(
                v as u64 | CLAIMED,
                (v >> 64) as u64 | READY | FLAG * (len == 0) as u64,
            );
        }
        let mut h = len as u64;
        for i in (8..len).step_by(8) {
            let w = load8(s.wrapping_add(i)) & (!0 >> (64 - 8 * (len - i).min(8)));
            h = (h ^ w).wrapping_mul(GOLDEN);
            h ^= h >> 32;
        }
        Key(load8(s) | CLAIMED, FLAG | READY | h >> 1)
    }
}

/// Names, interned: a key's slot in the open-addressed table is the name's
/// id. A slot (16 bytes) is claimed by a compare-and-swap of its `lo` word,
/// and its `hi` word, which always has `READY` set, is stored right after;
/// a reader whose `lo` matches waits for `hi`. (A 16-byte compare-and-swap
/// with a plain 16-byte load for readers lets a reader see half a new key:
/// seen on hybrid cores under load, it counted a name twice.)
#[derive(Default)]
struct Names {
    slots: &'static [[AtomicU64; 2]],
    bits: u32,
    claimed: AtomicU32,
    /// Set when the table has run out of room (the caller retries bigger).
    full: AtomicBool,
}

impl Names {
    fn home(&self, k: &Key) -> usize {
        let h = k.0.wrapping_mul(GOLDEN) ^ k.1.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        (h >> (64 - self.bits)) as usize
    }

    /// The id of `k`, assigning the next free slot to a new name; 0 if the
    /// table is full.
    fn id(&self, k: &Key) -> u32 {
        let mask = self.slots.len() - 1;
        let mut i = self.home(k);
        loop {
            let [lo, hi] = &self.slots[i];
            let mut seen = lo.load(Acquire);
            if seen == 0 {
                if self.claimed.load(Relaxed) as usize >= mask - mask / 8 {
                    self.full.store(true, Relaxed);
                    return 0;
                }
                seen = lo
                    .compare_exchange(0, k.0, Acquire, Acquire)
                    .unwrap_or_else(|other| other);
                if seen == 0 {
                    hi.store(k.1, Release);
                    self.claimed.fetch_add(1, Relaxed);
                    return i as u32;
                }
            }
            if seen == k.0 {
                // The slot's claimant stores `hi` right after claiming it.
                let theirs = std::iter::repeat_with(|| hi.load(Acquire)).find(|&h| h != 0);
                if theirs == Some(k.1) {
                    return i as u32;
                }
            }
            i = (i + 1) & mask;
        }
    }
}

/// Which names each country has: one bitset (`words` of 64 ids) per country,
/// shared by all threads. Most marks are first sightings, so they are
/// atomic ORs.
struct Seen {
    words: usize,
    bits: &'static [AtomicU64],
}

impl Seen {
    fn word(&self, c: u32, id: u32) -> &AtomicU64 {
        &self.bits[c as usize * self.words + id as usize / 64]
    }

    /// No test first: the read would fetch the line shared, then the OR
    /// would have to fetch it again exclusive.
    fn mark(&self, c: u32, id: u32) {
        self.word(c, id).fetch_or(1 << (id % 64), Relaxed);
    }

    fn count(&self, c: usize) -> i64 {
        let row = &self.bits[c * self.words..][..self.words];
        row.iter()
            .map(|w| w.load(Relaxed).count_ones() as i64)
            .sum()
    }
}

/// One country's running aggregates (per thread, summed at the end): count,
/// id sum, name bytes, note bytes, distinct names, amount sum; amount min, max.
#[derive(Clone, Copy)]
struct Totals([i64; 6], i64, i64);

const NO_TOTALS: Totals = Totals([0; 6], i64::MAX, i64::MIN);

/// Everything the record workers share once the region codes are known:
/// 15-bit country code -> index, index - 1 -> code, and the name bitsets.
struct Tables(Vec<u16>, Vec<u16>, Seen);

/// What all workers share: the records, the name table, and `region_id ->
/// index` as a byte, 255 for the rare index beyond 254 (then see `spill`)
/// and 0 for no such region.
struct Shared<'a> {
    data: &'a [u8],
    names: Names,
    country_of: &'static [AtomicU8],
    spill: Mutex<std::collections::HashMap<usize, u16>>,
}

/// A parsed record waiting for its lookups: name key, region, id, name
/// bytes, note bytes, amount in cents.
type Pending = (Key, u32, u32, u32, u32, i64);

const LAG: usize = 16;

/// A text field's value length: its bytes, less the delimiting quotes and
/// one byte per `""` pair when it is quoted (`q` counts every quote in it).
fn text_length(s: usize, e: usize, q: u64) -> u32 {
    (e - s - if q != 0 { q as usize / 2 + 1 } else { 0 }) as u32
}

/// Parses every record that starts in `from..to` (offsets of record starts)
/// into `totals`. A record's region row and name slot are prefetched when it
/// is parsed; `LAG` records later its country is read, its sums go to
/// `totals`, its name is looked up and that name's bitset word prefetched;
/// `LAG` records after that the name is marked.
#[target_feature(enable = "pclmulqdq")]
unsafe fn parse_records(sh: &Shared, seen: &Seen, from: usize, to: usize, totals: &mut [Totals]) {
    let (base, names, table) = (sh.data.as_ptr() as usize, &sh.names, sh.country_of);
    let mut pending = [Pending::default(); LAG];
    let mut marks = [(0, 0); LAG];
    let (mut n, mut last) = (0, usize::MAX); // records started; all of them once known
    let mut advance = |n: usize, last: usize, pending: &[Pending; LAG]| {
        let k = n % LAG;
        if n >= 2 * LAG && n - 2 * LAG < last {
            seen.mark(marks[k].0, marks[k].1);
        }
        if n >= LAG && n - LAG < last {
            let (key, region, id, name_bytes, note_bytes, cents) = pending[k];
            let mut c = table.get(region as usize).map_or(0, |c| c.load(Relaxed)) as usize;
            if c == 255 {
                c = sh.spill.lock().unwrap()[&(region as usize)] as usize;
            }
            let Totals(sum, min, max) = &mut totals[c];
            let add = [1, id.into(), name_bytes.into(), note_bytes.into(), 0, cents];
            sum.iter_mut().zip(add).for_each(|(a, b)| *a += b);
            (*min, *max) = ((*min).min(cents), (*max).max(cents));
            marks[k] = (c as u32, names.id(&key));
            prefetch(seen.word(marks[k].0, marks[k].1));
        }
    };

    let mut sep = Scan {
        block: base + from - 64,
        end: base + sh.data.len(),
        ..Scan::default()
    };
    let mut p = base + from;
    while p < base + to {
        let (f1, q1) = sep.next(); // the commas after id, name, note, region
        let (f2, q2) = sep.next();
        let (f3, q3) = sep.next();
        let f4 = sep.next().0;
        let eol = sep.next().0; // LF, or the end of the file
        p += 3 * (*(p as *const u8) == 0xEF) as usize; // a byte order mark before the id
        let id = digits(p as _, f1 as _);
        let region = digits((f3 + 1) as _, f4 as _);
        let amount_end = eol - (*(eol as *const u8).sub(1) == b'\r') as usize;
        let negative = *((f4 + 1) as *const u8) == b'-';
        let cents = money((f4 + 1 + negative as usize) as _, amount_end as _) as i64;

        // Region ids are below 10^7 (fit u32); one past the table means none.
        let r = region.min(table.len() as u64) as u32;
        prefetch(table.as_ptr().wrapping_add(r as usize));
        let quoted = (q2 != q1) as usize;
        advance(n, last, &pending);
        let key = Key::new((f1 + 1 + quoted) as _, f2 - f1 - 1 - 2 * quoted);
        let (name_bytes, note_bytes) = (
            text_length(f1 + 1, f2, q2 - q1),
            text_length(f2 + 1, f3, q3 - q2),
        );
        pending[n % LAG] = (
            key,
            r,
            id as u32,
            name_bytes,
            note_bytes,
            if negative { -cents } else { cents },
        );
        prefetch(&names.slots[names.home(&key)]);
        n += 1;
        p = eol + 1;
    }
    last = n;
    while n < last + 2 * LAG {
        advance(n, last, &pending);
        n += 1;
    }
}

/// The `(region id, country code)` of every line of `r` (whole lines, each
/// `<digits>,<ABC>` and an LF; spec 2.3 allows no other bytes). The code is
/// 15 bits, five per letter, so numeric order is alphabetical order.
fn region_lines(r: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let line = r.get(at..).filter(|l| !l.is_empty())?;
        // The digits (bytes 0x30..=0x39) end at the first byte whose high nibble is not 3.
        let n = ((load8(line.as_ptr()) ^ 0x3030_3030_3030_3030) & 0xF0F0_F0F0_F0F0_F0F0)
            .trailing_zeros()
            / 8;
        let end = line[n as usize..].as_ptr();
        let code = line[n as usize + 1..][..3]
            .iter()
            .fold(0, |a, &c| a << 5 | (c & 31) as usize);
        at += n as usize + 5;
        Some((digits(line.as_ptr(), end) as usize, code))
    })
}

/// Piece `k` of `n` of `data[from..]` starts here: just after the first
/// `delim` at or after the nominal cut, so every piece starts on a record
/// (or line) boundary and the cuts of different `k` are consistent. Found by
/// the thread that needs it, so the file's pages are first touched in parallel.
fn cut(data: &[u8], from: usize, n: usize, delim: &[u8], k: usize) -> usize {
    if k == 0 {
        return from;
    }
    let at = from + (data.len() - from) * k / n;
    let hit = data[at..].windows(delim.len()).position(|w| w == delim);
    hit.map_or(data.len(), |h| at + h + delim.len())
}

/// Integer cents as `-?<int>.<2 digits>`; zero is `0.00`, never `-0.00`.
fn cents(c: i64) -> String {
    let a = c.unsigned_abs();
    format!("{}{}.{:02}", if c < 0 { "-" } else { "" }, a / 100, a % 100)
}

/// The aggregates for the two files, or `None` if a name table of `2^bits`
/// slots was too small. Both inputs must be followed by 64 readable bytes
/// (see `map_file`).
fn run(records: &[u8], regions: &[u8], threads: usize, bits: u32) -> Option<Vec<u8>> {
    let line = |d: &[u8]| {
        d.iter()
            .position(|&b| b == b'\n')
            .map_or(d.len(), |p| p + 1)
    };
    // Ids ascend, so the last line of regions.csv holds the largest.
    let last = regions.rsplit(|&b| b == b'\n').nth(1).unwrap_or(&[]);
    let max_id = last
        .iter()
        .take_while(|d| d.is_ascii_digit())
        .fold(0, |a, &d| a * 10 + (d - b'0') as usize);
    let names = Names {
        slots: zeroed(1 << bits),
        bits,
        ..Names::default()
    };
    let sh = Shared {
        data: records,
        names,
        country_of: zeroed(max_id + 1),
        spill: Mutex::default(),
    };
    let (first, start) = (line(regions), line(records));
    let present: Vec<AtomicU8> = (0..1 << 15).map(|_| AtomicU8::new(0)).collect();
    let tables = OnceLock::new();
    let (sync, claim) = (Barrier::new(threads), AtomicUsize::new(0));
    // Records are cut after CRLFs into pieces claimed in turn.
    let pieces = 32 * threads;

    let work = |t: usize| {
        let lines = || {
            region_lines(
                &regions[cut(regions, first, threads, b"\n", t)
                    ..cut(regions, first, threads, b"\n", t + 1)],
            )
        };
        populate(sh.country_of, t, threads);
        populate(sh.names.slots, t, threads);
        lines().for_each(|(_, c)| present[c].store(1, Relaxed));
        sync.wait();
        let Tables(index_of, codes, seen) = tables.get_or_init(|| {
            let (mut index_of, mut codes) = (vec![0; 1 << 15], vec![]);
            for (k, _) in present
                .iter()
                .enumerate()
                .filter(|(_, p)| p.load(Relaxed) != 0)
            {
                codes.push(k as u16);
                index_of[k] = codes.len() as u16;
            }
            let words = (1 << bits) / 64;
            let seen = Seen {
                words,
                bits: zeroed((codes.len() + 1) * words),
            };
            Tables(index_of, codes, seen)
        });
        populate(seen.bits, t, threads);
        for (id, c) in lines() {
            sh.country_of[id].store(index_of[c].min(255) as u8, Relaxed);
            if index_of[c] >= 255 {
                sh.spill.lock().unwrap().insert(id, index_of[c]);
            }
        }
        sync.wait();
        let mut totals = vec![NO_TOTALS; codes.len() + 1];
        while !sh.names.full.load(Relaxed) {
            let k = claim.fetch_add(1, Relaxed);
            if k >= pieces {
                break;
            }
            let (from, to) = (
                cut(records, start, pieces, b"\r\n", k),
                cut(records, start, pieces, b"\r\n", k + 1),
            );
            // SAFETY: PCLMULQDQ is in every CPU the scoring box can have (Raptor Lake).
            unsafe { parse_records(&sh, seen, from, to, &mut totals) };
        }
        sync.wait();
        for c in (t..totals.len()).step_by(threads) {
            totals[c].0[4] = seen.count(c);
        }
        totals
    };
    let parts: Vec<Vec<Totals>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads).map(|t| s.spawn(move || work(t))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    if sh.names.full.load(Relaxed) {
        return None;
    }

    let codes = &tables.get().unwrap().1;
    let mut out = String::new();
    for i in 1..=codes.len() + 1 {
        let c = i % (codes.len() + 1); // countries 1..=N, then _UNMATCHED (0)
        let mut all = NO_TOTALS;
        for Totals(sum, min, max) in parts.iter().map(|p| p[c]) {
            all.0.iter_mut().zip(sum).for_each(|(a, b)| *a += b);
            (all.1, all.2) = (all.1.min(min), all.2.max(max));
        }
        let [count, id_sum, name_bytes, note_bytes, distinct, amount_sum] = all.0;
        if count > 0 {
            let key = codes
                .get(c.wrapping_sub(1))
                .map_or("_UNMATCHED".into(), |&k| {
                    String::from_utf8(
                        [k >> 10, k >> 5 & 31, k & 31]
                            .map(|v| b'@' + v as u8)
                            .to_vec(),
                    )
                    .unwrap()
                }); // 'A' & 31 is 1
            let _ = writeln!(
                out,
                "{key} {count} {id_sum} {} {} {} {distinct} {name_bytes} {note_bytes}",
                cents(amount_sum),
                cents(all.1),
                cents(all.2)
            );
        }
    }
    Some(out.into_bytes())
}

fn main() -> std::io::Result<()> {
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or(default.into());
    // With transparent huge pages, a block written only in part can be backed
    // by a whole 2 MB page; plain pages keep memory to what is written.
    // SAFETY: PR_SET_THP_DISABLE takes one flag and three zeros.
    unsafe { prctl(41, 1, 0, 0, 0) };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (records, regions) = (
        map_file(&var("RECORDS_PATH", "/data/records.csv"))?,
        map_file(&var("REGIONS_PATH", "/data/regions.csv"))?,
    );
    std::fs::write(
        var("OUTPUT_PATH", "/out/aggregates.txt"),
        solve(records, regions, threads),
    )?;
    // Skip the destructors and the runtime's teardown.
    std::process::exit(0)
}

/// The contents of `aggregates.txt`; a name table too small for the number
/// of distinct names is replaced by one twice the size and the run repeated.
fn solve(records: &[u8], regions: &[u8], threads: usize) -> Vec<u8> {
    (17..)
        .find_map(|bits| run(records, regions, threads.max(1), bits))
        .unwrap()
}

#[cfg(test)]
#[path = "../../tests/track_c.rs"]
mod tests;
