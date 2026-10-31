//! Track A - Ingest. Aggregates per-(hour, endpoint) request counts, exact
//! nearest-rank p50/p95/p99 latency and 5xx counts from a space-delimited
//! log (`ts method endpoint status latency_us`), as in specs/a-ingest.md.
//!
//! One pool of threads does everything in three phases.
//!
//! Phase 1, parse. The log is memory-mapped and cut into line-aligned pieces
//! that the threads claim in order. The timestamp is always 27 bytes and the
//! 72-hour span is fixed, so a line's hour index comes from its day and hour
//! digits alone. One 64-byte scan after the timestamp finds the method's end,
//! the endpoint's end and the line's end. Endpoints are interned in one
//! lock-free table shared by all threads, and each line files a 4-byte record
//! (latency, endpoint id, 5xx flag) under its hour.
//!
//! Between the phases one thread sorts the endpoint names, so that each id gets
//! its rank in output order.
//!
//! Phase 2, hours. Threads claim hours in order. An hour's latencies are
//! counting-sorted by endpoint rank into per-thread scratch and three
//! selections per group give the percentiles. Each group's numbers are kept
//! along with the exact size the hour's text will take, since the width of a
//! number is known without formatting it.
//!
//! Phase 3, text. One thread turns the sizes into offsets, sizes the output
//! file and maps it; then every thread formats its hours straight into the
//! file. No write call is made, and the output's pages fill only after the
//! records have been released, so the peak is the larger of the two, not the
//! sum.

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Barrier, Mutex, OnceLock};

/// 2026-10-30T18:00Z up to 2026-11-02T17:59:59Z.
const HOURS: usize = 72;
/// A record packs the latency above a 13-bit key: a 12-bit endpoint id and the 5xx flag. Latencies of 2^19 us or
/// more (about 3% of lines) do not fit and are kept on a side list with their key instead.
const KEY_BITS: u32 = 13;
const ERR: u32 = 1 << 12;
const ID_MASK: u32 = ERR - 1;
const PACKED_MAX: u32 = 1 << 19;

/// Bit i is set when byte `p + i` (32 are read) equals `c`.
#[inline(always)]
unsafe fn eq32(p: *const u8, c: u8) -> u32 {
    _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p.cast()), _mm256_set1_epi8(c as i8))) as u32
}

#[inline(always)]
unsafe fn load8(p: *const u8) -> u64 {
    p.cast::<u64>().read_unaligned()
}

/// The value of the `len` (1 to 10) digits that end just before `end`, with no loop: the 16 bytes before `end`, minus
/// `'0'` and with the bytes ahead of the number zeroed, are combined 2, 4 and then 8 digits at a time.
#[inline(always)]
unsafe fn decode(end: *const u8, len: u32) -> u32 {
    let v = _mm_subs_epu8(_mm_loadu_si128(end.sub(16).cast()), _mm_set1_epi8(b'0' as i8));
    let lane = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
    let v = _mm_and_si128(v, _mm_cmpgt_epi8(lane, _mm_set1_epi8((15 - len) as i8)));
    let v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A)); // pairs (10, 1)
    let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_0064)); // pairs (100, 1)
    let v = _mm_madd_epi16(_mm_packus_epi32(v, v), _mm_set1_epi32(0x0001_2710)); // pairs (10000, 1)
    (_mm_cvtsi128_si32(v) as u32 as u64 * 100_000_000 + _mm_extract_epi32::<1>(v) as u32 as u64) as u32
}

/// The number of decimal digits in `x`.
#[inline(always)]
fn width(x: u32) -> usize {
    x.checked_ilog10().map_or(1, |l| l as usize + 1)
}

