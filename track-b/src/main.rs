//! Track B - Index. Answers 50,000 BOOL/PHRASE queries from `queries.txt`
//! against `docs.txt` (`id<TAB>text` per line), writing for each query
//! the match count, the sum of matching ids, and the ten smallest ids.
//!
//! Spec: real-spec/speed-demon-2026/specs/b-index.md. The queries are
//! readable from the start, and the spec says outright that indexing
//! only the terms the queries use is legitimate. So:
//!
//! 1. Read the queries first and give every distinct term a dense id.
//! 2. Cut the corpus into pieces of a few MB (line-aligned). Each worker
//!    thread takes a piece, tokenizes it (AVX2 finds the token bytes, an
//!    inline-key table maps each token to its query-term id or a sink id),
//!    builds that piece's own postings (u16 piece-local doc numbers), runs
//!    ALL queries against the piece, and folds each answer into its own
//!    per-query accumulators: count, sum, and the first ten ids.
//! 3. Pieces, hence ids, ascend through a worker's run, so merging the
//!    workers' accumulators gives the exact global answer. A piece is a few
//!    MB, so the token ids and postings the queries touch stay in cache and
//!    the whole index never exists at once.
//! 4. BOOL is sorted-list set algebra, strictly left to right. PHRASE
//!    intersects its terms' lists, then verifies each candidate document by
//!    scanning its stored token ids for the exact run.

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
    // SAFETY: a fresh read-only private mapping of a file we hold open; the
    // result is checked below and never unmapped, so the slice stays valid.
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

// --- Tokenizer (spec section 2.2) ---------------------------------------

/// Bit `i` is set when `c[i]` is a token byte: `a`-`z`, `A`-`Z` or `0`-`9`.
/// `b | 0x20` folds `A`-`Z` onto `a`-`z` and leaves digits alone, and no
/// other byte lands in `a`-`z` after the fold. Every other byte, including
/// every byte >= 0x80, separates tokens (spec 2.2) without UTF-8 decoding.
fn token_mask(c: &[u8; 64]) -> u64 {
    use std::arch::x86_64::*;
    #[inline(always)]
    fn half(c: &[u8]) -> u64 {
        // SAFETY: the build targets x86-64-v3 (.cargo/config.toml), so AVX2 is available; `c` is the
        // 32 bytes read by one unaligned load.
        unsafe {
            let v = _mm256_loadu_si256(c.as_ptr() as *const __m256i);
            let l = _mm256_sub_epi8(
                _mm256_or_si256(v, _mm256_set1_epi8(0x20)),
                _mm256_set1_epi8(0x61),
            );
            let d = _mm256_sub_epi8(v, _mm256_set1_epi8(0x30));
            let is_l = _mm256_cmpeq_epi8(_mm256_min_epu8(l, _mm256_set1_epi8(25)), l);
            let is_d = _mm256_cmpeq_epi8(_mm256_min_epu8(d, _mm256_set1_epi8(9)), d);
            _mm256_movemask_epi8(_mm256_or_si256(is_l, is_d)) as u32 as u64
        }
    }
    half(&c[..32]) | half(&c[32..]) << 32
}

/// Calls `f(start, end)` for every token `text[start..end]`, in order, until
/// `f` returns false. Token bytes come from 64-byte masks: a token starts
/// at a set bit whose predecessor is clear and ends at the clear bit that
/// follows it.
#[inline(always)]
fn scan_tokens(text: &[u8], mut f: impl FnMut(usize, usize) -> bool) {
    let (mut open, mut start, mut prev) = (false, 0usize, 0u64);
    for base in (0..text.len()).step_by(64) {
        let m = match text.get(base..base + 64) {
            Some(c) => token_mask(c.try_into().unwrap()),
            None => {
                // Last, partial chunk: pad with separators.
                let mut chunk = [b' '; 64];
                chunk[..text.len() - base].copy_from_slice(&text[base..]);
                token_mask(&chunk)
            }
        };
        let shifted = m << 1 | prev;
        let (mut starts, mut ends) = (m & !shifted, !m & shifted);
        prev = m >> 63;
        while ends != 0 {
            let end = base + ends.trailing_zeros() as usize;
            ends &= ends - 1;
            if !open {
                start = base + starts.trailing_zeros() as usize;
                starts &= starts - 1;
            }
            open = false;
            if !f(start, end) {
                return;
            }
        }
        if starts != 0 {
            open = true;
            start = base + starts.trailing_zeros() as usize;
        }
    }
    if open {
        f(start, text.len());
    }
}

// --- Term table ---------------------------------------------------------

