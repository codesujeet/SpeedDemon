//! Track B — Index. Answers 50,000 BOOL/PHRASE queries from `queries.txt`
//! against `docs.txt` (`id<TAB>text` per line), writing for each query
//! the match count, the sum of matching ids, and the ten smallest ids.
//!
//! Spec: real-spec/speed-demon-2026/specs/b-index.md. The queries are
//! readable from the start, and the spec says outright that indexing
//! only the terms the queries use is legitimate. So:
//!
//! 1. Read the queries first and give every distinct query term a dense
//!    id.
//! 2. Tokenize the corpus once, in parallel. Each token becomes its
//!    query-term id, or `NONE` when no query uses it (it still holds its
//!    position). Each document's ids are stored contiguously, and each
//!    query term gets a sorted doc-id list. No positions are stored.
//! 3. BOOL queries are sorted-list set operations, strictly left to right.
//!    PHRASE queries intersect their terms' doc lists, rarest first, then
//!    check each candidate by scanning its stored ids for the exact run -
//!    the reference's strategy (verify candidates by re-reading the
//!    document), but over small integers instead of re-tokenized text.
//! 4. Queries are answered in parallel and written in input order.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
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

// --- FxHash-style hasher (research/track-a-ingest.md) -------------------

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default)]
struct FxHasher {
    hash: u64,
}
impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            let word = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
            self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
            bytes = &bytes[8..];
        }
        if !bytes.is_empty() {
            let mut buf = [0u8; 8];
            buf[..bytes.len()].copy_from_slice(bytes);
            let word = u64::from_ne_bytes(buf);
            self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
        }
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}
type FxBuildHasher = BuildHasherDefault<FxHasher>;

// --- Tokenizer (spec section 2.2) ---------------------------------------

/// Byte -> lowercased token byte, or 0 for a separator. ASCII `A`-`Z`
/// fold to `a`-`z`; `a`-`z` and `0`-`9` map to themselves; every other
/// byte - including every byte of a multi-byte UTF-8 sequence, which all
/// have the high bit set - is a separator. That is the spec's whole
/// tokenizer definition, applied without ever decoding UTF-8.
const fn build_class() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        let c = b as u8;
        t[b] = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else if c.is_ascii_uppercase() {
            c + 32
        } else {
            0
        };
        b += 1;
    }
    t
}
static CLASS: [u8; 256] = build_class();

/// Query terms are at most 32 bytes (grammar), so a longer corpus token
/// can never match one: it is reported with length 0, which no term has,
/// but it still takes up a position.
const MAX_TERM: usize = 32;

/// Calls `f(buf, len)` for every token of `text`, in order: the token is
/// `buf[..len]`, lowercased into a reused stack buffer (no allocation per
/// token). Bytes of `buf` past `len` are stale and must be ignored.
#[inline]
fn for_each_token(text: &[u8], mut f: impl FnMut(&[u8; MAX_TERM], usize)) {
    let mut buf = [0u8; MAX_TERM];
    let mut len = 0usize;
    for &b in text {
        let c = CLASS[b as usize];
        if c != 0 {
            if len < MAX_TERM {
                buf[len] = c;
            }
            len += 1;
        } else if len > 0 {
            f(&buf, if len <= MAX_TERM { len } else { 0 });
            len = 0;
        }
    }
    if len > 0 {
        f(&buf, if len <= MAX_TERM { len } else { 0 });
    }
}

// --- Term table ---------------------------------------------------------

/// Query term -> dense id, for the corpus scan, which looks up every one
/// of the corpus's 36M tokens. A general `HashMap<&[u8], u32>` measured
/// ~130ns per lookup here: control bytes, then the bucket, then the key
/// bytes behind the slice pointer - three dependent memory accesses.
/// This is open addressing at <= 50% load with the key inline: a slot
/// holds the token's first 8 bytes and last 8 bytes as `u64`s plus its
/// length, which pins down any token of up to 16 bytes exactly, so a
/// typical lookup is one hash and one cache-line probe. Only tokens
/// longer than 16 bytes compare against the stored term bytes.
struct TermTable<'a> {
    slots: Vec<Slot>,
    shift: u32,
    mask: usize,
    terms: Vec<&'a [u8]>,
}

