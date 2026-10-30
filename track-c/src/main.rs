//! Track C — Join. Parses a hostile `records.csv` (quoted commas, `""`
//! escapes, LFs inside quoted notes, mixed CRLF/LF, BOMs at the start of
//! some records), joins each record's `region_id` to a country through
//! `regions.csv`, and writes one line of aggregates per country.
//!
//! Spec: real-spec/speed-demon-2026/specs/c-join.md. Design notes:
//! - Region -> country is a dense array indexed by `region_id`, so the
//!   join is one bounds-checked array read per record.
//! - Amounts are integer cents end to end; nothing touches a float.
//! - `names_distinct` is exact, by byte comparison. Each country belongs
//!   to one worker; the parse only notes where each record's name is, in
//!   that worker's bucket, and the owner then counts distinct names per
//!   country with a set of name slices. No ids, no global table.
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

/// Where one record's unescaped name is: `len` bytes at `at` in the
/// input, or in the parsing worker's `unescaped` buffer when the raw
/// name contained `""` escapes.
#[derive(Clone, Copy)]
struct NameRef {
    group: u16,
    in_unescaped: bool,
    at: u32,
    len: u32,
}

/// One parse worker's result: per-group aggregates (all but
/// `names_distinct`), its records' names bucketed by the worker that owns
/// each name's group, and the unescaped copies of names that had `""`.
struct Partial {
    groups: Vec<Agg>,
    names: Vec<Vec<NameRef>>,
    unescaped: Vec<u8>,
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
    let owners = part.names.len();
    let mut i = start;
    while i < end {
        if data[i..].starts_with(BOM) {
            i += 3;
        }
        let (id, next) = digits_until_comma(data, i);
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

        let name = if npairs == 0 {
            NameRef { group: g as u16, in_unescaped: false, at: ns as u32, len: (ne - ns) as u32 }
        } else {
            let at = part.unescaped.len();
            let mut k = ns;
            while k < ne {
                part.unescaped.push(data[k]);
                k += if data[k] == b'"' { 2 } else { 1 };
            }
            NameRef { group: g as u16, in_unescaped: true, at: at as u32, len: (part.unescaped.len() - at) as u32 }
        };
        part.names[g % owners].push(name);
    }
}