/// Puts the k-th smallest of `a` at `a[k]`, the smaller ones before it, without a branch on the data: each step
/// partitions around the median of three by Lomuto's scheme with an unconditional swap, the boundary moving by the
/// comparison's result (`select_nth_unstable` mispredicts about every other element). Many equal values could make a
/// round shrink the range by only one, so after 40 rounds the library's selection finishes.
fn select(mut a: &mut [u32], mut k: usize) {
    for round in 0.. {
        let n = a.len();
        if n <= 1 {
            return;
        }
        if round == 40 {
            a.select_nth_unstable(k);
            return;
        }
        let m = n / 2; // sort a[0], a[m], a[n - 1], then the median is the pivot
        if a[m] < a[0] {
            a.swap(m, 0);
        }
        if a[n - 1] < a[m] {
            a.swap(n - 1, m);
        }
        if a[m] < a[0] {
            a.swap(m, 0);
        }
        a.swap(m, n - 1);
        let pivot = a[n - 1];
        let mut lt = 0;
        let p = a.as_mut_ptr();
        for i in 0..n - 1 {
            // SAFETY: lt <= i < n - 1.
            unsafe {
                let x = *p.add(i);
                *p.add(i) = *p.add(lt);
                *p.add(lt) = x;
                lt += usize::from(x < pivot);
            }
        }
        a.swap(lt, n - 1);
        if k == lt {
            return;
        }
        let rest = std::mem::take(&mut a);
        if k < lt {
            a = &mut rest[..lt];
        } else {
            k -= lt + 1;
            a = &mut rest[lt + 1..];
        }
    }
}

/// Exact nearest-rank p50, p95 and p99 of `group` (rank `(p * n + 99) / 100`, 1-based, so index that minus one).
/// A selection leaves only larger values after its rank, so each higher rank is selected among those: half the
/// group for p95, a twentieth for p99. (An equal rank is already in place.)
fn percentiles(group: &mut [u32]) -> [u32; 3] {
    let n = group.len();
    let ranks = [50, 95, 99].map(|p| (p * n).div_ceil(100) - 1);
    let mut from = 0;
    for r in ranks {
        if r >= from {
            select(&mut group[from..], r - from);
            from = r + 1;
        }
    }
    ranks.map(|r| group[r])
}

/// The first `c` at or after `s`; one is known to follow.
#[inline(always)]
unsafe fn next_byte(mut s: *const u8, c: u8) -> *const u8 {
    loop {
        let m = eq32(s, c);
        if m != 0 {
            return s.add(m.trailing_zeros() as usize);
        }
        s = s.add(32);
    }
}

/// Endpoint names to ids, one table for all threads. Open addressing on a CRC of the name's first and last eight
/// bytes (masked when the name is shorter; the rest of the line keeps both loads in bounds), and a name's id is its
/// slot. A new name is copied into the arena, so the names compared on every line sit together, and then published
/// with one compare-and-swap; a thread that loses the race for the slot compares against the winner's name.
struct Names {
    /// `length << 32 | arena offset`, 0 if free.
    slot: Vec<AtomicU64>,
    used: AtomicU32,
    /// `ARENA + 32` zero bytes (so the pages are charged only when written), owned through this pointer and written
    /// only inside ranges reserved with `used`.
    arena: *mut u8,
}

// SAFETY: the arena is written only in a range that one thread reserved, before it publishes the range with a
// release store, and read only after an acquire load of that slot. (The 32-byte compares also load the bytes next to a
// name, which another thread may be writing; those lanes are masked off and never used.)
unsafe impl Sync for Names {}
unsafe impl Send for Names {}

impl Names {
    /// A seed has 1,842 names.
    const SLOTS: usize = 1 << 12;
    const ARENA: usize = Self::SLOTS * 128;

    fn new() -> Self {
        let arena = Box::into_raw(vec![0u8; Self::ARENA + 32].into_boxed_slice()).cast::<u8>();
        Names { slot: (0..Self::SLOTS).map(|_| AtomicU64::new(0)).collect(), used: AtomicU32::new(0), arena }
    }