/// Asks the CPU to start loading the cache line holding `p`.
#[inline(always)]
fn prefetch<T>(p: &T) {
    // SAFETY: a prefetch is only a hint; it never faults or changes memory.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_prefetch(p as *const T as *const i8, std::arch::x86_64::_MM_HINT_T0)
    }
}

/// Query terms are at most 32 bytes (grammar), so a longer corpus token
/// can never match one: it gets length 0, which no term has, but it still
/// takes up a position.
const MAX_TERM: usize = 32;

/// `u64` at `at`, little-endian, zero-padded past the end of `text`.
#[inline(always)]
fn load8(text: &[u8], at: usize) -> u64 {
    match text.get(at..at + 8) {
        Some(w) => u64::from_le_bytes(w.try_into().unwrap()),
        None => text[at..]
            .iter()
            .enumerate()
            .fold(0, |w, (i, &b)| w | (b as u64) << (8 * i)),
    }
}

/// Key of the token `text[s..e]`: its first 8 bytes (zero-padded), its last
/// 8 bytes (0 when it fits in the first) and its length, case-folded, which
/// pins down any token of up to 16 bytes exactly. Length 0 for a token too
/// long to match any term. Branch-free for the lengths that vary (token
/// lengths are unpredictable, so every branch on them costs a mispredict).
#[inline(always)]
fn key(text: &[u8], s: usize, e: usize) -> (u64, u64, u32) {
    const LOW: u64 = 0x2020_2020_2020_2020;
    let len = e - s;
    if len > MAX_TERM {
        return (0, 0, 0);
    }
    let head = (load8(text, s) | LOW) & (!0u64 >> (64 - 8 * len.min(8)));
    let tail = (load8(text, e.saturating_sub(8)) | LOW) & 0u64.wrapping_sub((len > 8) as u64);
    (head, tail, len as u32)
}

/// Term -> dense id, for the corpus scan, which looks up every one of the
/// corpus's 36M tokens. A general `HashMap<&[u8], u32>` measured ~130ns per
/// lookup here: control bytes, then the bucket, then the key bytes behind
/// the slice pointer - three dependent memory accesses. This is a bucketed
/// hash table with the key inline: one 64-byte bucket holds three entries
/// and is compared with no branches, so a lookup is one hash, one cache
/// line and no mispredict in the common case. Only tokens longer than 16
/// bytes compare against the stored term bytes, and a full bucket spills to
/// the next one.
struct TermTable<'a> {
    buckets: Vec<Bucket>,
    terms: Vec<&'a [u8]>,
}

/// Three entries: key halves, and `len << 26 | id` (0 = empty; every term
/// has at least 1 byte).
#[derive(Clone, Copy, Default)]
#[repr(align(64))]
struct Bucket {
    head: [u64; 3],
    tail: [u64; 3],
    meta: [u32; 3],
}

const ID_BITS: u32 = 26;

