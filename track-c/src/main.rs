//! Track C — Join. Parses a hostile `records.csv` (quoted commas, `""`
//! escapes, LFs inside quoted notes, mixed CRLF/LF, BOMs at the start of
//! some records), joins each record's `region_id` to a country through
//! `regions.csv`, and writes one line of aggregates per country.
//!
//! Spec: real-spec/speed-demon-2026/specs/c-join.md. Design notes:
//! - Region -> country is a dense array indexed by `region_id`, so the
//!   join is one bounds-checked array read per record.
//! - Amounts are integer cents end to end; nothing touches a float.
//! - A record is parsed straight from the mapped bytes: digit runs with a
//!   branch-free SWAR conversion, quoted fields with an AVX2 search for the
//!   closing quote (a `"` followed by another `"` is an escaped pair).
//! - `names_distinct` keeps nothing per record. All workers share one
//!   lock-free table that numbers each distinct name (by a 64-bit hash of
//!   its raw bytes; escaping is one to one, so equal names have equal raw
//!   bytes) and one bit per (country, name number). A country's distinct
//!   count is the popcount of its row. Lookups are batched and prefetched
//!   so their cache misses overlap.
//! - The file is parsed in parallel, in many more chunks than threads that
//!   workers claim from a counter (cores differ in speed). A chunk cut is
//!   the first LF that is followed by a digit or a BOM, which spec 2.2
//!   makes a record boundary: names are letters and notes lowercase words.
//! - The last few KB are copied behind zero padding so the SIMD loads never
//!   run past the mapping.

use std::arch::x86_64::{
    __m256i, _mm256_and_si256, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8,
    _mm256_set1_epi8, _mm256_storeu_si256,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

/// Maps `path` read-only for the life of the process. The harness
/// pre-warms `/data` into the page cache before the run, so those pages
/// are charged outside this run's cgroup and mapping them adds nothing to
/// peak memory; `fs::read` would copy the whole file into charged heap.
#[cfg(unix)]
fn map_file(path: &str) -> std::io::Result<&'static [u8]> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    }
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        return Ok(&[]);
    }
    // PROT_READ = 1, MAP_PRIVATE = 2.
    let p = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
    if p as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::slice::from_raw_parts(p, len) })
}

#[cfg(not(unix))]
fn map_file(path: &str) -> std::io::Result<&'static [u8]> {
    Ok(Box::leak(std::fs::read(path)?.into_boxed_slice()))
}

// --- Regions ------------------------------------------------------------

/// Countries in first-seen order, and `region_id -> group + 1` (0 means
/// "no such region").
struct Regions {
    countries: Vec<[u8; 3]>,
    /// Group + 1 as one byte, 255 for the rare group beyond the 254th (then see `spill`).
    group_of: Vec<u8>,
    spill: std::collections::HashMap<usize, u16>,
}

fn parse_regions(data: &[u8]) -> Regions {
    // Spec 2.3: a code is three bytes `A`-`Z`, so five bits each identify it.
    let mut index = vec![0u16; 1 << 15];
    let mut countries = Vec::new();
    let mut spill = std::collections::HashMap::new();
    // Ids ascend, so the last line has the largest; size the table once.
    let last = data.rsplit(|&b| b == b'\n').nth(1).unwrap_or(&[]);
    let max_id = last
        .iter()
        .take_while(|d| d.is_ascii_digit())
        .fold(0usize, |a, &d| a * 10 + (d - b'0') as usize);
    let mut group_of = vec![0u8; max_id + 1];
    let mut at = data
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |p| p + 1);
    while at < data.len() {
        let mut id = 0usize;
        while data[at] != b',' {
            id = id * 10 + (data[at] - b'0') as usize;
            at += 1;
        }
        let code = &data[at + 1..at + 4];
        let slot = &mut index[code.iter().fold(0usize, |a, &c| a << 5 | (c & 31) as usize)];
        if *slot == 0 {
            countries.push([code[0], code[1], code[2]]);
            *slot = countries.len() as u16;
        }
        if id >= group_of.len() {
            group_of.resize(id + 1, 0);
        }
        group_of[id] = (*slot).min(255) as u8;
        if *slot >= 255 {
            spill.insert(id, *slot);
        }
        at += 4;
        while data[at] != b'\n' {
            at += 1;
        }
        at += 1;
    }
    Regions {
        countries,
        group_of,
        spill,
    }
}

