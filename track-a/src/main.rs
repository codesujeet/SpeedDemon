//! Track A — Ingest. Aggregates per-(hour, endpoint) request counts, exact
//! nearest-rank p50/p95/p99 latency and 5xx counts from a space-delimited
//! log (`ts method endpoint status latency_us`), as in specs/a-ingest.md.
//!
//! Phase 1: workers take 1 MiB chunks of the mapped input from an atomic cursor and append every line to a
//! private vector for its hour, as one `u32` (endpoint number, error flag, 20-bit latency; the 1% of lines with
//! longer latencies go to a second vector as a whole `u64`). Phase 2: endpoints are ranked by name once; each
//! hour is then gathered from all workers as `rank << 33 | latency << 1 | error`, sorted, and every run of one
//! rank is a group. Hour blocks are written in hour order as soon as all earlier hours are done.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

/// A stored line is `endpoint << 21 | error << 20 | latency`.
const LAT_MASK: u32 = (1 << 20) - 1;
/// The spec gives every seed a catalogue of exactly 1,842 endpoints, so 11 bits of endpoint number are enough.
const MAX_ENDPOINTS: usize = 1 << 11;
/// The spec fixes the span: 72 hours from 2026-10-30T18 to 2026-11-02T17.
const HOURS: usize = 72;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];
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

/// The lines of one hour that one worker has seen: `small` as described at `LAT_MASK`, and the lines with a
/// longer latency as `endpoint << 33 | latency << 1 | error`.
#[derive(Default)]
struct Lines {
    small: Vec<u32>,
    big: Vec<u64>,
}

struct Worker {
    d: &'static [u8],
    /// The endpoint numbers handed out so far, shared by all workers.
    names: &'static Mutex<HashMap<&'static [u8], u32>>,
    /// Endpoint table: `offset of the name in store | length << 32 | number << 40`; 0 is a free slot.
    table: Vec<u64>,
    /// A copy of each name this worker has seen, one after the other (so that the names the lines are compared
    /// with sit together), with 32 zero bytes before them and 32 bytes of capacity after (the compares mask
    /// whatever lies outside a name).
    store: Vec<u8>,
    hours: Vec<Lines>,
}

