// Speed Demon 2026, Track C (Join).
//
// Both files are memory-mapped, and every phase is cut into pieces that any
// thread of one pool claims; a phase starts once the one before is done:
//  1. regions.csv is split by lines; each thread flags the country codes it
//     sees. Countries get indices 1..N in code order (0 is _UNMATCHED).
//  2. Each thread fills its slice of a direct table region_id -> country index.
//  3. records.csv is split at CRLFs: the generator never writes a CR inside
//     quotes, so the byte after a CRLF always starts a record. Each record is
//     read field by field: the id's comma is found in one 8-byte word, a quoted
//     field's end by stepping through the bit mask of its quotes (a "" pair is
//     an escape), so commas and LFs inside quotes cost nothing, and the region's
//     comma and the LF in one 32-byte compare each. Numbers are decoded
//     eight digits at a time. Sums go to the thread's own per-country totals. Names
//     go through a shared lock-free intern table to dense ids, which are marked
//     in one bitset per country; both steps are prefetched records ahead. A
//     mark that sets a new bit adds one to the country's distinct names.
#include <bits/stdc++.h>
#include <fcntl.h>
#include <immintrin.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <unistd.h>

using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;

// Zero-filled anonymous memory; pages are charged only once touched.
template <class T>
T* zeroed(size_t n) {
    void* p = mmap(nullptr, n * sizeof(T), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) perror("mmap"), _exit(1);
    return static_cast<T*>(p);
}

// A table that every thread writes all over is populated up front: faulting
// it in page by page from twelve threads at once made the threads queue on
// the kernel's page-table locks (about 12% of all cycles), and each page was
// faulted twice, once on its first read (the shared zero page) and again on
// its first write. Thread t of n populates its share of the n pieces in one
// call, so the work is spread too. (Without MADV_POPULATE_WRITE the pages
// are simply faulted in later.)
template <class T>
void populate_share(T* p, size_t n, int t, int nthreads) {
    const size_t pages = (n * sizeof(T) + 4095) / 4096, from = pages * t / nthreads, to = pages * (t + 1) / nthreads;
    if (to > from) madvise(reinterpret_cast<char*>(p) + from * 4096, (to - from) * 4096, MADV_POPULATE_WRITE);
}

// Map a file over a spare zero page, so 64-byte loads past its end are safe.
const char* map_file(const char* path, size_t& size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) perror(path), _exit(1);
    size = lseek(fd, 0, SEEK_END);
    auto* p = static_cast<char*>(
        mmap(nullptr, size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    if (size && mmap(p, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == MAP_FAILED) perror("mmap"), _exit(1);
    close(fd);
    return p;
}

u64 load8(const char* p) {
    u64 v;
    memcpy(&v, p, 8);
    return v;
}

// Bit i is set when byte i of the 32 at p equals c.
u32 match32(const char* p, char c) {
    __m256i v = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(p));
    return _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(c)));
}


// Two numbers, each from the eight bytes that end where it does (the first
// byte is the lowest), reduced to digit values with the bytes before it masked
// to leading zeros, then folded in pairs, quads and halves: lane 0 the 1 to 8
// digits [s0, e0), lane 1 an amount [s1, e1) in cents, whose dot (third from
// the end, at most 99999.99) is given weight 0.
std::pair<u32, u32> fold(const char* s0, const char* e0, const char* s1, const char* e1) {
    static constexpr auto KEEP = [] {
        std::array<u64, 9> a{};
        for (int k = 1; k <= 8; ++k) a[k] = 0x0F0F0F0F0F0F0F0F & ~u64(0) << 8 * (8 - k);
        return a;
    }();
    __m128i x = _mm_and_si128(_mm_set_epi64x(load8(e1 - 8), load8(e0 - 8)), _mm_set_epi64x(KEEP[e1 - s1], KEEP[e0 - s0]));
    x = _mm_maddubs_epi16(x, _mm_setr_epi16(0x010A, 0x010A, 0x010A, 0x010A, 0x010A, 0x010A, 1, 0x010A));
    x = _mm_madd_epi16(x, _mm_set1_epi32(0x00010064));
    x = _mm_packus_epi32(x, x);
    x = _mm_madd_epi16(x, _mm_setr_epi16(10000, 1, 1000, 1, 0, 0, 0, 0));
    return {_mm_cvtsi128_si32(x), _mm_extract_epi32(x, 1)};
}

