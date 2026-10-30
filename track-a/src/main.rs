//! Track A — Ingest. Aggregates per-(hour, endpoint) request counts, exact
//! nearest-rank p50/p95/p99 latency and 5xx counts from a space-delimited
//! log (`ts method endpoint status latency_us`), as in specs/a-ingest.md.
//!
//! The input is mapped and cut into 2 MiB morsels that workers take from an
//! atomic cursor. A worker parses a morsel into packed `u64` records and
//! counting-sorts them into one run of latencies per (hour, endpoint). Then
//! one worker per hour merges that hour's runs from the few morsels that
//! touch it, picks the percentiles by selection, and formats the lines, which
//! are written in hour order as they complete.
//!
//! Techniques and sources: research/compiler-flags.md (target-cpu),
//! research/track-a-ingest.md (SWAR, FxHash), research/parallelism.md
//! (morsel-driven dispatch).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

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

// --- SWAR byte scanning (research/track-a-ingest.md) --------------------

#[inline]
fn broadcast(b: u8) -> u64 {
    (b as u64) * 0x0101_0101_0101_0101
}

#[inline]
fn has_zero_byte(v: u64) -> bool {
    v.wrapping_sub(0x0101_0101_0101_0101) & !v & 0x8080_8080_8080_8080 != 0
}

/// Where the method, endpoint and latency of the line at `line_start` end,
/// as (endpoint start, endpoint end, latency end). One 64-byte window
/// starting at the method holds the next four delimiters (the spaces after
/// the method, the endpoint and the status, and the line feed) for almost
/// every line; a line too long for it, or too near the end of the data,
/// takes the byte-wise path.
#[inline]
fn split_line(data: &[u8], line_start: usize, end: usize) -> (usize, usize, usize) {
    use std::arch::x86_64::*;
    let q = line_start + 28;
    if q + 64 <= data.len() {
        // SAFETY: the 64 bytes at `q` are inside `data`; AVX2 is enabled for the build (target-cpu=x86-64-v3).
        let mut bits = unsafe {
            let (space, line_feed) = (_mm256_set1_epi8(b' ' as i8), _mm256_set1_epi8(b'\n' as i8));
            let window = |at: usize| {
                let v = _mm256_loadu_si256(data.as_ptr().add(at) as *const __m256i);
                _mm256_movemask_epi8(_mm256_or_si256(_mm256_cmpeq_epi8(v, space), _mm256_cmpeq_epi8(v, line_feed))) as u32 as u64
            };
            window(q) | window(q + 32) << 32
        };
        if bits.count_ones() >= 4 {
            let mut at = [0usize; 4];
            for slot in &mut at {
                *slot = q + bits.trailing_zeros() as usize;
                bits &= bits - 1;
            }
            return (at[0] + 1, at[1], at[3]);
        }
    }
    let endpoint_start = find_delim_swar(data, q, end) + 1;
    let endpoint_end = find_delim_swar(data, endpoint_start, end);
    (endpoint_start, endpoint_end, find_delim_swar(data, endpoint_end + 5, end))
}

/// Finds the next space *or* newline at or after `start`, bounded by
/// `end` — one combined SWAR scan instead of two separate ones. The
/// original `process_morsel` found the line's newline first (one pass
/// over the whole line), then searched for each of the 4 field-splitting
/// spaces within that already-bounded line (a second pass collectively
/// covering the same bytes again) — every byte in a line was touched
/// twice. Checking both patterns against the same loaded 8-byte chunk
/// costs one extra XOR+compare per chunk (the load, not the compare, is
/// the expensive part), so fusing them touches each byte once instead of
/// twice. Returns `end` if neither delimiter appears before `end` (the
/// last field of a line with no trailing newline, or malformed input —
/// same fallback the original's `.unwrap_or(line.len())` used).
#[inline]
fn find_delim_swar(data: &[u8], start: usize, end: usize) -> usize {
    let pattern_space = broadcast(b' ');
    let pattern_nl = broadcast(b'\n');
    let mut i = start.min(end);
    while i + 8 <= end {
        let chunk = u64::from_ne_bytes(data[i..i + 8].try_into().unwrap());
        if has_zero_byte(chunk ^ pattern_space) || has_zero_byte(chunk ^ pattern_nl) {
            for j in 0..8 {
                let b = data[i + j];
                if b == b' ' || b == b'\n' {
                    return i + j;
                }
            }
        }
        i += 8;
    }
    while i < end {
        let b = data[i];
        if b == b' ' || b == b'\n' {
            return i;
        }
        i += 1;
    }
    end
}

