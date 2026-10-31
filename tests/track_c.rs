//! Unit tests for track-c/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

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

/// `bytes` as a file mapping, the way `main` reads its inputs (the parse
/// releases the pages of finished pieces, which would wipe heap memory).
fn mapped(bytes: &[u8]) -> &'static [u8] {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("track-c-test-{}-{n}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let mapping = map_file(path.to_str().unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    mapping
}

fn run_on(records: &[u8], regions: &[u8], threads: usize) -> String {
    String::from_utf8(solve(mapped(records), mapped(regions), threads)).unwrap()
}

#[test]
fn matches_the_spec_worked_example_exactly() {
    assert_eq!(run_on(WORKED_RECORDS, WORKED_REGIONS, 1), WORKED_OUTPUT);
}

/// More workers than records: boundaries land on every record, inside
/// and next to multi-line quoted notes, and right before BOMs. The
/// CRLF cuts must still find real record boundaries.
#[test]
fn parallel_parse_matches_single_threaded() {
    for threads in [2, 3, 4, 7, 12, 40] {
        assert_eq!(
            run_on(WORKED_RECORDS, WORKED_REGIONS, threads),
            WORKED_OUTPUT,
            "threads={threads}"
        );
    }
}

#[test]
fn cents_format_matches_spec() {
    let fmt = cents;
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
    assert_eq!(
        run_on(records, regions, 1),
        "AAA 1 1 1.00 1.00 1.00 1 2 4\n"
    );
}

/// A file big enough for real chunk cuts: the worked example's records
/// repeated, so every count scales and the distinct counts do not.
#[test]
fn many_chunks_match_the_scaled_worked_example() {
    let header_end = WORKED_RECORDS.iter().position(|&b| b == b'\n').unwrap() + 1;
    let mut records = WORKED_RECORDS[..header_end].to_vec();
    for _ in 0..3000 {
        records.extend_from_slice(&WORKED_RECORDS[header_end..]);
    }
    let expected = "AAA 15000 51000 4553250.00 -0.75 1420.00 4 72000 165000\n\
BBB 3000 12000 -15750.00 -5.25 -5.25 1 45000 105000\n\
CCC 3000 27000 -150.00 -0.05 -0.05 1 9000 3000\n\
_UNMATCHED 6000 45000 3000.00 -1.00 2.00 2 63000 3000\n";
    for threads in [1, 3, 12] {
        assert_eq!(
            run_on(&records, WORKED_REGIONS, threads),
            expected,
            "threads={threads}"
        );
    }
}

/// More distinct names than the name table starts with: the scan restarts
/// with a bigger one and the count stays exact.
#[test]
fn name_table_grows_when_there_are_too_many_names() {
    let mut records = b"id,name,note,region_id,amount\n".to_vec();
    for i in 1..=140_000 {
        records.extend_from_slice(format!("{i},\"n{i}\",\"x\",1,1.00\n").as_bytes());
    }
    let out = run_on(&records, b"region_id,country\n1,AAA\n", 4);
    assert!(out.starts_with("AAA 140000 "), "{out}");
    assert!(out.contains(" 140000 "), "{out}");
}

/// More than 254 countries: the one-byte region table spills to a map.
#[test]
fn more_than_254_countries() {
    let code = |i: usize| {
        format!(
            "{}{}A",
            (b'A' + (i / 26) as u8) as char,
            (b'A' + (i % 26) as u8) as char
        )
    };
    let mut regions = b"region_id,country\n".to_vec();
    let mut records = b"id,name,note,region_id,amount\n".to_vec();
    for i in 0..300 {
        regions.extend_from_slice(format!("{},{}\n", i + 1, code(i)).as_bytes());
        records.extend_from_slice(format!("{},\"n\",\"x\",{},1.00\n", i + 1, i + 1).as_bytes());
    }
    let out = run_on(&records, &regions, 2);
    assert_eq!(out.lines().count(), 300);
    assert!(
        out.lines()
            .all(|l| l.contains(" 1 ") && l.contains(" 1.00 ")),
        "{out}"
    );
}