// The value of the 1 to 8 decimal digits in [s, e).
u64 digits(const char* s, const char* e) { return fold(s, e, e, e).first; }

// The first c at or after p (one is known to follow). 64 bytes at a time:
// a note is often longer than 32, and the loop's exit is then predictable.
[[gnu::always_inline]] inline const char* find(const char* p, char c) {
    for (;; p += 64)
        if (u64 m = match32(p, c) | u64(match32(p + 32, c)) << 32) return p + __builtin_ctzll(m);
}

// The comma after a number of at most 8 digits starting at p.
const char* comma_after(const char* p) {
    return p + __builtin_ctz(_mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128((const __m128i*)p), _mm_set1_epi8(','))));
}

// The text field (name or note) at p, moving p past its comma: its raw bytes
// (between the quotes, if quoted) and its value's length (one byte less per
// "" pair).
struct Text {
    const char* at;
    u32 raw, length;
};

[[gnu::always_inline]] inline Text text(const char*& p) {
    const char* s = p;
    if (*s != '"') {
        p = find(s, ',') + 1;
        return {s, u32(p - 1 - s), u32(p - 1 - s)};
    }
    // The quotes after the opening one, 64 bytes at a time, are stepped
    // through on their bit mask: a quote followed by another is a "" pair,
    // the first one that is not closes the field.
    u32 pairs = 0;
    const char* q = s + 1;
    for (u64 m = match32(q, '"') | u64(match32(q + 32, '"')) << 32;;) {
        u32 i = _tzcnt_u64(m);
        if (i >= 63) {  // none, or the byte after it is out of view: move on
            q += i;
            m = match32(q, '"') | u64(match32(q + 32, '"')) << 32;
            continue;
        }
        if (!(m >> (i + 1) & 1)) {
            q += i;
            break;
        }
        ++pairs;
        m &= m - 1;
        m &= m - 1;
    }
    p = q + 2;
    return {s + 1, u32(q - s - 1), u32(q - s - 1 - pairs)};
}

// Names, interned by their raw bytes between the quotes. Escaping is one to
// one, so equal raw bytes mean equal values (and the empty name is one value
// whether quoted or not). A name's key is a 64-bit hash of those bytes and
// their length; two of the 100,000 pool names share one with a chance of
// about 3 in 10^10. A key's slot in the open-addressed table is the name's id;
// an empty slot is claimed with a compare-and-swap. At 8 bytes a slot the
// table is 1 MB, small enough to stay in a core's L2 while the input streams
// past, and eight slots share a cache line, so a probe rarely leaves it.
class NameTable {
public:
    // The pool holds 100,000 names.
    static constexpr u32 BITS = 17, SLOTS = 1 << BITS;

    static u64 key(const char* s, u32 len) {
        static constexpr char KEEP[32] = {-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1};
        __m128i head = _mm_loadu_si128((const __m128i*)s);
        head = _mm_and_si128(head, _mm_loadu_si128((const __m128i*)(KEEP + 16 - std::min(len, 16u))));
        u64 h = (u64(_mm_cvtsi128_si64(head)) * 0x9E3779B97F4A7C15ull ^ len) + u64(_mm_extract_epi64(head, 1)) * 0xBF58476D1CE4E5B9ull;
        for (u32 i = 16; i < len; i += 8) {
            u64 w = load8(s + i);
            if (len - i < 8) w &= (u64(1) << 8 * (len - i)) - 1;
            h = (h ^ w) * 0x94D049BB133111EBull;
        }
        return h | 1;  // 0 marks a free slot
    }

    void prefetch(u64 k) const { _mm_prefetch((const char*)&slots_[k >> (64 - BITS)], _MM_HINT_T0); }

    // Slots are probed four at a time, from the aligned group of four the key
    // hashes to: one 32-byte compare finds the key, or the first free slot,
    // which is claimed (no key ever sits past a free slot, as none is freed).
    u32 id(u64 k) {
        for (u32 i = k >> (64 - BITS) & ~3u;; i = (i + 4) % SLOTS) {
            __m256i v = _mm256_load_si256((const __m256i*)&slots_[i]);
            u32 hit = _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(v, _mm256_set1_epi64x(k))));
            if (hit) return i + __builtin_ctz(hit);
            u32 free = _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(v, _mm256_setzero_si256())));
            for (; free; free &= free - 1) {
                u32 j = i + __builtin_ctz(free);
                u64 s = 0;
                if (slots_[j].compare_exchange_strong(s, k) || s == k) return j;
            }
        }
    }

    // Each of n threads populates its share up front (see zeroed).
    void populate(int t, int n) const { populate_share(slots_, SLOTS, t, n); }