/// `len == 0` marks an empty slot (every term is at least 1 byte).
#[derive(Clone, Copy, Default)]
struct Slot {
    head: u64,
    tail: u64,
    len: u32,
    id: u32,
}

/// The first 8 bytes (zero-padded) and the last 8 bytes (0 when the
/// token fits in `head`) of `buf[..len]`, as little-endian `u64`s.
#[inline]
fn head_tail(buf: &[u8; MAX_TERM], len: usize) -> (u64, u64) {
    let head = u64::from_le_bytes(buf[..8].try_into().unwrap());
    let head = if len >= 8 { head } else { head & ((1u64 << (8 * len)) - 1) };
    let tail = if len > 8 { u64::from_le_bytes(buf[len - 8..len].try_into().unwrap()) } else { 0 };
    (head, tail)
}

#[inline]
fn slot_hash(head: u64, tail: u64, len: usize) -> u64 {
    let h = (head ^ (len as u64).rotate_left(48)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (h ^ tail.rotate_left(29)).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
}

impl<'a> TermTable<'a> {
    /// `terms[id]` is the term with that id.
    fn new(terms: Vec<&'a [u8]>) -> Self {
        let cap = (terms.len() * 2).next_power_of_two().max(16);
        let mut table = TermTable {
            slots: vec![Slot::default(); cap],
            shift: 64 - cap.trailing_zeros(),
            mask: cap - 1,
            terms,
        };
        for id in 0..table.terms.len() {
            let term = table.terms[id];
            let mut buf = [0u8; MAX_TERM];
            buf[..term.len()].copy_from_slice(term);
            let (head, tail) = head_tail(&buf, term.len());
            let mut i = (slot_hash(head, tail, term.len()) >> table.shift) as usize;
            while table.slots[i].len != 0 {
                i = (i + 1) & table.mask;
            }
            table.slots[i] = Slot { head, tail, len: term.len() as u32, id: id as u32 };
        }
        table
    }

    /// The id of the token with this `head`, `tail` and `len`, or `NONE` if
    /// no query uses it. `long` holds the token's bytes and is only read
    /// when `len > 16`.
    #[inline]
    fn get(&self, head: u64, tail: u64, len: usize, long: Option<&[u8; MAX_TERM]>) -> u32 {
        if len == 0 {
            return NONE;
        }
        let mut i = (slot_hash(head, tail, len) >> self.shift) as usize;
        loop {
            let s = self.slots[i];
            if s.len == 0 {
                return NONE;
            }
            if s.head == head
                && s.tail == tail
                && s.len as usize == len
                && long.is_none_or(|buf| self.terms[s.id as usize] == &buf[..len])
            {
                return s.id;
            }
            i = (i + 1) & self.mask;
        }
    }
}

// --- Queries ------------------------------------------------------------

enum Query {
    /// First term id, then (operator, term id) pairs, left to right.
    Bool(u32, Vec<(Op, u32)>),
    Phrase(Vec<u32>),
    Invalid,
}

#[derive(Clone, Copy)]
enum Op {
    And,
    Or,
    Not,
}

/// Every distinct term across all queries, mapped to a dense id.
type Dict<'a> = HashMap<&'a [u8], u32, FxBuildHasher>;

/// A token that no query uses.
const NONE: u32 = u32::MAX;

/// Parses every query, giving each distinct term a dense id. Returns the
/// queries and the terms indexed by id.
fn parse_queries(data: &[u8]) -> (Vec<Query>, Vec<&[u8]>) {
    let mut dict = Dict::default();
    let mut queries = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let q = if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let mut toks = rest.split(|&b| b == b' ');
            match toks.next() {
                Some(first) => {
                    let first = term_id(&mut dict, first);
                    let mut ops = Vec::new();
                    while let (Some(op), Some(term)) = (toks.next(), toks.next()) {
                        let op = match op {
                            b"AND" => Op::And,
                            b"OR" => Op::Or,
                            _ => Op::Not,
                        };
                        ops.push((op, term_id(&mut dict, term)));
                    }
                    Query::Bool(first, ops)
                }
                None => Query::Invalid,
            }
        } else if let Some(inner) = line
            .strip_prefix(b"PHRASE \"")
            .and_then(|r| r.strip_suffix(b"\""))
        {
            Query::Phrase(inner.split(|&b| b == b' ').map(|t| term_id(&mut dict, t)).collect())
        } else {
            Query::Invalid
        };
        queries.push(q);
    }
    let mut terms: Vec<&[u8]> = vec![&[]; dict.len()];
    for (term, id) in dict {
        terms[id as usize] = term;
    }
    (queries, terms)
}

