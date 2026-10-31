# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

The graph file is mapped and split at line boundaries into one slice per
thread. Each thread parses its slice and files every edge under the worker
that owns the edge's tail (workers own contiguous ranges of node ids) as one
`u64` (key within the range, neighbour, weight; a weight too big for its
field goes whole into a short side list). A worker then counts and places only
its own 1/12 of the edges, into its own slice of the adjacency array, with no
shared counters.

The CSR stores each edge in one 32-bit word: the neighbour in the high bits
and the weight in the low bits, as many as are left after the bits a node id
needs (11 for the full graph, so most weights fit). A weight that does not fit
puts the all-ones marker in the word and the exact weight in the next word, so
every `u32` weight is exact; row offsets count words.

The node ids in the input are a random relabelling of a road-like lattice, so
a search would hit a random cache line on every relaxation. The program gives
every node a new id with a parallel ordering: workers race on one claimed byte
per node and grow compact regions of up to 1024 nodes by breadth-first search
from seeds (every 256th id first, then every node still unclaimed), and a
region is a run of new ids in BFS order, so neighbours end up a few ids apart.
A worker copies each finished region's rows into its own buffer while they are
still in cache; after the growth, the buffers are renamed (neighbours mapped
through the new ids) into the final adjacency array in parallel, which makes
the separate permutation pass unnecessary. The ordering differs from run to
run; distances do not depend on labels. The reverse CSR is built from the
renamed forward CSR by counting in-degrees and placing each edge at an
atomically claimed slot (its edges are in local order, so this touches few
cache lines and needs no copy of the edge list).

Queries are independent, so 12 workers take them from an atomic counter and
answer each with bidirectional Dijkstra: side 0 searches the forward CSR
from the source, side 1 the reverse CSR from the target, always growing the
side with the smaller queue. Each side's queue is a monotone radix heap (an
entry sits in the bucket of the highest bit in which its key differs from the
last key taken out), which has no data-dependent sift loops and halved the
query phase compared with a 4-ary heap. The search stops when the two queue
minima sum to at least the best meeting distance, or as soon as either queue
empties (that side has settled everything it can reach); relaxations at or
above the best distance are skipped. A worker's labels live in pages of 16
node ids taken from a pool the first time a search touches a node of the page
(a page table maps ids to pages), so a worker holds a few MB, not an array
over all nodes; a label pair is the 32-bit distance + 1 from the source and to
the target, so a zeroed page means "unreached". Distances are `u64` because
sums of full-range `u32` weights pass 2^32; the few that do not fit a 32-bit
label are marked in the page and kept exactly in a small per-query table.
Answers are written in input order. Freed memory is returned to the system
between the build phases (`malloc_trim`), so one phase's garbage does not add
to the next phase's peak.

Assumptions: node ids fit `u32` with at least one weight bit to spare (n below
2^31), edge words fit `u32` (the row offsets), and the graph is road-like so
that region growing finds compact regions; on any other graph the order is
still a valid permutation and the answers are the same, only slower.

Tests (spec worked example at 1, 4 and 12 threads, a randomized comparison
against a plain Dijkstra over every pair on small graphs with zero weights,
huge weights, parallel edges, self-loops and unreachable pairs, heap
behaviour) are in `tests/track_d.rs` at the repository root, compiled into the
test build only (`cargo test` in this folder runs them).

## Dependencies

None.

## With another 24 hours

Every phase left is bound by random memory access: the original-id CSR (about
80 ms), the region growth with the rename (about 115 ms), the reverse CSR (about
40 ms) and the queries (about 160-200 ms with 12 workers; the heap is roughly
a third of that, so a Dial-style bucket queue is the next thing to try). The
peak (about 90 MB) is now the query phase: both CSRs (54 MB) plus 12 workers'
label pools. A landmark or contraction-style preprocessing would make queries
cheaper still, but the 2,000-query count makes it unlikely to pay for its
build time.