// --- FxHash-style hasher (research/track-a-ingest.md) -------------------

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default)]
struct FxHasher {
    hash: u64,
}
impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            let word = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
            self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
            bytes = &bytes[8..];
        }
        if !bytes.is_empty() {
            let mut buf = [0u8; 8];
            buf[..bytes.len()].copy_from_slice(bytes);
            let word = u64::from_ne_bytes(buf);
            self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
        }
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}
type FxBuildHasher = BuildHasherDefault<FxHasher>;

#[inline]
fn parse_u32(bytes: &[u8]) -> u32 {
    let mut v: u32 = 0;
    for &b in bytes {
        v = v * 10 + (b - b'0') as u32;
    }
    v
}

/// Packs "YYYY-MM-DDTHH" into one ascending u64 — a stable hour-bucket
/// key without any calendar arithmetic. See research/track-a-ingest.md.
#[inline]
fn hour_bucket_key(ts: &[u8]) -> u64 {
    let year = parse_u32(&ts[0..4]) as u64;
    let month = parse_u32(&ts[5..7]) as u64;
    let day = parse_u32(&ts[8..10]) as u64;
    let hour = parse_u32(&ts[11..13]) as u64;
    ((year * 100 + month) * 100 + day) * 100 + hour
}

fn unpack_hour_key(key: u64) -> (u64, u64, u64, u64) {
    let hour = key % 100;
    let rest = key / 100;
    let day = rest % 100;
    let rest = rest / 100;
    let month = rest % 100;
    let year = rest / 100;
    (year, month, day, hour)
}

/// One group of one morsel: the lines with this hour (`slot` indexes the
/// morsel's `hours`) and this endpoint (`ep` indexes its `names`), whose
/// latencies are `latencies[start..start + len]` of the morsel.
struct Group {
    slot: u32,
    ep: u32,
    errors: u32,
    start: u32,
    len: u32,
}

/// What one morsel's lines add up to, kept small and flat. Groups are
/// sorted by (slot, ep). The same (hour, endpoint) usually shows up in only
/// a couple of morsels, because the log is close to time order, so the
/// final merge has little to combine.
struct Morsel<'a> {
    hours: Vec<u64>,
    names: Vec<&'a [u8]>,
    groups: Vec<Group>,
    latencies: Vec<u32>,
}

/// A morsel's line is packed into one `u64`: hour slot (11 bits), endpoint
/// number (20 bits), error flag (1 bit) and latency (32 bits). Sorting the
/// packed values groups the lines with no hash map or pointer in the way.
const GROUP_SHIFT: u32 = 33;
/// Morsels are small enough that their slot and endpoint numbers fit.
const MAX_MORSEL: usize = 1 << 24;

/// Parses one morsel [start, end) of already line-aligned bytes. `ids`
/// maps an endpoint to its number within the morsel and is reused (cleared)
/// from one morsel to the next.
fn process_morsel<'a>(
    data: &'a [u8],
    start: usize,
    end: usize,
    ids: &mut HashMap<&'a [u8], u32, FxBuildHasher>,
) -> Morsel<'a> {
    ids.clear();
    let mut morsel = Morsel { hours: Vec::new(), names: Vec::new(), groups: Vec::new(), latencies: Vec::new() };
    let mut lines: Vec<u64> = Vec::with_capacity((end - start) / 48 + 1);
    let mut line_start = start;

    // Every line is `ts method endpoint status latency` with a 27-byte
    // timestamp and a 3-digit status (spec section 2), so only the method,
    // endpoint and latency need a delimiter scan.
    while line_start < end {
        let ts = &data[line_start..line_start + 27];
        let (endpoint_start, endpoint_end, latency_end) = split_line(data, line_start, end);
        let status = &data[endpoint_end + 1..endpoint_end + 4];

        let hour = hour_bucket_key(ts);
        let slot = morsel.hours.iter().position(|&h| h == hour).unwrap_or_else(|| {
            morsel.hours.push(hour);
            assert!(morsel.hours.len() <= 1 << 11, "more hours in one morsel than the packed line format holds");
            morsel.hours.len() - 1
        });
        let endpoint = &data[endpoint_start..endpoint_end];
        let next = morsel.names.len() as u32;
        let ep = *ids.entry(endpoint).or_insert(next);
        if ep == next {
            morsel.names.push(endpoint);
        }
        let error = u64::from(status[0] >= b'5');
        let latency = parse_u32(&data[endpoint_end + 5..latency_end]);
        lines.push((slot as u64) << 53 | (ep as u64) << GROUP_SHIFT | error << 32 | latency as u64);
        line_start = latency_end + 1;
    }

    // Counting sort of the lines by (slot, endpoint): one pass to count, one
    // to place each latency in its group's run of `latencies`.
    let endpoints = morsel.names.len();
    let group_of = |l: u64| (l >> 53) as usize * endpoints + (l >> GROUP_SHIFT & 0xF_FFFF) as usize;
    let mut ends = vec![0u32; morsel.hours.len() * endpoints + 1];
    for &l in &lines {
        ends[group_of(l) + 1] += 1;
    }
    for g in 1..ends.len() {
        ends[g] += ends[g - 1];
    }
    let mut errors = vec![0u32; ends.len() - 1];
    let mut next = ends.clone();
    morsel.latencies = vec![0; lines.len()];
    for &l in &lines {
        let g = group_of(l);
        morsel.latencies[next[g] as usize] = l as u32;
        next[g] += 1;
        errors[g] += (l >> 32 & 1) as u32;
    }
    for g in 0..errors.len() {
        if ends[g + 1] > ends[g] {
            let (slot, ep) = (g / endpoints, g % endpoints);
            morsel.groups.push(Group {
                slot: slot as u32,
                ep: ep as u32,
                errors: errors[g],
                start: ends[g],
                len: ends[g + 1] - ends[g],
            });
        }
    }
    morsel
}

