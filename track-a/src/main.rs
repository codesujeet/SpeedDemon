//! Track A — Ingest. Aggregates per-(hour, endpoint) request counts, exact
//! nearest-rank p50/p95/p99 latency and 5xx counts from a space-delimited
//! log (`ts method endpoint status latency_us`), as in specs/a-ingest.md.
//!
//! The input is mapped and cut into morsels that workers take from an atomic
//! cursor. A worker turns each line into one packed `u32` (endpoint number,
//! error flag, latency, the latency kept in a side list when it is 65535 or more) and appends it to its own list for the line's hour;
//! endpoint numbers come from a table the worker keeps for the whole run,
//! backed by one shared table that hands out the numbers. When the scan is
//! done, workers take one hour at a time: they count its lines per endpoint
//! (the packed values of all workers for that hour), place the latencies in
//! one run per endpoint, select the percentiles and format the lines, which
//! are written in hour order as they complete.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};

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
    // SAFETY: a fresh read-only private mapping of `len` bytes of an open file; it is never unmapped.
    let p = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
    if p as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `p` points to `len` readable bytes that stay mapped for the life of the process.
    Ok(unsafe { std::slice::from_raw_parts(p, len) })
}

#[cfg(not(unix))]
fn map_file(path: &str) -> std::io::Result<&'static [u8]> {
    Ok(Box::leak(std::fs::read(path)?.into_boxed_slice()))
}

