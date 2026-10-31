//! Track D: Traverse. Loads a weighted directed graph (`graph.txt`: header
//! "n m", then m lines "u v w") into CSR adjacency arrays, renumbers the
//! nodes so that neighbours are close in memory, then answers point-to-point
//! shortest-path queries (`queries.txt`: "s t" per line) with bidirectional
//! Dijkstra, in input order. See README.md for the approach.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering::Relaxed};

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

/// Runs `f(0..parts)` on one thread each and returns the results in order.
fn par<R: Send>(parts: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = (0..parts).map(|i| s.spawn(move || f(i))).collect();
        handles.into_iter().map(|h| h.join().expect("worker panicked")).collect()
    })
}

/// A raw output array that several workers fill at positions no two of them share.
struct Disjoint<T>(*mut T);
// SAFETY: every user writes only positions it alone owns (see each call site).
unsafe impl<T: Send> Sync for Disjoint<T> {}
impl<T> Disjoint<T> {
    /// # Safety
    /// `i` must be in bounds and written by no other thread.
    unsafe fn put(&self, i: usize, v: T) {
        self.0.add(i).write(v);
    }
}

/// The `N` numbers of every line of `data`, in order (the lines hold exactly `N` unsigned decimals).
fn numbers<const N: usize>(data: &[u8]) -> impl Iterator<Item = [u32; N]> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        let mut t = [0u32; N];
        for x in &mut t {
            while i < data.len() && !data[i].is_ascii_digit() {
                i += 1;
            }
            if i == data.len() {
                return None;
            }
            while i < data.len() && data[i].is_ascii_digit() {
                *x = *x * 10 + (data[i] - b'0') as u32;
                i += 1;
            }
        }
        Some(t)
    })
}

/// `(tail, head, weight)` of every edge line in `data`.
fn parse_edges(data: &[u8]) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
    numbers::<3>(data).map(|[u, v, w]| (u, v, w))
}