/// Morsel-driven dispatch: a shared atomic cursor over nominal byte
/// offsets, each independently snapped forward to the next line boundary
/// so adjacent morsels agree on the split point without coordination.
/// Returns every morsel's result; they are combined when the output is
/// written, not copied together here.
fn process_parallel<'a>(data: &'a [u8], num_threads: usize, morsel_size: usize) -> Vec<Morsel<'a>> {
    assert!(morsel_size <= MAX_MORSEL, "morsel too large for the packed line format");
    let cursor = AtomicUsize::new(0);
    let n = data.len();

    let snap_fwd = |nominal: usize| -> usize {
        if nominal == 0 {
            0
        } else if nominal >= n {
            n
        } else {
            data[nominal..].iter().position(|&b| b == b'\n').map_or(n, |i| nominal + i + 1)
        }
    };

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut ids = HashMap::default();
                    let mut mine = Vec::new();
                    loop {
                        let nominal_start = cursor.fetch_add(morsel_size, Ordering::Relaxed);
                        if nominal_start >= n {
                            break;
                        }
                        let start = snap_fwd(nominal_start);
                        let end = snap_fwd(nominal_start + morsel_size);
                        if start < end {
                            mine.push(process_morsel(data, start, end, &mut ids));
                        }
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("worker thread panicked")).collect()
    })
}

/// Exact nearest-rank percentile via `select_nth_unstable` — O(n) per
/// call (introselect), avoiding a full O(n log n) sort of the group's
/// latencies. `p` is a whole percentage (50, 95, 99), and the rank is
/// computed with the spec's own integer formula — `ceil(p*n/100)`,
/// i.e. `(p*n + 99) / 100` in integer arithmetic — rather than any
/// floating-point approximation of it, so there's no float-vs-integer
/// rounding mismatch risk against an exact-hash correctness gate.
fn percentile(latencies: &mut [u32], p: u64) -> u32 {
    let n = latencies.len();
    if n == 0 {
        return 0;
    }
    let rank = (p * n as u64).div_ceil(100) as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    *latencies.select_nth_unstable(idx).1
}

/// Set `TRACK_A_TIMING=1` to print a per-phase breakdown to stderr.
fn run(input_path: &str, output_path: &str, num_threads: usize) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_A_TIMING").is_ok();
    macro_rules! phase {
        ($label:expr, $body:expr) => {{
            let t = std::time::Instant::now();
            let v = $body;
            if verbose {
                eprintln!("track-a: {} in {:.1}ms", $label, t.elapsed().as_secs_f64() * 1000.0);
            }
            v
        }};
    }

    let data = phase!("map input", map_file(input_path)?);

    let morsel_size = 2 * 1024 * 1024; // 2 MiB, per research/parallelism.md
    let locals = phase!("parse+group (parallel)", process_parallel(data, num_threads.max(1), morsel_size));
    phase!("merge, format and write", {
        let mut file = std::fs::File::create(output_path)?;
        format_groups(&locals, num_threads.max(1), &mut file)
    })
}