fn term_id<'a>(dict: &mut Dict<'a>, term: &'a [u8]) -> u32 {
    let next = dict.len() as u32;
    *dict.entry(term).or_insert(next)
}

// --- Corpus -------------------------------------------------------------

/// One line-aligned slice of `docs.txt`, tokenized: documents
/// `first_doc..first_doc + doc_start.len() - 1`, with document `first_doc
/// + i`'s term ids at `ids[doc_start[i]..doc_start[i + 1]]`.
struct Chunk {
    first_doc: u32,
    doc_start: Vec<u32>,
    ids: Vec<u32>,
}

/// The tokenized corpus (chunks in document order) plus every query
/// term's sorted doc list, stored CSR-style: term `t`'s list is
/// `flat[offsets[t]..offsets[t + 1]]`.
struct Corpus {
    chunks: Vec<Chunk>,
    offsets: Vec<usize>,
    flat: Vec<u32>,
}

impl Corpus {
    /// Document `doc`'s token ids.
    #[inline]
    fn doc(&self, doc: u32) -> &[u32] {
        let c = &self.chunks[self.chunks.partition_point(|c| c.first_doc <= doc) - 1];
        let i = (doc - c.first_doc) as usize;
        &c.ids[c.doc_start[i] as usize..c.doc_start[i + 1] as usize]
    }

    /// The sorted ids of the documents containing term `t`.
    #[inline]
    fn postings(&self, t: u32) -> &[u32] {
        &self.flat[self.offsets[t as usize]..self.offsets[t as usize + 1]]
    }
}

/// A tokenized chunk plus its own postings, CSR-style over local
/// offsets: term `t`'s docs in this chunk are `flat[offsets[t]..offsets[t + 1]]`.
struct ChunkOut {
    chunk: Chunk,
    offsets: Vec<usize>,
    flat: Vec<u32>,
}

/// Tokenizes one chunk, then builds its postings in two passes instead
/// of pushing onto one growable `Vec` per term. Pushing meant every
/// occurrence of a query term touched that term's `Vec` header and the
/// tail of its heap buffer - two dependent cache misses per token across
/// ~100k separately growing vectors. Here pass A only bumps a per-term
/// count in two small dense arrays (`last`, `count`), which stay in
/// cache; pass B prefix-sums the counts into offsets and re-reads this
/// chunk's token-id array (sequential) to write each doc id into one
/// flat array.
///
/// Each line carries its id (`id<TAB>text`) and ids are dense and
/// ascending (spec 2.1), so only the chunk's first id is parsed.
fn tokenize_chunk(text: &[u8], table: &TermTable, n_terms: usize) -> ChunkOut {
    let mut chunk = Chunk { first_doc: 0, doc_start: vec![0], ids: Vec::with_capacity(text.len() / 6) };
    let mut last = vec![NONE; n_terms];
    let mut count = vec![0usize; n_terms];
    let mut doc: Option<u32> = None;
    let mut keys: Vec<(u64, u64, usize)> = Vec::new();
    let mut longs: Vec<[u8; MAX_TERM]> = Vec::new();
    for line in text.split(|&b| b == b'\n') {
        let Some(tab) = line.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let d = match doc {
            Some(prev) => prev + 1,
            None => {
                let first = line[..tab].iter().fold(0u32, |acc, &c| acc * 10 + (c - b'0') as u32);
                chunk.first_doc = first;
                first
            }
        };
        doc = Some(d);
        // Three tight loops over the document instead of one loop mixing
        // byte scanning, the table probe and the counters: the probes are
        // independent of each other, so the CPU keeps many of their cache
        // misses in flight at once.
        keys.clear();
        longs.clear();
        for_each_token(&line[tab + 1..], |buf, len| {
            let (head, tail) = head_tail(buf, len);
            keys.push((head, tail, len));
            if len > 16 {
                longs.push(*buf);
            }
        });
        let first = chunk.ids.len();
        let mut longs_in_order = longs.iter();
        chunk.ids.extend(keys.iter().map(|&(head, tail, len)| {
            table.get(head, tail, len, if len > 16 { longs_in_order.next() } else { None })
        }));
        for &id in &chunk.ids[first..] {
            if id != NONE && last[id as usize] != d {
                last[id as usize] = d;
                count[id as usize] += 1;
            }
        }
        chunk.doc_start.push(chunk.ids.len() as u32);
    }

    let mut offsets = Vec::with_capacity(n_terms + 1);
    let mut total = 0;
    offsets.push(0);
    for &c in &count {
        total += c;
        offsets.push(total);
    }
    let mut cursor = offsets[..n_terms].to_vec();
    let mut flat = vec![0u32; total];
    last.fill(NONE);
    for (i, w) in chunk.doc_start.windows(2).enumerate() {
        let d = chunk.first_doc + i as u32;
        for &id in &chunk.ids[w[0] as usize..w[1] as usize] {
            if id != NONE && last[id as usize] != d {
                last[id as usize] = d;
                flat[cursor[id as usize]] = d;
                cursor[id as usize] += 1;
            }
        }
    }
    ChunkOut { chunk, offsets, flat }
}