private:
    std::atomic<u64>* slots_ = zeroed<std::atomic<u64>>(SLOTS);  // 0 = free
};

// One country's running aggregates (per thread, summed at the end).
struct Totals {
    u64 count = 0, id_sum = 0, text_bytes = 0, distinct = 0;  // text_bytes: name << 32 | note
    i64 amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

// Which names each country has: one bitset over name ids per country, shared
// by all threads. Most marks are first sightings, so they are atomic ORs.
struct NamesSeen {
    static constexpr u32 WORDS = NameTable::SLOTS / 64;
    std::atomic<u64>* bits = zeroed<std::atomic<u64>>(256 * WORDS);

    auto& word(u32 c, u32 name) { return bits[c * WORDS + name / 64]; }

    // For writing: the OR needs the line exclusive, and a plain prefetch would
    // fetch it shared first.
    void prefetch(u32 c, u32 name) { __builtin_prefetch(&word(c, name), 1); }

    // Whether this is the first time the name is marked for the country, so
    // the distinct counts come out of the marking (an OR returning the old
    // bit) with no pass over the bitsets afterwards. No test first: the read
    // would fetch the line shared, then the OR would fetch it again exclusive.
    bool mark(u32 c, u32 name) {
        u64 bit = u64(1) << name % 64;
        return !(word(c, name).fetch_or(bit, std::memory_order_relaxed) & bit);
    }
};

// Parse every record that starts in [p, end). A record's region row and name
// slot are prefetched when it is parsed; LAG records later its country is
// read, its sums go to totals, its name is looked up and that name's bitset
// word prefetched; LAG records after that the name is marked. So each of the
// three random reads (region table, name table, bitsets) has a steady lead. The
// rings start and end full of a dummy record in the unused country SPARE (the
// table's row past the last region), so no step tests whether its slot is real.
constexpr u32 SPARE = 255;

void parse_records(const char* p, const char* end, const char* file_end, const u8* country_of,
                   size_t regions, NameTable names, NamesSeen seen, Totals* totals) {
    constexpr u32 LAG = 16;
    struct Pending {
        u64 name;
        u32 region, id;
        u64 text_bytes;
        i64 cents;
    } pending[LAG];
    struct Mark {
        u32 country, id;
    } marks[LAG];
    const Pending dummy{1, u32(regions) + 1, 0, 0, 0};
    std::fill_n(pending, LAG, dummy);
    std::fill_n(marks, LAG, Mark{SPARE, 0});
    u32 n = 0;
    auto advance = [&](const Pending& next) __attribute__((always_inline)) {
        u32 k = n++ % LAG;
        totals[marks[k].country].distinct += seen.mark(marks[k].country, marks[k].id);
        const Pending& old = pending[k];
        u32 c = country_of[old.region];
        Totals& t = totals[c];
        ++t.count;
        t.id_sum += old.id;
        t.amount_sum += old.cents;
        t.amount_min = std::min(t.amount_min, old.cents);
        t.amount_max = std::max(t.amount_max, old.cents);
        t.text_bytes += old.text_bytes;
        marks[k] = {c, names.id(old.name)};
        seen.prefetch(c, marks[k].id);
        pending[k] = next;
    };

    while (p < end) {
        // The file's pages are scattered in memory, and the hardware prefetcher
        // stops at each 4 KB page; this reaches into the next one in time.
        _mm_prefetch(p + 2048, _MM_HINT_T0);
        if (u8(*p) == 0xEF) p += 3;  // a byte order mark before the id
        const char* f1 = comma_after(p);
        u64 id = digits(p, f1);
        p = f1 + 1;
        Text name = text(p), note = text(p);
        // The region (at most 7 digits) and the amount (at most 9 bytes) end
        // within the 32 bytes at p, so one load gives both the comma and the
        // LF, if any.
        u32 lf = match32(p, '\n');
        const char* f4 = p + __builtin_ctz(match32(p, ','));
        const char* eol = lf ? p + __builtin_ctz(lf) : file_end;
        i64 negative = f4[1] == '-';
        auto [region, money] = fold(p, f4, f4 + 1 + negative, eol - (eol[-1] == '\r'));
        i64 cents = (i64(money) ^ -negative) + negative;

        // Region ids are below 10^7 (fit u32); the table's spare row is past the last.
        u32 r = std::min(u64(region), u64(regions));
        _mm_prefetch((const char*)(country_of + r), _MM_HINT_T0);
        u64 key = NameTable::key(name.at, name.raw);
        advance({key, r, u32(id), u64(name.length) << 32 | note.length, cents});
        names.prefetch(key);
        p = eol + 1;
    }
    for (u32 i = 0; i < 2 * LAG; ++i) advance(dummy);
}

// 15-bit code of a three-letter country; numeric order is alphabetical order.
u32 code(const char* p) { return (p[0] - 'A') << 10 | (p[1] - 'A') << 5 | (p[2] - 'A'); }

// The region line "id,ABC" at p, moving p to the next line. An id has at most
// 7 digits, so the comma is the lowest zero byte of the line's first 8 bytes
// XOR ','.
std::pair<u64, u32> region_line(const char*& p) {
    const char* comma = comma_after(p);
    std::pair<u64, u32> line{digits(p, comma), code(comma + 1)};
    p = comma + 5 + (comma[4] == '\r');  // past the country and the line end (LF or CRLF)
    return line;
}

// A phase of work in pieces. Every thread claims pieces until none are left,
// then sleeps until all are done. It waits for the work, not for every thread
// to arrive: a thread starts milliseconds after the one before it, and the
// late ones just join in wherever the work has got to.
struct Phase {
    std::atomic<int> claimed{0}, done{0};
    const int pieces;