impl Worker {
    /// The number of the endpoint `d[es..ee]`, taken from the shared names the first time this worker sees it.
    #[inline(always)]
    unsafe fn endpoint(&mut self, es: usize, ee: usize) -> u32 {
        let (base, len) = (self.d.as_ptr(), ee - es);
        let bits = 8 * len.min(8) as u32;
        let a = load8(base.add(es)) & (u64::MAX >> (64 - bits));
        let b = load8(base.add(ee - 8)) & (u64::MAX << (64 - bits));
        let mut i = ((a ^ b.rotate_left(29) ^ len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 51) as usize;
        loop {
            let slot = *self.table.get_unchecked(i);
            if slot == 0 {
                return self.insert(es, ee, i);
            }
            if (slot >> 32) as u8 as usize == len && same(base.add(es), self.store.as_ptr().add(slot as u32 as usize), len) {
                return (slot >> 40) as u32;
            }
            i = (i + 1) % SLOTS;
        }
    }

    #[cold]
    fn insert(&mut self, es: usize, ee: usize, i: usize) -> u32 {
        let mut names = self.names.lock().unwrap();
        let next = names.len() as u32;
        let id = *names.entry(&self.d[es..ee]).or_insert(next);
        assert!((id as usize) < MAX_ENDPOINTS, "more endpoints than the spec's catalogue of 1,842");
        let off = self.store.len();
        self.store.extend_from_slice(&self.d[es..ee]);
        self.store.reserve(32);
        self.table[i] = off as u64 | ((ee - es) as u64) << 32 | u64::from(id) << 40;
        id
    }

    /// Parses the lines that start in `[start, end)` (`end` is a line start). After the 27-byte timestamp every
    /// line has four delimiters (the spaces after method, endpoint and status, and the line feed), which are
    /// found with one 64-byte compare for most lines; the status is three digits, so the latency is known too.
    fn scan(&mut self, start: usize, end: usize) {
        let base = self.d.as_ptr();
        let mut s = start;
        while s < end {
            // SAFETY: see `load8`.
            unsafe {
                let digit = |i: usize| usize::from(*base.add(s + i) - b'0');
                let hour = (digit(6) * 31 + digit(8) * 10 + digit(9) - 30) * 24 + digit(11) * 10 + digit(12) - 18;
                let (mut p, mut m, mut at) = (s + 28, delimiters(base.add(s + 28)), [0; 4]);
                for a in &mut at {
                    while m == 0 {
                        (p, m) = (p + 64, delimiters(base.add(p + 64)));
                    }
                    *a = p + m.trailing_zeros() as usize;
                    m &= m - 1;
                }
                let ee = at[1];
                let id = self.endpoint(at[0] + 1, ee);
                let line = id << 21 | u32::from(*base.add(ee + 1) >= b'5') << 20;
                let latency = parse_latency(base, ee + 5, at[3]);
                let lines = &mut self.hours[hour];
                if latency < LAT_MASK {
                    lines.small.push(line | latency);
                } else {
                    lines.big.push(u64::from(line >> 21) << 33 | u64::from(latency) << 1 | u64::from(line >> 20 & 1));
                }
                s = at[3] + 1;
            }
        }
    }
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

/// Exact nearest-rank p50, p95 and p99 of `lat`: the spec's integer rank `(p * n + 99) / 100`, 1-based. Up to 32
/// values are sorted; for more, each rank is selected inside what lies above the one before.
fn percentiles(lat: &mut [u32]) -> [u32; 3] {
    let n = lat.len();
    let at = [50, 95, 99].map(|p| (p * n).div_ceil(100) - 1);
    if n <= 32 {
        lat.sort_unstable();
        return at.map(|i| lat[i]);
    }
    // From 33 values on the three ranks are different.
    let (_, p50, above) = lat.select_nth_unstable(at[0]);
    let (_, p95, above) = above.select_nth_unstable(at[1] - at[0] - 1);
    [*p50, *p95, *above.select_nth_unstable(at[2] - at[1] - 1).1]
}

/// Calls `f(endpoint number, error, latency)` for every line in `part`.
fn each(part: &Lines, mut f: impl FnMut(usize, u64, u32)) {
    part.small.iter().for_each(|&w| f((w >> 21) as usize, u64::from(w >> 20 & 1), w & LAT_MASK));
    part.big.iter().for_each(|&k| f((k >> 33) as usize, k & 1, (k >> 1) as u32));
}

/// The output lines of one hour. `parts` holds this hour's lines from every worker, `rank` the position of each
/// endpoint number in byte order and `names` the endpoints in that order.
fn format_hour(hour: usize, parts: &[Lines], rank: &[u32], names: &[&[u8]]) -> Vec<u8> {
    // Per endpoint, in byte order: the number of lines in the low 32 bits and the number of errors above.
    let mut count = vec![0u64; names.len()];
    parts.iter().for_each(|part| each(part, |id, error, _| count[rank[id] as usize] += 1 | error << 32));
    let mut end = vec![0; names.len()];
    let mut total = 0;
    for (e, c) in end.iter_mut().zip(&count) {
        total += *c as u32 as usize;
        *e = total;
    }
    // Places the latencies of each endpoint in one run (filling it from the back).
    let mut lat = vec![0; total];
    parts.iter().for_each(|part| {
        each(part, |id, _, latency| {
            end[rank[id] as usize] -= 1;
            lat[end[rank[id] as usize]] = latency;
        })
    });
    let mut out = Vec::with_capacity(total * 3 + 4096);
    let prefix = format!("{}T{:02}:00:00Z ", DAYS[(hour + 18) / 24], (hour + 18) % 24);
    let mut buf = [0u8; 20];
    for (r, &c) in count.iter().enumerate().filter(|e| *e.1 != 0) {
        let (n, errors) = (c as u32 as usize, c >> 32);
        let [p50, p95, p99] = percentiles(&mut lat[end[r]..end[r] + n]);
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(names[r]);
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
fn process(d: &'static [u8], threads: usize, chunk: usize, sink: &mut (impl Write + Send)) -> std::io::Result<()> {
    let n = d.len();
    // The first line start at or after `x` (a line start exactly at `x` goes to the chunk before): adjacent chunks agree.
    let snap = |x: usize| if x >= n { n } else if x == 0 { 0 } else { x + d[x..].iter().position(|&b| b == b'\n').unwrap() + 1 };
    let (cursor, next) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let names: &'static Mutex<HashMap<&[u8], u32>> = Box::leak(Box::default());
    let workers: Vec<Vec<Mutex<Lines>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (cursor, snap) = (&cursor, &snap);
                scope.spawn(move || {
                    // Capacity 128 KiB: such blocks come from `mmap`, so freeing one gives its pages back at once.
                    let hours = (0..HOURS).map(|_| Lines { small: Vec::with_capacity(1 << 15), big: Vec::new() }).collect();
                    let mut worker = Worker { d, names, table: vec![0; SLOTS], store: vec![0; 32], hours };
                    loop {
                        let from = cursor.fetch_add(chunk, Relaxed);
                        if from >= n {
                            break;
                        }
                        worker.scan(snap(from), snap(from + chunk));
                    }
                    worker.hours.into_iter().map(Mutex::new).collect()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut sorted: Vec<(&[u8], u32)> = std::mem::take(&mut *names.lock().unwrap()).into_iter().collect();
    sorted.sort_unstable();
    let mut rank = vec![0; sorted.len()];
    for (r, &(_, id)) in sorted.iter().enumerate() {
        rank[id as usize] = r as u32;
    }
    let sorted: Vec<&[u8]> = sorted.into_iter().map(|e| e.0).collect();
    let out = Mutex::new((0, vec![None; HOURS], sink));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                
                loop {
                    let hour = next.fetch_add(1, Relaxed);
                    if hour >= HOURS {
                        return;
                    }
                    let parts: Vec<Lines> = workers.iter().map(|w| std::mem::take(&mut *w[hour].lock().unwrap())).collect();
                    let block = format_hour(hour, &parts, &rank, &sorted);
                    drop(parts);
                    // Writes this block and the ones after it that are already done, if it is the next in line.
                    let (written, waiting, sink) = &mut *out.lock().unwrap();
                    waiting[hour] = Some(block);
                    while let Some(block) = waiting.get_mut(*written).and_then(Option::take) {
                        sink.write_all(&block).expect("cannot write the output");
                        *written += 1;
                    }
                }
            });
        }
    });
    Ok(())
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
