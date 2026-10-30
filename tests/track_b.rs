//! Unit tests for track-b/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

use super::*;

fn tokens(text: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for_each_token(text, |buf, len| out.push(buf[..len].to_vec()));
    out
}

/// The term table against a plain `HashMap`, over terms built to
/// stress its key layout: every length from 1 to 32, many sharing
/// their first 8 bytes, pairs whose first and last 8 bytes agree but
/// whose lengths differ, and non-members one byte away from members.
#[test]
fn term_table_matches_a_hash_map() {
    let mut terms: Vec<Vec<u8>> = Vec::new();
    for len in 1..=MAX_TERM {
        terms.push((0..len).map(|i| b'a' + (i % 26) as u8).collect());
        terms.push((0..len).map(|i| b'0' + (i % 10) as u8).collect());
    }
    for i in 0..2000u32 {
        terms.push(format!("abcdefgh{i}").into_bytes());
        terms.push(format!("x{i}y").into_bytes());
    }
    terms.push(b"aaaaaaaabbbbbbbb".to_vec());
    terms.push(b"aaaaaaaacbbbbbbbb".to_vec());
    terms.push(b"aaaaaaaaccbbbbbbbb".to_vec());
    terms.sort();
    terms.dedup();
    let refs: Vec<&[u8]> = terms.iter().map(|t| t.as_slice()).collect();
    let table = TermTable::new(refs.clone());
    let map: HashMap<&[u8], u32> = refs.iter().enumerate().map(|(i, &t)| (t, i as u32)).collect();

    let mut probes: Vec<Vec<u8>> = terms.clone();
    for t in &terms {
        let mut near = t.clone();
        let last = near.len() - 1;
        near[last] = if near[last] == b'z' { b'0' } else { b'z' };
        probes.push(near);
        if t.len() < MAX_TERM {
            let mut longer = t.clone();
            longer.push(b'q');
            probes.push(longer);
        }
    }
    for p in &probes {
        let mut buf = [0u8; MAX_TERM];
        buf.fill(b'#'); // stale bytes past `len` must be ignored
        buf[..p.len()].copy_from_slice(p);
        let want = map.get(p.as_slice()).copied().unwrap_or(NONE);
        let (head, tail) = head_tail(&buf, p.len());
        let got = table.get(head, tail, p.len(), Some(&buf));
        assert_eq!(got, want, "{:?}", String::from_utf8_lossy(p));
    }
    assert_eq!(table.get(0, 0, 0, None), NONE);
}

#[test]
fn write_u64_matches_to_string_for_representative_values() {
    for &v in &[0u64, 1, 9, 10, 99, 100, 1234, 999_999, u32::MAX as u64, u64::MAX] {
        let mut out = Vec::new();
        write_u64(&mut out, v);
        assert_eq!(out, v.to_string().as_bytes());
    }
}

#[test]
fn tokenizer_is_byte_based_ascii_only() {
    // Spec section 2.2's own worked cases.
    assert_eq!(tokens("Zürich".as_bytes()), vec![b"z".to_vec(), b"rich".to_vec()]);
    assert_eq!(tokens("naïve".as_bytes()), vec![b"na".to_vec(), b"ve".to_vec()]);
    assert_eq!(tokens("273\u{212A}".as_bytes()), vec![b"273".to_vec()]);
    assert_eq!(tokens("\u{130}STANBUL".as_bytes()), vec![b"stanbul".to_vec()]);
    assert_eq!(tokens(b"007 7"), vec![b"007".to_vec(), b"7".to_vec()]);
    assert_eq!(tokens(b"habeas-corpus court's"), vec![
        b"habeas".to_vec(),
        b"corpus".to_vec(),
        b"court".to_vec(),
        b"s".to_vec()
    ]);
    assert_eq!(tokens(b"U.S.C."), vec![b"u".to_vec(), b"s".to_vec(), b"c".to_vec()]);
}

#[test]
fn overlong_tokens_keep_their_position_but_match_nothing() {
    let long = "a".repeat(MAX_TERM + 1);
    let text = format!("x {long} y");
    assert_eq!(tokens(text.as_bytes()), vec![b"x".to_vec(), Vec::new(), b"y".to_vec()]);
}

