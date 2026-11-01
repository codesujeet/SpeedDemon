//! Speed Demon 2026, Track B (Index), in Rust with no dependencies.
//!
//! The queries are read first and only their terms are looked for. One scan of
//! docs.txt turns each token into a term id through a 64-bit hash table, checks
//! phrases where their rarest term occurs, and files BOOL terms as bitmaps or
//! per-term lists; the BOOL queries then run on sorted lists or bitmaps.
use std::arch::x86_64::*;
use std::borrow::Cow;
use std::fmt::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Barrier, Mutex, OnceLock};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn _exit(status: i32) -> !;
}

const THREADS: usize = 12;
const BUCKETS: usize = 32;
const GAP: u32 = 15;
const ID_BITS: u32 = 18;
const ID_MASK: u64 = (1 << ID_BITS) - 1;
const BIG: usize = 768;

fn map(path: &str) -> &'static [u8] {
    let file = std::fs::File::open(path).expect(path);
    let len = file.metadata().unwrap().len() as usize;
    unsafe {
        let p = mmap(std::ptr::null_mut(), len + 4096, 1, 0x22, -1, 0);
        let ok = len == 0 || mmap(p, len, 1, 0x12, file.as_raw_fd(), 0) == p;
        assert!(p as isize != -1 && ok);
        std::slice::from_raw_parts(p, len)
    }
}

type Job = &'static (dyn Fn(usize) + Sync);
static JOB: Mutex<Option<Job>> = Mutex::new(None);
static GATE: OnceLock<Barrier> = OnceLock::new();

fn on_threads<R: Send>(f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    if GATE.set(Barrier::new(THREADS)).is_ok() {
        for t in 1..THREADS {
            std::thread::spawn(move || loop {
                GATE.get().unwrap().wait();
                let job = JOB.lock().unwrap().unwrap(); // the guard is dropped here, before the job runs
                job(t);
                GATE.get().unwrap().wait();
            });
        }
    }
    let gate = GATE.get().unwrap();
    let out: Vec<Mutex<Option<R>>> = (0..THREADS).map(|_| Mutex::new(None)).collect();
    let run = |t: usize| *out[t].lock().unwrap() = Some(f(t));
    let run: &(dyn Fn(usize) + Sync) = &run;
    *JOB.lock().unwrap() = Some(unsafe { std::mem::transmute::<_, Job>(run) });
    (gate.wait(), run(0), gate.wait());
    let take = |r: Mutex<Option<R>>| r.into_inner().unwrap().unwrap();
    out.into_iter().map(take).collect()
}

fn number(text: &[u8], i: usize) -> u32 {
    let digits = text[i..].iter().take_while(|b| b.is_ascii_digit());
    digits.fold(0, |v, &b| v * 10 + (b - b'0') as u32)
}

// Three AES rounds over the masked 32 bytes (the high half is the first round key): every output bit
// depends on every input byte, in seven instructions where a multiply mix took thirty.
#[target_feature(enable = "aes")]
unsafe fn hash(p: *const u8, len: usize) -> u64 {
    static KEEP: [u128; 4] = [!0, !0, 0, 0];
    let mask = _mm256_loadu_si256(KEEP.as_ptr().cast::<u8>().add(32 - len).cast());
    let v = _mm256_or_si256(_mm256_loadu_si256(p.cast()), _mm256_set1_epi8(0x20));
    let w = _mm256_and_si256(v, mask);
    let k = _mm_set1_epi64x(0x9E37_79B9_7F4A_7C15u64 as i64);
    let x = _mm_aesenc_si128(_mm256_castsi256_si128(w), _mm256_extracti128_si256::<1>(w));
    let x = _mm_aesenc_si128(_mm_aesenc_si128(x, k), k);
    // The top bit is always set, so that no hash has the tag of a free slot.
    _mm_cvtsi128_si64(x) as u64 | 1 << 63
}

// Open addressing over 32-byte buckets of four slots, each the hash's top bits above
// id + 1 (0 if free). One compare settles nearly every lookup, a term or not, with no
// branch on which: telling those apart in a loop mispredicted about one token in three.
struct Terms {
    slot: &'static mut [__m256i],
    n: usize,
}

