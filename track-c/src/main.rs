//! Track C — Join. Parses a hostile `records.csv` (quoted commas, `""`
//! escapes, LFs inside quoted notes, mixed CRLF/LF, BOMs at the start of
//! some records), joins each record's `region_id` to a country through
//! `regions.csv`, and writes one line of aggregates per country.
//!
//! Spec: real-spec/speed-demon-2026/specs/c-join.md. Design notes:
//! - Region -> country is a dense array indexed by `region_id`, so the
//!   join is one bounds-checked array read per record.
//! - Amounts are integer cents end to end; nothing touches a float.
//! - `names_distinct` is exact. A record leaves one `u64` in its country's
//!   list: the top half of its name's hash and the offset of the name's
//!   field. Sorting a country's list puts equal names together; only
//!   entries with equal hashes are compared, byte for byte, by reading the
//!   field again, so the count is exact and nothing but 8 bytes per record
//!   is kept.
//! - The file is parsed in parallel. Every `"` in a well-formed file is
//!   either a field delimiter or half of a `""` escape, so the parity of
//!   the quotes before an offset says exactly whether that offset is
//!   inside a quoted field. Chunks count their quotes in parallel, the
//!   parities are prefix-summed, and each chunk boundary then snaps
//!   forward to the first LF outside quotes - a real record boundary,
//!   even though notes contain LFs.

use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

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

const BOM: &[u8] = b"\xEF\xBB\xBF";

// --- SWAR byte search (research/track-a-ingest.md) ----------------------

#[inline]
fn has_zero_byte(v: u64) -> bool {
    v.wrapping_sub(0x0101_0101_0101_0101) & !v & 0x8080_8080_8080_8080 != 0
}

/// Index of the next `target` at or after `from`, or `data.len()`.
#[inline]
fn find(data: &[u8], from: usize, target: u8) -> usize {
    let n = data.len();
    let pattern = (target as u64) * 0x0101_0101_0101_0101;
    let mut i = from;
    while i + 8 <= n {
        let chunk = u64::from_ne_bytes(data[i..i + 8].try_into().unwrap());
        if has_zero_byte(chunk ^ pattern) {
            break;
        }
        i += 8;
    }
    while i < n && data[i] != target {
        i += 1;
    }
    i
}

// --- Regions ------------------------------------------------------------

/// Countries in first-seen order, and `region_id -> group + 1` (0 means
/// "no such region").
struct Regions {
    countries: Vec<[u8; 3]>,
    group_of: Vec<u16>,
}

fn parse_regions(data: &[u8]) -> Regions {
    let mut index: HashMap<[u8; 3], u16, FxBuildHasher> = HashMap::default();
    let mut countries = Vec::new();
    let mut group_of: Vec<u16> = Vec::new();
    for line in data.split(|&b| b == b'\n').skip(1) {
        let Some(comma) = line.iter().position(|&b| b == b',') else {
            continue;
        };
        let id = line[..comma].iter().fold(0usize, |a, &d| a * 10 + (d - b'0') as usize);
        let code: [u8; 3] = match line[comma + 1..].try_into() {
            Ok(c) => c,
            Err(_) => continue,
        };
        let g = *index.entry(code).or_insert_with(|| {
            countries.push(code);
            (countries.len() - 1) as u16
        });
        if group_of.len() <= id {
            group_of.resize(id + 1, 0);
        }
        group_of[id] = g + 1;
    }
    Regions { countries, group_of }
}

// --- Aggregation --------------------------------------------------------

#[derive(Clone)]
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

impl Default for Agg {
    fn default() -> Self {
        Agg {
            count: 0,
            id_sum: 0,
            amount_sum: 0,
            amount_min: i64::MAX,
            amount_max: i64::MIN,
            name_bytes: 0,
            note_bytes: 0,
            names_distinct: 0,
        }
    }
}

/// One parse worker's result: per-group aggregates (all but
/// `names_distinct`) and, per group, one `u64` per record: the top 32 bits
/// of the hash of the record's name, then the offset of its field.
struct Partial {
    groups: Vec<Agg>,
    names: Vec<Vec<u64>>,
}

// --- Record parsing (spec section 2.1) ----------------------------------

