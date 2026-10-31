//! Track A — Ingest. Aggregates per-(hour, endpoint) request counts, exact
//! nearest-rank p50/p95/p99 latency and 5xx counts from a space-delimited
//! log (`ts method endpoint status latency_us`), as in specs/a-ingest.md.
//!
//! One pass over the mapped input and nothing else is shared in the hot loop:
//! workers take 1 MiB chunks from an atomic cursor and turn each line into one
//! `u32` (endpoint number, error flag, 20-bit latency; a longer latency is
//! followed by its full value) appended to a private arena. The endpoint is
//! found in a private table that stores the offset of the first occurrence and
//! verifies it byte for byte. After a barrier, workers take one hour at a time:
//! count per endpoint, place the latencies in one run per endpoint, select the
//! percentiles, format, and hand the block to the main thread, which writes
//! the blocks in hour order.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{mpsc, Barrier, Mutex, OnceLock};

/// A stored line is `endpoint << 21 | error << 20 | latency`; a latency that does not fit 20 bits is stored as
/// `LAT_MASK` and followed by a whole `u32` holding the latency.
const LAT_MASK: u32 = (1 << 20) - 1;
/// The spec gives every seed a catalogue of exactly 1,842 endpoints, so 11 bits of endpoint number are enough.
const MAX_ENDPOINTS: usize = 1 << 11;
/// Slots of a worker's endpoint table (at most a quarter used).
const SLOTS: usize = 1 << 13;
/// Bytes after the end of the input that may be read (the vector loads of the last line).
const PAD: usize = 128;

/// Maps `path` read-only and returns it with at least `PAD` readable zero bytes after the end. The harness
/// pre-warms `/data` into the page cache, so mapped pages are not charged to this run; `fs::read` would copy.
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
    // SAFETY: an anonymous read-only reservation of `len` + one page, then the file mapped over its start (MAP_FIXED
    // replaces only pages of this reservation); the page after the file is zero, as is the rest of the last file page.
    let p = unsafe {
        let anon = mmap(std::ptr::null_mut(), len + 4096, 1, 0x22, -1, 0);
        if anon as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        mmap(anon, len, 1, 0x12, file.as_raw_fd(), 0)
    };
    if p as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `p` points to `len` readable bytes that stay mapped for the life of the process.
    Ok(unsafe { std::slice::from_raw_parts(p, len) })
}

#[cfg(not(unix))]
fn map_file(path: &str) -> std::io::Result<&'static [u8]> {
    let mut data = std::fs::read(path)?;
    let len = data.len();
    data.resize(len + PAD, 0);
    Ok(&Box::leak(data.into_boxed_slice())[..len])
}

/// SAFETY (all raw reads below): `p` and the bytes read are inside the mapped input or its `PAD` zero bytes, which
/// holds for every line of well-formed input (the spec guarantees it).
#[inline(always)]
unsafe fn load8(p: *const u8) -> u64 {
    p.cast::<u64>().read_unaligned()
}

/// Bit i is set when byte `p + i` is a space or a line feed (the only bytes of a line below `!`).
#[inline(always)]
unsafe fn delimiters(p: *const u8) -> u64 {
    use std::arch::x86_64::*;
    let bang = _mm256_set1_epi8(0x21);
    let half = |o: usize| _mm256_movemask_epi8(_mm256_cmpgt_epi8(bang, _mm256_loadu_si256(p.add(o).cast()))) as u32 as u64;
    half(0) | half(32) << 32
}