// --- Aggregation --------------------------------------------------------

#[derive(Clone, Default)]
struct Agg {
    count: u64,
    id_sum: u64,
    amount_sum: i64,
    amount_min: i64,
    amount_max: i64,
    name_bytes: u64,
    note_bytes: u64,
    names_distinct: u64,
}

impl Agg {
    fn new() -> Agg {
        Agg {
            amount_min: i64::MAX,
            amount_max: i64::MIN,
            ..Agg::default()
        }
    }
}

// --- Distinct names -----------------------------------------------------

/// Shared by all workers. `slots` is an open-addressing table with
/// `2 << id_bits` entries; an entry is the hash with its low `id_bits`
/// bits replaced by the name's number (1 up to `1 << id_bits`, 0 = empty),
/// so the slot index and the stored bits together cover all 64 hash bits.
/// `bits` holds one bit per (group, number).
struct Names {
    slots: Vec<AtomicU64>,
    bits: Vec<AtomicU64>,
    issued: AtomicUsize,
    full: AtomicBool,
    id_bits: u32,
}

/// `n` zeroed atomics; the allocator hands back untouched zero pages, so
/// only the part that is used costs memory.
fn zeroed(n: usize) -> Vec<AtomicU64> {
    let mut v = std::mem::ManuallyDrop::new(vec![0u64; n]);
    // SAFETY: AtomicU64 has the size and alignment of u64, and zero is a valid value.
    unsafe { Vec::from_raw_parts(v.as_mut_ptr().cast(), v.len(), v.capacity()) }
}

impl Names {
    fn new(id_bits: u32, groups: usize) -> Names {
        Names {
            slots: zeroed(2 << id_bits),
            bits: zeroed(groups << (id_bits - 6)),
            issued: AtomicUsize::new(0),
            full: AtomicBool::new(false),
            id_bits,
        }
    }

    /// The number of the name with hash `h`, assigning the next free one to
    /// a new name; 0 if the table has run out of numbers.
    #[inline]
    fn number(&self, h: u64) -> usize {
        let mask = self.slots.len() - 1;
        let mut s = h as usize & mask;
        loop {
            let mut cur = self.slots[s].load(Relaxed);
            if cur == 0 {
                let id = self.issued.fetch_add(1, Relaxed) + 1;
                if id >= 1 << self.id_bits {
                    self.full.store(true, Relaxed);
                    return 0;
                }
                let new = (h >> self.id_bits << self.id_bits) | id as u64;
                cur = self.slots[s]
                    .compare_exchange(0, new, Relaxed, Relaxed)
                    .unwrap_or_else(|other| other);
                if cur == 0 {
                    cur = new;
                }
            }
            if (cur ^ h) >> self.id_bits == 0 {
                return cur as usize & ((1 << self.id_bits) - 1);
            }
            s = (s + 1) & mask;
        }
    }

    fn distinct(&self, group: usize) -> u64 {
        let row = &self.bits[group << (self.id_bits - 6)..(group + 1) << (self.id_bits - 6)];
        row.iter()
            .map(|w| w.load(Relaxed).count_ones() as u64)
            .sum()
    }
}

const BATCH: usize = 16;

/// Name sightings on their way into the shared structures. A sighting is
/// first numbered in the table, and one batch later its bit is set, so the
/// cache misses of up to `BATCH` lookups overlap (prefetches are issued a
/// batch ahead) instead of each stalling the parser.
struct Sightings<'a> {
    names: &'a Names,
    seen: [(usize, u64); BATCH],
    seen_len: usize,
    bits: [(usize, u64); BATCH],
    bits_len: usize,
}

impl<'a> Sightings<'a> {
    fn new(names: &'a Names) -> Self {
        Sightings {
            names,
            seen: [(0, 0); BATCH],
            seen_len: 0,
            bits: [(0, 0); BATCH],
            bits_len: 0,
        }
    }

    #[inline]
    fn push(&mut self, group: usize, h: u64) {
        let slot = &self.names.slots[h as usize & (self.names.slots.len() - 1)];
        // SAFETY: a prefetch is a hint; it never faults or changes memory.
        unsafe {
            std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(
                (slot as *const AtomicU64).cast(),
            )
        };
        self.seen[self.seen_len] = (group, h);
        self.seen_len += 1;
        if self.seen_len == BATCH {
            self.flush();
        }
    }