#[inline]
fn load8(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

/// Bit i is set when byte `at + i` is a space or a line feed.
#[inline]
fn delimiters(d: &[u8], at: usize) -> u64 {
    use std::arch::x86_64::*;
    assert!(at + 64 <= d.len());
    // SAFETY: the 64 bytes at `at` are inside `d` (asserted); AVX2 is enabled for the build (target-cpu=x86-64-v3).
    unsafe {
        let (space, line_feed) = (_mm256_set1_epi8(b' ' as i8), _mm256_set1_epi8(b'\n' as i8));
        let window = |o: usize| {
            let v = _mm256_loadu_si256(d.as_ptr().add(at + o) as *const __m256i);
            _mm256_movemask_epi8(_mm256_or_si256(_mm256_cmpeq_epi8(v, space), _mm256_cmpeq_epi8(v, line_feed))) as u32 as u64
        };
        window(0) | window(32) << 32
    }
}

/// Whether the `len` (1 to 125) bytes at `a` and at `b` are equal, from four
/// masked 32-byte compares, so that the cost does not depend on `len`.
#[inline]
fn same_name(d: &[u8], a: usize, b: usize, len: usize) -> bool {
    use std::arch::x86_64::*;
    assert!(a.max(b) + 128 <= d.len());
    // SAFETY: the 128 bytes at `a` and at `b` are inside `d` (asserted); AVX2 is enabled for the build (target-cpu=x86-64-v3).
    unsafe {
        let mut diff = _mm256_setzero_si256();
        let index = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
        for k in 0..4 {
            let load = |at: usize| _mm256_loadu_si256(d.as_ptr().add(at + 32 * k) as *const __m256i);
            let inside = _mm256_cmpgt_epi8(_mm256_set1_epi8((len as i32 - 32 * k as i32) as i8), index);
            diff = _mm256_or_si256(diff, _mm256_and_si256(_mm256_xor_si256(load(a), load(b)), inside));
        }
        _mm256_testz_si256(diff, diff) != 0
    }
}

/// (endpoint start, endpoint end, line feed) of the line at `ls`. The 27-byte
/// timestamp is skipped; the next four delimiters (after the method, the
/// endpoint, the status, and the line feed) are almost always in the first
/// 64-byte window, and longer lines take further windows.
#[inline]
fn split(d: &[u8], ls: usize) -> (usize, usize, usize) {
    let (mut at, mut found, mut n) = (ls + 28, [0usize; 4], 0);
    loop {
        let mut bits = delimiters(d, at);
        if n == 0 && bits.count_ones() >= 4 {
            for slot in &mut found {
                *slot = at + bits.trailing_zeros() as usize;
                bits &= bits - 1;
            }
            return (found[0] + 1, found[1], found[3]);
        }
        while bits != 0 && n < 4 {
            found[n] = at + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            n += 1;
        }
        if n == 4 {
            return (found[0] + 1, found[1], found[3]);
        }
        at += 64;
    }
}

/// The decimal number in `d[s..e]` (1 to 10 digits): the last eight digits
/// by SWAR on one 8-byte load that ends at `e`, the others one by one.
#[inline]
fn parse_latency(d: &[u8], s: usize, e: usize) -> u64 {
    let len = e - s;
    let n = len.min(8);
    // The digits are the top `n` bytes of the load; the bytes below are cleared, which reads as leading zeros.
    let w = load8(d, e - 8) & 0x0F0F_0F0F_0F0F_0F0F & (u64::MAX << (64 - 8 * n));
    let w = w * 10 + (w >> 8);
    let mut v = ((w & 0x0000_00FF_0000_00FF) * 0x000F_4240_0000_0064
        + ((w >> 16) & 0x0000_00FF_0000_00FF) * 0x0000_2710_0000_0001)
        >> 32;
    if len > 8 {
        let high = d[s..e - 8].iter().fold(0, |a, &c| a * 10 + u64::from(c - b'0'));
        v += high * 100_000_000;
    }
    v & 0xFFFF_FFFF
}

/// The endpoint numbers handed out so far and the names that go with them.
#[derive(Default)]
struct Names {
    ids: HashMap<&'static [u8], u32>,
    list: Vec<&'static [u8]>,
}

/// One slot of a worker's endpoint table: where an occurrence of the name
/// is in the worker's input, and its number. `len == 0` marks a free slot.
#[derive(Clone, Copy, Default)]
struct Slot {
    start: usize,
    len: u32,
    id: u32,
}

/// The lines one worker found for one hour: one `u32` per line (`error << 31 |
/// endpoint << 16 | latency`), where a latency of 65535 or more is stored as
/// 65535 and its real value is the next entry of the second list.
type Bucket = (Vec<u32>, Vec<u32>);

/// What one worker found: for each hour it saw, the lines of that hour.
struct Worker {
    d: &'static [u8],
    table: Vec<Slot>,
    used: usize,
    hours: Vec<((u64, u64), Bucket)>,
    cur_key: (u64, u64),
    cur: usize,
}

impl Worker {
    fn new(d: &'static [u8]) -> Self {
        Worker { d, table: vec![Slot::default(); 4096], used: 0, hours: Vec::new(), cur_key: (0, 0), cur: 0 }
    }

    /// Table position at which the name `d[s..e]` starts probing.
    #[inline]
    fn home(&self, s: usize, e: usize) -> usize {
        let (d, len) = (self.d, e - s);
        let (a, b, c) = if len >= 8 {
            (load8(d, s), load8(d, e - 8), load8(d, s + (len - 8) / 2))
        } else {
            (load8(d, s) & ((1u64 << (8 * len)) - 1), 0, 0)
        };
        let h = (a ^ b.rotate_left(21) ^ c.rotate_left(42) ^ len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (h >> 32) as usize & (self.table.len() - 1)
    }

    /// The number of the endpoint `d[s..e]`, taken from the shared table the
    /// first time this worker sees it.
    #[inline]
    fn endpoint(&mut self, s: usize, e: usize, names: &Mutex<Names>) -> u32 {
        let d = self.d;
        let mask = self.table.len() - 1;
        let mut i = self.home(s, e);
        while self.table[i].len != 0 {
            let slot = self.table[i];
            if slot.len as usize == e - s && same_name(d, slot.start, s, e - s) {
                return slot.id;
            }
            i = (i + 1) & mask;
        }
        let mut shared = names.lock().unwrap();
        let next = shared.list.len() as u32;
        let id = *shared.ids.entry(&d[s..e]).or_insert(next);
        assert!(id < 1 << 15, "more than 32,768 distinct endpoints");
        if id == next {
            shared.list.push(&d[s..e]);
        }
        drop(shared);
        self.table[i] = Slot { start: s, len: (e - s) as u32, id };
        self.used += 1;
        if self.used * 2 > self.table.len() {
            let bigger = vec![Slot::default(); self.table.len() * 2];
            let old = std::mem::replace(&mut self.table, bigger);
            for slot in old.into_iter().filter(|s| s.len != 0) {
                let mut j = self.home(slot.start, slot.start + slot.len as usize);
                while self.table[j].len != 0 {
                    j = (j + 1) & (self.table.len() - 1);
                }
                self.table[j] = slot;
            }
        }
        id
    }

    /// Parses the lines that start in `[start, end)`.
    fn scan(&mut self, start: usize, end: usize, names: &Mutex<Names>) {
        let d = self.d;
        let mut ls = start;
        while ls < end {
            let (es, ee, nl) = split(d, ls);
            let id = self.endpoint(es, ee, names);
            let error = u32::from(d[ee + 1] >= b'5');
            let latency = parse_latency(d, ee + 5, nl);
            let key = (load8(d, ls), load8(d, ls + 5));
            if key != self.cur_key {
                self.cur_key = key;
                self.cur = self.hours.iter().position(|h| h.0 == key).unwrap_or_else(|| {
                    self.hours.push((key, (Vec::with_capacity(8192), Vec::new())));
                    self.hours.len() - 1
                });
            }
            let bucket = &mut self.hours[self.cur].1;
            let low = if latency < 0xFFFF {
                latency as u32
            } else {
                bucket.1.push(latency as u32);
                0xFFFF
            };
            bucket.0.push(error << 31 | id << 16 | low);
            ls = nl + 1;
        }
    }
}

/// `YYYY-MM-DDTHH`, the 13 bytes that make an hour, from its two overlapping loads.
fn hour_text(key: (u64, u64)) -> [u8; 13] {
    let mut text = [0u8; 13];
    text[..8].copy_from_slice(&key.0.to_le_bytes());
    text[8..].copy_from_slice(&key.1.to_le_bytes()[3..]);
    text
}

/// Morsel-driven scan: a shared atomic cursor over nominal byte offsets, each
/// snapped forward to the next line start, so adjacent morsels agree on the
/// split point without coordination. The last few hundred bytes are scanned
/// from a zero-padded copy, so the 64-byte loads never run past the data.
fn scan_parallel(data: &'static [u8], threads: usize, morsel: usize) -> (Vec<Worker>, Vec<&'static [u8]>) {
    let n = data.len();
    let snap = |nominal: usize, limit: usize| match nominal {
        0 => 0,
        x if x >= limit => limit,
        x => x + data[x..limit].iter().position(|&b| b == b'\n').unwrap() + 1,
    };
    let split_at = snap(n.saturating_sub(512), n);
    let (cursor, names) = (AtomicUsize::new(0), Mutex::new(Names::default()));
    let mut workers = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut worker = Worker::new(data);
                    loop {
                        let nominal = cursor.fetch_add(morsel, Ordering::Relaxed);
                        if nominal >= split_at {
                            return worker;
                        }
                        worker.scan(snap(nominal, split_at), snap(nominal + morsel, split_at), &names);
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("worker thread panicked")).collect::<Vec<_>>()
    });
    let mut padded = data[split_at..].to_vec();
    let tail = padded.len();
    padded.resize(tail + 128, 0);
    let mut last = Worker::new(Box::leak(padded.into_boxed_slice()));
    last.scan(0, tail, &names);
    workers.push(last);
    (workers, names.into_inner().unwrap().list)
}

