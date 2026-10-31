//! Unit tests for track-b/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

use super::*;

fn tokens(text: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    scan_tokens(text, |s, e| {
        out.push(text[s..e].to_ascii_lowercase());
        true
    });
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
    let mut table = TermTable::new(0);
    for (i, &t) in refs.iter().enumerate() {
        assert_eq!(table.intern(t), i as u32);
    }
    assert_eq!(
        table.intern(refs[7]),
        7,
        "interning a known term returns its id"
    );
    let map: std::collections::HashMap<&[u8], u32> = refs
        .iter()
        .enumerate()
        .map(|(i, &t)| (t, i as u32))
        .collect();

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
        // Mixed case, with neighbours either side that must be ignored.
        let mut buf = b"###".to_vec();
        buf.extend(p.iter().map(|&c| {
            if c.is_ascii_lowercase() && p.len() % 2 == 1 {
                c - 32
            } else {
                c
            }
        }));
        buf.extend_from_slice(b"###");
        let want = map.get(p.as_slice()).copied().unwrap_or(table.none());
        let got = {
            let k = key(&buf, 3, 3 + p.len());
            table.get(k, table.home(k), &buf, 3)
        };
        assert_eq!(got, want, "{:?}", String::from_utf8_lossy(p));
    }
    assert_eq!(
        table.get((0, 0, 0), table.home((0, 0, 0)), b"", 0),
        table.none()
    );
}

#[test]
fn write_u64_matches_to_string_for_representative_values() {
    for &v in &[
        0u64,
        1,
        9,
        10,
        99,
        100,
        1234,
        999_999,
        u32::MAX as u64,
        u64::MAX,
    ] {
        let mut out = Vec::new();
        write_u64(&mut out, v);
        assert_eq!(out, v.to_string().as_bytes());
    }
}

#[test]
fn tokenizer_is_byte_based_ascii_only() {
    // Spec section 2.2's own worked cases.
    assert_eq!(
        tokens("Zürich".as_bytes()),
        vec![b"z".to_vec(), b"rich".to_vec()]
    );
    assert_eq!(
        tokens("naïve".as_bytes()),
        vec![b"na".to_vec(), b"ve".to_vec()]
    );
    assert_eq!(tokens("273\u{212A}".as_bytes()), vec![b"273".to_vec()]);
    assert_eq!(
        tokens("\u{130}STANBUL".as_bytes()),
        vec![b"stanbul".to_vec()]
    );
    assert_eq!(tokens(b"007 7"), vec![b"007".to_vec(), b"7".to_vec()]);
    assert_eq!(
        tokens(b"habeas-corpus court's"),
        vec![
            b"habeas".to_vec(),
            b"corpus".to_vec(),
            b"court".to_vec(),
            b"s".to_vec()
        ]
    );
    assert_eq!(
        tokens(b"U.S.C."),
        vec![b"u".to_vec(), b"s".to_vec(), b"c".to_vec()]
    );
}

#[test]
fn overlong_tokens_keep_their_position_but_match_nothing() {
    let long = "a".repeat(MAX_TERM + 1);
    let docs = format!(
        "0	x {long} y
1	x y
"
    );
    assert_eq!(key(long.as_bytes(), 0, long.len()).2, 0);
    assert_eq!(
        run_on(
            &docs,
            "PHRASE \"x y\"
BOOL x AND y
",
            1
        ),
        "1 1 1
2 1 0 1
"
    );
}

/// The set operations against naive reference implementations, over
/// size ratios on both sides of the binary-search switch in
/// `intersect`.
#[test]
fn set_operations_match_naive_versions() {
    let lists: Vec<Vec<u16>> = vec![
        vec![],
        vec![5],
        (0..40).map(|i| i * 3).collect(),
        (0..2000).map(|i| i * 2 + 1).collect(),
        (0..5000).collect(),
        vec![0, 1, 2, 4_999, 10_000],
    ];
    for a in &lists {
        for b in &lists {
            let want_and: Vec<u16> = a.iter().copied().filter(|x| b.contains(x)).collect();
            let want_not: Vec<u16> = a.iter().copied().filter(|x| !b.contains(x)).collect();
            let mut want_or: Vec<u16> = a.iter().chain(b.iter()).copied().collect();
            want_or.sort_unstable();
            want_or.dedup();
            let mut out = Vec::new();
            intersect(a, b, &mut out);
            assert_eq!(out, want_and, "and {} {}", a.len(), b.len());
            difference(a, b, &mut out);
            assert_eq!(out, want_not, "not {} {}", a.len(), b.len());
            union(a, b, &mut out);
            assert_eq!(out, want_or, "or {} {}", a.len(), b.len());
        }
    }
}

fn run_on(docs: &str, queries: &str, threads: usize) -> String {
    run_pieces(docs, queries, threads, PIECE_BYTES)
}