/// Record-aligned chunk boundaries over `data[body..]`, `parts` of them
/// (some possibly empty), via the quote-parity scan described above.
fn chunk_bounds(data: &[u8], body: usize, parts: usize) -> Vec<usize> {
    let len = data.len() - body;
    let nominal: Vec<usize> = (0..=parts).map(|k| body + len * k / parts).collect();
    let quotes: Vec<usize> = std::thread::scope(|s| {
        let handles: Vec<_> = nominal
            .windows(2)
            .map(|w| s.spawn(move || data[w[0]..w[1]].iter().filter(|&&b| b == b'"').count()))
            .collect();
        handles.into_iter().map(|h| h.join().expect("quote counter panicked")).collect()
    });

    let mut bounds = vec![body];
    let mut parity = 0usize;
    for k in 1..parts {
        parity += quotes[k - 1];
        let prev = *bounds.last().unwrap();
        // The parity is for `nominal[k]`. If the previous boundary already
        // lies past it, resume there instead: a record boundary is always
        // outside quotes.
        let (mut i, mut inside) = if nominal[k] >= prev {
            (nominal[k], parity % 2 == 1)
        } else {
            (prev, false)
        };
        while i < data.len() {
            match data[i] {
                b'"' => inside = !inside,
                b'\n' if !inside => break,
                _ => {}
            }
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
    let threads = threads.max(1);
    let bounds = chunk_bounds(data, body, threads);
    let partials: Vec<Partial> = std::thread::scope(|s| {
        let handles: Vec<_> = bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                s.spawn(move || {
                    let mut part = Partial {
                        groups: vec![Agg::default(); n_groups],
                        names: vec![Vec::new(); threads],
                        unescaped: Vec::new(),
                    };
                    parse_records(data, a, b, regions, &mut part);
                    part
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("parse worker panicked")).collect()
    });

    // Distinct names: owner `w` handles the groups with `g % threads == w`,
    // reading its bucket from every parse worker. A group's names are kept
    // as (hash, name) pairs and sorted by hash, which stays inside the cache
    // for a group's few thousand names where one big hash set would not;
    // only names with equal hashes are compared byte for byte, so the count
    // is exact.
    let distinct: Vec<(usize, u64)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                let partials = &partials;
                s.spawn(move || {
                    let mut by_group: Vec<Vec<(u64, &[u8])>> = vec![Vec::new(); n_groups.div_ceil(threads)];
                    for p in partials {
                        for r in &p.names[w] {
                            let src: &[u8] = if r.in_unescaped { &p.unescaped } else { data };
                            let name = &src[r.at as usize..(r.at + r.len) as usize];
                            by_group[r.group as usize / threads].push((FxBuildHasher::default().hash_one(name), name));
                        }
                    }
                    let mut counts = Vec::new();
                    for (i, names) in by_group.iter_mut().enumerate() {
                        names.sort_unstable_by_key(|e| e.0);
                        let mut distinct = 0;
                        for run in names.chunk_by(|a, b| a.0 == b.0) {
                            let mut same: Vec<&[u8]> = run.iter().map(|e| e.1).collect();
                            same.sort_unstable();
                            same.dedup();
                            distinct += same.len() as u64;
                        }
                        counts.push((i * threads + w, distinct));
                    }
                    counts
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
        if g < n_groups {
            out[g].names_distinct = n;
        }
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
mod tests {
    use super::*;

    /// specs/c-join.md section 4's worked example (also `WORKED_RECORDS`
    /// in the organisers' tests/test_c_join.py), byte for byte.
    const WORKED_RECORDS: &[u8] = b"\xEF\xBB\xBFid,name,note,region_id,amount\r\n\
1,\"Kaphar, Titus\",\"said \"\"no\"\" twice\",7,1420.00\r\n\
2,\"Betts\",\"a note that\nspans two lines\",7,88.50\n\
3,,\"\",12,0.00\r\n\
4,\"trailing space \",\"comma, quote \"\" and newline\ntogether\",3,-5.25\n\
\xEF\xBB\xBF5,\"Betts \",\"one\ntwo\nthree\",12,-0.75\r\n\
6,\"\",,12,10.00\r\n\
\xEF\xBB\xBF7,\"Ann \"\"Annie\"\" Lee\",\"x\",5,2.00\n\
8,\"Nobody\",\"\",4000000,-1.00\r\n\
9,\"Neg\",\"n\",20,-0.05\r\n";

    const WORKED_REGIONS: &[u8] = b"region_id,country\n3,BBB\n7,AAA\n12,AAA\n20,CCC\n21,DDD\n";

    const WORKED_OUTPUT: &str = "AAA 5 17 1517.75 -0.75 1420.00 4 24 55\n\
BBB 1 4 -5.25 -5.25 -5.25 1 15 35\n\
CCC 1 9 -0.05 -0.05 -0.05 1 3 1\n\
_UNMATCHED 2 15 1.00 -1.00 2.00 2 21 1\n";

    fn run_on(records: &[u8], regions: &[u8], threads: usize) -> String {
        let regions = parse_regions(regions);
        let groups = aggregate(records, &regions, threads);
        String::from_utf8(render(&regions, &groups)).unwrap()
    }

    #[test]
    fn matches_the_spec_worked_example_exactly() {
        assert_eq!(run_on(WORKED_RECORDS, WORKED_REGIONS, 1), WORKED_OUTPUT);
    }

    /// More workers than records: boundaries land on every record, inside
    /// and next to multi-line quoted notes, and right before BOMs. The
    /// quote-parity scan must still find real record boundaries.
    #[test]
    fn parallel_parse_matches_single_threaded() {
        for threads in [2, 3, 4, 7, 12, 40] {
            assert_eq!(run_on(WORKED_RECORDS, WORKED_REGIONS, threads), WORKED_OUTPUT, "threads={threads}");
        }
    }

    #[test]
    fn cents_format_matches_spec() {
        let fmt = |c| {
            let mut v = Vec::new();
            write_cents(&mut v, c);
            String::from_utf8(v).unwrap()
        };
        assert_eq!(fmt(-525), "-5.25");
        assert_eq!(fmt(-5), "-0.05");
        assert_eq!(fmt(0), "0.00");
        assert_eq!(fmt(142000), "1420.00");
        assert_eq!(fmt(9_999_999), "99999.99");
    }

    #[test]
    fn quoted_fields_starting_or_ending_with_an_escaped_quote() {
        // Spec 2.2: "a quoted field can begin with `\"\"\"` or end with `\"\"\"`".
        let records = b"\xEF\xBB\xBFid,name,note,region_id,amount\r\n\
1,\"\"\"Q\",\"\"\"hi\"\"\",1,1.00\r\n";
        let regions = b"region_id,country\n1,AAA\n";
        // name `"Q` (2 bytes), note `"hi"` (4 bytes).
        assert_eq!(run_on(records, regions, 1), "AAA 1 1 1.00 1.00 1.00 1 2 4\n");
    }
}
