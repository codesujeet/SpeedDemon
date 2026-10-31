//! Unit tests for track-a/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

use super::*;

/// The input followed by `PAD` zero bytes, as `process` requires.
fn padded(data: &[u8]) -> &'static [u8] {
    let mut v = data.to_vec();
    v.resize(data.len() + PAD, 0);
    &Box::leak(v.into_boxed_slice())[..data.len()]
}

/// Runs the scan and the output phase on `data` and returns the output bytes.
fn summarize(data: &[u8], threads: usize, chunk: usize) -> Vec<u8> {
    let mut out = Vec::new();
    process(padded(data), threads, chunk, &mut out).unwrap();
    out
}

/// A plain, slow implementation of the spec, for comparison.
fn reference(data: &[u8]) -> Vec<u8> {
    let mut groups: BTreeMap<(String, String), (Vec<u64>, u64)> = BTreeMap::new();
    for line in std::str::from_utf8(data).unwrap().lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let g = groups.entry((f[0][..13].to_string(), f[2].to_string())).or_default();
        g.0.push(f[4].parse().unwrap());
        g.1 += u64::from(f[3].starts_with('5'));
    }
    let mut out = String::new();
    for ((hour, endpoint), (mut lat, errors)) in groups {
        lat.sort_unstable();
        let n = lat.len() as u64;
        let rank = |p: u64| lat[((p * n + 99) / 100 - 1) as usize];
        out += &format!(
            "{hour}:00:00Z {endpoint} {n} {} {} {} {errors} {}\n",
            rank(50),
            rank(95),
            rank(99),
            errors * 1_000_000 / n
        );
    }
    out.into_bytes()
}

#[test]
fn parse_latency_reads_one_to_ten_digits() {
    for v in [0u64, 7, 10, 99, 123, 4096, 99_999, 1_000_000, 12_345_678, 99_999_999, 100_000_000, 987_654_321, 4_294_967_295] {
        let line = format!("2026-10-30T18:00:00.000000Z GET /x 200 {v}\n");
        let s = line.find("200 ").unwrap() + 4;
        // SAFETY: the digits are inside `line`, which is longer than eight bytes before them.
        assert_eq!(unsafe { parse_latency(line.as_ptr(), s, line.len() - 1) }, v as u32);
    }
}

#[test]
fn percentiles_match_a_full_sort() {
    for n in [1usize, 2, 3, 4, 5, 20, 21, 99, 100, 101, 1000] {
        let mut data: Vec<u32> = (0..n as u32).map(|i| i.wrapping_mul(2_654_435_761) % 50).collect();
        let mut sorted = data.clone();
        sorted.sort_unstable();
        let rank = |p: usize| sorted[(p * n).div_ceil(100) - 1];
        assert_eq!(percentiles(&mut data), [rank(50), rank(95), rank(99)], "n={n}");
    }
    // Spec section 3.4: with n = 1 all three equal the single value; with n = 101 the ranks are 51, 96 and 100.
    assert_eq!(percentiles(&mut [42]), [42, 42, 42]);
    assert_eq!(percentiles(&mut (1..=101).rev().collect::<Vec<u32>>()), [51, 96, 100]);
}

#[test]
fn any_thread_count_and_morsel_size_gives_the_reference_output() {
    // Many hours (also out of order), endpoints of 1 to 125 bytes, latencies of 1 to 10
    // digits, and a file long enough to have a morsel boundary in every other line.
    let mut data = Vec::new();
    let mut x = 12345u64;
    for i in 0..6000u64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let hour = (x >> 40) % 30;
        let len = 1 + (x >> 20) as usize % 125;
        let endpoint = format!("/{}", "e".repeat(len - 1));
        let endpoint = if i % 9 == 0 { "/".to_string() } else { endpoint };
        let latency = if i % 7 == 0 { 4_294_967_295 } else { (x >> 33) % 10u64.pow(1 + (i % 10) as u32) };
        let method = ["GET", "OPTIONS", "DELETE"][(i % 3) as usize];
        let status = [200, 404, 499, 500, 599][(x >> 10) as usize % 5];
        data.extend_from_slice(
            format!("2026-11-01T{:02}:07:18.569411Z {method} {endpoint} {status} {latency}\n", hour % 24).as_bytes(),
        );
    }
    let expected = reference(&data);
    for (threads, morsel) in [(1, 1 << 20), (4, 1000), (3, 97), (8, 4096), (2, 16)] {
        assert_eq!(summarize(&data, threads, morsel), expected, "threads={threads} morsel={morsel}");
    }
}

#[test]
fn a_full_catalogue_of_similar_endpoints() {
    // 2,000 names that share their first and last eight bytes, so their table slots collide, with lengths from 25 to 125.
    let mut data = Vec::new();
    for i in 0..30_000u32 {
        let (k, pad) = (i % 2000, "p".repeat(i as usize % 2000 % 100));
        data.extend_from_slice(format!("2026-10-30T18:00:00.000000Z PUT /endpoint/{pad}/{k}/last/x {} {i}\n", 200 + i % 400).as_bytes());
    }
    assert_eq!(summarize(&data, 3, 1 << 14), reference(&data));
}

#[test]
fn files_shorter_than_one_window_work() {
    for n in 1..4 {
        let data: Vec<u8> = (0..n).flat_map(|i| format!("2026-10-30T18:00:00.000000Z GET /a{i} 200 {i}\n").into_bytes()).collect();
        assert_eq!(summarize(&data, 4, 1 << 20), reference(&data));
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