#[inline(always)]
fn bucket_hash(k: (u64, u64, u32)) -> u64 {
    let h = (k.0 ^ (k.2 as u64).rotate_left(48)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (h ^ k.1.rotate_left(29)).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
}

impl<'a> TermTable<'a> {
    fn new() -> Self {
        TermTable {
            buckets: vec![Bucket::default(); 1 << 10],
            terms: Vec::new(),
        }
    }

    #[inline(always)]
    fn home(&self, k: (u64, u64, u32)) -> usize {
        (bucket_hash(k) >> (64 - self.buckets.len().trailing_zeros())) as usize
    }

    /// Stores `k -> id` in the first free entry from its home bucket on.
    fn place(&mut self, k: (u64, u64, u32), id: u32) {
        let mask = self.buckets.len() - 1;
        let mut i = self.home(k);
        loop {
            let b = &mut self.buckets[i];
            if let Some(j) = b.meta.iter().position(|&m| m == 0) {
                (b.head[j], b.tail[j], b.meta[j]) = (k.0, k.1, k.2 << ID_BITS | id);
                return;
            }
            i = (i + 1) & mask;
        }
    }

    /// The id of the term `term` (1..=32 bytes, lowercase), adding it if
    /// new. Any other length gets an id nothing in the corpus maps to.
    fn intern(&mut self, term: &'a [u8]) -> u32 {
        let mut buf = [0u8; 40];
        let len = term.len().min(buf.len());
        buf[..len].copy_from_slice(&term[..len]);
        let k = key(&buf, 0, if term.len() > MAX_TERM { 40 } else { len });
        let id = self.terms.len() as u32;
        if k.2 != 0 {
            let found = self.get(k, self.home(k), term, 0);
            if found != self.none() {
                return found;
            }
            if id as usize >= self.buckets.len() * 2 {
                let bigger = vec![Bucket::default(); self.buckets.len() * 2];
                let old = std::mem::replace(&mut self.buckets, bigger);
                for b in &old {
                    for j in (0..3).filter(|&j| b.meta[j] != 0) {
                        self.place(
                            (b.head[j], b.tail[j], b.meta[j] >> ID_BITS),
                            b.meta[j] & ((1 << ID_BITS) - 1),
                        );
                    }
                }
            }
            self.place(k, id);
        }
        self.terms.push(term);
        id
    }

    /// The sink id: what a token that no query uses maps to.
    #[inline(always)]
    fn none(&self) -> u32 {
        self.terms.len() as u32
    }

    /// The id of the token `text[s..s + len]` whose key is `k`, or the sink
    /// id. `text` is only read (case-folded) when `len > 16`.
    #[inline(always)]
    fn get(&self, k: (u64, u64, u32), home: usize, text: &[u8], s: usize) -> u32 {
        let b = &self.buckets[home];
        let (mut id, mut hit) = (0u32, 0u32);
        for j in 0..3 {
            let m =
                ((b.head[j] == k.0) & (b.tail[j] == k.1) & (b.meta[j] >> ID_BITS == k.2)) as u32;
            id |= (b.meta[j] & ((1 << ID_BITS) - 1)) & m.wrapping_neg();
            hit |= m;
        }
        // Common case: a short token that hit, or missed a bucket with room.
        // The one branch is almost never taken, so it predicts well.
        if (k.2 > 16) | (k.2 == 0) | ((hit == 0) & (b.meta[2] != 0)) {
            return self.get_slow(k, text, s);
        }
        id | (self.none() & hit.wrapping_sub(1))
    }

    #[cold]
    fn get_slow(&self, k: (u64, u64, u32), text: &[u8], s: usize) -> u32 {
        if k.2 == 0 {
            return self.none();
        }
        let mask = self.buckets.len() - 1;
        let mut i = self.home(k);
        loop {
            let b = &self.buckets[i];
            for j in 0..3 {
                let id = b.meta[j] & ((1 << ID_BITS) - 1);
                if b.meta[j] == 0 {
                    return self.none();
                }
                if b.head[j] == k.0
                    && b.tail[j] == k.1
                    && b.meta[j] >> ID_BITS == k.2
                    && (k.2 <= 16
                        || self.terms[id as usize]
                            .iter()
                            .zip(&text[s..])
                            .all(|(&a, &b)| a == b | 0x20))
                {
                    return id;
                }
            }
            i = (i + 1) & mask;
        }
    }
}

// --- Queries ------------------------------------------------------------

/// Parsed queries in flat form: query `q`'s entries are
/// `entries[start[q]..start[q + 1]]`. A PHRASE's entries are its term ids.
/// A BOOL's are its first term id, then `op << 30 | term id` per operator
/// (0 AND, 1 OR, 2 NOT). An unparseable line has no entries.
struct Queries {
    entries: Vec<u32>,
    start: Vec<u32>,
    phrase: Vec<bool>,
}

impl Queries {
    fn len(&self) -> usize {
        self.phrase.len()
    }
    fn get(&self, q: usize) -> &[u32] {
        &self.entries[self.start[q] as usize..self.start[q + 1] as usize]
    }
}

fn parse_queries(data: &[u8]) -> (Queries, TermTable<'_>) {
    let mut table = TermTable::new();
    let mut qs = Queries {
        entries: Vec::new(),
        start: vec![0],
        phrase: Vec::new(),
    };
    for line in data.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let mut words = line.split(|&b| b == b' ');
        let kind = words.next();
        let phrase = kind == Some(&b"PHRASE"[..]);
        if kind == Some(&b"BOOL"[..]) {
            let mut first = true;
            let mut op = 0;
            for w in words {
                if first {
                    qs.entries.push(table.intern(w));
                } else if op == 0 {
                    op = match w {
                        b"AND" => 1,
                        b"OR" => 2,
                        _ => 3,
                    };
                } else {
                    qs.entries.push((op - 1) << 30 | table.intern(w));
                    op = 0;
                }
                first = false;
            }
        } else if phrase {
            for w in words {
                let t = w.strip_prefix(b"\"").unwrap_or(w);
                qs.entries
                    .push(table.intern(t.strip_suffix(b"\"").unwrap_or(t)));
            }
        }
        qs.start.push(qs.entries.len() as u32);
        qs.phrase.push(phrase);
    }
    (qs, table)
}

