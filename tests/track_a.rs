//! Unit tests for track-a/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

use super::*;

#[derive(Default)]
struct GroupData {
    count: u64,
    errors: u64,
    latencies: Vec<u32>,
}

/// Folds the morsels into one map keyed by (hour, endpoint), for easy inspection.
fn merged<'a>(morsels: Vec<Morsel<'a>>) -> HashMap<(u64, &'a [u8]), GroupData> {
    let mut all: HashMap<(u64, &[u8]), GroupData> = HashMap::new();
    for m in &morsels {
        for g in &m.groups {
            let from = g.start as usize;
            let entry = all.entry((m.hours[g.slot as usize], m.names[g.ep as usize])).or_default();
            entry.count += g.len as u64;
            entry.errors += g.errors as u64;
            entry.latencies.extend_from_slice(&m.latencies[from..from + g.len as usize]);
        }
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