/// The decimal number at `s..e` (1 to 10 digits): the last eight digits by SWAR on one 8-byte load that ends at `e`.
#[inline(always)]
unsafe fn parse_latency(base: *const u8, s: usize, e: usize) -> u32 {
    let n = (e - s).min(8);
    // The digits are the top `n` bytes of the load; the bytes below are cleared, which reads as leading zeros.
    let w = load8(base.add(e - 8)) & 0x0F0F_0F0F_0F0F_0F0F & (u64::MAX << (64 - 8 * n));
    let w = w * 10 + (w >> 8);
    let mut v = ((w & 0x0000_00FF_0000_00FF) * 0x000F_4240_0000_0064 + ((w >> 16) & 0x0000_00FF_0000_00FF) * 0x0000_2710_0000_0001) >> 32;
    if e - s > 8 {
        let high = (s..e - 8).fold(0, |a, i| a * 10 + u64::from(*base.add(i) - b'0'));
        v += high * 100_000_000;
    }
    v as u32
}

/// Whether the `len` bytes at `p` and `q` are equal (`len` >= 1): the first and the last 32 bytes (masked to the
/// name when it is shorter), and 32-byte blocks between them for the rare name over 64 bytes.
#[inline(always)]
unsafe fn same(p: *const u8, q: *const u8, len: usize) -> bool {
    use std::arch::x86_64::*;
    let eq = |o: isize| _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(p.offset(o).cast()), _mm256_loadu_si256(q.offset(o).cast()))) as u32;
    // Name bytes `0..min(len, 32)` are the low bits of the first load, the last `min(len, 32)` the high bits of the second.
    let (low, high) = (u32::MAX >> (32 - len.min(32)), u32::MAX << (32 - len.min(32)));
    let mut ok = (eq(0) & low == low) & (eq(len as isize - 32) & high == high);
    if len > 64 {
        ok = ok && (32..len as isize - 32).step_by(32).all(|o| eq(o) == u32::MAX);
    }
    ok
}

/// The endpoint numbers handed out so far and the names that go with them.
#[derive(Default)]
struct Names {
    ids: HashMap<&'static [u8], u32>,
    list: Vec<&'static [u8]>,
}

/// What one worker found: all its lines in scan order, and where the hour (`YYYY-MM-DDTHH` as two overlapping
/// 8-byte loads) changes: `(first load, second load, index in arena)`.
struct Part {
    arena: Vec<u32>,
    runs: Vec<(u64, u64, usize)>,
}

struct Worker {
    d: &'static [u8],
    /// Endpoint table: `offset of the name in store | length << 32 | number << 40`; 0 is a free slot.
    table: Vec<u64>,
    /// A copy of each name this worker has seen, one after the other (so that the names the lines are compared
    /// with sit together), with 32 zero bytes before them and 32 bytes of capacity after (the compares mask
    /// whatever lies outside a name).
    store: Vec<u8>,
    part: Part,
    key: (u64, u64),
    pos: Vec<u32>,
}

impl Worker {
    fn new(d: &'static [u8]) -> Self {
        // A line has at least 40 bytes and takes at most two words.
        let arena = Vec::with_capacity(d.len() / 20 + 16);
        Worker { d, table: vec![0; SLOTS], store: vec![0; 32], part: Part { arena, runs: Vec::new() }, key: (0, 0), pos: vec![0; 1016] }
    }