// --- One piece of the corpus --------------------------------------------

/// A piece may hold at most this many documents, so a piece-local doc
/// number fits a `u16`.
const MAX_DOCS: usize = 1 << 16;

/// A tokenized piece of the corpus plus its postings. Everything is reused
/// from piece to piece by one worker.
#[derive(Default)]
struct Piece {
    /// Id of the piece's first document.
    base: u32,
    /// Every token's term id (the sink id for tokens no query uses);
    /// document `i`'s run is `ids[dstart[i]..dstart[i + 1]]`.
    ids: Vec<u32>,
    dstart: Vec<u32>,
    /// Each document's distinct term ids, `uniq[ustart[i]..ustart[i + 1]]`.
    uniq: Vec<u32>,
    ustart: Vec<u32>,
    /// Term `t`'s sorted piece-local doc numbers: `post[off[t]..off[t + 1]]`.
    post: Vec<u16>,
    /// Per term: where its list starts in `post`, and its length.
    info: Vec<[u32; 2]>,
    /// Words per bitmap, and the bitmaps of the terms in at least that many
    /// documents (`map_at[t]` is where term `t`'s starts in `maps`).
    w: usize,
    maps: Vec<u64>,
    map_at: Vec<u32>,
    /// Bit `t` is set when term `t` is in some document of the piece. A
    /// small array that stays in L1, unlike `off`.
    present: Vec<u64>,
    /// Per term: the stamp of the last document seen with it, and how many
    /// documents of this piece contain it.
    last: Vec<u32>,
    count: Vec<u32>,
    stamp: u32,
    /// Scratch: the current document's token keys, start offsets and home
    /// buckets.
    keys: Vec<((u64, u64, u32), u32, u32)>,
}

impl Piece {
    #[inline(always)]
    fn list(&self, t: u32) -> &[u16] {
        let [at, n] = self.info[t as usize];
        &self.post[at as usize..(at + n) as usize]
    }

    /// Whether term `t` is in so many documents that a bitmap beats a list.
    #[inline(always)]
    fn dense(&self, t: u32) -> bool {
        self.info[t as usize][1] as usize >= self.w
    }

    #[inline(always)]
    fn map(&self, t: u32) -> &[u64] {
        &self.maps[self.map_at[t as usize] as usize..][..self.w]
    }

    #[inline(always)]
    fn has(&self, t: u32) -> bool {
        self.present[t as usize / 64] >> (t % 64) & 1 != 0
    }

    /// Whether the query `e` can match anything in this piece, judged only
    /// by which terms occur in it: a PHRASE needs every term, and a BOOL is
    /// run over presence (AND, OR; NOT can only remove). `false` is exact.
    fn possible(&self, e: &[u32], phrase: bool) -> bool {
        if phrase {
            return e.iter().all(|&t| self.has(t));
        }
        e[1..].iter().fold(self.has(e[0]), |p, &x| {
            let h = self.has(x & !(3 << 30));
            [p & h, p | h, p][(x >> 30) as usize]
        })
    }

    /// Looks up the current document's tokens and records its distinct terms.
    fn flush(&mut self, table: &TermTable, text: &[u8]) {
        let from = self.ids.len();
        self.ids.extend(
            self.keys
                .iter()
                .map(|&(k, s, h)| table.get(k, h as usize, text, s as usize)),
        );
        let mut u = self.uniq.len();
        self.uniq.resize(u + self.keys.len(), 0);
        self.stamp += 1;
        for &id in &self.ids[from..] {
            let new = self.last[id as usize] != self.stamp;
            self.last[id as usize] = self.stamp;
            self.count[id as usize] += new as u32;
            self.uniq[u] = id;
            u += new as usize;
        }
        self.uniq.truncate(u);
        self.keys.clear();
    }