/// Records built from known values (names up to 40 bytes over a small alphabet
/// with commas, quotes and spaces, so many share prefixes and lengths around
/// 16; notes with LFs; random BOMs and terminators; some unknown regions)
/// against totals computed straight from those values, at several thread
/// counts.
#[test]
fn generated_records_match_totals_computed_from_the_values() {
    use std::collections::{BTreeMap, HashSet};
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut rand = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let text = |rand: &mut dyn FnMut(u64) -> u64, alphabet: &[u8], max: u64| -> Vec<u8> {
        let len = [0, 1, 7, 8, 9, 15, 16, 17, 23, 24, 25, 40, rand(max)][rand(13) as usize];
        (0..len)
            .map(|_| alphabet[rand(alphabet.len() as u64) as usize])
            .collect()
    };
    let mut regions = b"region_id,country\n".to_vec();
    let countries: Vec<String> = (0..7)
        .map(|c| format!("{}{}Z", (b'A' + c) as char, (b'Q' - c) as char))
        .collect();
    for id in 1..=300 {
        regions.extend(format!("{id},{}\n", countries[id % 7]).bytes());
    }
    let mut records = b"\xEF\xBB\xBFid,name,note,region_id,amount\r\n".to_vec();
    let mut want: BTreeMap<String, (u64, u64, i64, i64, i64, HashSet<Vec<u8>>, u64, u64)> =
        BTreeMap::new();
    let quote = |v: &[u8]| -> Vec<u8> { [b"\"", &v.replace_quotes()[..], b"\""].concat() };
    let mut id = 0;
    for i in 0..20_000 {
        id += 1 + rand(3);
        let (name, note) = (
            text(&mut rand, b"ab ,\"", 60),
            text(&mut rand, b"ab ,\"\n", 90),
        );
        let region = [rand(320) + 1, 4_000_000][(rand(20) == 0) as usize];
        let cents = rand(2_000_000) as i64 - 100_000;
        let field = |v: &[u8], empty_unquoted: bool| {
            if v.is_empty() && empty_unquoted {
                vec![]
            } else {
                quote(v)
            }
        };
        if i > 0 && rand(30) == 0 {
            records.extend(b"\xEF\xBB\xBF");
        }
        let amount = format!(
            "{}{}.{:02}",
            if cents < 0 { "-" } else { "" },
            cents.abs() / 100,
            cents.abs() % 100
        );
        let line = [
            id.to_string().into_bytes(),
            field(&name, rand(2) == 0),
            field(&note, rand(2) == 0),
            region.to_string().into_bytes(),
            amount.into_bytes(),
        ];
        records.extend(line.join(&b','));
        records.extend(if rand(2) == 0 {
            &b"\r\n"[..]
        } else {
            &b"\n"[..]
        });
        let key = if (1..=300).contains(&region) {
            countries[region as usize % 7].clone()
        } else {
            "_UNMATCHED".to_string()
        };
        let w = want.entry(key).or_default();
        w.0 += 1;
        w.1 += id;
        w.2 += cents;
        w.3 = if w.0 == 1 { cents } else { w.3.min(cents) };
        w.4 = if w.0 == 1 { cents } else { w.4.max(cents) };
        w.5.insert(name.clone());
        w.6 += name.len() as u64;
        w.7 += note.len() as u64;
    }
    let money = |c: i64| {
        format!(
            "{}{}.{:02}",
            if c < 0 { "-" } else { "" },
            c.abs() / 100,
            c.abs() % 100
        )
    };
    let expected: String = want
        .iter()
        .map(|(k, w)| {
            format!(
                "{k} {} {} {} {} {} {} {} {}\n",
                w.0,
                w.1,
                money(w.2),
                money(w.3),
                money(w.4),
                w.5.len(),
                w.6,
                w.7
            )
        })
        .collect();
    for threads in [1, 2, 5, 12] {
        assert_eq!(
            run_on(&records, &regions, threads),
            expected,
            "threads={threads}"
        );
    }
}

trait ReplaceQuotes {
    fn replace_quotes(&self) -> Vec<u8>;
}

impl ReplaceQuotes for [u8] {
    /// Every `"` doubled, the way a quoted field is written.
    fn replace_quotes(&self) -> Vec<u8> {
        self.iter()
            .flat_map(|&b| if b == b'"' { vec![b, b] } else { vec![b] })
            .collect()
    }
}