/// Exact nearest-rank percentiles p50, p95 and p99 of `lat` (the spec's
/// integer rank `(p * n + 99) / 100`, 1-based), by selecting the largest
/// rank first and the others inside what stays below it.
fn percentiles(lat: &mut [u32]) -> [u32; 3] {
    let n = lat.len();
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

/// The output lines of one hour, given the packed lines of every worker for
/// that hour. `order` lists the endpoint numbers by endpoint bytes.
fn format_hour(text: &[u8; 13], parts: &[&Bucket], names: &[&[u8]], order: &[u32], scratch: &mut Scratch) -> Vec<u8> {
    let (count, start, lat) = (&mut scratch.count, &mut scratch.start, &mut scratch.lat);
    count.clear();
    count.resize(names.len(), 0);
    for &line in parts.iter().flat_map(|b| &b.0) {
        count[(line >> 16 & 0x7FFF) as usize] += 1 | u64::from(line >> 31) << 32;
    }
    let mut total = 0;
    for &id in order {
        start[id as usize] = total;
        total += count[id as usize] as u32 as usize;
    }
    lat.resize(total, 0);
    for (lines, big) in parts.iter().copied() {
        let mut big = big.iter();
        for &line in lines {
            let at = &mut start[(line >> 16 & 0x7FFF) as usize];
            lat[*at] = if line & 0xFFFF == 0xFFFF { *big.next().unwrap() } else { line & 0xFFFF };
            *at += 1;
        }
    }
    let mut out = Vec::with_capacity(total * 3 + 4096);
    let mut buf = [0u8; 20];
    for &id in order {
        let n = count[id as usize] as u32 as usize;
        if n == 0 {
            continue;
        }
        let end = start[id as usize];
        let [p50, p95, p99] = percentiles(&mut lat[end - n..end]);
        let errors = count[id as usize] >> 32;
        out.extend_from_slice(text);
        out.extend_from_slice(b":00:00Z ");
        out.extend_from_slice(names[id as usize]);
        // floor(errors * 1_000_000 / count): integer division already floors.
        for v in [n as u64, p50.into(), p95.into(), p99.into(), errors, errors * 1_000_000 / n as u64] {
            out.push(b' ');
            out.extend_from_slice(write_u64(&mut buf, v));
        }
        out.push(b'\n');
    }
    out
}

/// Per-thread buffers reused from hour to hour.
#[derive(Default)]
struct Scratch {
    count: Vec<u64>,
    start: Vec<usize>,
    lat: Vec<u32>,
}

/// Formats every hour in parallel and writes the blocks to `sink` in hour
/// order, each as soon as it and all earlier hours are done.
fn write_hours(workers: &[Worker], names: &[&[u8]], threads: usize, sink: &mut impl Write) -> std::io::Result<()> {
    let mut by_hour: BTreeMap<[u8; 13], Vec<&Bucket>> = BTreeMap::new();
    for (key, lines) in workers.iter().flat_map(|w| &w.hours) {
        by_hour.entry(hour_text(*key)).or_default().push(lines);
    }
    let hours: Vec<_> = by_hour.into_iter().collect();
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_unstable_by_key(|&id| names[id as usize]);

    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel::<(usize, Vec<u8>)>();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(hours.len()) {
            let (sender, next, hours, order) = (sender.clone(), &next, &hours, &order);
            scope.spawn(move || {
                let mut scratch = Scratch { start: vec![0; names.len()], ..Scratch::default() };
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((text, parts)) = hours.get(index) else { return };
                    sender.send((index, format_hour(text, parts, names, order, &mut scratch))).expect("writer stopped");
                }
            });
        }
        drop(sender);
        let mut waiting: Vec<Option<Vec<u8>>> = vec![None; hours.len()];
        let mut written = 0;
        for (index, block) in receiver {
            waiting[index] = Some(block);
            while let Some(block) = waiting.get_mut(written).and_then(Option::take) {
                sink.write_all(&block)?;
                written += 1;
            }
        }
        Ok(())
    })
}