    /// Tokenizes the whole documents at the front of `text` (at most
    /// `MAX_DOCS`), whose first line is document `first`'s... and builds their
    /// postings. Returns the number of bytes used.
    fn build(&mut self, text: &[u8], table: &TermTable) -> usize {
        let nt = table.terms.len() + 1;
        self.last.resize(nt, 0);
        self.count.clear();
        self.count.resize(nt, 0);
        for v in [
            &mut self.ids,
            &mut self.dstart,
            &mut self.uniq,
            &mut self.ustart,
        ] {
            v.clear();
        }
        let mut used = text.len();
        scan_tokens(text, |s, e| {
            // The first token of a line is its id: it opens a document.
            if s == 0 || text[s - 1] == b'\n' {
                if !self.dstart.is_empty() {
                    self.flush(table, text);
                }
                if self.dstart.len() == MAX_DOCS {
                    used = s;
                    return false;
                }
                if self.dstart.is_empty() {
                    self.base = text[s..e]
                        .iter()
                        .fold(0u32, |a, &c| a * 10 + (c - b'0') as u32);
                }
                self.dstart.push(self.ids.len() as u32);
                self.ustart.push(self.uniq.len() as u32);
            } else {
                // Start the bucket's cache miss now; the lookup is a few
                // dozen tokens later, when the line has arrived.
                let k = key(text, s, e);
                let home = table.home(k);
                prefetch(&table.buckets[home]);
                self.keys.push((k, s as u32, home as u32));
            }
            true
        });
        self.flush(table, text);
        self.dstart.push(self.ids.len() as u32);
        self.ustart.push(self.uniq.len() as u32);

        self.w = (self.dstart.len() - 1).div_ceil(64).max(1);
        self.info.clear();
        self.present.clear();
        self.present.resize(nt / 64 + 1, 0);
        let mut total = 0;
        for (t, &c) in self.count.iter().enumerate() {
            self.info.push([total, c]);
            total += c;
            self.present[t / 64] |= ((c != 0) as u64) << (t % 64);
        }
        self.post.clear();
        self.post.resize(total as usize, 0);
        let mut cur: Vec<u32> = self.info.iter().map(|i| i[0]).collect();
        for (d, w) in self.ustart.windows(2).enumerate() {
            for &id in &self.uniq[w[0] as usize..w[1] as usize] {
                self.post[cur[id as usize] as usize] = d as u16;
                cur[id as usize] += 1;
            }
        }
        self.maps.clear();
        self.map_at.resize(nt, 0);
        for t in (0..nt).filter(|&t| self.count[t] as usize >= self.w) {
            let (at, n, base) = (
                self.info[t][0] as usize,
                self.count[t] as usize,
                self.maps.len(),
            );
            self.map_at[t] = base as u32;
            self.maps.resize(base + self.w, 0);
            set_bits(&mut self.maps[base..], &self.post[at..at + n], true);
        }
        used
    }

    /// Document `d`'s token ids.
    #[inline(always)]
    fn doc(&self, d: u16) -> &[u32] {
        &self.ids[self.dstart[d as usize] as usize..self.dstart[d as usize + 1] as usize]
    }
}

// --- Set operations on sorted doc lists ---------------------------------

/// `a ∩ b` into `out`. When one list is far shorter, each of its ids is
/// found in the longer one by binary search over the part not yet passed,
/// which costs O(short · log long) instead of a full O(short + long) merge.
fn intersect(a: &[u16], b: &[u16], out: &mut Vec<u16>) {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    out.clear();
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
        return;
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
}

fn union(a: &[u16], b: &[u16], out: &mut Vec<u16>) {
    out.clear();
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
}

/// `a \ b` into `out`.
fn difference(a: &[u16], b: &[u16], out: &mut Vec<u16>) {
    out.clear();
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j >= b.len() || b[j] != x {
            out.push(x);
        }
    }
}

/// The members of the sorted list `l` whose bit in `m` is `keep`, into `out`.
fn filter(l: &[u16], m: &[u64], keep: bool, out: &mut Vec<u16>) {
    out.clear();
    out.resize(l.len(), 0);
    let mut n = 0;
    for &d in l {
        out[n] = d;
        n += ((m[d as usize / 64] >> (d % 64) & 1 != 0) == keep) as usize;
    }
    out.truncate(n);
}

/// Sets (or clears) the bit of every doc number in `l`.
fn set_bits(m: &mut [u64], l: &[u16], on: bool) {
    for &d in l {
        let bit = 1u64 << (d % 64);
        if on {
            m[d as usize / 64] |= bit;
        } else {
            m[d as usize / 64] &= !bit;
        }
    }
}

// --- Evaluation ---------------------------------------------------------

/// Reused buffers for the running result of one query.
#[derive(Default)]
struct Scratch {
    a: Vec<u16>,
    b: Vec<u16>,
    ma: Vec<u64>,
    mb: Vec<u64>,
    ord: Vec<u32>,
}

/// Where a running result lives: a term's list or bitmap in the piece, or
/// one of the two scratch buffers (lists and bitmaps have one pair each).
#[derive(Clone, Copy)]
enum Loc {
    Term(u32),
    A,
    B,
}

