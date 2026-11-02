# Track D: Traverse (Rust)

Reads `/data/graph.txt` (a directed weighted graph, one edge per line) and the queries, and writes `/out/distances.txt`: the shortest distance for each query, `-1` if the target is unreachable. The exact task and output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md). The public graph has 1,740,512 nodes, 5,000,000 edges and 2,000 queries.

## Approach

One Rust file, `src/main.rs`, std only (no crates), built for x86-64-v3 through `.cargo/config.toml`. Threads come from `available_parallelism`, capped at 12.

1. **Parse once.** `graph.txt` is mapped over a spare zero page and cut at line ends into one range per thread. Each thread files every edge, as sequential appends, under the part of the node range its tail falls in (parts of 16,384 nodes, about 107). An edge is one 32-bit word: the head in the low bits and the weight above it; a weight too large for its field is stored as all ones with its value on a side list. Numbers up to 7 digits are read from one 8-byte load. Self-loops are dropped.
2. **Build each part on its own.** A thread takes a part, counts its nodes' out-degrees and places its edges inside a 64 KB counter array, so the adjacency is built with no atomics.
3. **Renumber for locality.** The input ids are a random relabelling of a road-like lattice. Threads grow compact blobs of 512 nodes by BFS (nodes claimed with a compare-exchange on one byte each, next rows and edges prefetched), and the graph is copied into that numbering and then reversed (in-degrees counted atomically, edges placed with atomic cursors). The numbering differs from run to run; the distances do not.
4. **Query.** Queries are sorted by their renumbered endpoints and taken from an atomic counter by persistent workers. Each answers with bidirectional Dijkstra over the forward and reverse graphs with 4-ary heaps, always growing the side with the smaller queue, and stops when the two smallest keys sum to at least the best meeting distance. A target with no in-edge is `-1` at once. Distances are `u64` and sit in pages of 256 handed out on first touch and taken back after each query. The process ends with `exit(0)` without freeing the big arrays.

## Assumptions and known limits

- `n` below 2^31, with the weight sharing the 32-bit word with a node id: weights that do not fit go to side lists, so all `u32` weights are exact. `m` below 2^32.
- At most 65,535 distance pages in use per side in one search (the page table holds `u16` entries).
- A part holds 2^14 nodes, so offsets within a part fit 16 bits.
- A worker thread that panics leaves the others waiting on a barrier, so a bug would hang rather than abort.

## Dependencies

None.

## Verification

Needs Docker; the datasets are generated with the contest's generator and are not in the repository. From the repository root:

```
python3 check.py d --data <directory with the graph.txt and queries.txt> --expected a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e
.\real-spec\dock.ps1 -Track d -Scale full -Runs 20
```

The first builds the track image offline and compares the SHA-256 of the output with the published hash of the seed-1 data; the second replicates the board run (pre-warmed volume, board flags, `memory.peak`) and prints MATCH or MISMATCH. The current source was checked against the published hash, generated seeds 2, 3 and 7, tiny and sample scales and an edge-case suite (66 cases); `cargo clippy -D warnings` was clean. There are no unit tests for this implementation (`tests/track_d.rs` targets the earlier one and is not compiled). The history with run IDs is in [`PERF.md`](PERF.md); the whole-repo architecture is in [`../docs/OVERVIEW.md`](../docs/OVERVIEW.md).

## Provenance

This source started as a draft written with ChatGPT by the team, then reviewed and tested here. Five lines were changed in review: one fixes a panic (a prefetch indexed the edge array at the offset of a trailing node with no out-edges, one past its end); the rest satisfy clippy. The earlier region-growing / radix-heap implementation and its history are in git history.