impl Terms {
    fn find(&self, h: u64) -> u32 {
        let mask = self.slot.len() - 1;
        let mut i = h as usize & mask;
        unsafe {
            let tag = _mm256_set1_epi64x((h & !ID_MASK) as i64);
            let keep = _mm256_set1_epi64x(!ID_MASK as i64);
            loop {
                let v = *self.slot.get_unchecked(i);
                let eq = _mm256_cmpeq_epi64(_mm256_and_si256(v, keep), tag);
                let free = _mm256_cmpeq_epi64(v, _mm256_setzero_si256());
                if _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_or_si256(eq, free))) != 0 {
                    // The hit slot's id + 1 by OR-ing the lanes: 0 on a miss, which becomes the id u32::MAX.
                    let x = _mm256_and_si256(v, eq);
                    let y = _mm_or_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256::<1>(x));
                    return _mm_cvtsi128_si64(_mm_or_si128(y, _mm_unpackhi_epi64(y, y))) as u32 & ID_MASK as u32;
                }
                i = (i + 1) & mask;
            }
        }
    }

    fn renumber(&mut self, new: &[u32]) {
        for b in self.slot.iter_mut() {
            let w: &mut [u64; 4] = unsafe { std::mem::transmute(b) };
            for s in w.iter_mut().filter(|s| **s != 0) {
                *s = (*s & !ID_MASK) | (new[(*s & ID_MASK) as usize - 1] + 1) as u64;
            }
        }
    }

    fn insert(&mut self, h: u64) -> u32 {
        let (mask, mut i) = (self.slot.len() - 1, h as usize & (self.slot.len() - 1));
        loop {
            let w: &mut [u64; 4] = unsafe { std::mem::transmute(&mut self.slot[i]) };
            for s in w.iter_mut() {
                if *s == 0 {
                    self.n += 1;
                    *s = (h & !ID_MASK) | self.n as u64;
                    return self.n as u32 - 1;
                }
                if *s >> ID_BITS == h >> ID_BITS {
                    return (*s & ID_MASK) as u32 - 1;
                }
            }
            i = (i + 1) & mask;
        }
    }
}

unsafe fn classes(p: *const u8) -> (u64, u64) {
    let half = |q: *const u8| {
        let v = _mm256_loadu_si256(q.cast());
        let c = |b: u8| _mm256_set1_epi8(b as i8);
        let le = |x: __m256i, n: u8| _mm256_cmpeq_epi8(_mm256_subs_epu8(x, c(n)), c(0));
        let a = le(_mm256_sub_epi8(_mm256_or_si256(v, c(0x20)), c(b'a')), 25);
        let d = le(_mm256_sub_epi8(v, c(b'0')), 9);
        let mask = |x: __m256i| _mm256_movemask_epi8(x) as u32 as u64;
        (mask(_mm256_or_si256(a, d)), mask(_mm256_cmpeq_epi8(v, c(b'\n'))))
    };
    let ((w0, n0), (w1, n1)) = (half(p), half(p.add(32)));
    (w0 | w1 << 32, n0 | n1 << 32)
}