    /// Sets the bits numbered by the previous batch, then numbers this one.
    fn flush(&mut self) {
        for &(word, bit) in &self.bits[..self.bits_len] {
            self.names.bits[word].fetch_or(bit, Relaxed);
        }
        self.bits_len = 0;
        for &(group, h) in &self.seen[..self.seen_len] {
            let id = self.names.number(h);
            if id != 0 {
                let word = (group << (self.names.id_bits - 6)) + id / 64;
                let ptr: *const AtomicU64 = &self.names.bits[word];
                // SAFETY: a hint to get the line ready for writing; it never faults.
                unsafe {
                    std::arch::asm!("prefetchw [{0}]", in(reg) ptr, options(nostack, preserves_flags))
                };
                self.bits[self.bits_len] = (word, 1 << (id % 64));
                self.bits_len += 1;
            }
        }
        self.seen_len = 0;
    }

    fn finish(mut self) {
        self.flush();
        self.flush();
    }
}

#[inline]
fn mum(a: u64, b: u64) -> u64 {
    let p = a as u128 * b as u128;
    p as u64 ^ (p >> 64) as u64
}

/// `data[at..at + 32]` as four words, the bytes from `left` on zeroed.
#[inline]
fn block(data: &[u8], at: usize, left: usize) -> [u64; 4] {
    // The first `left` bytes of this 64-byte window's 32-byte slice are 0xFF.
    static EDGE: [u8; 64] = {
        let mut t = [0u8; 64];
        let mut i = 0;
        while i < 32 {
            t[i] = 0xFF;
            i += 1;
        }
        t
    };
    let (src, mask) = (&data[at..at + 32], &EDGE[32 - left.min(32)..][..32]);
    let mut w = [0u64; 4];
    // SAFETY: AVX2 is part of the build target; `src`, `mask` and `w` are 32 bytes each.
    unsafe {
        let v = _mm256_and_si256(
            _mm256_loadu_si256(src.as_ptr().cast::<__m256i>()),
            _mm256_loadu_si256(mask.as_ptr().cast::<__m256i>()),
        );
        _mm256_storeu_si256(w.as_mut_ptr().cast::<__m256i>(), v);
    }
    w
}

/// 64-bit hash of `data[from..to]`; `data` must extend 32 bytes past `to`.
#[inline]
fn hash_name(data: &[u8], from: usize, to: usize) -> u64 {
    let mut h = (to - from) as u64 ^ 0x9E37_79B9_7F4A_7C15;
    let mut at = from;
    loop {
        let w = block(data, at, to.saturating_sub(at));
        h = mum(
            mum(w[0] ^ 0xA076_1D64_78BD_642F, w[1] ^ 0xE703_7ED1_A0B4_28DB) ^ h,
            mum(w[2] ^ 0x8EBC_6AF0_9C88_C6E3, w[3] ^ 0x5899_65CC_7537_4CC3),
        );
        at += 32;
        if at >= to {
            return h;
        }
    }
}

// --- Record parsing (spec section 2.1) ----------------------------------

/// Bit mask of the `"` bytes in `data[at..at + 32]`.
#[inline]
fn quotes(data: &[u8], at: usize) -> u32 {
    let block = &data[at..at + 32];
    // SAFETY: AVX2 is part of the build target (.cargo/config.toml, x86-64-v3)
    // and `block` is exactly 32 readable bytes.
    unsafe {
        let v = _mm256_loadu_si256(block.as_ptr().cast::<__m256i>());
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'"' as i8))) as u32
    }
}

/// A `text` field starting at `at`: returns (content start, content end,
/// number of `""` escapes inside, index just past the field's `,`).
/// The value is the content with every `""` read as one `"`.
#[inline]
fn text_field(data: &[u8], at: usize) -> (usize, usize, usize, usize) {
    if data[at] != b'"' {
        let c = at
            + data[at..]
                .iter()
                .position(|&b| b == b',')
                .unwrap_or(data.len() - at - 1);
        return (at, c, 0, c + 1);
    }
    let start = at + 1;
    let (mut j, mut pairs) = (start, 0);
    loop {
        let m = quotes(data, j);
        if m == 0 {
            j += 32;
            continue;
        }
        let q = j + m.trailing_zeros() as usize;
        if data[q + 1] == b'"' {
            pairs += 1;
            j = q + 2;
        } else {
            // Closing quote; a name or note is always followed by `,`.
            return (start, q, pairs, q + 2);
        }
    }
}

