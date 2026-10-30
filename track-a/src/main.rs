//! Track A — Ingest. Aggregates per-(endpoint, hour) request counts,
//! exact p50/p95/p99 latency, and error rate from a space-delimited log
//! (`ts method endpoint status latency_us`).
//!
//! Output format/filenames are a best-effort guess made before the real
//! spec drops at freeze (Oct 28) — see README.md for exactly what to
//! re-check once it does. The processing pipeline below (SWAR scanning,
//! morsel-driven parallel dispatch, FxHash grouping, exact selection-based
//! percentiles) is the real, reusable, already-benchmarked work; only the
//! I/O surface (`INPUT_PATH`/`OUTPUT_PATH` defaults and the output
//! columns) should need touching once the real contract is known.
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

#[inline]
fn parse_u16(bytes: &[u8]) -> u16 {
    let mut v: u16 = 0;
    for &b in bytes {
        v = v * 10 + (b - b'0') as u16;
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

#[derive(Default)]
struct GroupData {
    count: u64,
    errors: u64,
    latencies: Vec<u32>,
}

/// (hour, endpoint), in that order: the real spec sorts output by hour
/// ascending, then endpoint ascending as unsigned bytes — `Ord` on this
/// tuple sorts exactly that way for free, since `hour_bucket_key` is
/// itself ascending-with-time and a byte slice's default `Ord` is
/// unsigned-byte lexicographic. The endpoint borrows straight from the
/// mapped input, so a line costs no allocation.
type GroupKey<'a> = (u64, &'a [u8]);
type GroupMap<'a> = HashMap<GroupKey<'a>, GroupData, FxBuildHasher>;

/// Processes one morsel [start, end) of already line-aligned bytes
/// directly into the caller's thread-local map — no per-morsel map is
/// allocated, so a popular (endpoint,hour) group's latencies are pushed
/// once per line, not copied again via a morsel-to-thread-local merge on
/// top of that. See research/parallelism.md.
fn process_morsel<'a>(data: &'a [u8], start: usize, end: usize, map: &mut GroupMap<'a>) {
    let mut line_start = start;

    while line_start < end {
        let mut fields: [&[u8]; 5] = [&[]; 5];
        let mut field_start = line_start;
        let mut line_end = end;
        for (idx, field_slot) in fields.iter_mut().enumerate() {
            let delim = find_delim_swar(data, field_start, end);
            *field_slot = &data[field_start..delim];
            // A newline found before the 5th field (a blank or malformed
            // short line) must end the line right there, same as the
            // original: its per-field search was bounded to a slice that
            // already excluded the newline, so it could never consume
            // one mid-scan the way a combined delimiter search could
            // without this check.
            let hit_newline = delim < end && data[delim] == b'\n';
            field_start = (delim + 1).min(end);
            if idx == 4 || hit_newline {
                line_end = delim;
                break;
            }
        }

        if !fields[0].is_empty() {
            let ts = fields[0];
            let endpoint = fields[2];
            let status = parse_u16(fields[3]);
            let latency_us = parse_u32(fields[4]);
            let hour = hour_bucket_key(ts);

            let entry = map.entry((hour, endpoint)).or_default();
            entry.count += 1;
            if status >= 500 {
                entry.errors += 1;
            }
            entry.latencies.push(latency_us);
        }
        line_start = line_end + 1;
    }
}