#[target_feature(enable = "aes")]
unsafe fn tokens(mut p: *const u8, t: &Terms, ids: &mut Vec<u32>, pre: *const u16) -> *const u8 {
    const LAG: usize = 4;
    let mut ring = [0u64; LAG];
    let (mut n, mut out, mask) = (0usize, ids.len(), t.slot.len() - 1);
    let (mut buf, mut cap) = (ids.as_mut_ptr(), ids.capacity());
    loop {
        // out <= cap always, so the subtraction cannot wrap; a window writes at most 32 + LAG ids past out.
        if cap - out < 32 + LAG {
            (ids.set_len(out), ids.reserve(1 << 12), buf = ids.as_mut_ptr(), cap = ids.capacity());
        }
        let mut look = |&h: &u64| {
            let id = t.find(h);
            _mm_prefetch::<_MM_HINT_T0>(pre.wrapping_add(id as usize).cast());
            (*buf.add(out), out) = (id.wrapping_sub(1), out + 1);
        };
        let mut push = |q: *const u8, len: u32| {
            let h = if len <= 32 { hash(q, len as usize) } else { 1 << 63 };
            _mm_prefetch::<_MM_HINT_T0>(t.slot.as_ptr().add(h as usize & mask).cast());
            if n >= LAG {
                look(&ring[n % LAG]);
            }
            (ring[n % LAG], n) = (h, n + 1);
        };
        _mm_prefetch::<_MM_HINT_T0>(p.wrapping_add(2048).cast());
        let (word, nl) = classes(p);
        let stop = nl.trailing_zeros();
        // Token starts and ends in the bytes before the newline; a token that reaches the window's last byte has
        // no end here, so its start is left over and the next window begins there.
        let w = word & nl.wrapping_sub(1) & !nl;
        let (mut starts, mut ends) = (w & !(w << 1), w & !(w >> 1 | 1 << 63));
        while ends != 0 {
            let s = starts.trailing_zeros();
            let len = ends.trailing_zeros() + 1 - s;
            (starts, ends) = (starts & starts.wrapping_sub(1), ends & ends.wrapping_sub(1));
            push(p.add(s as usize), len);
        }
        let mut next = starts.trailing_zeros().min(64) as usize;
        if next == 0 {
            next = (0..).find(|&i| !(*p.add(i)).is_ascii_alphanumeric()).unwrap();
            push(p, next as u32);
        }
        if nl != 0 {
            (n.saturating_sub(LAG)..n).for_each(|i| look(&ring[i % LAG]));
            ids.set_len(out);
            return p.add(stop as usize + 1);
        }
        p = p.add(next);
    }
}

enum Docs<'a> {
    List(Vec<u32>),
    Bits(Cow<'a, [u64]>),
}

fn has(bits: &[u64], d: u32) -> bool {
    bits[d as usize / 64] >> (d % 64) & 1 != 0
}

fn apply<'a>(cur: Docs<'a>, op: u8, term: Docs<'a>, words: usize) -> Docs<'a> {
    use Docs::*;
    let bit = |d: u32| (d as usize / 64, 1u64 << (d % 64));
    match (cur, op, term) {
        (Bits(mut a), _, Bits(b)) => {
            for (x, &y) in a.to_mut().iter_mut().zip(b.iter()) {
                *x = match op {
                    b'A' => *x & y,
                    b'O' => *x | y,
                    _ => *x & !y,
                };
            }
            Bits(a)
        }
        (Bits(b), b'A', List(mut v)) | (List(mut v), b'A' | b'N', Bits(b)) => {
            (v.retain(|&d| has(&b, d) == (op == b'A')), List(v)).1
        }
        (Bits(mut a), _, List(b)) | (List(b), _, Bits(mut a)) => {
            let x = a.to_mut();
            for (w, m) in b.into_iter().map(bit) {
                x[w] = if op == b'O' { x[w] | m } else { x[w] & !m };
            }
            Bits(a)
        }
        (List(mut a), b'A' | b'N', List(b)) if a.len() * 16 < b.len() => {
            let mut j = 0;
            a.retain(|&d| {
                j += b[j..].partition_point(|&x| x < d);
                (j < b.len() && b[j] == d) == (op == b'A')
            });
            List(a)
        }
        (List(a), _, List(b)) if a.len() + b.len() > if op == b'O' { 2048 } else { BIG } => {
            let mut bits = vec![0u64; words];
            b.iter().for_each(|&d| bits[d as usize / 64] |= 1 << (d % 64));
            apply(List(a), op, Bits(Cow::Owned(bits)), words)
        }
        (List(a), b'O', List(b)) => {
            let (mut v, mut i, mut j) = (Vec::with_capacity(a.len() + b.len()), 0, 0);
            while i < a.len() && j < b.len() {
                let (x, y) = (a[i], b[j]);
                v.push(x.min(y));
                (i, j) = (i + (x <= y) as usize, j + (y <= x) as usize);
            }
            v.extend_from_slice(&a[i..]);
            v.extend_from_slice(&b[j..]);
            List(v)
        }
        (List(mut a), _, List(b)) => {
            let (mut i, mut j, mut k) = (0, 0, 0);
            while i < a.len() && j < b.len() {
                let (x, y) = (a[i], b[j]);
                a[k] = x;
                k += if op == b'A' { x == y } else { x < y } as usize;
                (i, j) = (i + (x <= y) as usize, j + (y <= x) as usize);
            }
            let rest = if op == b'A' { 0 } else { a.len() - i };
            (a.copy_within(i..i + rest, k), a.truncate(k + rest));
            List(a)
        }
    }
}