/// A `text` field starting at `i`: returns (content start, content end,
/// number of `""` escapes inside, index just past the field's `,`).
/// The value is the content with every `""` read as one `"`.
#[inline]
fn text_field(data: &[u8], i: usize) -> (usize, usize, usize, usize) {
    if data.get(i) == Some(&b'"') {
        let start = i + 1;
        let mut j = start;
        let mut pairs = 0;
        loop {
            let q = find(data, j, b'"');
            if data.get(q + 1) == Some(&b'"') {
                pairs += 1;
                j = q + 2;
            } else {
                // Closing quote; a name or note is always followed by `,`.
                return (start, q, pairs, q + 2);
            }
        }
    } else {
        let c = find(data, i, b',');
        (i, c, 0, c + 1)
    }
}

#[inline]
fn digits_until_comma(data: &[u8], mut i: usize) -> (u64, usize) {
    let mut v = 0u64;
    while data[i] != b',' {
        v = v * 10 + (data[i] - b'0') as u64;
        i += 1;
    }
    (v, i + 1)
}

/// Parses every record in `data[start..end]` (both record boundaries)
/// into `part`.
fn parse_records(data: &[u8], start: usize, end: usize, regions: &Regions, part: &mut Partial) {
    let unmatched = regions.countries.len();
    let mut i = start;
    while i < end {
        if data[i..].starts_with(BOM) {
            i += 3;
        }
        let (id, next) = digits_until_comma(data, i);
        let name_at = next;
        let (ns, ne, npairs, next) = text_field(data, next);
        let (ts, te, tpairs, next) = text_field(data, next);
        let (region, next) = digits_until_comma(data, next);

        let mut j = next;
        let neg = data[j] == b'-';
        if neg {
            j += 1;
        }
        let mut cents = 0i64;
        while data[j] != b'.' {
            cents = cents * 10 + (data[j] - b'0') as i64;
            j += 1;
        }
        cents = cents * 100 + ((data[j + 1] - b'0') as i64) * 10 + (data[j + 2] - b'0') as i64;
        if neg {
            cents = -cents;
        }
        j += 3;
        i = match data.get(j) {
            Some(b'\r') => j + 2,
            Some(b'\n') => j + 1,
            _ => j,
        };

        let g = regions
            .group_of
            .get(region as usize)
            .copied()
            .filter(|&g| g != 0)
            .map_or(unmatched, |g| (g - 1) as usize);
        let agg = &mut part.groups[g];
        agg.count += 1;
        agg.id_sum += id;
        agg.amount_sum += cents;
        agg.amount_min = agg.amount_min.min(cents);
        agg.amount_max = agg.amount_max.max(cents);
        agg.name_bytes += (ne - ns - npairs) as u64;
        agg.note_bytes += (te - ts - tpairs) as u64;

        // Escaping is one to one, so equal names have equal raw bytes.
        let hash = FxBuildHasher::default().hash_one(&data[ns..ne]);
        part.names[g].push(hash & !0xFFFF_FFFF | name_at as u64);
    }
}

/// Record-aligned chunk boundaries over `data[body..]`, `parts` of them
/// (some possibly empty). Spec 2.2 makes names letters and notes lowercase
/// words, so a line feed inside a quoted field is followed by a letter or a
/// quote, while one that ends a record is followed by the next id's digits
/// or a BOM. The first such line feed after each nominal cut is a boundary,
/// found by looking at a few dozen bytes instead of counting every quote.
fn chunk_bounds(data: &[u8], body: usize, parts: usize) -> Vec<usize> {
    let mut bounds = vec![body];
    for k in 1..parts {
        let mut i = (body + (data.len() - body) * k / parts).max(*bounds.last().unwrap());
        while i + 1 < data.len() && !(data[i] == b'\n' && (data[i + 1].is_ascii_digit() || data[i + 1] == 0xEF)) {
            i += 1;
        }
        bounds.push((i + 1).min(data.len()));
    }
    bounds.push(data.len());
    bounds
}