/// Splits `docs` into line-aligned chunks, tokenizes them in parallel,
/// and joins every term's per-chunk lists into one flat array. Chunks
/// are in file order and ids ascend through the file, so joining in
/// chunk order keeps each list sorted. The join is parallel too: the
/// output is term-major, so a contiguous range of terms is a contiguous
/// range of `flat`, and each worker gets its own disjoint `&mut` slice.
fn build_corpus(docs: &[u8], table: &TermTable, threads: usize) -> Corpus {
    let n_terms = table.terms.len();
    let threads = threads.max(1);
    let mut bounds = vec![0usize];
    for k in 1..threads {
        let from = (docs.len() * k / threads).max(*bounds.last().unwrap());
        let at = docs[from..].iter().position(|&b| b == b'\n').map_or(docs.len(), |i| from + i + 1);
        bounds.push(at);
    }
    bounds.push(docs.len());

    let parts: Vec<ChunkOut> = std::thread::scope(|s| {
        let handles: Vec<_> = bounds
            .windows(2)
            .map(|w| {
                let text = &docs[w[0]..w[1]];
                s.spawn(move || tokenize_chunk(text, table, n_terms))
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("tokenize worker panicked")).collect()
    });
    let parts: Vec<ChunkOut> = parts.into_iter().filter(|p| p.chunk.doc_start.len() > 1).collect();

    let mut offsets = Vec::with_capacity(n_terms + 1);
    let mut total = 0;
    offsets.push(0);
    for t in 0..n_terms {
        total += parts.iter().map(|p| p.offsets[t + 1] - p.offsets[t]).sum::<usize>();
        offsets.push(total);
    }
    let mut flat = vec![0u32; total];

    // Term ranges of roughly equal size, one per worker.
    let mut ranges = vec![0usize];
    for k in 1..threads {
        let target = total * k / threads;
        let t = offsets.partition_point(|&o| o < target).min(n_terms);
        ranges.push(t.max(*ranges.last().unwrap()));
    }
    ranges.push(n_terms);
    std::thread::scope(|s| {
        let mut rest: &mut [u32] = &mut flat;
        for r in ranges.windows(2) {
            let (t0, t1) = (r[0], r[1]);
            let (mine, tail) = rest.split_at_mut(offsets[t1] - offsets[t0]);
            rest = tail;
            let (parts, offsets) = (&parts, &offsets);
            s.spawn(move || {
                let base = offsets[t0];
                for (t, &start) in (t0..t1).zip(&offsets[t0..t1]) {
                    let mut at = start - base;
                    for p in parts {
                        let seg = &p.flat[p.offsets[t]..p.offsets[t + 1]];
                        mine[at..at + seg.len()].copy_from_slice(seg);
                        at += seg.len();
                    }
                }
            });
        }
    });

    let chunks = parts.into_iter().map(|p| p.chunk).collect();
    Corpus { chunks, offsets, flat }
}

// --- Set operations on sorted doc lists ---------------------------------

/// `a ∩ b`. When one list is far shorter, each of its ids is found in
/// the longer one by binary search over the part not yet passed, which
/// costs O(short · log long) instead of a full O(short + long) merge.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    if small.len() * 32 < big.len() {
        let mut rest = big;
        for &x in small {
            match rest.binary_search(&x) {
                Ok(i) => {
                    out.push(x);
                    rest = &rest[i + 1..];
                }
                Err(i) => rest = &rest[i..],
            }
            if rest.is_empty() {
                break;
            }
        }
        return out;
    }
    let (mut i, mut j) = (0, 0);
    while i < small.len() && j < big.len() {
        if small[i] == big[j] {
            out.push(small[i]);
            i += 1;
            j += 1;
        } else if small[i] < big[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if a[i] < b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// `a \ b`.
fn difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j >= b.len() || b[j] != x {
            out.push(x);
        }
    }
    out
}