/// A finished answer within a piece: doc numbers as a sorted list or a bitmap.
enum Res<'a> {
    List(&'a [u16]),
    Map(&'a [u64]),
}

/// A running result: a sorted list of doc numbers, or a bitmap of them.
#[derive(Clone, Copy)]
struct Cur {
    loc: Loc,
    map: bool,
}

impl Scratch {
    fn list<'a>(&'a self, pc: &'a Piece, loc: Loc) -> &'a [u16] {
        match loc {
            Loc::Term(t) => pc.list(t),
            Loc::A => &self.a,
            Loc::B => &self.b,
        }
    }

    fn map<'a>(&'a self, pc: &'a Piece, loc: Loc) -> &'a [u64] {
        match loc {
            Loc::Term(t) => pc.map(t),
            Loc::A => &self.ma,
            Loc::B => &self.mb,
        }
    }

    /// Strictly left to right, no precedence (spec section 3). The running
    /// result starts as a borrow of the first term's list (or bitmap), so a
    /// query that never needs to combine anything copies nothing. Terms in
    /// many of the piece's documents are bitmaps, which combine in a handful
    /// of word operations instead of a branchy merge.
    fn eval_bool<'a>(&'a mut self, pc: &'a Piece, e: &[u32]) -> Res<'a> {
        let mut cur = Cur {
            loc: Loc::Term(e[0]),
            map: pc.dense(e[0]),
        };
        for &x in &e[1..] {
            let (op, r) = (x >> 30, x & !(3 << 30));
            let r_map = pc.dense(r);
            let lhs_empty = !cur.map && self.list(pc, cur.loc).is_empty();
            if (lhs_empty && op != 1) || (pc.info[r as usize][1] == 0 && op != 0) {
                // Nothing to add (OR, NOT of an empty list) or nothing left.
                continue;
            }
            if lhs_empty {
                cur = Cur {
                    loc: Loc::Term(r),
                    map: r_map,
                };
                continue;
            }
            let to_a = !matches!(cur.loc, Loc::A);
            let (mut ol, mut om) = if to_a {
                (std::mem::take(&mut self.a), std::mem::take(&mut self.ma))
            } else {
                (std::mem::take(&mut self.b), std::mem::take(&mut self.mb))
            };
            let (l, m): (&[u16], &[u64]) = if cur.map {
                (&[], self.map(pc, cur.loc))
            } else {
                (self.list(pc, cur.loc), &[])
            };
            let into_map = match (cur.map, r_map) {
                (false, false) => {
                    let r = pc.list(r);
                    [intersect, union, difference][op as usize](l, r, &mut ol);
                    false
                }
                (false, true) if op != 1 => {
                    filter(l, pc.map(r), op == 0, &mut ol);
                    false
                }
                (true, false) if op == 0 => {
                    filter(pc.list(r), m, true, &mut ol);
                    false
                }
                (false, true) => {
                    om.clear();
                    om.extend_from_slice(pc.map(r));
                    set_bits(&mut om, l, true);
                    true
                }
                (true, false) => {
                    om.clear();
                    om.extend_from_slice(m);
                    set_bits(&mut om, pc.list(r), op == 1);
                    true
                }
                (true, true) => {
                    om.clear();
                    om.extend(
                        m.iter()
                            .zip(pc.map(r))
                            .map(|(&a, &b)| [a & b, a | b, a & !b][op as usize]),
                    );
                    true
                }
            };
            if to_a {
                (self.a, self.ma) = (ol, om);
            } else {
                (self.b, self.mb) = (ol, om);
            }
            cur = Cur {
                loc: if to_a { Loc::A } else { Loc::B },
                map: into_map,
            };
        }
        if cur.map {
            Res::Map(self.map(pc, cur.loc))
        } else {
            Res::List(self.list(pc, cur.loc))
        }
    }

    /// Documents whose token ids contain `e` as a consecutive run.
    /// Candidates are the intersection of every term's doc list, rarest
    /// first (usually a phrase's rare anchor term leaves a handful); each is
    /// then checked by scanning its ids for the rarest term and comparing
    /// the run around it. A repeated term (`"x x x"`) needs that many
    /// consecutive `x`s, which the comparison checks directly.
    fn eval_phrase<'a>(&'a mut self, pc: &'a Piece, e: &[u32]) -> &'a [u16] {
        self.a.clear();
        if e.iter().any(|&t| pc.list(t).is_empty()) {
            // Some term is absent from the whole piece (nearly always).
            return &self.a;
        }
        let by_rarity = &mut self.ord;
        by_rarity.clear();
        by_rarity.extend_from_slice(e);
        by_rarity.sort_unstable_by_key(|&t| pc.list(t).len());
        by_rarity.dedup();
        self.a.extend_from_slice(pc.list(by_rarity[0]));
        for &t in &by_rarity[1..] {
            if self.a.is_empty() {
                break;
            }
            intersect(&self.a, pc.list(t), &mut self.b);
            std::mem::swap(&mut self.a, &mut self.b);
        }
        let pivot = e.iter().position(|&t| t == by_rarity[0]).unwrap();
        self.a.retain(|&d| {
            let ids = pc.doc(d);
            ids.iter().enumerate().any(|(i, &t)| {
                t == e[pivot] && i >= pivot && ids.get(i - pivot..i - pivot + e.len()) == Some(e)
            })
        });
        &self.a
    }
}

/// One worker's answers so far: per query the match count and id sum, and a
/// log of `query << 32 | id` for the first ten ids it saw per query. A
/// worker sees ids in ascending order, so those are its ten smallest.
struct Acc {
    count: Vec<u32>,
    sum: Vec<u64>,
    log: Vec<u64>,
}

/// `POS_MASK[j]` has the bits whose position has bit `j` set.
const POS_MASK: [u64; 6] = [
    0xAAAA_AAAA_AAAA_AAAA,
    0xCCCC_CCCC_CCCC_CCCC,
    0xF0F0_F0F0_F0F0_F0F0,
    0xFF00_FF00_FF00_FF00,
    0xFFFF_0000_FFFF_0000,
    0xFFFF_FFFF_0000_0000,
];

/// Runs every query against the piece `pc`, folding its matches into `acc`.
fn answer_piece(pc: &Piece, qs: &Queries, sc: &mut Scratch, acc: &mut Acc, full: &[AtomicU32]) {
    for (q, full_at) in full.iter().enumerate() {
        let e = qs.get(q);
        if e.is_empty() {
            continue;
        }
        if !pc.possible(e, qs.phrase[q]) {
            continue;
        }
        let res = if qs.phrase[q] {
            Res::List(sc.eval_phrase(pc, e))
        } else {
            sc.eval_bool(pc, e)
        };
        // Count and sum of the piece-local numbers; a bitmap is never
        // spelled out: the sum of positions is six masked popcounts a word.
        let (n, sum) = match res {
            Res::List(l) => (l.len() as u64, l.iter().map(|&d| d as u64).sum::<u64>()),
            Res::Map(m) => m.iter().enumerate().fold((0, 0), |(n, sum), (w, &x)| {
                let c = x.count_ones() as u64;
                let by_bit: u64 = (0..6)
                    .map(|j| ((x & POS_MASK[j]).count_ones() as u64) << j)
                    .sum();
                (n + c, sum + 64 * w as u64 * c + by_bit)
            }),
        };
        if n == 0 {
            continue;
        }
        let seen = acc.count[q] as usize;
        acc.count[q] += n as u32;
        acc.sum[q] += n * pc.base as u64 + sum;
        // Once some worker holds ten ids from pieces up to base B, ids from
        // pieces after B can never be among the ten smallest.
        let want = if pc.base > full_at.load(Ordering::Relaxed) {
            0
        } else {
            10usize.saturating_sub(seen)
        };
        if seen as u64 + n >= 10 {
            full_at.fetch_min(pc.base, Ordering::Relaxed);
        }
        let key = (q as u64) << 32 | pc.base as u64;
        match res {
            Res::List(l) => acc.log.extend(l.iter().take(want).map(|&d| key + d as u64)),
            Res::Map(m) => {
                let mut left = want;
                for (w, &x) in m.iter().enumerate() {
                    let mut x = x;
                    while x != 0 && left > 0 {
                        acc.log
                            .push(key + (w * 64) as u64 + x.trailing_zeros() as u64);
                        (left, x) = (left - 1, x & (x - 1));
                    }
                }
            }
        }
    }
}

/// Where piece `p` of size `piece` bytes starts: the first line start at or
/// after `p * piece`.
fn piece_start(docs: &[u8], p: usize, piece: usize) -> usize {
    let at = (p * piece).min(docs.len());
    if at == 0 || at == docs.len() {
        return at;
    }
    docs[at - 1..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(docs.len(), |i| at + i)
}

/// Workers take pieces from a shared counter (so a slow piece can't strand
/// the rest) and answer all queries on each.
fn answer_all(
    docs: &[u8],
    qs: &Queries,
    table: &TermTable,
    threads: usize,
    piece: usize,
) -> Vec<Acc> {
    let n_pieces = docs.len().div_ceil(piece);
    let next = AtomicUsize::new(0);
    let full: Vec<AtomicU32> = (0..qs.len()).map(|_| AtomicU32::new(u32::MAX)).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.max(1))
            .map(|_| {
                s.spawn(|| {
                    let (mut pc, mut sc) = (Piece::default(), Scratch::default());
                    let mut acc = Acc {
                        count: vec![0; qs.len()],
                        sum: vec![0; qs.len()],
                        log: Vec::new(),
                    };
                    loop {
                        let p = next.fetch_add(1, Ordering::Relaxed);
                        if p >= n_pieces {
                            return acc;
                        }
                        let (mut from, to) =
                            (piece_start(docs, p, piece), piece_start(docs, p + 1, piece));
                        while from < to {
                            from += pc.build(&docs[from..to], table);
                            answer_piece(&pc, qs, &mut sc, &mut acc, &full);
                        }
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker panicked"))
            .collect()
    })
}

/// One output line's content: `count sum id_1 .. id_m`, m = min(10, count).
#[derive(Clone, Copy, Default)]
struct Answer {
    count: u32,
    sum: u64,
    first: [u32; 10],
}

/// Combines the workers' accumulators, in parallel over ranges of queries:
/// counts and sums add, and each query's ten smallest ids are the ten
/// smallest of the workers' logs.
fn merge(accs: &[Acc], n: usize, threads: usize) -> Vec<Answer> {
    let mut out = vec![Answer::default(); n];
    let per = n.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (t, part) in out.chunks_mut(per).enumerate() {
            s.spawn(move || {
                let lo = t * per;
                for (i, a) in part.iter_mut().enumerate() {
                    a.count = accs.iter().map(|x| x.count[lo + i]).sum();
                    a.sum = accs.iter().map(|x| x.sum[lo + i]).sum();
                }
                let mine = |&&e: &&u64| (lo..lo + part.len()).contains(&((e >> 32) as usize));
                let mut log: Vec<u64> = accs
                    .iter()
                    .flat_map(|x| &x.log)
                    .filter(mine)
                    .copied()
                    .collect();
                log.sort_unstable();
                for run in log.chunk_by(|a, b| a >> 32 == b >> 32) {
                    let a = &mut part[(run[0] >> 32) as usize - lo];
                    for (slot, &e) in a.first.iter_mut().zip(run) {
                        *slot = e as u32;
                    }
                }
            });
        }
    });
    out
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

/// Bytes of `docs.txt` per piece. Small enough that a piece's token ids and
/// postings stay in a core's cache while all 50,000 queries run on it,
/// large enough that the per-piece cost of running every query is amortised.
const PIECE_BYTES: usize = 1 << 20;

/// Set `TRACK_B_TIMING=1` to print a per-phase breakdown to stderr.
fn run(
    docs_path: &str,
    queries_path: &str,
    output_path: &str,
    threads: usize,
    piece: usize,
) -> std::io::Result<()> {
    let verbose = std::env::var("TRACK_B_TIMING").is_ok();
    macro_rules! phase {
        ($label:expr, $body:expr) => {{
            let t = std::time::Instant::now();
            let v = $body;
            if verbose {
                eprintln!(
                    "track-b: {} in {:.1}ms",
                    $label,
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
            v
        }};
    }

    let queries_data = phase!("map queries", map_file(queries_path)?);
    let (queries, table) = phase!("parse queries", parse_queries(queries_data));
    let docs_data = phase!("map docs", map_file(docs_path)?);
    let accs = phase!(
        "index and answer",
        answer_all(docs_data, &queries, &table, threads, piece)
    );
    let answers = phase!("merge", merge(&accs, queries.len(), threads));
    let out = phase!("render", render(&answers));
    phase!("write output", std::fs::write(output_path, out))?;

    // Freeing the tables at exit is pure cost; the OS reclaims them anyway
    // (track-b/PERF.md).
    std::mem::forget((accs, table, queries));
    Ok(())
}

fn main() {
    let docs_path = std::env::var("DOCS_PATH").unwrap_or_else(|_| "/data/docs.txt".to_string());
    let queries_path =
        std::env::var("QUERIES_PATH").unwrap_or_else(|_| "/data/queries.txt".to_string());
    let output_path =
        std::env::var("OUTPUT_PATH").unwrap_or_else(|_| "/out/results.txt".to_string());
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let start = std::time::Instant::now();
    match run(
        &docs_path,
        &queries_path,
        &output_path,
        threads,
        PIECE_BYTES,
    ) {
        Ok(()) => {
            eprintln!(
                "track-b: done in {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
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