unsafe fn bit_sums(b: &[u64]) -> (u64, u64) {
    let t = |x: u128| _mm256_broadcastsi128_si256(std::mem::transmute(x));
    let cnt = t(0x0403_0302_0302_0201_0302_0201_0201_0100);
    let lo = t(0x0606_0505_0404_0303_0303_0202_0101_0000);
    let hi = t(0x1612_110d_100c_0b07_0f0b_0a06_0905_0400);
    let (f, z) = (_mm256_set1_epi8(15), _mm256_setzero_si256());
    let at = _mm256_set1_epi64x(0x3830_2820_1810_0800);
    let (mut acc, mut word) = ([z; 3], _mm256_setr_epi64x(0, 1, 2, 3));
    for q in b.chunks_exact(4) {
        let v = _mm256_loadu_si256(q.as_ptr().cast());
        let l = _mm256_and_si256(v, f);
        let h = _mm256_and_si256(_mm256_srli_epi16::<4>(v), f);
        let c = _mm256_add_epi8(_mm256_shuffle_epi8(cnt, l), _mm256_shuffle_epi8(cnt, h));
        let s = _mm256_add_epi8(_mm256_shuffle_epi8(lo, l), _mm256_shuffle_epi8(hi, h));
        let n = _mm256_sad_epu8(c, z);
        acc[0] = _mm256_add_epi64(acc[0], n);
        acc[1] = _mm256_add_epi64(acc[1], _mm256_mul_epu32(n, word));
        let j = _mm256_madd_epi16(_mm256_maddubs_epi16(c, at), _mm256_set1_epi16(1));
        acc[2] = _mm256_add_epi64(acc[2], _mm256_add_epi64(_mm256_sad_epu8(s, z), j));
        word = _mm256_add_epi64(word, _mm256_set1_epi64x(4));
    }
    let total = |k: usize| -> u64 { std::mem::transmute::<_, [u64; 4]>(acc[k]).iter().sum() };
    let j = total(2);
    let sum = 64 * total(1) + (j as u32 as u64) + (j >> 32);
    (total(0), sum)
}

fn line(out: &mut String, docs: &Docs) {
    match docs {
        Docs::List(v) => {
            let sum: u64 = v.iter().map(|&d| d as u64).sum();
            write!(out, "{} {sum}", v.len()).unwrap();
            v.iter().take(10).for_each(|d| write!(out, " {d}").unwrap());
        }
        Docs::Bits(b) => {
            let (count, sum) = unsafe { bit_sums(b) };
            write!(out, "{count} {sum}").unwrap();
            let mut rem = 10;
            for (w, &x) in b.iter().enumerate() {
                let mut m = x;
                while m != 0 && rem > 0 {
                    write!(out, " {}", w as u32 * 64 + m.trailing_zeros()).unwrap();
                    (m, rem) = (m & (m - 1), rem - 1);
                }
                if rem == 0 {
                    break;
                }
            }
        }
    }
    out.push('\n');
}

fn walk(s: &[u8], first: u32, t: &Terms, pre: *const u16, mut f: impl FnMut(u32, &[u32])) -> u32 {
    let (mut p, end) = (s.as_ptr(), s.as_ptr_range().end);
    let (mut doc, mut ids) = (first, Vec::with_capacity(1 << 12));
    while p < end {
        unsafe {
            while *p != b'\t' {
                p = p.add(1);
            }
            ids.clear();
            p = tokens(p, t, &mut ids, pre);
        }
        (f(doc, &ids), doc += 1);
    }
    doc - first
}

