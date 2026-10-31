//! Unit tests for track-d/src/main.rs. They live outside the track folder and are
//! compiled into the binary's test build only (`#[path]` in main.rs).

use super::*;

/// The forward and reverse graph of `edges` and the renaming, built the way `run` builds them.
fn graphs(n: u32, edges: &[Edge], threads: usize) -> (Csr, Csr, Vec<u32>) {
    let lines: String = edges.iter().map(|(u, v, w)| format!("{u} {v} {w}
")).collect();
    let (_, fwd, rev, new_id) = load_graph(format!("{n} {}
{lines}", edges.len()).as_bytes(), threads, |_| ());
    (fwd, rev, new_id)
}

type Edge = (u32, u32, u32);

/// The bidirectional search against a plain O(n^2) Dijkstra on random
/// graphs with every awkward feature the spec lists: weight 0, weights
/// up to 2^32 - 1 (so sums pass 2^32), parallel edges, duplicate
/// lines, self-loops, edgeless nodes, one-way edges and unreachable
/// pairs. Checks all (s, t) pairs, with one reused `Scratch` so stale
/// epoch state would show up too. This is what guards the choice of
/// which side to expand: correctness must not depend on it.
#[test]
fn bidirectional_matches_plain_dijkstra_on_random_graphs() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    for _ in 0..300 {
        let n = 2 + next(24) as u32;
        let m = next(4 * n as u64 + 1) as usize;
        let mut edges: Vec<(u32, u32, u32)> = (0..m)
            .map(|_| {
                let w = match next(5) {
                    0 => 0,
                    1 => u32::MAX - next(3) as u32,
                    _ => 1 + next(20) as u32,
                };
                (next(n as u64) as u32, next(n as u64) as u32, w)
            })
            .collect();
        if let Some(&e) = edges.first() {
            edges.push(e); // a duplicate line
        }

        let mut dist = vec![vec![u64::MAX; n as usize]; n as usize];
        for (s, d) in dist.iter_mut().enumerate() {
            d[s] = 0;
            let mut done = vec![false; n as usize];
            for _ in 0..n {
                let Some(u) = (0..n as usize).filter(|&u| !done[u] && d[u] != u64::MAX).min_by_key(|&u| d[u]) else {
                    break;
                };
                done[u] = true;
                for &(a, b, w) in &edges {
                    if a as usize == u && d[u] + (w as u64) < d[b as usize] {
                        d[b as usize] = d[u] + w as u64;
                    }
                }
            }
        }

        let (fwd, rev, id) = graphs(n, &edges, 1 + next(5) as usize);
        let mut scratch = Scratch::new(n as usize);
        for s in 0..n {
            for t in 0..n {
                let want = (dist[s as usize][t as usize] != u64::MAX).then_some(dist[s as usize][t as usize]);
                let got = bidirectional_dijkstra(&fwd, &rev, id[s as usize], id[t as usize], &mut scratch);
                assert_eq!(got, want, "s={s} t={t} edges={edges:?}");
            }
        }
    }
}

#[test]
fn radix_heap_pops_in_ascending_dist_order() {
    // Duplicate keys, out-of-order pushes and a key of 2^40 (a high bucket).
    let dists = [50u64, 3, 3, 0, 100, 7, 42, 1, 1, 99, 8, 2, 0, 63, 17, 1 << 40];
    let mut heap = RadixHeap::new();
    for (i, &d) in dists.iter().enumerate() {
        heap.push(State { dist: d, node: i as u32 });
    }
    let mut sorted = dists.to_vec();
    sorted.sort_unstable();
    let mut popped = Vec::new();
    while let Some(s) = heap.pop() {
        popped.push(s.dist);
    }
    assert_eq!(popped, sorted);
    assert_eq!(heap.len(), 0);
}

#[test]
fn radix_heap_interleaved_push_pop_stays_ordered() {
    // Pushes never go below the last pop, as in Dijkstra.
    let mut heap = RadixHeap::new();
    for d in [10u64, 5, 20, 1] {
        heap.push(State { dist: d, node: 0 });
    }
    assert_eq!(heap.pop().map(|s| s.dist), Some(1));
    heap.push(State { dist: 2, node: 0 });
    assert_eq!(heap.min(), Some(2));
    assert_eq!(heap.pop().map(|s| s.dist), Some(2));
    assert_eq!(heap.pop().map(|s| s.dist), Some(5));
    heap.push(State { dist: 5, node: 0 });
    assert_eq!(heap.pop().map(|s| s.dist), Some(5));
    assert_eq!(heap.pop().map(|s| s.dist), Some(10));
    assert_eq!(heap.pop().map(|s| s.dist), Some(20));
    assert_eq!(heap.pop(), None);
    heap.push(State { dist: 7, node: 0 });
    heap.clear();
    assert_eq!(heap.min(), None);
}

#[test]
fn parses_header_and_edges() {
    let data = b"4 3
0 1 10
1 2 20
2 3 5
";
    let (n, parts) = split_graph(data, 1);
    assert_eq!(n, 4);
    let all: Vec<_> = parts.iter().flat_map(|p| parse_edges(p)).collect();
    assert_eq!(all, vec![(0, 1, 10), (1, 2, 20), (2, 3, 5)]);
    let (_, split) = split_graph(data, 3);
    assert_eq!(split.iter().flat_map(|p| parse_edges(p)).collect::<Vec<_>>(), all);
}

#[test]
fn csr_rows_hold_the_edges_of_each_node() {
    let (fwd, rev, id) = graphs(4, &[(0, 1, 10), (0, 1, 3), (2, 0, 7), (3, 3, 1)], 2);
    let row = |g: &Csr, u: u32| {
        let mut r: Vec<_> = g.row(id[u as usize]).iter().map(|&(v, w)| (id.iter().position(|&x| x == v).unwrap() as u32, w)).collect();
        r.sort();
        r
    };
    assert_eq!(row(&fwd, 0), vec![(1, 3), (1, 10)]);
    assert_eq!(row(&rev, 0), vec![(2, 7)]);
    assert_eq!(row(&rev, 1), vec![(0, 3), (0, 10)]);
    assert_eq!(row(&fwd, 3), vec![(3, 1)]);
    assert!(row(&fwd, 1).is_empty());
}

#[test]
fn shortest_path_matches_known_graph() {
    let (fwd, rev, id) = graphs(4, &[(0, 1, 10), (1, 2, 20), (2, 3, 5), (0, 3, 100)], 2);
    let mut scratch = Scratch::new(4);
    let mut ask = |s: usize, t: usize| bidirectional_dijkstra(&fwd, &rev, id[s], id[t], &mut scratch);
    assert_eq!(ask(0, 3), Some(35));
    assert_eq!(ask(0, 0), Some(0));
    assert_eq!(ask(3, 0), None);
    // The scratch is reused: repeats and overlapping node sets must not leak stale labels.
    assert_eq!(ask(0, 3), Some(35));
    assert_eq!(ask(1, 2), Some(20));
    assert_eq!(ask(0, 3), Some(35));
    assert_eq!(ask(2, 1), None);
}

#[test]
fn parses_query_pairs() {
    let data = b"0 3\n1740511 999\n5 5\n";
    let queries = parse_queries(data);
    assert_eq!(queries, vec![(0, 3), (1_740_511, 999), (5, 5)]);
}

/// specs/d-traverse.md section 4's worked example, byte for byte: a
/// duplicate edge line, parallel edges with the lighter listed second,
/// a weight-0 path, a self-loop, a node on no edge line, sums above
/// 2^32 - 1, and unreachable pairs both ways.
#[test]
fn matches_the_spec_worked_example_exactly() {
    let graph = b"8 12\n0 1 4\n1 2 3\n0 2 9\n2 0 5\n2 1 6\n1 3 0\n3 4 0\n4 4 7\n\
4 5 4294967295\n2 1 2\n5 6 4294967295\n0 1 4\n";
    let queries = b"0 2\n2 1\n1 4\n4 4\n7 7\n0 6\n4 5\n6 0\n0 7\n1 0\n2 1\n";
    let expected = "7\n2\n0\n0\n0\n8589934594\n4294967295\n-1\n-1\n8\n2\n";

    let dir = std::env::temp_dir().join(format!("track-d-spec-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let graph_path = dir.join("graph.txt");
    let queries_path = dir.join("queries.txt");
    let out_path = dir.join("distances.txt");
    std::fs::write(&graph_path, graph).unwrap();
    std::fs::write(&queries_path, queries).unwrap();

    for threads in [1, 4, 12] {
        run(graph_path.to_str().unwrap(), queries_path.to_str().unwrap(), out_path.to_str().unwrap(), threads).unwrap();
        assert_eq!(std::fs::read_to_string(&out_path).unwrap(), expected, "threads={threads}");
    }

    let output = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(output, expected);
    assert_eq!(output.len(), 42); // the spec states "exactly these 42 bytes"

    let _ = std::fs::remove_dir_all(&dir);
}

/// The locality order is a permutation of all nodes, also when several regions and
/// unreachable pieces are involved, and distances do not change under it.
#[test]
fn region_order_names_every_node_once() {
    let n = 5000u32;
    let edges: Vec<Edge> = (0..n - 1).filter(|u| u % 997 != 0).flat_map(|u| [(u, u + 1, 3), (u + 1, u, 4)]).collect();
    let (fwd, rev, id) = graphs(n, &edges, 4);
    let mut seen = id.clone();
    seen.sort_unstable();
    assert!(seen.iter().enumerate().all(|(i, &x)| i as u32 == x));
    let mut scratch = Scratch::new(n as usize);
    assert_eq!(bidirectional_dijkstra(&fwd, &rev, id[1], id[900], &mut scratch), Some(3 * 899));
    assert_eq!(bidirectional_dijkstra(&fwd, &rev, id[900], id[1], &mut scratch), Some(4 * 899));
    assert_eq!(bidirectional_dijkstra(&fwd, &rev, id[900], id[1100], &mut scratch), None);
}