// --- Evaluation ---------------------------------------------------------

/// Strictly left to right, no precedence (spec section 3). The running
/// result starts as a borrow of the first term's list, so a single-term
/// query copies nothing.
fn eval_bool<'a>(c: &'a Corpus, first: u32, ops: &[(Op, u32)], owned: &'a mut Vec<u32>) -> &'a [u32] {
    let mut acc: Option<Vec<u32>> = None;
    for &(op, t) in ops {
        let cur = acc.as_deref().unwrap_or(c.postings(first));
        let rhs = c.postings(t);
        acc = Some(match op {
            Op::And => intersect(cur, rhs),
            Op::Or => union(cur, rhs),
            Op::Not => difference(cur, rhs),
        });
    }
    match acc {
        Some(v) => {
            *owned = v;
            owned
        }
        None => c.postings(first),
    }
}

/// Documents whose token ids contain `terms` as a consecutive run.
/// Candidates are the intersection of every term's doc list, rarest
/// first (usually a phrase's rare anchor term leaves a handful); each
/// candidate is then checked by scanning its ids. A repeated term
/// (`"x x x"`) needs that many consecutive `x`s, which the scan checks
/// directly.
fn eval_phrase(c: &Corpus, terms: &[u32]) -> Vec<u32> {
    let mut by_rarity: Vec<u32> = terms.to_vec();
    by_rarity.sort_unstable_by_key(|&t| c.postings(t).len());
    by_rarity.dedup();
    let mut candidates: Vec<u32> = c.postings(by_rarity[0]).to_vec();
    for &t in &by_rarity[1..] {
        if candidates.is_empty() {
            break;
        }
        candidates = intersect(&candidates, c.postings(t));
    }
    candidates.retain(|&d| c.doc(d).windows(terms.len()).any(|w| w == terms));
    candidates
}

/// One output line's content: `count sum id_1 .. id_m`, m = min(10, count).
#[derive(Clone, Copy)]
struct Answer {
    count: u32,
    sum: u64,
    first: [u32; 10],
}

impl Answer {
    fn of(ids: &[u32]) -> Self {
        let mut first = [0u32; 10];
        let m = ids.len().min(10);
        first[..m].copy_from_slice(&ids[..m]);
        Answer { count: ids.len() as u32, sum: ids.iter().map(|&d| d as u64).sum(), first }
    }
}

fn answer(c: &Corpus, q: &Query) -> Answer {
    match q {
        Query::Bool(first, ops) => {
            let mut owned = Vec::new();
            Answer::of(eval_bool(c, *first, ops, &mut owned))
        }
        Query::Phrase(terms) => Answer::of(&eval_phrase(c, terms)),
        Query::Invalid => Answer::of(&[]),
    }
}