/// Morsel-driven dispatch: a shared atomic cursor over nominal byte
/// offsets, each independently snapped forward to the next line boundary
/// so adjacent morsels agree on the split point without coordination.
/// Returns one map per worker; they are combined key by key when the
/// results are written, not copied together here.
fn process_parallel<'a>(data: &'a [u8], num_threads: usize, morsel_size: usize) -> Vec<GroupMap<'a>> {
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
                    let mut local = GroupMap::default();
                    loop {
                        let nominal_start = cursor.fetch_add(morsel_size, Ordering::Relaxed);
                        if nominal_start >= n {
                            break;
                        }
                        let start = snap_fwd(nominal_start);
                        let end = snap_fwd(nominal_start + morsel_size);
                        if start >= end {
                            continue;
                        }
                        process_morsel(data, start, end, &mut local);
                    }
                    local
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("worker thread panicked")).collect()
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
    let blocks = phase!("merge+percentiles+format (parallel)", format_groups(&locals, num_threads.max(1)));
    phase!("write output", {
        let mut file = std::fs::File::create(output_path)?;
        blocks.iter().try_for_each(|block| file.write_all(block))
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
/// one hour at a time. The workers' groups are collected as (key, data)
/// pairs and ordered by hour alone, a cheap integer sort; a worker then
/// takes a whole hour, orders its few thousand endpoints, and merges the
/// runs of equal keys, which are the same group seen by different workers.
/// No map is searched. The per-hour blocks come back in hour order, ready
/// to be written one after the other.
fn format_groups(locals: &[GroupMap], num_threads: usize) -> Vec<Vec<u8>> {
    let mut entries: Vec<(GroupKey, &GroupData)> =
        locals.iter().flat_map(|m| m.iter().map(|(k, g)| (*k, g))).collect();
    entries.sort_unstable_by_key(|e| e.0 .0);
    let hours: Vec<&[(GroupKey, &GroupData)]> = entries.chunk_by(|a, b| a.0 .0 == b.0 .0).collect();

    let next = AtomicUsize::new(0);
    let mut blocks: Vec<(usize, Vec<u8>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..num_threads.min(hours.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    let mut latencies: Vec<u32> = Vec::new();
                    let mut buf = [0u8; 20];
                    loop {
                        let hour = next.fetch_add(1, Ordering::Relaxed);
                        let Some(items) = hours.get(hour) else { break };
                        let mut items = items.to_vec();
                        items.sort_unstable_by(|a, b| a.0 .1.cmp(b.0 .1));
                        let mut out = Vec::new();
                        for group in items.chunk_by(|a, b| a.0 == b.0) {
                            let key = group[0].0;
                            let (mut count, mut errors) = (0u64, 0u64);
                            latencies.clear();
                            for (_, data) in group {
                                count += data.count;
                                errors += data.errors;
                                latencies.extend_from_slice(&data.latencies);
                            }
                            let p50 = percentile(&mut latencies, 50);
                            let p95 = percentile(&mut latencies, 95);
                            let p99 = percentile(&mut latencies, 99);
                            // floor(errors * 1_000_000 / count), per spec; integer
                            // division of non-negatives already floors.
                            let error_ppm = errors * 1_000_000 / count;

                            let (year, month, day, hour) = unpack_hour_key(key.0);
                            write_padded(&mut out, year, 4, &mut buf);
                            out.push(b'-');
                            write_padded(&mut out, month, 2, &mut buf);
                            out.push(b'-');
                            write_padded(&mut out, day, 2, &mut buf);
                            out.push(b'T');
                            write_padded(&mut out, hour, 2, &mut buf);
                            out.extend_from_slice(b":00:00Z ");
                            out.extend_from_slice(key.1);
                            for v in [count, p50 as u64, p95 as u64, p99 as u64, errors, error_ppm] {
                                out.push(b' ');
                                out.extend_from_slice(write_u64(&mut buf, v));
                            }
                            out.push(b'\n');
                        }
                        mine.push((hour, out));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("format worker panicked")).collect()
    });
    blocks.sort_unstable_by_key(|b| b.0);
    blocks.into_iter().map(|b| b.1).collect()
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
mod tests {
    use super::*;

    /// Folds the per-worker maps into one, for easy inspection.
    fn merged(locals: Vec<GroupMap<'_>>) -> GroupMap<'_> {
        let mut all = GroupMap::default();
        for (k, v) in locals.into_iter().flatten() {
            let g: &mut GroupData = all.entry(k).or_default();
            g.count += v.count;
            g.errors += v.errors;
            g.latencies.extend(v.latencies);
        }
        all
    }

    #[test]
    fn parses_sample_lines_and_groups_by_endpoint_and_hour() {
        let data = b"2026-10-30T18:07:18.569411Z GET /api/v2/products 200 8177\n\
2026-10-30T18:07:21.136274Z GET /api/v2/products 503 81573\n\
2026-10-30T19:07:27.963350Z PUT /v2/ab_tests 200 6381\n";
        let groups = merged(process_parallel(data, 1, 1024));
        // 1 group for (products, hour 18) covering both product lines,
        // 1 group for (ab_tests, hour 19) - 2 distinct (endpoint,hour) keys.
        assert_eq!(groups.len(), 2);
        let key18 = (hour_bucket_key(b"2026-10-30T18"), b"/api/v2/products" as &[u8]);
        let g = groups.get(&key18).unwrap();
        assert_eq!(g.count, 2);
        assert_eq!(g.errors, 1);
    }

    #[test]
    fn blank_lines_are_skipped_without_corrupting_the_next_line() {
        // A blank line in the middle (and a double-blank right after)
        // specifically exercises find_delim_swar's combined space/
        // newline scan: it must stop exactly at the blank line's own
        // newline instead of running past it into the next real line's
        // bytes, which a naive fusion of the two separate scans could do.
        let data = b"2026-10-30T18:07:18.569411Z GET /api/v2/products 200 8177\n\
\n\
\n\
2026-10-30T19:07:27.963350Z PUT /v2/ab_tests 200 6381\n";
        let groups = merged(process_parallel(data, 1, 1024));
        assert_eq!(groups.len(), 2);
        let key18 = (hour_bucket_key(b"2026-10-30T18"), b"/api/v2/products" as &[u8]);
        let key19 = (hour_bucket_key(b"2026-10-30T19"), b"/v2/ab_tests" as &[u8]);
        assert_eq!(groups.get(&key18).unwrap().count, 1);
        assert_eq!(groups.get(&key19).unwrap().count, 1);
    }

    #[test]
    fn percentile_matches_full_sort_reference() {
        let mut data: Vec<u32> = (1..=1000).rev().collect();
        let mut sorted = data.clone();
        sorted.sort_unstable();
        for &p in &[50u64, 95, 99] {
            let mut d = data.clone();
            let got = percentile(&mut d, p);
            let rank = (p * sorted.len() as u64).div_ceil(100) as usize;
            let expected = sorted[rank.saturating_sub(1).min(sorted.len() - 1)];
            assert_eq!(got, expected, "p={p}");
        }
        let _ = &mut data; // silence unused-mut in case of future edits
    }

    #[test]
    fn percentile_rank_matches_spec_worked_cases() {
        // Spec section 3.4: "With n = 1 all three equal the single
        // value. With n = 101, the ranks are 51, 96 and 100."
        let one = vec![42u32];
        assert_eq!(percentile(&mut one.clone(), 50), 42);
        assert_eq!(percentile(&mut one.clone(), 95), 42);
        assert_eq!(percentile(&mut one.clone(), 99), 42);

        let hundred_one: Vec<u32> = (1..=101).collect();
        assert_eq!(percentile(&mut hundred_one.clone(), 50), 51);
        assert_eq!(percentile(&mut hundred_one.clone(), 95), 96);
        assert_eq!(percentile(&mut hundred_one.clone(), 99), 100);
    }

    #[test]
    fn single_threaded_and_multi_threaded_dispatch_agree() {
        let mut data = Vec::new();
        for i in 0..5000u32 {
            let hour = i % 5;
            let ep = i % 7;
            data.extend_from_slice(
                format!(
                    "2026-10-30T{:02}:00:00.000000Z GET /ep/{} {} {}\n",
                    hour,
                    ep,
                    if i % 11 == 0 { 503 } else { 200 },
                    1000 + i
                )
                .as_bytes(),
            );
        }
        let single = merged(process_parallel(&data, 1, 4096));
        let multi = merged(process_parallel(&data, 6, 4096));
        assert_eq!(single.len(), multi.len());
        for (k, v) in &single {
            let mv = multi.get(k).expect("key missing in multi-threaded result");
            assert_eq!(v.count, mv.count, "count mismatch for {k:?}");
            assert_eq!(v.errors, mv.errors, "errors mismatch for {k:?}");
            let mut a = v.latencies.clone();
            let mut b = mv.latencies.clone();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "latencies mismatch for {k:?}");
        }
    }

    #[test]
    fn output_matches_the_spec_worked_example_exactly() {
        // specs/a-ingest.md section 3, the worked example - literal input
        // and expected output bytes, both copied verbatim.
        let data = b"2026-10-30T19:00:00.000000Z GET /v2/orders/{id} 200 1500\n\
2026-10-30T18:59:59.999999Z GET /v2/orders/{id} 200 1841\n\
2026-10-30T18:00:00.000000Z POST /v2/checkout 503 204117\n\
2026-10-30T18:00:00.000000Z POST /v2/checkout 201 30000\n\
2026-10-30T18:00:00.115332Z GET /health 200 62\n\
2026-10-30T18:00:00.115332Z GET /health 200 62\n\
2026-10-30T18:42:17.000001Z HEAD /health 200 4294967295\n\
2026-10-30T18:30:00.000001Z OPTIONS / 200 0\n\
2026-10-30T18:12:00.500000Z POST /v2/checkout 500 0\n\
2026-10-30T18:12:00.600000Z POST /v2/checkout 499 1500\n\
2026-10-30T18:12:00.700000Z POST /v2/checkout 599 2500\n\
2026-10-30T18:20:00.000000Z GET /v2/orders/{id} 500 900\n\
2026-10-30T18:21:00.000000Z DELETE /v2/orders/{id} 404 77\n\
2026-10-30T19:05:00.000000Z GET /v2/orders_legacy 200 60\n\
2026-10-30T19:05:00.000001Z POST /v2/orders-export 202 20\n\
2026-10-30T19:05:00.000002Z GET /v2/orders 200 10\n\
2026-10-31T23:59:59.999999Z POST /v2/checkout 502 7\n\
2026-11-01T00:00:00.000000Z POST /v2/checkout 503 8\n";
        let expected = "2026-10-30T18:00:00Z / 1 0 0 0 0 0\n\
2026-10-30T18:00:00Z /health 3 62 4294967295 4294967295 0 0\n\
2026-10-30T18:00:00Z /v2/checkout 5 2500 204117 204117 3 600000\n\
2026-10-30T18:00:00Z /v2/orders/{id} 3 900 1841 1841 1 333333\n\
2026-10-30T19:00:00Z /v2/orders 1 10 10 10 0 0\n\
2026-10-30T19:00:00Z /v2/orders-export 1 20 20 20 0 0\n\
2026-10-30T19:00:00Z /v2/orders/{id} 1 1500 1500 1500 0 0\n\
2026-10-30T19:00:00Z /v2/orders_legacy 1 60 60 60 0 0\n\
2026-10-31T23:00:00Z /v2/checkout 1 7 7 7 1 1000000\n\
2026-11-01T00:00:00Z /v2/checkout 1 8 8 8 1 1000000\n";

        let dir = std::env::temp_dir().join(format!("track-a-spec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input_path = dir.join("requests.log");
        let output_path = dir.join("summary.txt");
        std::fs::write(&input_path, data).unwrap();

        run(input_path.to_str().unwrap(), output_path.to_str().unwrap(), 4).unwrap();
        let output = std::fs::read_to_string(&output_path).unwrap();
        assert_eq!(output, expected);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        let dir = std::env::temp_dir().join(format!("track-a-empty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input_path = dir.join("requests.log");
        let output_path = dir.join("summary.txt");
        std::fs::write(&input_path, b"").unwrap();

        run(input_path.to_str().unwrap(), output_path.to_str().unwrap(), 4).unwrap();
        let output = std::fs::read(&output_path).unwrap();
        assert_eq!(output.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn morsel_boundary_never_splits_or_drops_a_line() {
        // Lines of varying length so morsel boundaries land mid-line at
        // some morsel size, forcing the snap-forward logic to matter.
        let mut data = Vec::new();
        let mut total_lines = 0u32;
        for i in 0..2000u32 {
            let pad = "x".repeat((i % 37) as usize);
            data.extend_from_slice(
                format!("2026-10-30T10:00:00.000000Z GET /{pad}/{i} 200 {i}\n").as_bytes(),
            );
            total_lines += 1;
        }
        for morsel_size in [16usize, 64, 100, 257, 4096] {
            let groups = merged(process_parallel(&data, 4, morsel_size));
            let counted: u64 = groups.values().map(|g| g.count).sum();
            assert_eq!(counted, total_lines as u64, "morsel_size={morsel_size}");
        }
    }
}