    /// The name held by slot value `v`.
    fn name(&self, v: u64) -> &[u8] {
        // SAFETY: `v` was published after its bytes were copied, and the arena lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.arena.add(v as u32 as usize), (v >> 32) as usize) }
    }

    /// The id of the `len` bytes at `s` (`len` >= 1; 32 bytes must be readable at `s`, which the rest of the line and
    /// the spare page after the input guarantee).
    #[inline(always)]
    unsafe fn id(&self, s: *const u8, len: usize) -> u32 {
        let (mut a, mut b) = (load8(s), 0);
        if len >= 8 {
            b = load8(s.add(len - 8));
        } else {
            a &= (1u64 << (8 * len)) - 1;
        }
        let first = _mm_crc32_u64(_mm_crc32_u64(len as u64, a), b) as usize;
        for k in 0..Self::SLOTS {
            let i = (first + k) % Self::SLOTS;
            let mut v = self.slot[i].load(Acquire);
            if v == 0 {
                v = self.insert(i, s, len);
            }
            if (v >> 32) as usize == len && self.same(v, s, len) {
                return i as u32;
            }
        }
        panic!("more names than slots");
    }

    /// Copies the name into the arena and tries to publish it in slot `i`; returns what the slot holds then.
    #[cold]
    unsafe fn insert(&self, i: usize, s: *const u8, len: usize) -> u64 {
        let at = self.used.fetch_add(len as u32, Relaxed) as usize;
        assert!(at + len <= Self::ARENA, "endpoint names do not fit the arena");
        std::ptr::copy_nonoverlapping(s, self.arena.add(at), len);
        let mine = (len as u64) << 32 | at as u64;
        match self.slot[i].compare_exchange(0, mine, AcqRel, Acquire) {
            Ok(_) => mine,
            Err(winner) => winner,
        }
    }

    /// 32 bytes can always be read from either name (the rest of the line and the spare page follow one, the arena's
    /// spare bytes the other), so a name of up to 32 bytes is compared in one step instead of a `memcmp` call.
    #[inline(always)]
    unsafe fn same(&self, v: u64, s: *const u8, len: usize) -> bool {
        let x = self.arena.add(v as u32 as usize);
        if len > 32 {
            return std::slice::from_raw_parts(x, len) == std::slice::from_raw_parts(s, len);
        }
        let load = |p: *const u8| _mm256_loadu_si256(p.cast());
        let eq = _mm256_movemask_epi8(_mm256_cmpeq_epi8(load(x), load(s))) as u32;
        let mask = u32::MAX >> (32 - len);
        eq & mask == mask
    }
}

impl Drop for Names {
    fn drop(&mut self) {
        // SAFETY: `arena` came from `Box::into_raw` of a boxed slice of this length.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.arena, Self::ARENA + 32)) });
    }
}

/// One hour's records from one thread, in a chain of 16 KB chunks carved in order from a region the thread maps up
/// front (one `mmap`, so no lock is shared).
#[repr(C)]
struct Chunk {
    prev: *mut Chunk,
    n: u32,
    hour: u32,
    rec: [u32; Chunk::CAP],
}

impl Chunk {
    const CAP: usize = (16384 - 16) / 4;
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
}

const MAP_FAILED: *mut u8 = !0 as *mut u8;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;
const MADV_DONTNEED: i32 = 4;
const PR_SET_THP_DISABLE: i32 = 41;

/// What one thread found: its records by hour.
struct Parser {
    /// The newest chunk of each hour.
    last: [*mut Chunk; HOURS],
    /// (latency, key) of the long ones.
    wide: Vec<Vec<(u32, u32)>>,
    /// Where the next chunk is carved.
    next: *mut Chunk,
    /// The first chunk not yet released; only changed by whoever holds the releasing flag.
    kept: AtomicPtr<Chunk>,
}

// SAFETY: after the parse the chunks are only read (a chunk is released only when every hour in it is finished), and
// `kept` is atomic.
unsafe impl Sync for Parser {}
unsafe impl Send for Parser {}