/// Answers all queries in parallel. Workers take fixed-size blocks of
/// queries from a shared counter, so a few slow queries can't strand the
/// rest; blocks are reassembled in input order.
fn answer_all(c: &Corpus, queries: &[Query], threads: usize) -> Vec<Answer> {
    const BLOCK: usize = 64;
    let n_blocks = queries.len().div_ceil(BLOCK);
    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, Vec<Answer>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.clamp(1, n_blocks.max(1)))
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Ordering::Relaxed);
                        if b >= n_blocks {
                            break;
                        }
                        let block = &queries[b * BLOCK..((b + 1) * BLOCK).min(queries.len())];
                        mine.push((b, block.iter().map(|q| answer(c, q)).collect()));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("query worker panicked")).collect()
    });
    let mut blocks: Vec<Vec<Answer>> = vec![Vec::new(); n_blocks];
    for (b, v) in parts.into_iter().flatten() {
        blocks[b] = v;
    }
    blocks.into_iter().flatten().collect()
}

// --- Output -------------------------------------------------------------

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

fn render(answers: &[Answer]) -> Vec<u8> {
    let mut out = Vec::with_capacity(answers.len() * 32);
    for a in answers {
        write_u64(&mut out, a.count as u64);
        out.push(b' ');
        write_u64(&mut out, a.sum);
        for &id in &a.first[..(a.count as usize).min(10)] {
            out.push(b' ');
            write_u64(&mut out, id as u64);
        }
        out.push(b'\n');
    }
    out
}

/// Set `TRACK_B_TIMING=1` to print a per-phase breakdown to stderr.
fn run(docs_path: &str, queries_path: &str, output_path: &str, threads: usize) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_B_TIMING").is_ok();
    macro_rules! phase {
        ($label:expr, $body:expr) => {{
            let t = std::time::Instant::now();
            let v = $body;
            if verbose {
                eprintln!("track-b: {} in {:.1}ms", $label, t.elapsed().as_secs_f64() * 1000.0);
            }
            v
        }};
    }

    let queries_data = phase!("map queries", map_file(queries_path)?);
    let (queries, terms) = phase!("parse queries", parse_queries(queries_data));
    let table = TermTable::new(terms);
    let docs_data = phase!("map docs", map_file(docs_path)?);
    let corpus = phase!("tokenize corpus", build_corpus(docs_data, &table, threads));
    let answers = phase!("answer queries", answer_all(&corpus, &queries, threads));
    let out = phase!("render", render(&answers));
    phase!("write output", std::fs::write(output_path, out))?;

    // Freeing the token arrays and postings at exit is pure cost; the OS
    // reclaims them anyway (track-b/PERF.md).
    std::mem::forget(corpus);
    std::mem::forget(table);
    std::mem::forget(queries);
    Ok(())
}

