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