/// The run of ASCII digits at `data[at..]` as (value, length).
#[inline]
fn digits(data: &[u8], at: usize) -> (u64, usize) {
    let w = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    let not_digit = (w & 0xF0F0_F0F0_F0F0_F0F0) ^ 0x3030_3030_3030_3030;
    let mut n = not_digit.trailing_zeros() as usize / 8;
    // Digits to the top bytes, so the first one lands in the highest decimal place.
    let w = (w << (64 - 8 * n.max(1)) & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let w = ((w & 0x00FF_00FF_00FF_00FF).wrapping_mul(6_553_601)) >> 16;
    let mut v = ((w & 0x0000_FFFF_0000_FFFF).wrapping_mul(42_949_672_960_001)) >> 32;
    while n >= 8 && data[at + n].is_ascii_digit() {
        v = v * 10 + (data[at + n] - b'0') as u64;
        n += 1;
    }
    (v, n)
}

/// Parses every record in `data[start..end]` (both record boundaries; the
/// 64 bytes after `end` must be readable) into `groups`.
fn parse_records(
    data: &[u8],
    start: usize,
    end: usize,
    regions: &Regions,
    sightings: &mut Sightings,
    groups: &mut [Agg],
) {
    let unmatched = regions.countries.len();
    let mut i = start;
    while i < end {
        i += 3 * (data[i] == 0xEF) as usize;
        let (id, n) = digits(data, i);
        let (ns, ne, npairs, next) = text_field(data, i + n + 1);
        let (ts, te, tpairs, next) = text_field(data, next);
        let (region, n) = digits(data, next);
        let mut j = next + n + 1;
        let neg = data[j] == b'-';
        j += neg as usize;
        let (whole, n) = digits(data, j);
        j += n + 1;
        let cents =
            (whole * 100 + (data[j] - b'0') as u64 * 10 + (data[j + 1] - b'0') as u64) as i64;
        let cents = if neg { -cents } else { cents };
        j += 2;
        // CRLF or LF; a missing final terminator ends the loop either way.
        i = j + 1 + (data[j] == b'\r') as usize;

        let g = match regions.group_of.get(region as usize) {
            Some(&255) => regions.spill[&(region as usize)] as usize - 1,
            Some(&g) if g != 0 => g as usize - 1,
            _ => unmatched,
        };
        let agg = &mut groups[g];
        agg.count += 1;
        agg.id_sum += id;
        agg.amount_sum += cents;
        agg.amount_min = agg.amount_min.min(cents);
        agg.amount_max = agg.amount_max.max(cents);
        agg.name_bytes += (ne - ns - npairs) as u64;
        agg.note_bytes += (te - ts - tpairs) as u64;
        sightings.push(g, hash_name(data, ns, ne));
    }
}

/// First record boundary after `from`: the position after an LF that is
/// followed by a digit (next id) or a BOM. Spec 2.2 makes names letters and
/// notes lowercase words, so an LF inside a quoted field is followed by a
/// letter or a quote.
fn cut(data: &[u8], from: usize) -> usize {
    let mut i = from;
    while i + 1 < data.len()
        && !(data[i] == b'\n' && (data[i + 1].is_ascii_digit() || data[i + 1] == 0xEF))
    {
        i += 1;
    }
    (i + 1).min(data.len())
}

/// Bytes at the end of the file that are copied behind padding and parsed
/// last, so no chunk's SIMD loads can run past the mapping.
const TAIL: usize = 8192;
const CHUNKS_PER_THREAD: usize = 32;

fn scan(data: &[u8], body: usize, regions: &Regions, threads: usize, names: &Names) -> Vec<Agg> {
    let n_groups = regions.countries.len() + 1;
    let tail_at = if data.len() - body > 2 * TAIL {
        cut(data, data.len() - TAIL)
    } else {
        body
    };
    let chunks = threads * CHUNKS_PER_THREAD;
    let bound = |k: usize| match k {
        0 => body,
        k if k == chunks => tail_at,
        k => cut(data, body + (tail_at - body) * k / chunks).min(tail_at),
    };
    let next = AtomicUsize::new(0);
    let mut parts: Vec<Vec<Agg>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut groups = vec![Agg::new(); n_groups];
                    let mut sightings = Sightings::new(names);
                    loop {
                        let k = next.fetch_add(1, Relaxed);
                        if k >= chunks {
                            sightings.finish();
                            return groups;
                        }
                        parse_records(
                            data,
                            bound(k),
                            bound(k + 1),
                            regions,
                            &mut sightings,
                            &mut groups,
                        );
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("parse worker panicked"))
            .collect()
    });

    let mut tail = data[tail_at..].to_vec();
    tail.resize(tail.len() + 64, 0);
    let mut groups = vec![Agg::new(); n_groups];
    let mut sightings = Sightings::new(names);
    parse_records(
        &tail,
        0,
        data.len() - tail_at,
        regions,
        &mut sightings,
        &mut groups,
    );
    sightings.finish();
    parts.push(groups);

    let mut out = vec![Agg::new(); n_groups];
    for part in &parts {
        for (dst, src) in out.iter_mut().zip(part) {
            if src.count == 0 {
                continue;
            }
            dst.count += src.count;
            dst.id_sum += src.id_sum;
            dst.amount_sum += src.amount_sum;
            dst.amount_min = dst.amount_min.min(src.amount_min);
            dst.amount_max = dst.amount_max.max(src.amount_max);
            dst.name_bytes += src.name_bytes;
            dst.note_bytes += src.note_bytes;
        }
    }
    out
}