fn main() {
    let docs_path = std::env::var("DOCS_PATH").unwrap_or_else(|_| "/data/docs.txt".to_string());
    let queries_path =
        std::env::var("QUERIES_PATH").unwrap_or_else(|_| "/data/queries.txt".to_string());
    let output_path =
        std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/results.txt".to_string());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let start = std::time::Instant::now();
    match run(&docs_path, &queries_path, &output_path, threads) {
        Ok(()) => {
            eprintln!("track-b: done in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
        Err(e) => {
            eprintln!("track-b: failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for_each_token(text, |buf, len| out.push(buf[..len].to_vec()));
        out
    }

    /// The term table against a plain `HashMap`, over terms built to
    /// stress its key layout: every length from 1 to 32, many sharing
    /// their first 8 bytes, pairs whose first and last 8 bytes agree but
    /// whose lengths differ, and non-members one byte away from members.
    #[test]
    fn term_table_matches_a_hash_map() {
        let mut terms: Vec<Vec<u8>> = Vec::new();
        for len in 1..=MAX_TERM {
            terms.push((0..len).map(|i| b'a' + (i % 26) as u8).collect());
            terms.push((0..len).map(|i| b'0' + (i % 10) as u8).collect());
        }
        for i in 0..2000u32 {
            terms.push(format!("abcdefgh{i}").into_bytes());
            terms.push(format!("x{i}y").into_bytes());
        }
        terms.push(b"aaaaaaaabbbbbbbb".to_vec());
        terms.push(b"aaaaaaaacbbbbbbbb".to_vec());
        terms.push(b"aaaaaaaaccbbbbbbbb".to_vec());
        terms.sort();
        terms.dedup();
        let refs: Vec<&[u8]> = terms.iter().map(|t| t.as_slice()).collect();
        let table = TermTable::new(refs.clone());
        let map: HashMap<&[u8], u32> = refs.iter().enumerate().map(|(i, &t)| (t, i as u32)).collect();

        let mut probes: Vec<Vec<u8>> = terms.clone();
        for t in &terms {
            let mut near = t.clone();
            let last = near.len() - 1;
            near[last] = if near[last] == b'z' { b'0' } else { b'z' };
            probes.push(near);
            if t.len() < MAX_TERM {
                let mut longer = t.clone();
                longer.push(b'q');
                probes.push(longer);
            }
        }
        for p in &probes {
            let mut buf = [0u8; MAX_TERM];
            buf.fill(b'#'); // stale bytes past `len` must be ignored
            buf[..p.len()].copy_from_slice(p);
            let want = map.get(p.as_slice()).copied().unwrap_or(NONE);
            let (head, tail) = head_tail(&buf, p.len());
            let got = table.get(head, tail, p.len(), Some(&buf));
            assert_eq!(got, want, "{:?}", String::from_utf8_lossy(p));
        }
        assert_eq!(table.get(0, 0, 0, None), NONE);
    }

    #[test]
    fn write_u64_matches_to_string_for_representative_values() {
        for &v in &[0u64, 1, 9, 10, 99, 100, 1234, 999_999, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            write_u64(&mut out, v);
            assert_eq!(out, v.to_string().as_bytes());
        }
    }

    #[test]
    fn tokenizer_is_byte_based_ascii_only() {
        // Spec section 2.2's own worked cases.
        assert_eq!(tokens("Zürich".as_bytes()), vec![b"z".to_vec(), b"rich".to_vec()]);
        assert_eq!(tokens("naïve".as_bytes()), vec![b"na".to_vec(), b"ve".to_vec()]);
        assert_eq!(tokens("273\u{212A}".as_bytes()), vec![b"273".to_vec()]);
        assert_eq!(tokens("\u{130}STANBUL".as_bytes()), vec![b"stanbul".to_vec()]);
        assert_eq!(tokens(b"007 7"), vec![b"007".to_vec(), b"7".to_vec()]);
        assert_eq!(tokens(b"habeas-corpus court's"), vec![
            b"habeas".to_vec(),
            b"corpus".to_vec(),
            b"court".to_vec(),
            b"s".to_vec()
        ]);
        assert_eq!(tokens(b"U.S.C."), vec![b"u".to_vec(), b"s".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn overlong_tokens_keep_their_position_but_match_nothing() {
        let long = "a".repeat(MAX_TERM + 1);
        let text = format!("x {long} y");
        assert_eq!(tokens(text.as_bytes()), vec![b"x".to_vec(), Vec::new(), b"y".to_vec()]);
    }

    /// The set operations against naive reference implementations, over
    /// size ratios on both sides of the binary-search switch in
    /// `intersect`.
    #[test]
    fn set_operations_match_naive_versions() {
        let lists: Vec<Vec<u32>> = vec![
            vec![],
            vec![5],
            (0..40).map(|i| i * 3).collect(),
            (0..2000).map(|i| i * 2 + 1).collect(),
            (0..5000).collect(),
            vec![0, 1, 2, 4_999, 10_000],
        ];
        for a in &lists {
            for b in &lists {
                let want_and: Vec<u32> = a.iter().copied().filter(|x| b.contains(x)).collect();
                let want_not: Vec<u32> = a.iter().copied().filter(|x| !b.contains(x)).collect();
                let mut want_or: Vec<u32> = a.iter().chain(b.iter()).copied().collect();
                want_or.sort_unstable();
                want_or.dedup();
                assert_eq!(intersect(a, b), want_and, "and {} {}", a.len(), b.len());
                assert_eq!(difference(a, b), want_not, "not {} {}", a.len(), b.len());
                assert_eq!(union(a, b), want_or, "or {} {}", a.len(), b.len());
            }
        }
    }

    fn run_on(docs: &str, queries: &str, threads: usize) -> String {
        // Tests run concurrently; a per-call counter keeps their temp
        // dirs (and input files) from colliding.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("track-b-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let docs_path = dir.join("docs.txt");
        let queries_path = dir.join("queries.txt");
        let out_path = dir.join("results.txt");
        std::fs::write(&docs_path, docs.as_bytes()).unwrap();
        std::fs::write(&queries_path, queries.as_bytes()).unwrap();
        run(docs_path.to_str().unwrap(), queries_path.to_str().unwrap(), out_path.to_str().unwrap(), threads)
            .unwrap();
        let out = std::fs::read_to_string(&out_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    const WORKED_DOCS: &str = "0\tThe surety posted bail; the BAIL BOND was forfeited.\n\
1\tWrit of Habeas-Corpus: see 42 U.S.C. \u{a7} 1983 (bail bond).\n\
2\t\n\
3\tZ\u{fc}rich bail\u{2014}bond, bail bail bond. Case 007.\n\
4\tIn the matter of the estate. Bond not posted, No. 7.\n";

    const WORKED_QUERIES: &str = "BOOL bail AND bond\n\
BOOL bail\n\
BOOL estate OR surety AND bail\n\
BOOL bond NOT bail\n\
BOOL bail NOT bail\n\
BOOL zurich\n\
BOOL rich AND z\n\
BOOL not AND bond\n\
BOOL 7\n\
BOOL the OR writ OR case\n\
PHRASE \"bail bond\"\n\
PHRASE \"bail bail bond\"\n\
PHRASE \"bond bail\"\n\
PHRASE \"habeas corpus\"\n\
PHRASE \"u s c 1983\"\n\
PHRASE \"estate bond\"\n\
PHRASE \"forfeited writ\"\n\
PHRASE \"in the matter of\"\n";

    const WORKED_RESULTS: &str = "3 4 0 1 3\n\
3 4 0 1 3\n\
1 0 0\n\
1 4 4\n\
0 0\n\
0 0\n\
1 3 3\n\
1 4 4\n\
1 4 4\n\
4 8 0 1 3 4\n\
3 4 0 1 3\n\
1 3 3\n\
1 3 3\n\
1 1 1\n\
1 1 1\n\
1 4 4\n\
0 0\n\
1 4 4\n";

    /// specs/b-index.md section 4's worked example, byte for byte.
    #[test]
    fn matches_the_spec_worked_example_exactly() {
        assert_eq!(run_on(WORKED_DOCS, WORKED_QUERIES, 1), WORKED_RESULTS);
    }

    /// More workers than documents: chunk boundaries land on every line
    /// and some chunks are empty; the joined postings and the doc -> chunk
    /// lookup must still give identical results.
    #[test]
    fn parallel_run_matches_single_threaded() {
        for threads in [2, 3, 5, 12] {
            assert_eq!(run_on(WORKED_DOCS, WORKED_QUERIES, threads), WORKED_RESULTS, "threads={threads}");
        }
    }

    #[test]
    fn phrase_needs_consecutive_positions_including_repeats() {
        let docs = "0\tthe dog barked at the enemy near the fights enemy today\n\
1\tcommon rare match here\n\
2\tcommon filler two\n\
3\tcommon filler rare match again\n\
4\tx y x x z\n\
5\tx x x\n";
        let queries = "PHRASE \"the fights enemy\"\n\
PHRASE \"rare match\"\n\
PHRASE \"the barked\"\n\
PHRASE \"x x\"\n\
PHRASE \"x x x\"\n\
PHRASE \"x x x x\"\n";
        let want = "1 0 0\n2 4 1 3\n0 0\n2 9 4 5\n1 5 5\n0 0\n";
        for threads in [1, 3] {
            assert_eq!(run_on(docs, queries, threads), want, "threads={threads}");
        }
    }
}