    /// The number of the endpoint `d[es..ee]`, taken from the shared names the first time this worker sees it.
    #[inline(always)]
    unsafe fn endpoint(&mut self, es: usize, ee: usize, names: &Mutex<Names>) -> u32 {
        let (base, len) = (self.d.as_ptr(), ee - es);
        let bits = 8 * len.min(8) as u32;
        let a = load8(base.add(es)) & (u64::MAX >> (64 - bits));
        let b = load8(base.add(ee - 8)) & (u64::MAX << (64 - bits));
        let mut i = ((a ^ b.rotate_left(29) ^ len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 51) as usize;
        loop {
            let slot = *self.table.get_unchecked(i);
            if slot == 0 {
                return self.insert(es, ee, i, names);
            }
            if (slot >> 32) as u8 as usize == len && same(base.add(es), self.store.as_ptr().add(slot as u32 as usize), len) {
                return (slot >> 40) as u32;
            }
            i = (i + 1) % SLOTS;
        }
    }

    #[cold]
    fn insert(&mut self, es: usize, ee: usize, i: usize, names: &Mutex<Names>) -> u32 {
        let mut shared = names.lock().unwrap();
        let next = shared.list.len() as u32;
        let id = *shared.ids.entry(&self.d[es..ee]).or_insert(next);
        if id == next {
            shared.list.push(&self.d[es..ee]);
        }
        assert!((id as usize) < MAX_ENDPOINTS, "more endpoints than the spec's catalogue of 1,842");
        let off = self.store.len();
        self.store.extend_from_slice(&self.d[es..ee]);
        self.store.reserve(32);
        self.table[i] = off as u64 | ((ee - es) as u64) << 32 | u64::from(id) << 40;
        id
    }

    /// Parses the lines that start in `[start, end)` (`end` is a line start). Every line has exactly five
    /// delimiters (spaces and the line feed), so the delimiters of a stretch of input are first listed as offsets
    /// (about 4 per 64 bytes, 8 written unconditionally), and the lines are then read from that list five at a time:
    /// no line has to be parsed to find the next.
    fn scan(&mut self, start: usize, end: usize, names: &Mutex<Names>) {
        let base = self.d.as_ptr();
        let first = self.part.arena.as_mut_ptr();
        let mut pos = std::mem::take(&mut self.pos);
        // SAFETY: the arena has room for two words per line of the whole input, `pos` for 1000 offsets plus the
        // 16 a window can add, and see `PAD` for the reads.
        unsafe {
            let mut w = first.add(self.part.arena.len());
            let (mut at, mut have) = (start, 0);
            while at < end || have >= 5 {
                while have < 1000 && at < end {
                    let mut m = delimiters(base.add(at));
                    if end - at < 64 {
                        m &= (1 << (end - at)) - 1;
                    }
                    let (out, count) = (pos.as_mut_ptr().add(have), m.count_ones() as usize);
                    for k in 0..16 {
                        if k < 8 || count > 8 {
                            *out.add(k) = (at as u32).wrapping_add(m.trailing_zeros());
                            m &= m.wrapping_sub(1);
                        }
                    }
                    have += count;
                    at += 64;
                }
                for i in 0..have / 5 {
                    let p = pos.as_ptr().add(5 * i);
                    let (ls, es, ee, nl) = ((*p - 27) as usize, (*p.add(1) + 1) as usize, *p.add(2) as usize, *p.add(4) as usize);
                    let id = self.endpoint(es, ee, names);
                    let error = u32::from(*base.add(ee + 1) >= b'5');
                    let latency = parse_latency(base, ee + 5, nl);
                    let key = (load8(base.add(ls)), load8(base.add(ls + 5)));
                    if key != self.key {
                        self.key = key;
                        self.part.runs.push((key.0, key.1, w.offset_from(first) as usize));
                    }
                    let line = id << 21 | error << 20;
                    if latency < LAT_MASK {
                        *w = line | latency;
                        w = w.add(1);
                    } else {
                        *w = line | LAT_MASK;
                        *w.add(1) = latency;
                        w = w.add(2);
                    }
                }
                pos.copy_within(have / 5 * 5..have, 0);
                have %= 5;
            }
            self.part.arena.set_len(w.offset_from(first) as usize);
        }
        self.pos = pos;
    }
}

/// The text of an hour (`YYYY-MM-DDTHH`) from its two overlapping loads.
fn hour_text(key: (u64, u64)) -> [u8; 13] {
    let mut text = [0u8; 13];
    text[..8].copy_from_slice(&key.0.to_le_bytes());
    text[8..].copy_from_slice(&key.1.to_le_bytes()[3..]);
    text
}

/// Exact nearest-rank percentiles p50, p95 and p99 of `lat` (the spec's
/// integer rank `(p * n + 99) / 100`, 1-based), by selecting the largest
/// rank first and the others inside what stays below it.
fn percentiles(lat: &mut [u32]) -> [u32; 3] {
    let n = lat.len();
    if n <= 32 {
        lat.sort_unstable();
        return [49, 94, 98].map(|p| lat[((p + 1) * n).div_ceil(100) - 1]);
    }
    let at = [49, 94, 98].map(|p| ((p + 1) * n).div_ceil(100) - 1);
    let (mut out, mut hi) = ([0; 3], n);
    for k in (0..3).rev() {
        if k < 2 && at[k] == at[k + 1] {
            out[k] = out[k + 1];
        } else {
            out[k] = *lat[..hi].select_nth_unstable(at[k]).1;
            hi = at[k];
        }
    }
    out
}

fn write_u64(buf: &mut [u8; 20], mut v: u64) -> &[u8] {
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return &buf[i..];
        }
    }
}