impl Parser {
    /// Room for a log of `size` bytes: a line is at least 40 bytes, and each hour may leave one chunk part full.
    /// Pages are charged only once written.
    fn new(size: usize) -> Self {
        let bytes = (size / 40 / Chunk::CAP + 1 + HOURS) * std::mem::size_of::<Chunk>();
        // SAFETY: a fresh anonymous mapping.
        let region = unsafe { mmap(std::ptr::null_mut(), bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
        assert!(region != MAP_FAILED, "mmap of the record region failed");
        Parser { last: [std::ptr::null_mut(); HOURS], wide: vec![Vec::new(); HOURS], next: region.cast(), kept: AtomicPtr::new(region.cast()) }
    }

    /// Files every line that starts in `p..end` (`p` and `end` are line starts) under its hour.
    ///
    /// SAFETY: the bytes up to `end` + 128 are readable and every line is well formed, as the spec promises.
    unsafe fn run(&mut self, names: &Names, mut p: *const u8, end: *const u8) {
        while p < end {
            // YYYY-MM-DDTHH: month's last digit at 6, day at 8-9, hour at 11-12.
            let digit = |i: usize| usize::from((*p.add(i)).wrapping_sub(b'0'));
            let (day, hour) = (digit(8) * 10 + digit(9), digit(11) * 10 + digit(12));
            let h = (if digit(6) == 0 { day.wrapping_sub(30) } else { day + 1 }).wrapping_mul(24).wrapping_add(hour).wrapping_sub(18);

            // Spaces and newlines in the 64 bytes after the timestamp. Only an endpoint near its 125-byte maximum
            // pushes past the window.
            let m = p.add(28);
            let sp = u64::from(eq32(m, b' ')) | u64::from(eq32(m.add(32), b' ')) << 32;
            let nl = u64::from(eq32(m, b'\n')) | u64::from(eq32(m.add(32), b'\n')) << 32;
            let name = m.add(sp.trailing_zeros() as usize + 1);
            let name_end = if nl != 0 { m.add((sp & (sp - 1)).trailing_zeros() as usize) } else { next_byte(name, b' ') };
            let eol = if nl != 0 { m.add(nl.trailing_zeros() as usize) } else { next_byte(name_end.add(5), b'\n') };
            let key = names.id(name, name_end.offset_from(name) as usize) | if *name_end.add(1) == b'5' { ERR } else { 0 };
            let latency = decode(eol, eol.offset_from(name_end) as u32 - 5); // after " SSS "
            p = eol.add(1);

            if latency >= PACKED_MAX {
                self.wide[h].push((latency, key));
                continue;
            }
            let mut c = self.last[h];
            if c.is_null() || (*c).n as usize == Chunk::CAP {
                let fresh = self.next;
                self.next = fresh.add(1);
                (*fresh).prev = c;
                (*fresh).n = 0;
                (*fresh).hour = h as u32;
                c = fresh;
                self.last[h] = c;
            }
            (*c).rec[(*c).n as usize] = latency << KEY_BITS | key;
            (*c).n += 1;
        }
    }

    /// The log is in arrival order, so chunks are carved roughly hour by hour and hours finish in the same order: the
    /// run of chunks at the front whose hours are all finished grows steadily. Hand it back once it reaches 64 KB,
    /// in one `madvise`, instead of one call (and TLB shootdown) per chunk.
    ///
    /// Only one thread at a time may call this.
    fn release(&self, finished: &[AtomicBool]) {
        let kept = self.kept.load(Relaxed);
        let mut c = kept;
        // SAFETY: chunks from `kept` to `next` are still mapped and hold their header.
        unsafe {
            while c < self.next && finished[(*c).hour as usize].load(Acquire) {
                c = c.add(1);
            }
            if c.offset_from(kept) >= 4 || (c == self.next && c > kept) {
                madvise(kept.cast(), c.offset_from(kept) as usize * std::mem::size_of::<Chunk>(), MADV_DONTNEED);
                self.kept.store(c, Relaxed);
            }
        }
    }

    /// Calls `f(endpoint rank, 5xx flag, latency)` for every record of hour `h`.
    fn each(&self, h: usize, rank: &[u32], mut f: impl FnMut(usize, u32, u32)) {
        let mut c = self.last[h];
        // SAFETY: the chain holds initialised chunks whose pages have not been released (hour `h` is not finished).
        while let Some(chunk) = unsafe { c.as_ref() } {
            for &r in &chunk.rec[..chunk.n as usize] {
                f(rank[(r & ID_MASK) as usize] as usize, r & ERR, r >> KEY_BITS);
            }
            c = chunk.prev;
        }
        for &(latency, key) in &self.wide[h] {
            f(rank[(key & ID_MASK) as usize] as usize, key & ERR, latency);
        }
    }
}

/// The endpoints in output order.
struct Ranking<'a> {
    names: Vec<&'a [u8]>,
    /// Endpoint id to rank.
    rank: Vec<u32>,
}

/// One output line's numbers.
struct Line {
    e: u32,
    n: u32,
    p50: u32,
    p95: u32,
    p99: u32,
    errors: u32,
}

impl Line {
    fn numbers(&self) -> [u32; 6] {
        let ppm = (u64::from(self.errors) * 1_000_000 / u64::from(self.n)) as u32;
        [self.n, self.p50, self.p95, self.p99, self.errors, ppm]
    }
}

/// The text of an hour's label, `YYYY-MM-DDTHH:00:00Z `, for hour index `h`.
fn label(h: usize) -> [u8; 21] {
    let (day, hour) = ((h + 18) / 24, (h + 18) % 24);
    let (month, dom) = if day < 2 { (10, 30 + day) } else { (11, day - 1) };
    let two = |x: usize| [b'0' + (x / 10) as u8, b'0' + (x % 10) as u8];
    let mut out = *b"2026-MM-DDTHH:00:00Z ";
    out[5..7].copy_from_slice(&two(month));
    out[8..10].copy_from_slice(&two(dom));
    out[11..13].copy_from_slice(&two(hour));
    out
}

/// Writes the decimal digits of `x` at the start of `out`; returns how many.
#[inline(always)]
fn put_number(out: &mut [u8], mut x: u32) -> usize {
    let w = width(x);
    for d in out[..w].iter_mut().rev() {
        *d = b'0' + (x % 10) as u8;
        x /= 10;
    }
    w
}

/// Scans `d` (followed by at least 128 readable bytes (the 64-byte window of the last line)) with `threads` threads that claim `pieces` line-aligned pieces
/// in turn, then summarises every hour in parallel. `create_output(total)` is called once, with the size of the
/// output, and returns the address of `total` writable bytes that the threads fill.
fn process(d: &[u8], threads: usize, pieces: usize, create_output: &(dyn Fn(usize) -> *mut u8 + Sync)) {
    let size = d.len();
    // Line-aligned pieces claimed in order: a thread that starts late or shares its core takes fewer.
    let mut cut = vec![size; pieces + 1];
    cut[0] = 0;
    for k in 1..pieces {
        cut[k] = cut[k - 1].max(size * k / pieces);
        while cut[k] > 0 && cut[k] < size && d[cut[k] - 1] != b'\n' {
            cut[k] += 1;
        }
    }
    let claim_piece = AtomicUsize::new(0);

    let names = Names::new();
    let (next_thread, parsers): (_, Vec<OnceLock<Parser>>) = (AtomicUsize::new(0), (0..threads).map(|_| OnceLock::new()).collect());
    let parsed = Barrier::new(threads);
    // Between the phases, endpoints are ranked in unsigned byte order (a proper prefix first), which is the order
    // lines are printed in an hour.
    let ranking: OnceLock<Ranking> = OnceLock::new();

    // An hour is finished once its records are copied out; one thread at a time then releases whatever chunks that
    // frees.
    let finished: Vec<AtomicBool> = (0..HOURS).map(|_| AtomicBool::new(false)).collect();
    let releasing = AtomicBool::new(false);
    let (claim, claim_text) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let lines: Vec<Mutex<Vec<Line>>> = (0..HOURS).map(|_| Mutex::new(Vec::new())).collect();
    // An hour's text size, then its offset in the output.
    let at: Vec<AtomicUsize> = (0..=HOURS).map(|_| AtomicUsize::new(0)).collect();
    let sized = Barrier::new(threads);
    let output: OnceLock<usize> = OnceLock::new();

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut parser = Parser::new(size);
                loop {
                    let k = claim_piece.fetch_add(1, Relaxed);
                    if k >= pieces {
                        break;
                    }
                    // SAFETY: `d` is a well-formed log followed by 128 readable bytes; the cuts are line starts.
                    unsafe { parser.run(&names, d.as_ptr().add(cut[k]), d.as_ptr().add(cut[k + 1])) };
                }
                assert!(parsers[next_thread.fetch_add(1, Relaxed)].set(parser).is_ok());
                parsed.wait();

                let ranking = ranking.get_or_init(|| {
                    let mut by_name: Vec<(&[u8], usize)> = Vec::new();
                    for (id, s) in names.slot.iter().enumerate() {
                        let v = s.load(Acquire);
                        if v != 0 {
                            by_name.push((names.name(v), id));
                        }
                    }
                    by_name.sort_unstable();
                    let mut rank = vec![0; Names::SLOTS];
                    for (r, &(_, id)) in by_name.iter().enumerate() {
                        rank[id] = r as u32;
                    }
                    Ranking { names: by_name.iter().map(|&(name, _)| name).collect(), rank }
                });
                let parsers: Vec<&Parser> = parsers.iter().map(|p| p.get().unwrap()).collect();
                {
                    // The scratch goes before the text phase, where the output counts.
                    let ne = ranking.names.len();
                    let (mut start, mut errors, mut cursor) = (vec![0u32; ne + 1], vec![0u32; ne], vec![0u32; ne]);
                    let mut latency = Vec::new();
                    loop {
                        let h = claim.fetch_add(1, Relaxed);
                        if h >= HOURS {
                            break;
                        }
                        // Counting sort of the hour's latencies by endpoint rank: count into start[e + 1], prefix,
                        // then place each latency at its group's cursor.
                        start.fill(0);
                        errors.fill(0);
                        for ps in &parsers {
                            ps.each(h, &ranking.rank, |e, err, _| {
                                start[e + 1] += 1;
                                errors[e] += u32::from(err != 0);
                            });
                        }
                        for e in 0..ne {
                            start[e + 1] += start[e];
                        }
                        latency.resize(start[ne] as usize, 0);
                        cursor.copy_from_slice(&start[..ne]);
                        for ps in &parsers {
                            ps.each(h, &ranking.rank, |e, _, l| {
                                latency[cursor[e] as usize] = l;
                                cursor[e] += 1;
                            });
                        }
                        finished[h].store(true, Release);
                        if !releasing.swap(true, Acquire) {
                            for ps in &parsers {
                                ps.release(&finished);
                            }
                            releasing.store(false, Release);
                        }

                        let mut hour_lines = Vec::new();
                        let mut bytes = 0;
                        for e in 0..ne {
                            let n = (start[e + 1] - start[e]) as usize;
                            if n == 0 {
                                continue;
                            }
                            let [p50, p95, p99] = percentiles(&mut latency[start[e] as usize..][..n]);
                            let line = Line { e: e as u32, n: n as u32, p50, p95, p99, errors: errors[e] };
                            // Label, name, six numbers each after a space, newline.
                            bytes += 21 + ranking.names[e].len() + 7;
                            bytes += line.numbers().into_iter().map(width).sum::<usize>();
                            hour_lines.push(line);
                        }
                        *lines[h].lock().unwrap() = hour_lines;
                        at[h].store(bytes, Relaxed);
                    }
                }
                sized.wait();

                let text = *output.get_or_init(|| {
                    // Every hour is finished: whatever chunks the last releases left go back now, before the
                    // output's pages start to count.
                    for ps in &parsers {
                        ps.release(&finished);
                    }
                    let mut total = 0;
                    for slot in &at {
                        total += slot.swap(total, Relaxed);
                    }
                    if total == 0 {
                        0
                    } else {
                        create_output(total) as usize
                    }
                });

                loop {
                    let h = claim_text.fetch_add(1, Relaxed);
                    if h >= HOURS {
                        break;
                    }
                    let hour_lines = std::mem::take(&mut *lines[h].lock().unwrap());
                    if !hour_lines.is_empty() {
                        let (from, to) = (at[h].load(Relaxed), at[h + 1].load(Relaxed));
                        // SAFETY: `text` maps all the output's bytes and each hour owns its own range of them.
                        let mut out = unsafe { std::slice::from_raw_parts_mut((text as *mut u8).add(from), to - from) };
                        let label = label(h);
                        for l in &hour_lines {
                            let name = ranking.names[l.e as usize];
                            let (head, rest) = std::mem::take(&mut out).split_at_mut(21 + name.len());
                            head[..21].copy_from_slice(&label);
                            head[21..].copy_from_slice(name);
                            out = rest;
                            for x in l.numbers() {
                                out[0] = b' ';
                                out = &mut out[1..];
                                let w = put_number(out, x);
                                out = &mut out[w..];
                            }
                            out[0] = b'\n';
                            out = &mut out[1..];
                        }
                        debug_assert!(out.is_empty());
                    }
                }
            });
        }
    });
}