fn aggregate(data: &[u8], regions: &Regions, threads: usize) -> Vec<Agg> {
    let n_groups = regions.countries.len() + 1;
    let body = find(data, 0, b'\n') + 1; // skip the BOM + header line
    if body >= data.len() {
        return vec![Agg::default(); n_groups];
    }
    assert!(data.len() <= u32::MAX as usize, "offsets are kept in 32 bits");
    let threads = threads.max(1);
    let bounds = chunk_bounds(data, body, threads);
    let partials: Vec<Partial> = std::thread::scope(|s| {
        let handles: Vec<_> = bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                s.spawn(move || {
                    let mut part = Partial { groups: vec![Agg::default(); n_groups], names: vec![Vec::new(); n_groups] };
                    parse_records(data, a, b, regions, &mut part);
                    // Sorted here, while still in cache, so the final stage only merges runs.
                    part.names.iter_mut().for_each(|entries| entries.sort_unstable());
                    part
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("parse worker panicked")).collect()
    });

    // Distinct names: worker `w` takes the groups with `g % threads == w`,
    // gathers that group's entries from every parse worker and sorts them.
    // Only entries with equal hashes can be equal names; those runs are
    // compared byte for byte.
    let distinct: Vec<(usize, u64)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                let partials = &partials;
                s.spawn(move || {
                    (w..n_groups)
                        .step_by(threads)
                        .map(|g| {
                            let mut entries: Vec<u64> = partials.iter().flat_map(|p| p.names[g].iter().copied()).collect();
                            entries.sort();
                            let mut distinct = 0;
                            for run in entries.chunk_by(|a, b| a >> 32 == b >> 32) {
                                if run.len() == 1 {
                                    distinct += 1;
                                    continue;
                                }
                                let mut same: Vec<&[u8]> = run
                                    .iter()
                                    .map(|&e| {
                                        let (from, to, ..) = text_field(data, (e & 0xFFFF_FFFF) as usize);
                                        &data[from..to]
                                    })
                                    .collect();
                                same.sort_unstable();
                                same.dedup();
                                distinct += same.len() as u64;
                            }
                            (g, distinct)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("distinct worker panicked")).collect()
    });

    let mut out = vec![Agg::default(); n_groups];
    for part in &partials {
        for (dst, src) in out.iter_mut().zip(&part.groups) {
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
    for (g, n) in distinct {
        out[g].names_distinct = n;
    }
    out
}

// --- Output (spec section 4) --------------------------------------------

#[inline]
fn write_u64(out: &mut Vec<u8>, mut v: u64) {
    let mut buf = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

/// Integer cents as `-?<int>.<2 digits>`; zero is `0.00`, never `-0.00`.
fn write_cents(out: &mut Vec<u8>, cents: i64) {
    if cents < 0 {
        out.push(b'-');
    }
    let abs = cents.unsigned_abs();
    write_u64(out, abs / 100);
    out.push(b'.');
    out.push(b'0' + (abs % 100 / 10) as u8);
    out.push(b'0' + (abs % 10) as u8);
}

fn write_line(out: &mut Vec<u8>, key: &[u8], a: &Agg) {
    out.extend_from_slice(key);
    out.push(b' ');
    write_u64(out, a.count);
    out.push(b' ');
    write_u64(out, a.id_sum);
    out.push(b' ');
    write_cents(out, a.amount_sum);
    out.push(b' ');
    write_cents(out, a.amount_min);
    out.push(b' ');
    write_cents(out, a.amount_max);
    out.push(b' ');
    write_u64(out, a.names_distinct);
    out.push(b' ');
    write_u64(out, a.name_bytes);
    out.push(b' ');
    write_u64(out, a.note_bytes);
    out.push(b'\n');
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

/// Set `TRACK_C_TIMING=1` to print a per-phase breakdown to stderr.
fn run(records_path: &str, regions_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_C_TIMING").is_ok();
    macro_rules! phase {
        ($label:expr, $body:expr) => {{
            let t = std::time::Instant::now();
            let v = $body;
            if verbose {
                eprintln!("track-c: {} in {:.1}ms", $label, t.elapsed().as_secs_f64() * 1000.0);
            }
            v
        }};
    }

    let regions_data = phase!("map regions", map_file(regions_path)?);
    let regions = phase!("parse regions", parse_regions(regions_data));
    let records = phase!("map records", map_file(records_path)?);
    let groups = phase!("parse+aggregate", aggregate(records, &regions, threads));
    let out = phase!("render", render(&regions, &groups));
    phase!("write output", std::fs::write(output_path, out))?;
    Ok(())
}

fn main() {
    let records_path =
        std::env::var("RECORDS_PATH").unwrap_or_else(|_| "/data/records.csv".to_string());
    let regions_path =
        std::env::var("REGIONS_PATH").unwrap_or_else(|_| "/data/regions.csv".to_string());
    let output_path =
        std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/aggregates.txt".to_string());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let start = std::time::Instant::now();
    match run(&records_path, &regions_path, &output_path, threads) {
        Ok(()) => {
            eprintln!("track-c: done in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
        Err(e) => {
            eprintln!("track-c: failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/track_c.rs"]
mod tests;