    template <class F>
    void run(F&& f) {
        for (int k; (k = claimed++) < pieces;)
            if (f(k), ++done == pieces) done.notify_all();
        for (int d; (d = done) < pieces;) done.wait(d);
    }
};

// Cuts of [from, size) into n ranges, each starting just after a delimiter.
std::vector<size_t> split(const char* data, size_t from, size_t size, int n, std::string_view delim) {
    std::vector<size_t> cut(n + 1, size);
    cut[0] = from;
    for (int t = 1; t < n; ++t) {
        size_t c = std::max(cut[t - 1], from + (size - from) * t / n);
        auto* hit = static_cast<const char*>(memmem(data + c, size - c, delim.data(), delim.size()));
        cut[t] = hit ? hit - data + delim.size() : size;
    }
    return cut;
}

void put_money(std::string& out, i64 cents) {
    u64 a = cents < 0 ? -u64(cents) : u64(cents);
    out += cents < 0 ? " -" : " ";
    out += std::to_string(a / 100);
    out += {'.', char('0' + a % 100 / 10), char('0' + a % 10)};
}

int main() {
    // With transparent huge pages, a block written only in part can be backed
    // by a whole 2 MB page; plain pages keep memory to what is written.
    prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
    const int nthreads = std::clamp<int>(std::thread::hardware_concurrency(), 1, 12);
    size_t rsize, size;
    const char* regions = map_file("/data/regions.csv", rsize);
    const char* records = map_file("/data/records.csv", size);

    // regions.csv: a header, then "id,ABC\n" lines in ascending id order, so
    // the last line holds the largest id.
    const char* last = static_cast<const char*>(memrchr(regions, '\n', rsize - 1)) + 1;
    const size_t nregions = atoll(last) + 1;
    u8* country_of = zeroed<u8>(nregions + 2);  // 0 = no such region; the last row is SPARE
    country_of[nregions + 1] = SPARE;
    std::vector<size_t> rcut = split(regions, strchr(regions, '\n') - regions + 1, rsize, nthreads, "\n");
    std::vector<std::atomic<u8>> present(1 << 15);
    std::array<u8, 1 << 15> index_of{};
    std::vector<u32> codes;  // country index - 1 -> code
    std::once_flag numbered;
    auto number_countries = [&] {
        for (u32 k = 0; k < present.size(); ++k)
            if (present[k]) codes.push_back(k), index_of[k] = codes.size();
        if (codes.size() >= SPARE) fputs("too many countries\n", stderr), _exit(1);
    };

    // records.csv: skip the BOM and header, then split after CRLFs into eight
    // pieces per thread, claimed in turn, so a thread whose pieces are slow
    // (more first sightings of names) is evened out by the others.
    size_t start = size ? static_cast<const char*>(memchr(records, '\n', size)) - records + 1 : 0;
    const int pieces = 8 * nthreads;
    std::vector<size_t> cut = split(records, start, size, pieces, "\r\n");
    std::vector<std::array<Totals, 256>> totals(nthreads);
    NameTable names;
    NamesSeen seen;

    // Populate tables and flag countries; populate bitsets and fill the region
    // table; parse records.
    Phase flag{.pieces = nthreads}, fill{.pieces = nthreads}, parse{.pieces = pieces};
    Phase flag2{.pieces = nthreads}, fill2{.pieces = nthreads};  // DIAGNOSTIC: the region phases run a second time
    std::vector<std::thread> pool;
    for (int t = 0; t < nthreads; ++t)
        pool.emplace_back([&, t] {
            flag.run([&](int k) {
                populate_share(country_of, nregions, k, nthreads);
                names.populate(k, nthreads);
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];)
                    present[region_line(p).second].store(1, std::memory_order_relaxed);
            });
            std::call_once(numbered, number_countries);
            fill.run([&](int k) {
                populate_share(seen.bits, (codes.size() + 1) * NamesSeen::WORDS, k, nthreads);
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];) {
                    auto [id, c] = region_line(p);
                    country_of[id] = index_of[c];
                }
            });
            flag2.run([&](int k) {  // DIAGNOSTIC: the same stores again, no populate
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];)
                    present[region_line(p).second].store(1, std::memory_order_relaxed);
            });
            fill2.run([&](int k) {  // DIAGNOSTIC
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];) {
                    auto [id, c] = region_line(p);
                    country_of[id] = index_of[c];
                }
            });
            parse.run([&](int k) {
                // Map the piece's pages in one call rather than by a fault
                // every 16 pages as the parse reaches them.
                uintptr_t from = uintptr_t(records + cut[k]) & ~uintptr_t(4095);
                madvise(reinterpret_cast<void*>(from), uintptr_t(records + cut[k + 1]) - from, MADV_POPULATE_READ);
                parse_records(records + cut[k], records + cut[k + 1], records + size, country_of, nregions, names,
                              seen, totals[t].data());
                // The piece is not read again: its pages are unmapped now, by
                // this thread, instead of all of the file's at exit by one.
                uintptr_t to = uintptr_t(records + cut[k + 1]) & ~uintptr_t(4095);
                if (to > from) madvise(reinterpret_cast<void*>(from), to - from, MADV_DONTNEED);
            });
        });
    for (auto& th : pool) th.join();

    std::string out;
    for (u32 i = 1; i <= codes.size() + 1; ++i) {
        u32 c = i % (codes.size() + 1);  // countries 1..N, then _UNMATCHED (0)
        Totals sum;
        for (const auto& per_thread : totals) {
            const Totals& t = per_thread[c];
            sum.count += t.count;
            sum.id_sum += t.id_sum;
            sum.text_bytes += t.text_bytes;
            sum.distinct += t.distinct;
            sum.amount_sum += t.amount_sum;
            sum.amount_min = std::min(sum.amount_min, t.amount_min);
            sum.amount_max = std::max(sum.amount_max, t.amount_max);
        }
        if (sum.count == 0) continue;
        if (c == 0) {
            out += "_UNMATCHED";
        } else {
            u32 k = codes[c - 1];
            out += {char('A' + (k >> 10)), char('A' + (k >> 5 & 31)), char('A' + (k & 31))};
        }
        for (u64 v : {sum.count, sum.id_sum}) out += ' ' + std::to_string(v);
        for (i64 v : {sum.amount_sum, sum.amount_min, sum.amount_max}) put_money(out, v);
        for (u64 v : {sum.distinct, sum.text_bytes >> 32, sum.text_bytes & 0xFFFFFFFF}) out += ' ' + std::to_string(v);
        out += '\n';
    }
    FILE* f = fopen("/out/aggregates.txt", "wb");
    if (!f) return perror("/out/aggregates.txt"), 1;
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    _exit(0);
}