#[inline]
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

#[inline]
fn write_padded(out: &mut Vec<u8>, v: u64, width: usize, buf: &mut [u8; 20]) {
    let digits = write_u64(buf, v);
    out.resize(out.len() + width.saturating_sub(digits.len()), b'0');
    out.extend_from_slice(digits);
}

/// Turns every (hour, endpoint) group into its output line, in parallel,
/// one hour at a time, and writes the lines to `sink` in hour order. A
/// worker takes an hour, collects that hour's groups from the few morsels
/// that touch it, orders them by endpoint bytes (which, after the hour, is
/// exactly the output order), and merges the runs of equal endpoints, which
/// are the same group seen in different morsels. No map is searched. The
/// finished hours are written as soon as every earlier hour is, so the
/// output is never held in memory all at once.
fn format_groups(morsels: &[Morsel], num_threads: usize, sink: &mut impl Write) -> std::io::Result<()> {
    let mut touches: Vec<(u64, &Morsel, u32)> = morsels
        .iter()
        .flat_map(|m| m.hours.iter().enumerate().map(move |(slot, &hour)| (hour, m, slot as u32)))
        .collect();
    touches.sort_unstable_by_key(|t| t.0);
    let hours: Vec<&[(u64, &Morsel, u32)]> = touches.chunk_by(|a, b| a.0 == b.0).collect();

    let next = AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel::<(usize, Vec<u8>)>();
    std::thread::scope(|scope| {
        for _ in 0..num_threads.min(hours.len()) {
            let sender = sender.clone();
            let (next, hours) = (&next, &hours);
            scope.spawn(move || {
                let mut latencies: Vec<u32> = Vec::new();
                let mut buf = [0u8; 20];
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(touching) = hours.get(index) else { break };
                    let mut items: Vec<(&[u8], &[u32], u32)> = Vec::new();
                    for &(_, m, slot) in *touching {
                        let first = m.groups.partition_point(|g| g.slot < slot);
                        for g in m.groups[first..].iter().take_while(|g| g.slot == slot) {
                            let from = g.start as usize;
                            items.push((m.names[g.ep as usize], &m.latencies[from..from + g.len as usize], g.errors));
                        }
                    }
                    items.sort_unstable_by(|a, b| a.0.cmp(b.0));

                    let (year, month, day, hour) = unpack_hour_key(touching[0].0);
                    let mut out = Vec::new();
                    for group in items.chunk_by(|a, b| a.0 == b.0) {
                        let (mut count, mut errors) = (0u64, 0u64);
                        latencies.clear();
                        for &(_, lats, errs) in group {
                            count += lats.len() as u64;
                            errors += errs as u64;
                            latencies.extend_from_slice(lats);
                        }
                        let p50 = percentile(&mut latencies, 50);
                        let p95 = percentile(&mut latencies, 95);
                        let p99 = percentile(&mut latencies, 99);
                        // floor(errors * 1_000_000 / count), per spec; integer
                        // division of non-negatives already floors.
                        let error_ppm = errors * 1_000_000 / count;

                        write_padded(&mut out, year, 4, &mut buf);
                        out.push(b'-');
                        write_padded(&mut out, month, 2, &mut buf);
                        out.push(b'-');
                        write_padded(&mut out, day, 2, &mut buf);
                        out.push(b'T');
                        write_padded(&mut out, hour, 2, &mut buf);
                        out.extend_from_slice(b":00:00Z ");
                        out.extend_from_slice(group[0].0);
                        for v in [count, p50 as u64, p95 as u64, p99 as u64, errors, error_ppm] {
                            out.push(b' ');
                            out.extend_from_slice(write_u64(&mut buf, v));
                        }
                        out.push(b'\n');
                    }
                    sender.send((index, out)).expect("writer stopped");
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

fn main() {
    let input_path =
        std::env::var("INPUT_PATH").unwrap_or_else(|_| "/data/requests.log".to_string());
    let output_path =
        std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/summary.txt".to_string());
    let num_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let start = std::time::Instant::now();
    match run(&input_path, &output_path, num_threads) {
        Ok(()) => {
            eprintln!(
                "track-a: done in {:.1} ms ({} threads)",
                start.elapsed().as_secs_f64() * 1000.0,
                num_threads
            );
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