/// The set operations against naive reference implementations, over
/// size ratios on both sides of the binary-search switch in
/// `intersect`.
#[test]
fn set_operations_match_naive_versions() {
    let lists: Vec<Vec<u32>> = vec![
        vec![],
        vec![5],
        (0..40).map(|i| i * 3).collect(),
        (0..2000).map(|i| i * 2 + 1).collect(),
        (0..5000).collect(),
        vec![0, 1, 2, 4_999, 10_000],
    ];
    for a in &lists {
        for b in &lists {
            let want_and: Vec<u32> = a.iter().copied().filter(|x| b.contains(x)).collect();
            let want_not: Vec<u32> = a.iter().copied().filter(|x| !b.contains(x)).collect();
            let mut want_or: Vec<u32> = a.iter().chain(b.iter()).copied().collect();
            want_or.sort_unstable();
            want_or.dedup();
            assert_eq!(intersect(a, b), want_and, "and {} {}", a.len(), b.len());
            assert_eq!(difference(a, b), want_not, "not {} {}", a.len(), b.len());
            assert_eq!(union(a, b), want_or, "or {} {}", a.len(), b.len());
        }
    }
}

fn run_on(docs: &str, queries: &str, threads: usize) -> String {
    // Tests run concurrently; a per-call counter keeps their temp
    // dirs (and input files) from colliding.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("track-b-test-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let docs_path = dir.join("docs.txt");
    let queries_path = dir.join("queries.txt");
    let out_path = dir.join("results.txt");
    std::fs::write(&docs_path, docs.as_bytes()).unwrap();
    std::fs::write(&queries_path, queries.as_bytes()).unwrap();
    run(docs_path.to_str().unwrap(), queries_path.to_str().unwrap(), out_path.to_str().unwrap(), threads)
        .unwrap();
    let out = std::fs::read_to_string(&out_path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const WORKED_DOCS: &str = "0\tThe surety posted bail; the BAIL BOND was forfeited.\n\
1\tWrit of Habeas-Corpus: see 42 U.S.C. \u{a7} 1983 (bail bond).\n\
2\t\n\
3\tZ\u{fc}rich bail\u{2014}bond, bail bail bond. Case 007.\n\
4\tIn the matter of the estate. Bond not posted, No. 7.\n";

const WORKED_QUERIES: &str = "BOOL bail AND bond\n\
BOOL bail\n\
BOOL estate OR surety AND bail\n\
BOOL bond NOT bail\n\
BOOL bail NOT bail\n\
BOOL zurich\n\
BOOL rich AND z\n\
BOOL not AND bond\n\
BOOL 7\n\
BOOL the OR writ OR case\n\
PHRASE \"bail bond\"\n\
PHRASE \"bail bail bond\"\n\
PHRASE \"bond bail\"\n\
PHRASE \"habeas corpus\"\n\
PHRASE \"u s c 1983\"\n\
PHRASE \"estate bond\"\n\
PHRASE \"forfeited writ\"\n\
PHRASE \"in the matter of\"\n";

const WORKED_RESULTS: &str = "3 4 0 1 3\n\
3 4 0 1 3\n\
1 0 0\n\
1 4 4\n\
0 0\n\
0 0\n\
1 3 3\n\
1 4 4\n\
1 4 4\n\
4 8 0 1 3 4\n\
3 4 0 1 3\n\
1 3 3\n\
1 3 3\n\
1 1 1\n\
1 1 1\n\
1 4 4\n\
0 0\n\
1 4 4\n";

/// specs/b-index.md section 4's worked example, byte for byte.
#[test]
fn matches_the_spec_worked_example_exactly() {
    assert_eq!(run_on(WORKED_DOCS, WORKED_QUERIES, 1), WORKED_RESULTS);
}

/// More workers than documents: chunk boundaries land on every line
/// and some chunks are empty; the joined postings and the doc -> chunk
/// lookup must still give identical results.
#[test]
fn parallel_run_matches_single_threaded() {
    for threads in [2, 3, 5, 12] {
        assert_eq!(run_on(WORKED_DOCS, WORKED_QUERIES, threads), WORKED_RESULTS, "threads={threads}");
    }
}

#[test]
fn phrase_needs_consecutive_positions_including_repeats() {
    let docs = "0\tthe dog barked at the enemy near the fights enemy today\n\
1\tcommon rare match here\n\
2\tcommon filler two\n\
3\tcommon filler rare match again\n\
4\tx y x x z\n\
5\tx x x\n";
    let queries = "PHRASE \"the fights enemy\"\n\
PHRASE \"rare match\"\n\
PHRASE \"the barked\"\n\
PHRASE \"x x\"\n\
PHRASE \"x x x\"\n\
PHRASE \"x x x x\"\n";
    let want = "1 0 0\n2 4 1 3\n0 0\n2 9 4 5\n1 5 5\n0 0\n";
    for threads in [1, 3] {
        assert_eq!(run_on(docs, queries, threads), want, "threads={threads}");
    }
}
