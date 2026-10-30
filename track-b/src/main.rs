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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

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

/// A tokenized chunk plus, for every term, how many of its documents
/// contain it.
struct ChunkOut {
    chunk: Chunk,
    count: Vec<u32>,
}

/// Tokenizes one chunk and counts, per term, the documents that contain it.
/// Counting only bumps two small dense arrays (`last`, `count`), which stay
/// in cache; pushing each occurrence onto a growable `Vec` per term would
/// touch the `Vec` header and the tail of its heap buffer, two dependent
/// cache misses per token across ~100k separately growing vectors. The
/// postings themselves are written later, once every chunk's counts give
/// each term list its final place.
///
/// Each line carries its id (`id<TAB>text`) and ids are dense and
/// ascending (spec 2.1), so only the chunk's first id is parsed.
fn tokenize_chunk(text: &[u8], table: &TermTable, n_terms: usize) -> ChunkOut {
    let mut chunk = Chunk { first_doc: 0, doc_start: vec![0], ids: Vec::with_capacity(text.len() / 6) };
    let mut last = vec![NONE; n_terms];
    let mut count = vec![0u32; n_terms];
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

    ChunkOut { chunk, count }
}

/// Splits `docs` into line-aligned chunks and tokenizes them in parallel.
/// Every term's document list is then written straight into its final place
/// in one flat array: term `t`'s slots are `offsets[t]..offsets[t + 1]`, and
/// within them the chunks come in file order, each chunk's share starting
/// where the earlier chunks' shares end. Ids ascend through the file, so each
/// list ends up sorted. The slots of different chunks are disjoint but
/// interleaved in memory, so they are written through atomic cells (relaxed
/// stores, the same instruction as a plain store).
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
        total += parts.iter().map(|p| p.count[t] as usize).sum::<usize>();
        offsets.push(total);
    }
    let mut flat = vec![0u32; total];
    // SAFETY: `AtomicU32` has the size and alignment of `u32`, and `flat` is
    // not touched through any other path while `cells` is in use.
    let cells: &[AtomicU32] = unsafe { &*(flat.as_mut_slice() as *mut [u32] as *const [AtomicU32]) };
    std::thread::scope(|s| {
        for (index, part) in parts.iter().enumerate() {
            let (parts, offsets) = (&parts, &offsets);
            s.spawn(move || {
                // Where this chunk's share of each term's list starts.
                let mut cursor = offsets[..n_terms].to_vec();
                for earlier in &parts[..index] {
                    for (at, &n) in cursor.iter_mut().zip(&earlier.count) {
                        *at += n as usize;
                    }
                }
                let chunk = &part.chunk;
                let mut last = vec![NONE; n_terms];
                for (i, w) in chunk.doc_start.windows(2).enumerate() {
                    let d = chunk.first_doc + i as u32;
                    for &id in &chunk.ids[w[0] as usize..w[1] as usize] {
                        if id != NONE && last[id as usize] != d {
                            last[id as usize] = d;
                            cells[cursor[id as usize]].store(d, Ordering::Relaxed);
                            cursor[id as usize] += 1;
                        }
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
#[path = "../../tests/track_b.rs"]
mod tests;