fn main() {
    let input_path = std::env::var("INPUT_PATH").unwrap_or_else(|_| "/data/requests.log".to_string());
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/summary.txt".to_string());
    // With transparent huge pages, a chunk region written only in part could be backed by whole 2 MB pages and count
    // against peak memory.
    // SAFETY: a plain prctl call.
    unsafe { prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 12);
    if let Err(e) = run(&input_path, &output_path, threads) {
        eprintln!("track-a: failed reading {input_path} or writing {output_path}: {e}");
        std::process::exit(1);
    }
    // Nothing is freed: the process ends here.
    std::process::exit(0);
}

fn run(input_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let input = std::fs::File::open(input_path)?;
    let out = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(output_path)?;

    // Map the log over one spare zero page, so the 64-byte scans stay in bounds.
    let size = input.metadata()?.len() as usize;
    // SAFETY: an anonymous read-only reservation of `size` + one page, then the file mapped over its start (MAP_FIXED
    // replaces only pages of this reservation); the page after the file is zero, as is the rest of the last file page.
    let log = unsafe {
        let log = mmap(std::ptr::null_mut(), size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if log == MAP_FAILED || (size > 0 && mmap(log, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, input.as_raw_fd(), 0) == MAP_FAILED) {
            return Err(std::io::Error::last_os_error());
        }
        std::slice::from_raw_parts(log, size)
    };

    // Called by one thread once the output size is known: size the file and map it, so the threads write the text
    // straight into it.
    let create_output = |total: usize| {
        if let Err(e) = out.set_len(total as u64) {
            eprintln!("track-a: cannot size {output_path}: {e}");
            std::process::exit(1);
        }
        // SAFETY: a shared mapping of the file just sized to `total` bytes; it stays mapped until the process ends.
        let text = unsafe { mmap(std::ptr::null_mut(), total, PROT_READ | PROT_WRITE, MAP_SHARED, out.as_raw_fd(), 0) };
        if text == MAP_FAILED {
            eprintln!("track-a: cannot map {output_path}: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        text
    };
    process(log, threads, 8 * threads, &create_output);
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/track_a.rs"]
mod tests;
