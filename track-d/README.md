# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

The graph file is mapped and split at line boundaries into one slice per
thread. Each thread parses its slice and files every edge under the worker
that owns the edge's tail (workers own contiguous ranges of node ids), so a
worker then counts and places only its own 1/12 of the edges, into its own
slice of the adjacency array, with no shared counters. Adjacency is one array
of `(neighbour, weight)` pairs plus row offsets (CSR).

The node ids in the input are a random relabelling of a road-like lattice, so
a search would hit a random cache line on every relaxation. The program gives
every node a new id with a parallel ordering:
workers race on one claimed byte per node and grow compact regions of up to
1024 nodes by breadth-first search from seeds (every 256th id first, then
every node still unclaimed), and a region is a run of new ids in BFS order, so
neighbours end up a few ids apart. The ordering differs from run to run;
distances do not depend on labels. The forward CSR is renamed into the new
order by a row copy, and the reverse CSR is built from the renamed forward
CSR (its edges are in local order, so filing them is cheap).

Queries are independent, so workers take them from an atomic counter and
answer each with bidirectional Dijkstra: side 0 searches the forward CSR
from the source, side 1 the reverse CSR from the target, always growing the
side with the smaller queue. Each side's queue is a monotone radix heap (an
entry sits in the bucket of the highest bit in which its key differs from the
last key taken out), which has no data-dependent sift loops and halved the
query phase compared with a 4-ary heap. The search stops when the two queue
minima sum to at least the best meeting distance, or as soon as either queue
empties (that side has settled everything it can reach); relaxations at or
above the best distance are skipped. Each of the 6 workers (one per physical
core) keeps one array of `[forward, backward]` labels per node, stored as
32-bit distance + 1 so a zeroed array means "unreached", and clears only the
nodes the last query touched. Distances are `u64` because sums of full-range
`u32` weights pass 2^32; the few that do not fit a 32-bit label are marked in
the array and kept exactly in a small per-query table. Answers are written in
input order.

Assumptions: node ids fit `u32`, edge counts fit `u32` (the row offsets), and
the graph is road-like so that region growing finds compact regions; on any
other graph the order is still a valid permutation and the answers are the
same, only slower.

Tests (spec worked example at 1, 4 and 12 threads, a randomized comparison
against a plain Dijkstra over every pair on small graphs with zero weights,
huge weights, parallel edges, self-loops and unreachable pairs, heap
behaviour) are in `tests/track_d.rs` at the repository root, compiled into the
test build only (`cargo test` in this folder runs them).

## Dependencies

None.

## With another 24 hours

The sequential-looking phases (the original-id CSR, the ordering, the renaming
and the reverse CSR) now cost about as much as the queries, and all are limited
by random memory access. A 4-byte edge (21 bits of target, 11 of weight, an
escape table for heavier weights) would halve both CSRs and the traffic; hash
labels would cut the 84 MB of per-worker label arrays to a few MB per worker
but measured about 25% slower queries (see `PERF.md`). A landmark or
contraction-style preprocessing would make queries cheaper still, but the
2,000-query count makes it unlikely to pay for its build time.
