# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

**Provenance.** This `src/main.rs` replaced the earlier region-growing /
radix-heap version. It was drafted with ChatGPT by the project owner and
then reviewed and tested here (hash checks on four generated full-size
datasets, repeated runs, the edge-case suite, `cargo clippy -D warnings`).
Five lines were changed: one fixes a panic (a prefetch indexed the edge array
at the offset of a trailing node with no out-edges, one past its end), the
rest satisfy clippy. The earlier implementation, its unit tests
(`tests/track_d.rs`, which target that implementation and are no longer
compiled) and its PERF history are in git history.

1. **Parse once.** `graph.txt` is mapped and cut at line boundaries into one
   range per thread (12). Each thread files every edge, as sequential
   appends, under the part of the node range its tail falls in (parts of
   16,384 nodes, about 107). An edge is one 32-bit word: the head in the low
   bits and the weight above it; a weight too large for its field is stored
   as all ones with its value on a side list. Numbers up to 7 digits are read
   from one 8-byte load. Self-loops are dropped.
2. **Build each part on its own.** A thread takes a part, counts its nodes'
   out-degrees and places its edges inside a 64 KB counter array, so the
   adjacency is built with no atomics.
3. **Renumber for locality.** The input ids are a random relabelling of a
   road-like lattice. Threads grow compact blobs of 512 nodes by BFS (nodes
   claimed with a compare-exchange on one byte each, next rows and edges
   prefetched), and the graph is copied into that numbering and then reversed
   (in-degrees counted atomically, edges placed with atomic cursors). The
   order differs from run to run; the distances do not.
4. **Query.** Queries are sorted by their renumbered endpoints and taken from
   an atomic counter by 12 persistent workers. Each answers with
   bidirectional Dijkstra over the forward and reverse graphs, 4-ary heaps,
   always growing the side with the smaller queue, stopping when the two
   smallest keys sum to at least the best meeting distance; a target with no
   in-edge is `-1` at once. Distances are `u64` and sit in pages of 512
   handed out on first touch and taken back after each query.

Assumptions: `n` below 2^31 with the weight sharing the 32-bit word with a
node id (heavy weights go to side lists, so all `u32` weights are exact), at
most 65,535 distance pages in use per side, and `m` below 2^32. Threads come
from `available_parallelism` capped at 12. A worker thread that panics
leaves the others waiting on a barrier, so a bug would hang, not abort.

There are no unit tests for this implementation; it is checked against the
published hash, generated datasets and the edge-case suite (see `PERF.md`).

## Dependencies

None.

## Dependencies

None.