fn aggregate(data: &[u8], regions: &Regions, threads: usize) -> Vec<Agg> {
    let n_groups = regions.countries.len() + 1;
    let body = data
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |p| p + 1); // skip the BOM + header line
    if body >= data.len() {
        return vec![Agg::new(); n_groups];
    }
    // Names are numbered from a table sized for 100 k distinct names (the
    // generator's pool); if there are more, start over with a bigger one.
    let mut id_bits = 17;
    loop {
        let names = Names::new(id_bits, n_groups);
        let mut out = scan(data, body, regions, threads.max(1), &names);
        if !names.full.load(Relaxed) {
            out.iter_mut()
                .enumerate()
                .for_each(|(g, a)| a.names_distinct = names.distinct(g));
            // The process ends soon; freeing the tables would only cost time.
            std::mem::forget(names);
            return out;
        }
        id_bits += 2;
    }
}

// --- Output (spec section 4) --------------------------------------------

/// Integer cents as `-?<int>.<2 digits>`; zero is `0.00`, never `-0.00`.
fn cents(c: i64) -> String {
    format!(
        "{}{}.{:02}",
        if c < 0 { "-" } else { "" },
        c.abs() / 100,
        c.abs() % 100
    )
}

fn write_line(out: &mut Vec<u8>, key: &[u8], a: &Agg) {
    out.extend_from_slice(key);
    let (min, max, sum) = (
        cents(a.amount_min),
        cents(a.amount_max),
        cents(a.amount_sum),
    );
    let rest = format!(
        " {} {} {sum} {min} {max} {} {} {}\n",
        a.count, a.id_sum, a.names_distinct, a.name_bytes, a.note_bytes
    );
    out.extend_from_slice(rest.as_bytes());
}

fn render(regions: &Regions, groups: &[Agg]) -> Vec<u8> {
    let mut order: Vec<usize> = (0..regions.countries.len()).collect();
    order.sort_unstable_by_key(|&g| regions.countries[g]);
    let mut out = Vec::new();
    for g in order {
        if groups[g].count > 0 {
            write_line(&mut out, &regions.countries[g], &groups[g]);
        }
    }
    // `_` (0x5F) sorts after every `[A-Z]{3}` code, so it is always last.
    let unmatched = &groups[regions.countries.len()];
    if unmatched.count > 0 {
        write_line(&mut out, b"_UNMATCHED", unmatched);
    }
    out
}

fn main() {
    let var =
        |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_string());
    let (records, regions, output) = (
        var("RECORDS_PATH", "/data/records.csv"),
        var("REGIONS_PATH", "/data/regions.csv"),
        var("OUTPUT_PATH", "/out/aggregates.txt"),
    );
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let result = map_file(&regions).and_then(|regions_data| {
        let regions = parse_regions(regions_data);
        let groups = aggregate(map_file(&records)?, &regions, threads);
        std::fs::write(&output, render(&regions, &groups))
    });
    if let Err(e) = result {
        eprintln!("track-c: failed: {e}");
        std::process::exit(1);
    }
    // Skip the destructors and the runtime's teardown.
    std::process::exit(0);
}

#[cfg(test)]
#[path = "../../tests/track_c.rs"]
mod tests;
