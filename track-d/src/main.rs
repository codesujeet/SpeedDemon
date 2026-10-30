//! Track D â€” Traverse. Loads a weighted directed graph (`graph.txt`:
//! header "n m", then m lines "u v w") into a CSR adjacency layout, then
//! answers point-to-point shortest-path queries (`queries.txt`: "source
//! target" per line) via bidirectional Dijkstra, in input order.
//!
//! Output format/filenames are a best-effort guess made before the real
//! spec drops at freeze â€” see README.md. The parse/CSR/search engine
//! below is the real work, from `prep/graph-csr`
//! (research/track-d-traverse.md): a comparison-based priority queue
//! (not Dial's algorithm/bucket queue â€” measured statistically tied
//! with a plain binary heap once `-C target-cpu=x86-64-v3` is active),
//! implemented as a 4-ary `DHeap` rather than `std::collections::
//! BinaryHeap` for better `sift_down` cache locality â€” see
//! `track-d/PERF.md`'s "round 3" for the measured ~10-12% win.

use std::collections::HashMap;
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

const HEAP_ARITY: usize = 4;

type Edge = (u32, u32, u32);

/// Parses every `u v w` triple in `data` (a run of whole lines).
fn parse_edges(data: &[u8], capacity: usize) -> Vec<Edge> {
    let mut edges = Vec::with_capacity(capacity);
    let mut triple: [u32; 3] = [0, 0, 0];
    let mut triple_idx = 0usize;
    let mut cur: u32 = 0;
    let mut in_num = false;
    for &b in data {
        if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u32;
            in_num = true;
        } else if in_num {
            triple[triple_idx] = cur;
            triple_idx += 1;
            cur = 0;
            in_num = false;
            if triple_idx == 3 {
                edges.push((triple[0], triple[1], triple[2]));
                triple_idx = 0;
            }
        }
    }
    if in_num && triple_idx == 2 {
        edges.push((triple[0], triple[1], cur));
    }
    edges
}

