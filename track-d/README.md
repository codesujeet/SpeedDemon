# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

The graph file is mapped and split at line boundaries into one slice per
thread, each parsed into its own edge list. The node ids in the input are a
random relabelling of a road-like lattice, so the program first builds a
CSR on the original ids, runs one breadth-first pass to give every node a
new id in BFS order (neighbours end up a few ids apart), and renames both
CSRs into that order. The serial BFS leaves the other cores idle, so the
reverse CSR is built beside it; renaming is a row copy with each neighbour
mapped to its new id, which is cheaper than rebuilding from the edge list.

Each CSR is built by workers that own contiguous ranges of nodes. The edge
parts are first filed under the worker that owns each edge's node, so a
worker counts and places only its own 1/12 of the edges: counts are
prefix-summed, and each worker writes only its own slice, so nothing is
shared and no atomics are needed. Adjacency is one array of
`(neighbour, weight)` pairs.

Queries are independent, so workers take them from an atomic counter and
answer each with bidirectional Dijkstra: side 0 searches the forward CSR
from the source, side 1 the reverse CSR from the target, always growing the
side with the smaller 4-ary heap. The search stops when the two queue tops
sum to at least the best meeting distance, or as soon as either queue
empties (that side has settled everything it can reach); relaxations at or
above the best distance are skipped. Each worker keeps one array of
`[forward, backward]` labels per node, stored as distance + 1 so a zeroed
array means "unreached", and clears only the nodes the last query touched.
Distances are `u64` because sums of full-range `u32` weights pass 2^32.
Answers are written in input order.

Tests (spec worked example at 1, 4 and 12 threads, a randomized comparison
against a plain Dijkstra over every pair on small graphs with zero weights,
huge weights, parallel edges, self-loops and unreachable pairs, heap
behaviour) are in `src/main.rs`.

## Dependencies

None.

## With another 24 hours

The serial BFS (about 200 ms) and the original forward CSR build before it
are the critical path, and the query phase (about 410-475 ms) is limited by
memory latency, evenly spread over the workers. Shrinking the per-worker
label arrays would cut memory, which is mostly those arrays. Packing the
heap entry into one `u64` was measured and gave 2-3%, which was not worth
its lines (see `PERF.md`). A landmark or contraction-style preprocessing
would make queries cheaper still, but the query mix and the 2,000-query
count make it unlikely to pay for its build time.