fn run_pieces(docs: &str, queries: &str, threads: usize, piece: usize) -> String {
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
    run(
        docs_path.to_str().unwrap(),
        queries_path.to_str().unwrap(),
        out_path.to_str().unwrap(),
        threads,
        piece,
    )
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

/// More workers than documents, and pieces so small that boundaries land
/// on every line (and some pieces are empty): the merged answers must not
/// change.
#[test]
fn pieces_and_workers_do_not_change_the_answers() {
    for threads in [2, 3, 12] {
        for piece in [1, 7, 40, 64, 100, 1 << 20] {
            assert_eq!(
                run_pieces(WORKED_DOCS, WORKED_QUERIES, threads, piece),
                WORKED_RESULTS,
                "{threads} {piece}"
            );
        }
    }
}

/// A piece never holds more than `MAX_DOCS` documents (doc numbers are
/// `u16`): 70,000 tiny documents in one piece are split into several.
#[test]
fn a_piece_is_split_at_the_document_cap() {
    let n = MAX_DOCS + 4_000;
    let docs: String = (0..n)
        .map(|i| {
            format!(
                "{i}	{}
",
                if i % 3 == 0 { "a b" } else { "c a" }
            )
        })
        .collect();
    let sum_a: u64 = (0..n as u64).sum();
    let sum_c: u64 = (0..n as u64).filter(|i| i % 3 != 0).sum();
    let want = format!(
        "{n} {sum_a} 0 1 2 3 4 5 6 7 8 9
{} {sum_c} 1 2 4 5 7 8 10 11 13 14
",
        n - n.div_ceil(3)
    );
    let got = run_pieces(
        &docs,
        "BOOL a
PHRASE \"c a\"
",
        3,
        1 << 30,
    );
    assert_eq!(got, want);
}

/// The 64-byte mask and the token scanner against a plain byte loop, over
/// every byte value and text that puts tokens across 64-byte chunk edges.
#[test]
fn scanner_matches_a_plain_tokenizer() {
    let is_tok = |b: u8| b.is_ascii_alphanumeric();
    let mut text: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    for len in [0usize, 1, 63, 64, 65, 127, 128, 129, 300] {
        for shift in 0..3 {
            let t: Vec<u8> = text[shift..shift + len]
                .iter()
                .map(|&b| if b % 3 == 0 { b'x' } else { b })
                .collect();
            let mut want = Vec::new();
            let mut i = 0;
            while i < t.len() {
                if is_tok(t[i]) {
                    let j = (i..t.len()).find(|&j| !is_tok(t[j])).unwrap_or(t.len());
                    want.push((i, j));
                    i = j;
                } else {
                    i += 1;
                }
            }
            let mut got = Vec::new();
            scan_tokens(&t, |s, e| {
                got.push((s, e));
                true
            });
            assert_eq!(got, want, "len {len} shift {shift}");
        }
    }
    text.fill(b'a'); // one token spanning many chunks
    let mut got = Vec::new();
    scan_tokens(&text, |s, e| {
        got.push((s, e));
        true
    });
    assert_eq!(got, vec![(0, 1000)]);
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

/// Random documents and queries against a brute-force evaluator, at piece
/// sizes that make a term's list a bitmap in some pieces and a sorted list in
/// others, so every list/bitmap combination of the BOOL operators runs.
#[test]
fn random_queries_match_a_brute_force_evaluator() {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let vocab: Vec<String> = (0..24).map(|i| format!("w{i}")).collect();
    // Skewed: low ranks are common, high ranks rare.
    let mut word = |r: &mut dyn FnMut(u64) -> u64| &vocab[(r(24) * r(24) / 24) as usize];
    let docs: Vec<Vec<String>> = (0..500)
        .map(|_| (0..rnd(30)).map(|_| word(&mut rnd).clone()).collect())
        .collect();
    let text: String = docs
        .iter()
        .enumerate()
        .map(|(i, d)| format!("{i}\t{}\n", d.join(" ")))
        .collect();
    let mut queries = String::new();
    let mut want = String::new();
    for _ in 0..600 {
        let k = 1 + rnd(5) as usize;
        let terms: Vec<String> = (0..k).map(|_| word(&mut rnd).clone()).collect();
        let hits: Vec<usize> = if rnd(3) == 0 && k > 1 {
            queries += &format!("PHRASE \"{}\"\n", terms.join(" "));
            let n = terms.len();
            (0..docs.len())
                .filter(|&i| docs[i].windows(n).any(|w| w == &terms[..]))
                .collect()
        } else {
            let ops: Vec<u64> = (1..k).map(|_| rnd(3)).collect();
            queries += &format!("BOOL {}", terms[0]);
            for (op, t) in ops.iter().zip(&terms[1..]) {
                queries += &format!(" {} {t}", ["AND", "OR", "NOT"][*op as usize]);
            }
            queries += "\n";
            let has = |t: &String, i: usize| docs[i].contains(t);
            (0..docs.len())
                .filter(|&i| {
                    ops.iter()
                        .zip(&terms[1..])
                        .fold(has(&terms[0], i), |acc, (op, t)| match op {
                            0 => acc && has(t, i),
                            1 => acc || has(t, i),
                            _ => acc && !has(t, i),
                        })
                })
                .collect()
        };
        want += &format!("{} {}", hits.len(), hits.iter().sum::<usize>());
        for h in hits.iter().take(10) {
            want += &format!(" {h}");
        }
        want += "\n";
    }
    for (threads, piece) in [(1, 1 << 20), (3, 700), (4, 3000), (12, 9000)] {
        assert_eq!(
            run_pieces(&text, &queries, threads, piece),
            want,
            "{threads} {piece}"
        );
    }
}