/// Reads the "n m" header, then parses the edge lines in `threads`
/// line-aligned slices at once; the result keeps one `Vec` per slice.
fn parse_graph(data: &[u8], threads: usize) -> (u32, Vec<Vec<Edge>>) {
    let mut nums = [0usize; 2];
    let mut idx = 0;
    let mut i = 0;
    let mut in_num = false;
    while i < data.len() && idx < 2 {
        let b = data[i];
        if b.is_ascii_digit() {
            nums[idx] = nums[idx] * 10 + (b - b'0') as usize;
            in_num = true;
        } else if in_num {
            idx += 1;
            in_num = false;
        }
        i += 1;
    }
    let body = &data[i..];
    let mut cuts = vec![0];
    for t in 1..threads {
        let mut p = (body.len() / threads * t).max(cuts[t - 1]);
        while p > 0 && p < body.len() && body[p - 1] != b'\n' {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(body.len());
    let capacity = nums[1] / threads * 9 / 8 + 16;
    let parts = std::thread::scope(|s| {
        let handles: Vec<_> = cuts
            .windows(2)
            .map(|w| {
                let slice = &body[w[0]..w[1]];
                s.spawn(move || parse_edges(slice, capacity))
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("parse worker panicked")).collect()
    });
    (nums[0] as u32, parts)
}

struct Csr {
    row_offset: Vec<u32>,
    /// `(neighbour, weight)` of every edge, grouped by node.
    adj: Vec<(u32, u32)>,
}

/// Builds the adjacency of every node, keyed by tail (or by head when
/// `reverse`). Each worker owns a contiguous range of nodes: it scans all
/// the edges once to count its nodes' degrees, then once more to place
/// them into its own disjoint slice of `col`/`weight`, so there is no
/// sharing and no atomics. Edges keep their input order within a node.
fn build_csr(node_count: u32, parts: &[Vec<Edge>], reverse: bool, threads: usize) -> Csr {
    let n = node_count as usize;
    let total: usize = parts.iter().map(Vec::len).sum();
    let workers = threads.clamp(1, n.max(1));
    let bounds: Vec<usize> = (0..=workers).map(|w| (n * w).div_ceil(workers)).collect();
    let split = move |&(u, v, w): &Edge| if reverse { (v as usize, u, w) } else { (u as usize, v, w) };

    // Every part files its edges under the worker that owns their key, so
    // a worker scans its own 1/workers of the edges instead of all of them.
    let buckets: Vec<Vec<Vec<Edge>>> = std::thread::scope(|s| {
        let handles: Vec<_> = parts
            .iter()
            .map(|part| {
                s.spawn(move || {
                    let mut mine = vec![Vec::new(); workers];
                    for e in part {
                        mine[split(e).0 * workers / n].push(*e);
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("csr worker panicked")).collect()
    });
    let buckets = &buckets;

    let counts: Vec<Vec<u32>> = std::thread::scope(|s| {
        let handles: Vec<_> = bounds
            .windows(2)
            .enumerate()
            .map(|(w, b)| {
                let (lo, width) = (b[0], b[1] - b[0]);
                s.spawn(move || {
                    let mut degree = vec![0u32; width];
                    for e in buckets.iter().flat_map(|mine| &mine[w]) {
                        degree[split(e).0 - lo] += 1;
                    }
                    degree
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("csr worker panicked")).collect()
    });
    let mut row_offset = vec![0u32; n + 1];
    for (b, degree) in bounds.windows(2).zip(&counts) {
        for (i, &d) in degree.iter().enumerate() {
            row_offset[b[0] + i + 1] = row_offset[b[0] + i] + d;
        }
    }

    let mut adj = vec![(0u32, 0u32); total];
    std::thread::scope(|s| {
        let mut rest = &mut adj[..];
        for (w, b) in bounds.windows(2).enumerate() {
            let (lo, hi) = (b[0], b[1]);
            let base = row_offset[lo];
            let len = (row_offset[hi] - base) as usize;
            let (mine, tail) = rest.split_at_mut(len);
            rest = tail;
            let mut cursor: Vec<u32> = row_offset[lo..hi].iter().map(|&o| o - base).collect();
            s.spawn(move || {
                for e in buckets.iter().flat_map(|mine| &mine[w]) {
                    let (key, target, weight) = split(e);
                    let pos = cursor[key - lo] as usize;
                    mine[pos] = (target, weight);
                    cursor[key - lo] += 1;
                }
            });
        }
    });
    Csr { row_offset, adj }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct State {
    dist: u64,
    node: u32,
}

/// A 4-ary (d-ary) min-heap, used in place of `std::collections::
/// BinaryHeap` (which is always binary/2-ary). Dijkstra's `sift_down`
/// after every `pop` compares a node against its children; a 2-ary
/// heap's two children for index `i` live at `2i+1`/`2i+2`, spread
/// across two different 64-byte cache lines for most `i`, while a 4-ary
/// heap's four children at `4i+1..4i+5` land in the same or an adjacent
/// cache line far more often â€” fewer L1 misses per `sift_down`, at the
/// cost of up to 3 comparisons instead of 1 to find the smallest child
/// (a cheap, branch-predictable trade against a cache miss). Tried only
/// after the allocator fix (`Scratch`, above) removed the dominant cost
/// â€” heap operations are the actual remaining bottleneck now, unlike
/// when this track was allocator-bound and no heap variant would have
/// shown a difference. See `track-d/PERF.md` for the measured result.
struct DHeap {
    data: Vec<State>,
}
impl DHeap {
    fn new() -> Self {
        DHeap { data: Vec::new() }
    }
    fn clear(&mut self) {
        self.data.clear();
    }
    fn peek(&self) -> Option<&State> {
        self.data.first()
    }
    fn len(&self) -> usize {
        self.data.len()
    }
    fn push(&mut self, item: State) {
        self.data.push(item);
        let mut i = self.data.len() - 1;
        while i > 0 {
            let parent = (i - 1) / HEAP_ARITY;
            if self.data[i].dist < self.data[parent].dist {
                self.data.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
    }
    fn pop(&mut self) -> Option<State> {
        if self.data.is_empty() {
            return None;
        }
        let last = self.data.len() - 1;
        self.data.swap(0, last);
        let popped = self.data.pop();
        let n = self.data.len();
        let mut i = 0;
        loop {
            let first_child = i * HEAP_ARITY + 1;
            if first_child >= n {
                break;
            }
            let last_child = (first_child + HEAP_ARITY).min(n);
            let mut min_child = first_child;
            for c in (first_child + 1)..last_child {
                if self.data[c].dist < self.data[min_child].dist {
                    min_child = c;
                }
            }
            if self.data[min_child].dist < self.data[i].dist {
                self.data.swap(i, min_child);
                i = min_child;
            } else {
                break;
            }
        }
        popped
    }
}

/// Per-worker search state, reused by every query so nothing is allocated
/// or zeroed per query. `label[v]` holds node `v`'s tentative distance
/// from the source (`[0]`) and to the target (`[1]`), each stored as
/// distance + 1 so that 0 means "not reached": a fresh calloc'd array is
/// valid as is, with no initialisation pass. Both directions of a node share
/// a cache line, which is what a relaxation reads (its own side, then the
/// other side for `best`). Almost every distance fits 32 bits, which halves
/// the array that every worker keeps; the few that do not (paths through
/// the edges of weight near 2^32) are marked `u32::MAX` and held exactly in
/// `big`. `touched` lists the nodes to clear before the next query.
struct Scratch {
    label: Vec<[u32; 2]>,
    big: HashMap<(u32, usize), u64>,
    touched: Vec<u32>,
    heaps: [DHeap; 2],
}
impl Scratch {
    fn new(n: usize) -> Self {
        Scratch { label: vec![[0; 2]; n], big: HashMap::new(), touched: Vec::new(), heaps: [DHeap::new(), DHeap::new()] }
    }

    fn reset(&mut self) {
        // A wide search is cleared by one sequential sweep, a narrow one node by node.
        if self.touched.len() > self.label.len() / 16 {
            self.label.fill([0; 2]);
        } else {
            for &v in &self.touched {
                self.label[v as usize] = [0; 2];
            }
        }
        self.big.clear();
        self.touched.clear();
        self.heaps.iter_mut().for_each(DHeap::clear);
    }

    /// Distance label of `v` on `side`, or `u64::MAX` if unreached.
    #[inline]
    fn get(&self, side: usize, v: u32) -> u64 {
        match self.label[v as usize][side] {
            0 => u64::MAX,
            u32::MAX => self.big[&(v, side)],
            label => label as u64 - 1,
        }
    }

    #[inline]
    fn set(&mut self, side: usize, v: u32, dist: u64) {
        let label = &mut self.label[v as usize];
        if label[0] | label[1] == 0 {
            self.touched.push(v);
        }
        if dist < u32::MAX as u64 - 1 {
            label[side] = dist as u32 + 1;
        } else {
            label[side] = u32::MAX;
            self.big.insert((v, side), dist);
        }
    }
}

/// Bidirectional Dijkstra over a reused `Scratch`: side 0 searches `fwd`
/// from the source, side 1 searches `rev` from the target, always
/// growing the side with the smaller queue. Any alternation is correct
/// under the `tf + tb >= best` stop rule; balancing by key instead lets a
/// source whose first edges weigh ~2^32 (a node with no in-edges) starve
/// its own side while the other settles the whole graph. Once either heap
/// is empty that side has settled everything it can reach and every
/// relaxation into a node the other side labelled already updated `best`,
/// so the loop ends: `best` is final, or `u64::MAX` for no path.
fn bidirectional_dijkstra(fwd: &Csr, rev: &Csr, source: u32, target: u32, s: &mut Scratch) -> Option<u64> {
    if source == target {
        return Some(0);
    }
    s.reset();
    let graphs = [fwd, rev];
    for (side, node) in [source, target].into_iter().enumerate() {
        s.set(side, node, 0);
        s.heaps[side].push(State { dist: 0, node });
    }

    let mut best = u64::MAX;
    while let (Some(tf), Some(tb)) = (s.heaps[0].peek().map(|h| h.dist), s.heaps[1].peek().map(|h| h.dist)) {
        if tf.saturating_add(tb) >= best {
            break;
        }
        let side = usize::from(s.heaps[0].len() > s.heaps[1].len());
        let State { dist, node } = s.heaps[side].pop().unwrap();
        if dist > s.get(side, node) {
            continue;
        }
        let g = graphs[side];
        for &(v, w) in &g.adj[g.row_offset[node as usize] as usize..g.row_offset[node as usize + 1] as usize] {
            let nd = dist + w as u64;
            if nd >= best {
                continue;
            }
            if nd < s.get(side, v) {
                s.set(side, v, nd);
                s.heaps[side].push(State { dist: nd, node: v });
            }
            let other = s.get(1 - side, v);
            if other != u64::MAX {
                best = best.min(nd + other);
            }
        }
    }
    (best != u64::MAX).then_some(best)
}

fn parse_queries(data: &[u8]) -> Vec<(u32, u32)> {
    let mut queries = Vec::new();
    let mut nums: [u32; 2] = [0, 0];
    let mut idx = 0usize;
    let mut cur = 0u32;
    let mut in_num = false;
    for &b in data {
        if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u32;
            in_num = true;
        } else if in_num {
            nums[idx] = cur;
            idx += 1;
            cur = 0;
            in_num = false;
            if idx == 2 {
                queries.push((nums[0], nums[1]));
                idx = 0;
            }
        }
    }
    if in_num && idx == 1 {
        queries.push((nums[0], cur));
    }
    queries
}

/// A new id for every node, in BFS order over out-edges: `new_id[old]`.
/// The spec says node ids are a random relabelling, so with the original
/// ids every relaxation lands on a random cache line. The main body is a
/// road-like lattice, and BFS order puts each node's neighbours a few
/// ids away, so a search's working set is a compact run of the CSR and
/// distance arrays instead of scattered pages. Nodes BFS from node 0
/// can't reach (no in-edges, islands) start their own BFS in id order.
/// Distances don't depend on labels, so answers are unchanged.
fn bfs_order(g: &Csr) -> (Vec<u32>, Vec<u32>) {
    let n = g.row_offset.len() - 1;
    let mut new_id = vec![u32::MAX; n];
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut head = 0;
    for root in 0..n {
        if new_id[root] != u32::MAX {
            continue;
        }
        new_id[root] = order.len() as u32;
        order.push(root as u32);
        while head < order.len() {
            let u = order[head] as usize;
            head += 1;
            for &(v, _) in &g.adj[g.row_offset[u] as usize..g.row_offset[u + 1] as usize] {
                if new_id[v as usize] == u32::MAX {
                    new_id[v as usize] = order.len() as u32;
                    order.push(v);
                }
            }
        }
    }
    (new_id, order)
}

/// Renames the nodes of `g` so node `order[i]` becomes `i`, mapping every
/// neighbour through `new_id`. Cheaper than rebuilding from the edge list:
/// one pass over the rows instead of two passes over all edges per worker.
/// Each worker fills the disjoint slice of its own run of new rows.
fn permute_csr(g: &Csr, order: &[u32], new_id: &[u32], threads: usize) -> Csr {
    let n = order.len();
    let mut row_offset = vec![0u32; n + 1];
    for (i, &old) in order.iter().enumerate() {
        let old = old as usize;
        row_offset[i + 1] = row_offset[i] + g.row_offset[old + 1] - g.row_offset[old];
    }
    let mut adj = vec![(0u32, 0u32); g.adj.len()];
    let workers = threads.clamp(1, n.max(1));
    std::thread::scope(|s| {
        let mut rest = &mut adj[..];
        for w in 0..workers {
            let (lo, hi) = (n * w / workers, n * (w + 1) / workers);
            let (mine, tail) = rest.split_at_mut((row_offset[hi] - row_offset[lo]) as usize);
            rest = tail;
            s.spawn(move || {
                let mut at = 0;
                for &old in &order[lo..hi] {
                    let old = old as usize;
                    for &(v, w) in &g.adj[g.row_offset[old] as usize..g.row_offset[old + 1] as usize] {
                        mine[at] = (new_id[v as usize], w);
                        at += 1;
                    }
                }
            });
        }
    });
    Csr { row_offset, adj }
}

/// Answers every query, in parallel: queries are independent, and the
/// spec allows answering them in any order as long as the output keeps
/// input order. Each worker owns one `Scratch` and pulls the next query
/// index from a shared counter, so a few expensive long-tail queries
/// can't strand the other workers behind a static split.
fn answer_queries(fwd: &Csr, rev: &Csr, queries: &[(u32, u32)], n: usize, threads: usize) -> Vec<Option<u64>> {
    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, Option<u64>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.clamp(1, queries.len().max(1)))
            .map(|_| {
                s.spawn(|| {
                    let mut scratch = Scratch::new(n);
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(src, dst)) = queries.get(i) else { break };
                        mine.push((i, bidirectional_dijkstra(fwd, rev, src, dst, &mut scratch)));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("query worker panicked")).collect()
    });
    let mut answers = vec![None; queries.len()];
    for (i, a) in parts.into_iter().flatten() {
        answers[i] = a;
    }
    answers
}

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

/// Set `TRACK_D_TIMING=1` to print a per-phase breakdown to stderr.
fn run(graph_path: &str, queries_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_D_TIMING").is_ok();
    macro_rules! phase {
        ($label:expr, $body:expr) => {{
            let t = std::time::Instant::now();
            let v = $body;
            if verbose {
                eprintln!("track-d: {} in {:.1}ms", $label, t.elapsed().as_secs_f64() * 1000.0);
            }
            v
        }};
    }

    let graph_data = phase!("map graph", map_file(graph_path)?);
    let (node_count, edges) = phase!("parse graph", parse_graph(graph_data, threads));
    let original = phase!("build CSR original", build_csr(node_count, &edges, false, threads));
    // The BFS is serial; the other cores build the reverse adjacency meanwhile.
    let (rev_original, (new_id, order)) = phase!("bfs reorder + CSR rev", {
        std::thread::scope(|s| {
            let rev = s.spawn(|| build_csr(node_count, &edges, true, threads.saturating_sub(1).max(1)));
            let ids = bfs_order(&original);
            (rev.join().expect("csr worker panicked"), ids)
        })
    });
    // Both CSRs now hold every edge; freeing the list keeps it out of the peak.
    drop(edges);
    let fwd = phase!("permute fwd", permute_csr(&original, &order, &new_id, threads));
    drop(original);
    let rev = phase!("permute rev", permute_csr(&rev_original, &order, &new_id, threads));
    drop((rev_original, order));

    let queries_data = phase!("map queries", map_file(queries_path)?);
    let queries: Vec<(u32, u32)> = phase!(
        "parse queries",
        parse_queries(queries_data)
            .into_iter()
            .map(|(s, t)| (new_id[s as usize], new_id[t as usize]))
            .collect()
    );

    let query_start = std::time::Instant::now();
    let answers = answer_queries(&fwd, &rev, &queries, node_count as usize, threads);
    let mut out: Vec<u8> = Vec::with_capacity(queries.len() * 12);
    for answer in answers {
        match answer {
            Some(d) => write_u64(&mut out, d),
            None => out.extend_from_slice(b"-1"),
        }
        out.push(b'\n');
    }
    if verbose {
        eprintln!(
            "track-d: answer {} queries in {:.1}ms",
            queries.len(),
            query_start.elapsed().as_secs_f64() * 1000.0
        );
    }

    phase!("write output", std::fs::write(output_path, out))
}

fn main() {
    let graph_path = std::env::var("GRAPH_PATH").unwrap_or_else(|_| "/data/graph.txt".to_string());
    let queries_path =
        std::env::var("QUERIES_PATH").unwrap_or_else(|_| "/data/queries.txt".to_string());
    let output_path =
        std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/distances.txt".to_string());

    let start = std::time::Instant::now();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    match run(&graph_path, &queries_path, &output_path, threads) {
        Ok(()) => {
            eprintln!("track-d: done in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
        Err(e) => {
            eprintln!("track-d: failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

            let fwd = build_csr(n, &[edges.clone()], false, 3);
            let rev = build_csr(n, &[edges.clone()], true, 3);
            let mut scratch = Scratch::new(n as usize);
            for s in 0..n {
                for t in 0..n {
                    let want = (dist[s as usize][t as usize] != u64::MAX).then_some(dist[s as usize][t as usize]);
                    assert_eq!(bidirectional_dijkstra(&fwd, &rev, s, t, &mut scratch), want, "s={s} t={t} edges={edges:?}");
                }
            }
        }
    }

    #[test]
    fn dheap_pops_in_ascending_dist_order() {
        // A pseudo-random-ish sequence, including duplicate dists and
        // out-of-order pushes/pops interleaved, to exercise both
        // sift-up (push) and sift-down (pop) across more than one
        // level of the 4-ary tree (need > 4 elements for a second
        // level to exist at all).
        let dists = [50u64, 3, 3, 0, 100, 7, 42, 1, 1, 99, 8, 2, 0, 63, 17];
        let mut heap = DHeap::new();
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
    }

    #[test]
    fn dheap_interleaved_push_pop_stays_a_valid_min_heap() {
        let mut heap = DHeap::new();
        for d in [10u64, 5, 20, 1] {
            heap.push(State { dist: d, node: 0 });
        }
        assert_eq!(heap.pop().map(|s| s.dist), Some(1));
        heap.push(State { dist: 2, node: 0 });
        assert_eq!(heap.pop().map(|s| s.dist), Some(2));
        assert_eq!(heap.pop().map(|s| s.dist), Some(5));
        heap.push(State { dist: 0, node: 0 });
        assert_eq!(heap.pop().map(|s| s.dist), Some(0));
        assert_eq!(heap.pop().map(|s| s.dist), Some(10));
        assert_eq!(heap.pop().map(|s| s.dist), Some(20));
        assert_eq!(heap.pop(), None);
    }

    #[test]
    fn parses_header_and_edges() {
        let data = b"4 3\n0 1 10\n1 2 20\n2 3 5\n";
        let (n, parts) = parse_graph(data, 1);
        assert_eq!(n, 4);
        assert_eq!(parts.concat(), vec![(0, 1, 10), (1, 2, 20), (2, 3, 5)]);
        let (_, split) = parse_graph(data, 3);
        assert_eq!(split.concat(), parts.concat());
    }

    #[test]
    fn shortest_path_matches_known_graph() {
        let data = b"4 4\n0 1 10\n1 2 20\n2 3 5\n0 3 100\n";
        let (n, parts) = parse_graph(data, 2);
        let edges = parts.concat();
        let fwd = build_csr(n, &[edges.clone()], false, 3);
        let rev = build_csr(n, &[edges.clone()], true, 3);
        let mut scratch = Scratch::new(n as usize);
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 0, 3, &mut scratch), Some(35));
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 0, 0, &mut scratch), Some(0));
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 3, 0, &mut scratch), None);
        // Scratch's dist/epoch arrays are never reset between calls (the
        // epoch counter makes stale entries from a previous query
        // invisible instead) â€” run several queries in a row, including
        // repeats and ones with overlapping node sets, and confirm no
        // cross-query contamination leaks into the answer.
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 0, 3, &mut scratch), Some(35));
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 1, 2, &mut scratch), Some(20));
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 0, 3, &mut scratch), Some(35));
        assert_eq!(bidirectional_dijkstra(&fwd, &rev, 2, 1, &mut scratch), None);
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
}