struct Units {
    v: Vec<u16>,
    prev: u32,
}

impl Units {
    fn add(&mut self, offset: u32, doc: u32) {
        let gap = doc - std::mem::replace(&mut self.prev, doc);
        self.v.push((offset << 4 | gap.min(GAP)) as u16);
        if gap >= GAP {
            self.v.push(gap as u16);
        }
    }
}

fn main() {
    unsafe { mallopt(-3, 1 << 16) };
    let qtext = map("/data/queries.txt");
    // Every token's lookup lands on a random line of the term table, so it is put on 2 MB pages (one TLB
    // entry instead of 512), faulted in before huge pages are turned off for the rest, where a page written
    // only in part would cost memory. It is handed back after the scan.
    let n = (qtext.len() / 32).next_power_of_two();
    let slot = unsafe {
        let len = (n * 32).next_multiple_of(1 << 21);
        let raw = mmap(std::ptr::null_mut(), len + (1 << 21), 3, 0x22, -1, 0);
        let p = raw.add(raw.align_offset(1 << 21));
        (madvise(p, len, 14), std::ptr::write_volatile(p, 0));
        prctl(41, 1u64, 0u64, 0u64, 0u64);
        std::slice::from_raw_parts_mut(p.cast::<__m256i>(), n)
    };
    let mut terms = Terms { slot, n: 0 };
    let (mut spans, mut ids, mut ops, mut hs) = (vec![], vec![], vec![], vec![]);
    // Each word's length is one count of the letter-or-digit bits after it; a word ends at a space, the closing
    // quote or the LF. Terms are lower case and operators upper case, so the first byte tells them apart.
    let mut i = 0;
    while i < qtext.len() {
        let (phrase, from) = (qtext[i] == b'P', ids.len() as u32);
        i += if phrase { 8 } else { 5 };
        loop {
            let len = unsafe { (!classes(qtext.as_ptr().add(i)).0).trailing_zeros() } as usize;
            let word = &qtext[i..i + len];
            match word[0].is_ascii_uppercase() {
                true => *ops.last_mut().unwrap() = word[0],
                false => (ids.push(0), hs.push(unsafe { hash(word.as_ptr(), word.len()) }), ops.push(0)).2,
            }
            i += len + 1;
            if qtext[i - 1] != b' ' {
                break;
            }
        }
        i += phrase as usize;
        spans.push((phrase, from, ids.len() as u32));
    }
    for (k, &h) in hs.iter().enumerate() {
        let ahead = hs.get(k + 16).map_or(0, |&h| h as usize & (terms.slot.len() - 1));
        unsafe { _mm_prefetch::<_MM_HINT_T0>(terms.slot.as_ptr().add(ahead).cast()) };
        ids[k] = terms.insert(h);
    }
    drop(hs);
    let nterms = terms.n;
    let text = map("/data/docs.txt");
    let mut cut: Vec<usize> = (0..=THREADS).map(|t| text.len() * t / THREADS).collect();
    for c in &mut cut[1..THREADS] {
        while *c < text.len() && text[*c - 1] != b'\n' {
            *c += 1;
        }
    }
    let mut last_line = text.len().saturating_sub(1);
    while last_line > 0 && text[last_line - 1] != b'\n' {
        last_line -= 1;
    }
    let ndocs = (!text.is_empty()) as usize * (number(text, last_line) as usize + 1);
    let first_doc = |c: usize| text.get(c).map_or(ndocs as u32, |_| number(text, c));
    let firsts: Vec<u32> = cut[..THREADS].iter().map(|&c| first_doc(c)).collect();

    let (nbits, a0, lists, nkeep, anchored, astart) = {
        let mut in_bool = vec![false; nterms];
        for &(_, from, to) in spans.iter().filter(|s| !s.0) {
            ids[from as usize..to as usize].iter().for_each(|&id| in_bool[id as usize] = true);
        }
        let samples = on_threads(|t| {
            let (mut df, mut last) = (vec![0u32; nterms + 1], vec![0u16; nterms + 1]);
            let sample = &text[cut[t]..cut[t] + (cut[t + 1] - cut[t]) / 64];
            let n = walk(sample, 0, &terms, last.as_ptr(), |doc, toks| {
                let mark = (doc + 1) as u16;
                for &id in toks {
                    let id = id.min(nterms as u32);
                    if std::mem::replace(&mut last[id as usize], mark) != mark {
                        df[id as usize] += 1;
                    }
                }
            });
            (n as usize, df)
        });
        let sampled: usize = samples.iter().map(|s| s.0).sum();
        let mut df = vec![0u32; nterms + 1];
        for (_, d) in samples {
            df.iter_mut().zip(d).for_each(|(a, b)| *a += b);
        }
        let (mut anchored, mut anchor) = (vec![], vec![false; nterms]);
        for (q, &(_, from, to)) in spans.iter().enumerate().filter(|s| s.1 .0) {
            let rarity = |&j: &u32| df[ids[j as usize] as usize];
            let j = (from..to).min_by_key(rarity).unwrap();
            let a = ids[j as usize];
            (anchored.push((a, q as u32, j - from)), anchor[a as usize] = true);
        }
        let class = |i: usize| match (in_bool[i], anchor[i]) {
            (true, false) => (df[i] as usize * 32 <= sampled) as usize,
            (true, true) => 2,
            (false, true) => 3,
            (false, false) => 4,
        };
        // The new order is the class, then the sampled document count, most frequent first (capped at 4095), then
        // the old id: a counting sort on a 15-bit key.
        let key = |i: usize| class(i) << 12 | 4095 - (df[i] as usize).min(4095);
        let mut at = vec![0u32; 5 << 12 | 1];
        (0..nterms).for_each(|i| at[key(i) + 1] += 1);
        (0..5 << 12).for_each(|k| at[k + 1] += at[k]);
        let [nbits, a0, lists, nkeep] = [1, 2, 3, 4].map(|c| at[c << 12] as usize);
        let new: Vec<u32> = (0..nterms).map(|i| (at[key(i)] += 1, at[key(i)] - 1).1).collect();
        terms.renumber(&new);
        ids.iter_mut().for_each(|id| *id = new[*id as usize]);
        anchored.iter_mut().for_each(|a| a.0 = new[a.0 as usize]);
        anchored.sort_unstable();
        let first_of = |i: usize| anchored.partition_point(|a| (a.0 as usize) < i);
        let astart: Vec<usize> = (a0..=nkeep).map(first_of).collect();
        (nbits, a0, lists, nkeep, anchored, astart)
    };

    let words = ndocs.div_ceil(256) * 4;
    let bits: Vec<AtomicU64> = unsafe { std::mem::transmute(vec![0u64; nbits * words]) };
    let found = Mutex::new(vec![]);
    let scans = on_threads(|t| {
        let (mut count, mut last) = (vec![0u16; nkeep + 1], vec![0u16; nkeep + 1]);
        let (mut mine, mut kept, mut anchors) = (vec![], vec![0; 1 << 12], vec![0; 1 << 12]);
        let mut buckets: Vec<Units> = (0..BUCKETS).map(|_| Units { v: Vec::with_capacity(1 << 15), prev: 0 }).collect();
        let first = firsts[t];
        let (mut pending, mut block) = (vec![0u64; nbits + 1], first as usize / 64);
        let flush = |pending: &mut [u64], w: usize| {
            for (b, x) in pending[..nbits].iter_mut().enumerate() {
                if *x != 0 {
                    bits[b * words + w].fetch_or(std::mem::take(x), Relaxed);
                }
            }
        };
        let (share, pre) = (&text[cut[t]..cut[t + 1]], last.as_ptr());
        walk(share, first, &terms, pre, |doc, toks| {
            if doc as usize / 64 != block {
                let w = std::mem::replace(&mut block, doc as usize / 64);
                flush(&mut pending, w);
            }
            let (mark, bit) = ((doc - first + 1) as u16, 1u64 << (doc % 64));
            let (nkeep, a0, nbits, lists, off) = (nkeep, a0, nbits, lists, doc - first);
            let (last, pending) = (&mut last[..=nkeep], &mut pending[..=nbits]);
            kept.resize(kept.len().max(toks.len()), 0);
            anchors.resize(anchors.len().max(toks.len()), 0);
            let (mut n, mut na, mut nk) = (0, 0, 0);
            unsafe {
                for (at, &id) in toks.iter().enumerate() {
                    let i = id as usize;
                    *pending.get_unchecked_mut(i.min(nbits)) |= bit; // slot nbits is a sink, flush skips it
                    *anchors.get_unchecked_mut(n) = at;
                    n += (i.wrapping_sub(nbits) < nkeep - nbits) as usize;
                }
                // Only the terms past the bitmaps and short of the phrase-only ones need the rest: `anchors` is
                // filtered in place into the phrase anchors, and `kept` gets a term's first token in the document.
                for j in 0..n {
                    let at = *anchors.get_unchecked(j);
                    let i = *toks.get_unchecked(at) as usize;
                    let new = std::mem::replace(last.get_unchecked_mut(i), mark) != mark;
                    *anchors.get_unchecked_mut(na) = at;
                    na += (i >= a0) as usize;
                    *kept.get_unchecked_mut(nk) = i as u32;
                    nk += (new & (i < lists)) as usize;
                }
            }
            for &at in &anchors[..na] {
                let k = (toks[at] as usize).min(nkeep) - a0;
                for &(_, q, j) in &anchored[astart[k]..astart[k + 1]] {
                    let span = spans[q as usize];
                    let phrase = &ids[span.1 as usize..span.2 as usize];
                    let s = at.wrapping_sub(j as usize);
                    if toks.get(s..s.wrapping_add(phrase.len())) == Some(phrase) {
                        mine.push((q, doc));
                    }
                }
            }
            for &i in &kept[..nk] {
                buckets[i as usize % BUCKETS].add(i / BUCKETS as u32, off);
                unsafe { *count.get_unchecked_mut(i as usize) += 1 };
            }
        });
        flush(&mut pending, block);
        // The share is not read again: its pages are dropped now, on every thread, rather than all of the
        // file's by one thread at exit.
        let (from, to) = ((share.as_ptr() as usize + 4095) & !4095, (share.as_ptr() as usize + share.len()) & !4095);
        if to > from {
            unsafe { madvise(from as *mut u8, to - from, 4) };
        }
        found.lock().unwrap().extend(mine);
        (count, Mutex::new(buckets))
    });
    unsafe { munmap(terms.slot.as_mut_ptr().cast(), terms.slot.len() * 32) };

    let (mut seg, mut start, mut end) = (vec![0u16; lists * THREADS], vec![0u32; lists], 0);
    for i in (0..BUCKETS).flat_map(|b| (b..lists).step_by(BUCKETS)) {
        let s = &mut seg[i * THREADS..][..THREADS];
        s.iter_mut().zip(&scans).for_each(|(n, c)| *n = c.0[i]);
        (start[i], end) = (end, end + s.iter().map(|&n| n as u32).sum::<u32>());
    }
    let buckets: Vec<_> = scans.into_iter().map(|s| s.1).collect();
    let post: Vec<AtomicU16> = unsafe { std::mem::transmute(vec![0u16; end as usize]) };
    // Every thread writes into every bucket's region of post, so a thread far ahead makes post's pages
    // resident while the others still hold their units: the peak then depended on scheduling (+9 MiB on a
    // loaded machine). No thread starts bucket b before all have finished bucket b - 2.
    let done: Vec<AtomicUsize> = (0..BUCKETS).map(|_| AtomicUsize::new(0)).collect();
    on_threads(|t| {
        let buckets = std::mem::take(&mut *buckets[t].lock().unwrap());
        let mut cursor = vec![0u32; lists.div_ceil(BUCKETS)];
        for (b, units) in buckets.into_iter().enumerate() {
            while b >= 2 && done[b - 2].load(Relaxed) < THREADS {
                std::thread::yield_now();
            }
            for (c, i) in cursor.iter_mut().zip((b..lists).step_by(BUCKETS)) {
                let before: u32 = seg[i * THREADS..][..t].iter().map(|&n| n as u32).sum();
                *c = start[i] + before;
            }
            // A unit is the term and a document gap below GAP, or else the gap follows as a unit of its own.
            let (mut doc, mut k, v) = (0u32, 0, &units.v);
            while k < v.len() {
                let (x, g) = (v[k], v[k] as u32 & GAP);
                k += 1;
                if g == GAP {
                    (doc, k) = (doc + v[k] as u32, k + 1);
                } else {
                    doc += g;
                }
                let c = &mut cursor[(x >> 4) as usize];
                (post[*c as usize].store(doc as u16, Relaxed), *c += 1);
            }
            done[b].fetch_add(1, Relaxed);
        }
    });
    let post: Vec<u16> = unsafe { std::mem::transmute(post) };
    let bits: Vec<u64> = unsafe { std::mem::transmute(bits) };
    let mut found = found.into_inner().unwrap();
    (found.sort_unstable(), found.dedup());

    unsafe { mallopt(-3, 64 << 20) };
    let docs = |id: u32| match id as usize {
        i if i < nbits => Docs::Bits(Cow::Borrowed(&bits[i * words..(i + 1) * words])),
        i => {
            let mut at = start[i] as usize;
            let segs = &seg[i * THREADS..][..THREADS];
            let mut v = Vec::with_capacity(segs.iter().map(|&n| n as usize).sum());
            for (&n, &first) in segs.iter().zip(&firsts) {
                v.extend(post[at..at + n as usize].iter().map(|&d| first + d as u32));
                at += n as usize;
            }
            Docs::List(v)
        }
    };
    let chunks: Vec<Mutex<String>> = spans.chunks(64).map(|_| Mutex::default()).collect();
    let next = AtomicUsize::new(0);
    on_threads(|_| loop {
        let k = next.fetch_add(1, Relaxed);
        let Some(chunk) = chunks.get(k) else { return };
        let mut out = String::new();
        for (q, &(phrase, from, to)) in spans.iter().enumerate().skip(k * 64).take(64) {
            let r = from as usize..to as usize;
            let (tids, ops) = (&ids[r.clone()], &ops[r]);
            let set = if phrase {
                let at = found.partition_point(|m| m.0 < q as u32);
                let end = at + found[at..].partition_point(|m| m.0 == q as u32);
                Docs::List(found[at..end].iter().map(|m| m.1).collect())
            } else {
                let (cur, steps) = (docs(tids[0]), ops.iter().zip(&tids[1..]));
                steps.fold(cur, |cur, (&op, &id)| match &cur {
                    Docs::List(v) if v.is_empty() && op == b'O' => docs(id),
                    Docs::List(v) if v.is_empty() => cur,
                    _ => apply(cur, op, docs(id), words),
                })
            };
            line(&mut out, &set);
        }
        *chunk.lock().unwrap() = out;
    });
    // The postings and bitmaps are not read again: each thread drops its twelfth now, rather than one thread
    // freeing all of them at exit.
    on_threads(|t| {
        for (p, len) in [(post.as_ptr() as usize, post.len() * 2), (bits.as_ptr() as usize, bits.len() * 8)] {
            let (a, b) = ((p + len * t / THREADS + 4095) & !4095, (p + len * (t + 1) / THREADS) & !4095);
            if b > a {
                unsafe { madvise(a as *mut u8, b - a, 4) };
            }
        }
    });
    let file = std::fs::File::create("/out/results.txt").expect("/out/results.txt");
    let mut out = std::io::BufWriter::new(file);
    for c in chunks {
        std::io::Write::write_all(&mut out, c.into_inner().unwrap().as_bytes()).unwrap();
    }
    std::io::Write::flush(&mut out).unwrap();
    unsafe { _exit(0) };
}