/// Everything the output phase needs once the scan is done.
struct Plan {
    /// Each hour's text and its lines, as the slices of every worker's arena that hold that hour.
    hours: Vec<([u8; 13], Vec<&'static [u32]>)>,
    names: Vec<&'static [u8]>,
    /// The endpoint numbers by endpoint bytes.
    order: Vec<u32>,
}

fn make_plan(parts: &'static [OnceLock<Part>], names: &Mutex<Names>) -> Plan {
    let mut by_hour: BTreeMap<[u8; 13], Vec<&'static [u32]>> = BTreeMap::new();
    for part in parts.iter().map(|p| p.get().unwrap()) {
        for (i, run) in part.runs.iter().enumerate() {
            let end = part.runs.get(i + 1).map_or(part.arena.len(), |r| r.2);
            by_hour.entry(hour_text((run.0, run.1))).or_default().push(&part.arena[run.2..end]);
        }
    }
    let names = std::mem::take(&mut names.lock().unwrap().list);
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_unstable_by_key(|&id| names[id as usize]);
    Plan { hours: by_hour.into_iter().collect(), names, order }
}

/// Per-thread buffers reused from hour to hour.
#[derive(Default)]
struct Scratch {
    count: Vec<u64>,
    start: Vec<usize>,
    lat: Vec<u32>,
}

/// The output lines of one hour of the plan.
fn format_hour(plan: &Plan, hour: usize, sc: &mut Scratch) -> Vec<u8> {
    let (text, parts) = &plan.hours[hour];
    let (count, start, lat) = (&mut sc.count, &mut sc.start, &mut sc.lat);
    count.clear();
    count.resize(plan.names.len(), 0);
    let mut lines = 0;
    for part in parts {
        let mut it = part.iter();
        while let Some(&w) = it.next() {
            count[(w >> 21) as usize] += 1 | u64::from(w >> 20 & 1) << 32;
            if w & LAT_MASK == LAT_MASK {
                it.next();
            }
            lines += 1;
        }
    }
    start.clear();
    start.extend(count.iter().scan(0, |total, &c| Some(std::mem::replace(total, *total + c as u32 as usize))));
    lat.resize(lines, 0);
    for part in parts {
        let mut it = part.iter();
        while let Some(&w) = it.next() {
            let at = &mut start[(w >> 21) as usize];
            lat[*at] = if w & LAT_MASK == LAT_MASK { *it.next().unwrap() } else { w & LAT_MASK };
            *at += 1;
        }
    }
    // The hour's lines are in `lat` now: give the pages of the arenas that hold only this hour back to the system, so
    // the output written so far is paid for by the input freed (the lines are most of the process's memory).
    #[cfg(unix)]
    for part in parts {
        extern "C" {
            fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
        }
        let (from, to) = (part.as_ptr() as usize, part.as_ptr() as usize + 4 * part.len());
        let (from, to) = ((from + 4095) & !4095, to & !4095);
        if to > from {
            // SAFETY: whole pages inside this hour's slice of an arena; only this hour reads them, and 4 is MADV_DONTNEED.
            unsafe { madvise(from as *mut u8, to - from, 4) };
        }
    }
    let mut out = Vec::with_capacity(lines * 3 + 4096);
    let mut buf = [0u8; 20];
    for &id in &plan.order {
        let n = count[id as usize] as u32 as usize;
        if n == 0 {
            continue;
        }
        let end = start[id as usize];
        let [p50, p95, p99] = percentiles(&mut lat[end - n..end]);
        let errors = count[id as usize] >> 32;
        out.extend_from_slice(text);
        out.extend_from_slice(b":00:00Z ");
        out.extend_from_slice(plan.names[id as usize]);
        // floor(errors * 1_000_000 / count): integer division already floors.
        for v in [n as u64, p50.into(), p95.into(), p99.into(), errors, errors * 1_000_000 / n as u64] {
            out.push(b' ');
            out.extend_from_slice(write_u64(&mut buf, v));
        }
        out.push(b'\n');
    }
    out
}

/// Scans `d` with `threads` workers taking `chunk`-byte chunks, then formats every hour in parallel and writes
/// the blocks to `sink` in hour order, each as soon as it and all earlier hours are done. `d` must be followed by
/// `PAD` readable bytes.
fn process(d: &'static [u8], threads: usize, chunk: usize, sink: &mut impl Write) -> std::io::Result<()> {
    let n = d.len();
    // The first line start at or after `x` (a line start exactly at `x` goes to the chunk before): adjacent chunks agree.
    let snap = |x: usize| if x >= n { n } else if x == 0 { 0 } else { x + d[x..].iter().position(|&b| b == b'\n').unwrap() + 1 };
    let (cursor, next, barrier) = (AtomicUsize::new(0), AtomicUsize::new(0), Barrier::new(threads));
    let names = Mutex::new(Names::default());
    let parts: &'static [OnceLock<Part>] = Box::leak((0..threads).map(|_| OnceLock::new()).collect());
    let plan: &'static OnceLock<Plan> = Box::leak(Box::default());
    let (sender, receiver) = mpsc::channel::<(usize, Vec<u8>)>();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let (sender, cursor, next, barrier, names) = (sender.clone(), &cursor, &next, &barrier, &names);
            scope.spawn(move || {
                let mut worker = Worker::new(d);
                loop {
                    let from = cursor.fetch_add(chunk, Relaxed);
                    if from >= n {
                        break;
                    }
                    worker.scan(snap(from), snap(from + chunk), names);
                }
                parts[t].set(worker.part).ok();
                barrier.wait();
                let plan = plan.get_or_init(|| make_plan(parts, names));
                let mut scratch = Scratch::default();
                loop {
                    let hour = next.fetch_add(1, Relaxed);
                    if hour >= plan.hours.len() {
                        return;
                    }
                    sender.send((hour, format_hour(plan, hour, &mut scratch))).expect("writer stopped");
                }
            });
        }
        drop(sender);
        let mut waiting: Vec<Option<Vec<u8>>> = Vec::new();
        let mut written = 0;
        for (index, block) in receiver {
            if waiting.len() <= index {
                waiting.resize(index + 1, None);
            }
            waiting[index] = Some(block);
            while let Some(block) = waiting.get_mut(written).and_then(Option::take) {
                sink.write_all(&block)?;
                written += 1;
            }
        }
        Ok(())
    })
}

fn main() {
    let input_path = std::env::var("INPUT_PATH").unwrap_or_else(|_| "/data/requests.log".to_string());
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/summary.txt".to_string());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if let Err(e) = run(&input_path, &output_path, threads) {
        eprintln!("track-a: failed reading {input_path} or writing {output_path}: {e}");
        std::process::exit(1);
    }
    // Nothing is freed: the process ends here.
    std::process::exit(0);
}

fn run(input_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    let data = map_file(input_path)?;
    let mut file = std::fs::File::create(output_path)?;
    process(data, threads, 1 << 20, &mut file)
}

#[cfg(test)]
#[path = "../../tests/track_a.rs"]
mod tests;