/// Splits the file into the node count and `parts` line-aligned slices of edge lines.
fn split_graph(data: &[u8], parts: usize) -> (usize, Vec<&[u8]>) {
    let header_end = data.iter().position(|&b| b == b'\n').map_or(data.len(), |p| p + 1);
    let n = data[..header_end].split(|b| !b.is_ascii_digit()).find(|s| !s.is_empty());
    let n = n.map_or(0, |s| s.iter().fold(0usize, |a, &b| a * 10 + (b - b'0') as usize));
    let body = &data[header_end..];
    let mut cuts = vec![0];
    for t in 1..parts {
        let mut p = (body.len() / parts * t).max(cuts[t - 1]);
        while p > 0 && p < body.len() && body[p - 1] != b'\n' {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(body.len());
    (n, cuts.windows(2).map(|w| &body[w[0]..w[1]]).collect())
}

struct Csr {
    row_offset: Vec<u32>,
    /// `(neighbour, weight)` of every edge, grouped by node.
    adj: Vec<(u32, u32)>,
}

impl Csr {
    fn row(&self, u: u32) -> &[(u32, u32)] {
        &self.adj[self.row_offset[u as usize] as usize..self.row_offset[u as usize + 1] as usize]
    }
}

/// Builds the CSR of the `(key, other, weight)` edges that `scan(part)` yields
/// for each of `parts` parts. Every part files its edges under the worker that
/// owns their key (worker `w` owns keys `n*w/workers..n*(w+1)/workers`); then
/// each worker counts and places only its own edges, into its own slice of the
/// adjacency array, so no counter or slot is shared and nothing is atomic.
/// `hint` is the expected number of edges a part files per worker.
fn build_csr<I: Iterator<Item = (u32, u32, u32)>>(n: usize, workers: usize, parts: usize, hint: usize, scan: impl Fn(usize) -> I + Sync) -> Csr {
    let filed = par(parts, |p| {
        let mut owned: Vec<Vec<(u32, u32, u32)>> = (0..workers).map(|_| Vec::with_capacity(hint)).collect();
        scan(p).for_each(|e| owned[e.0 as usize * workers / n].push(e));
        owned
    });
    let bounds = |w: usize| (n * w).div_ceil(workers);
    let degrees = par(workers, |w| {
        let lo = bounds(w);
        let mut degree = vec![0u32; bounds(w + 1) - lo];
        for e in filed.iter().flat_map(|owned| &owned[w]) {
            degree[e.0 as usize - lo] += 1;
        }
        degree
    });
    let mut row_offset = vec![0u32; n + 1];
    for (w, degree) in degrees.iter().enumerate() {
        for (i, &d) in degree.iter().enumerate() {
            row_offset[bounds(w) + i + 1] = row_offset[bounds(w) + i] + d;
        }
    }
    let mut adj = vec![(0u32, 0u32); row_offset[n] as usize];
    let out = Disjoint(adj.as_mut_ptr());
    par(workers, |w| {
        let lo = bounds(w);
        let mut cursor = row_offset[lo..bounds(w + 1)].to_vec();
        for &(key, other, weight) in filed.iter().flat_map(|owned| &owned[w]) {
            let c = &mut cursor[key as usize - lo];
            // SAFETY: the slots of rows [lo, hi) belong to this worker alone, and `*c` walks
            // exactly the `degree` slots of the row.
            unsafe { out.put(*c as usize, (other, weight)) };
            *c += 1;
        }
    });
    Csr { row_offset, adj }
}

/// The reverse of `g`. Its edges are in local order after the renaming, so
/// counting the in-degrees and then placing each edge at an atomically claimed
/// slot of its head's row touches few distinct cache lines, and no copy of the
/// edge list is needed (the order of edges within a row is arbitrary).
fn transpose(g: &Csr, parts: usize) -> Csr {
    let n = g.row_offset.len() - 1;
    let rows = |p: usize| (n * p / parts) as u32..(n * (p + 1) / parts) as u32;
    let degree: Vec<AtomicU32> = (0..=n).map(|_| AtomicU32::new(0)).collect();
    par(parts, |p| {
        for &(v, _) in rows(p).flat_map(|u| g.row(u)) {
            degree[v as usize + 1].fetch_add(1, Relaxed);
        }
    });
    let mut row_offset: Vec<u32> = degree.into_iter().map(AtomicU32::into_inner).collect();
    for i in 0..n {
        row_offset[i + 1] += row_offset[i];
    }
    let cursor: Vec<AtomicU32> = row_offset.iter().map(|&o| AtomicU32::new(o)).collect();
    let mut adj = vec![(0u32, 0u32); row_offset[n] as usize];
    let out = Disjoint(adj.as_mut_ptr());
    par(parts, |p| {
        for u in rows(p) {
            for &(v, w) in g.row(u) {
                // SAFETY: each edge claims its own slot of row `v`, which has one slot per edge counted.
                unsafe { out.put(cursor[v as usize].fetch_add(1, Relaxed) as usize, (u, w)) };
            }
        }
    });
    Csr { row_offset, adj }
}

/// New node order (`order[new] = old`) that puts neighbours close together.
/// Node ids are a random relabelling of a road-like lattice, so with the input
/// ids every relaxation lands on a random cache line. Workers grow compact
/// regions of up to `REGION` nodes by breadth-first search from seeds, racing
/// on a claimed flag per node; a region is a run of ids in BFS order. Every
/// `STRIDE`-th id is a seed in the first round, then every node still
/// unclaimed is one, so each node ends up in some region. The result is
/// not deterministic, which is fine: distances do not depend on labels.
fn region_order(g: &Csr, workers: usize) -> Vec<u32> {
    const REGION: usize = 1024;
    const STRIDE: usize = 256;
    let n = g.row_offset.len() - 1;
    let claimed: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
    let claim = |v: u32| claimed[v as usize].swap(1, Relaxed) == 0;
    let mut regions: Vec<Vec<u32>> = Vec::new();
    let mut seeds: Vec<u32> = (0..n as u32).step_by(STRIDE).collect();
    while !seeds.is_empty() {
        let next = AtomicUsize::new(0);
        let found = par(workers, |_| {
            let mut mine = Vec::new();
            while let Some(&seed) = seeds.get(next.fetch_add(1, Relaxed)) {
                if !claim(seed) {
                    continue;
                }
                let mut region = vec![seed];
                let mut head = 0;
                while head < region.len() && region.len() < REGION {
                    for &(v, _) in g.row(region[head]) {
                        if region.len() < REGION && claim(v) {
                            region.push(v);
                        }
                    }
                    head += 1;
                }
                mine.push(region);
            }
            mine
        });
        regions.extend(found.into_iter().flatten());
        seeds = (0..n as u32).filter(|&v| claimed[v as usize].load(Relaxed) == 0).collect();
    }
    regions.concat()
}

/// Renames the nodes of `g` so node `order[i]` becomes `i`, mapping every
/// neighbour through `new_id`. One pass over the rows; each worker fills the
/// disjoint slice of its own run of new rows.
fn permute_csr(g: &Csr, order: &[u32], new_id: &[u32], workers: usize) -> Csr {
    let n = order.len();
    let mut row_offset = vec![0u32; n + 1];
    for (i, &old) in order.iter().enumerate() {
        row_offset[i + 1] = row_offset[i] + g.row_offset[old as usize + 1] - g.row_offset[old as usize];
    }
    let mut adj = vec![(0u32, 0u32); g.adj.len()];
    let out = Disjoint(adj.as_mut_ptr());
    par(workers, |w| {
        let mut at = row_offset[n * w / workers] as usize;
        for &old in &order[n * w / workers..n * (w + 1) / workers] {
            for &(v, weight) in g.row(old) {
                // SAFETY: new rows [lo, hi) of worker `w` occupy exactly the slots it walks.
                unsafe { out.put(at, (new_id[v as usize], weight)) };
                at += 1;
            }
        }
    });
    Csr { row_offset, adj }
}

/// Reads the graph and returns `(node count, forward CSR, reverse CSR, new_id)`,
/// both CSRs in the locality order and `new_id[old]` the new name of node `old`.
fn load_graph(data: &[u8], threads: usize, mut phase: impl FnMut(&str)) -> (usize, Csr, Csr, Vec<u32>) {
    let (n, slices) = split_graph(data, threads);
    let original = build_csr(n, threads, slices.len(), data.len() / 16 / threads / threads + 16, |p| parse_edges(slices[p]));
    phase("forward CSR on the input ids");
    let order = region_order(&original, threads);
    let mut new_id = vec![0u32; n];
    let named = Disjoint(new_id.as_mut_ptr());
    par(threads, |w| {
        let lo = n * w / threads;
        for (i, &old) in order[lo..n * (w + 1) / threads].iter().enumerate() {
            // SAFETY: `order` is a permutation, so no two workers write the same entry.
            unsafe { named.put(old as usize, (lo + i) as u32) };
        }
    });
    phase("locality order");
    let fwd = permute_csr(&original, &order, &new_id, threads);
    drop((original, order));
    phase("permute");
    let rev = transpose(&fwd, threads);
    phase("reverse CSR");
    (n, fwd, rev, new_id)
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
struct State {
    dist: u64,
    node: u32,
}

/// A monotone radix heap: an entry lives in the bucket numbered by the highest
/// bit in which its key differs from the last key taken out, so a push is one
/// store and keys that pop soon sit in low buckets. Taking the minimum
/// empties the lowest occupied bucket into lower ones. Dijkstra never pushes
/// below the last pop, which is all it needs; it has no sift loops whose
/// branches the CPU can not predict.
struct RadixHeap {
    buckets: Vec<Vec<State>>,
    occupied: u128,
    last: u64,
    len: usize,
}
impl RadixHeap {
    fn new() -> Self {
        RadixHeap { buckets: (0..65).map(|_| Vec::new()).collect(), occupied: 0, last: 0, len: 0 }
    }
    fn clear(&mut self) {
        while self.occupied != 0 {
            self.buckets[self.occupied.trailing_zeros() as usize].clear();
            self.occupied &= self.occupied - 1;
        }
        self.last = 0;
        self.len = 0;
    }
    fn len(&self) -> usize {
        self.len
    }
    fn push(&mut self, item: State) {
        let b = (64 - (item.dist ^ self.last).leading_zeros()) as usize;
        self.buckets[b].push(item);
        self.occupied |= 1 << b;
        self.len += 1;
    }
    /// The smallest key, moving entries down so that it sits in bucket 0.
    fn min(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        if self.occupied & 1 == 0 {
            let b = self.occupied.trailing_zeros() as usize;
            let mut items = std::mem::take(&mut self.buckets[b]);
            self.occupied &= !(1 << b);
            self.last = items.iter().map(|s| s.dist).min().unwrap();
            self.len -= items.len();
            for item in items.drain(..) {
                self.push(item);
            }
            self.buckets[b] = items;
        }
        Some(self.last)
    }
    fn pop(&mut self) -> Option<State> {
        self.min()?;
        let item = self.buckets[0].pop();
        if self.buckets[0].is_empty() {
            self.occupied &= !1;
        }
        self.len -= 1;
        item
    }
}

/// Labels of one page of `PAGE` consecutive node ids.
const PAGE: usize = 16;

/// Per-worker search state, reused by every query. The numbering puts a
/// node's neighbours a few ids away, so a search touches runs of consecutive
/// ids. Labels live in pages of `PAGE` ids, allocated from a pool the first
/// time a search touches any id of the page (`page_of` maps id / `PAGE` to
/// the page, 0 for none), so a worker holds a few MB instead of an array over
/// all nodes. A label pair holds the node's tentative distance from the
/// source (`[0]`) and to the target (`[1]`) as distance + 1, so 0 means "not
/// reached" and a fresh page is all zeros. Almost every distance fits 32 bits;
/// the few that do not (paths through the edges of weight near 2^32) are
/// marked `u32::MAX` and held exactly in `big`.
struct Scratch {
    page_of: Vec<u32>,
    pool: Vec<[u32; 2]>,
    /// Pages handed out, to give back before the next query.
    pages: Vec<u32>,
    big: HashMap<(u32, usize), u64>,
    heaps: [RadixHeap; 2],
}
impl Scratch {
    fn new(n: usize) -> Self {
        Scratch {
            page_of: vec![0; n.div_ceil(PAGE)],
            pool: Vec::new(),
            pages: Vec::new(),
            big: HashMap::new(),
            heaps: [RadixHeap::new(), RadixHeap::new()],
        }
    }

    fn reset(&mut self) {
        for &p in &self.pages {
            self.page_of[p as usize] = 0;
        }
        self.pages.clear();
        self.pool.clear();
        self.big.clear();
        self.heaps.iter_mut().for_each(RadixHeap::clear);
    }

    /// Index in the pool of the label pair of `v`, giving its page a zeroed slot first if needed.
    #[inline]
    fn slot(&mut self, v: u32) -> usize {
        let page = (v as usize) / PAGE;
        if self.page_of[page] == 0 {
            self.pool.resize(self.pool.len() + PAGE, [0; 2]);
            self.page_of[page] = (self.pool.len() / PAGE) as u32;
            self.pages.push(page as u32);
        }
        (self.page_of[page] as usize - 1) * PAGE + v as usize % PAGE
    }

    /// Distance label of node `v` (in slot `i`) on `side`, or `u64::MAX` if unreached.
    #[inline]
    fn get(&self, i: usize, v: u32, side: usize) -> u64 {
        match self.pool[i][side] {
            0 => u64::MAX,
            u32::MAX => self.big[&(v, side)],
            label => label as u64 - 1,
        }
    }

    #[inline]
    fn set(&mut self, i: usize, v: u32, side: usize, dist: u64) {
        if dist < u32::MAX as u64 - 1 {
            self.pool[i][side] = dist as u32 + 1;
        } else {
            self.pool[i][side] = u32::MAX;
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
        let i = s.slot(node);
        s.set(i, node, side, 0);
        s.heaps[side].push(State { dist: 0, node });
    }

    let mut best = u64::MAX;
    while let (Some(tf), Some(tb)) = (s.heaps[0].min(), s.heaps[1].min()) {
        if tf.saturating_add(tb) >= best {
            break;
        }
        let side = usize::from(s.heaps[0].len() > s.heaps[1].len());
        let State { dist, node } = s.heaps[side].pop().unwrap();
        let at = s.slot(node);
        if dist > s.get(at, node, side) {
            continue;
        }
        for &(v, w) in graphs[side].row(node) {
            let nd = dist + w as u64;
            if nd >= best {
                continue;
            }
            let i = s.slot(v);
            if nd < s.get(i, v, side) {
                s.set(i, v, side, nd);
                s.heaps[side].push(State { dist: nd, node: v });
            }
            let other = s.get(i, v, 1 - side);
            if other != u64::MAX {
                best = best.min(nd + other);
            }
        }
    }
    (best != u64::MAX).then_some(best)
}

fn parse_queries(data: &[u8]) -> Vec<(u32, u32)> {
    numbers::<2>(data).map(|[s, t]| (s, t)).collect()
}

/// Answers every query, in parallel: queries are independent, and the
/// spec allows answering them in any order as long as the output keeps
/// input order. Each worker owns one `Scratch` and pulls the next query
/// index from a shared counter, so a few expensive long-tail queries
/// can't strand the other workers behind a static split.
fn answer_queries(fwd: &Csr, rev: &Csr, queries: &[(u32, u32)], n: usize, threads: usize) -> Vec<Option<u64>> {
    let next = AtomicUsize::new(0);
    let parts = par(threads.clamp(1, queries.len().max(1)), |_| {
        let mut scratch = Scratch::new(n);
        let mut mine = Vec::new();
        loop {
            let i = next.fetch_add(1, Relaxed);
            let Some(&(src, dst)) = queries.get(i) else { break };
            mine.push((i, bidirectional_dijkstra(fwd, rev, src, dst, &mut scratch)));
        }
        mine
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
    let mut last = std::time::Instant::now();
    let mut phase = |label: &str| {
        if verbose {
            eprintln!("track-d: {label} in {:.1}ms", last.elapsed().as_secs_f64() * 1000.0);
        }
        last = std::time::Instant::now();
    };

    let graph_data = map_file(graph_path)?;
    phase("map graph");
    let (node_count, fwd, rev, new_id) = load_graph(graph_data, threads, &mut phase);
    let queries: Vec<(u32, u32)> = parse_queries(map_file(queries_path)?)
        .into_iter()
        .map(|(s, t)| (new_id[s as usize], new_id[t as usize]))
        .collect();
    drop(new_id);
    phase("parse queries");

    // One worker per hardware thread: a worker's labels are a few MB, so they are cheap.
    let answers = answer_queries(&fwd, &rev, &queries, node_count, threads);
    phase("answer queries");
    let mut out: Vec<u8> = Vec::with_capacity(queries.len() * 12);
    for answer in answers {
        match answer {
            Some(d) => write_u64(&mut out, d),
            None => out.extend_from_slice(b"-1"),
        }
        out.push(b'\n');
    }
    std::fs::write(output_path, out)?;
    phase("write output");
    Ok(())
}

fn main() {
    let graph_path = std::env::var("GRAPH_PATH").unwrap_or_else(|_| "/data/graph.txt".to_string());
    let queries_path = std::env::var("QUERIES_PATH").unwrap_or_else(|_| "/data/queries.txt".to_string());
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/distances.txt".to_string());

    let start = std::time::Instant::now();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    match run(&graph_path, &queries_path, &output_path, threads) {
        Ok(()) => eprintln!("track-d: done in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0),
        Err(e) => {
            eprintln!("track-d: failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/track_d.rs"]
mod tests;