/// Set `TRACK_A_TIMING=1` to print a per-phase breakdown to stderr.
fn run(input_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_A_TIMING").is_ok();
    let mut t = std::time::Instant::now();
    let mut lap = |label: &str| {
        if verbose {
            eprintln!("track-a: {label} in {:.1}ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        t = std::time::Instant::now();
    };
    let data = map_file(input_path)?;
    lap("map input");
    let (workers, names) = scan_parallel(data, threads.max(1), 1 << 20);
    lap("scan (parallel)");
    let mut file = std::fs::File::create(output_path)?;
    write_hours(&workers, &names, threads.max(1), &mut file)?;
    // The process exits next; skip freeing the per-hour lists one by one.
    std::mem::forget(workers);
    lap("count, select, format and write");
    Ok(())
}

fn main() {
    let input_path = std::env::var("INPUT_PATH").unwrap_or_else(|_| "/data/requests.log".to_string());
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/summary.txt".to_string());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let start = std::time::Instant::now();
    match run(&input_path, &output_path, threads) {
        Ok(()) => {
            eprintln!("track-a: done in {:.1} ms ({threads} threads)", start.elapsed().as_secs_f64() * 1000.0);
        }
        Err(e) => {
            eprintln!("track-a: failed reading {input_path} or writing {output_path}: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/track_a.rs"]
mod tests;
