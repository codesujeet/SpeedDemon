// Speed Demon 2026, Track A (Ingest).
//
// One pool of threads does everything in two phases.
//
// Phase 1, parse. The log is memory-mapped and cut into line-aligned ranges,
// one per thread. The timestamp is always 27 bytes and the 72-hour span is
// fixed, so a line's hour index comes from its day and hour digits alone. One
// 64-byte scan after the timestamp finds the method's end, the endpoint's end
// and the line's end. Endpoints are interned in one lock-free table shared by
// all threads, and each line files a 4-byte record (latency, endpoint id, 5xx
// flag) under its hour.
//
// Between the phases the last thread to arrive sorts the names, so each
// endpoint id gets its rank in output order.
//
// Phase 2, hours. Threads claim hours in order. An hour's ~43k latencies are
// counting-sorted by endpoint rank into per-thread scratch and three
// selections per group give the percentiles. Each group's numbers are kept
// (24 bytes a line) along with the exact size the hour's text will take, since
// a number's width is known without formatting it.
//
// Phase 3, text. The last thread into the barrier turns the sizes into
// offsets, sizes the output file and maps it; then every thread formats its
// hours straight into the file. No hour waits for another and no write call
// queues on the file's lock, and the output's pages fill only after the
// records have been released.
#include <bits/stdc++.h>
#include <fcntl.h>
#include <immintrin.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <unistd.h>

using u32 = uint32_t;
using u64 = uint64_t;

constexpr int HOURS = 72;  // 2026-10-30T18:00Z to 2026-11-02T17:59:59Z
// A record packs the latency above a 13-bit key: a 12-bit endpoint id and
// the 5xx flag. Latencies of 2^19 us or more (about 3% of lines) do not fit
// and are kept on a side list with their key instead.
constexpr u32 KEY_BITS = 13, ERR = 1u << 12, ID_MASK = ERR - 1, PACKED_MAX = 1u << 19;

// Bit i is set when byte i of the 32 at p equals c.
inline u32 eq32(const char* p, char c) {
    __m256i v = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(p));
    return _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(c)));
}

// Bit i is set when byte i of the 16 at p is a space.
inline u32 eq16(const char* p) {
    return _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128((const __m128i*)p), _mm_set1_epi8(' ')));
}

// The number of decimal digits in x.
inline u32 width(u32 x) {
    static constexpr u32 p10[] = {1, 10, 100, 1000, 10000, 100000, 1000000, 10000000, 100000000, 1000000000};
    // About the bit length times log10(2).
    u32 t = (32 - __builtin_clz(x | 1)) * 1233 >> 12;
    return t + ((x | 1) >= p10[t]);
}

// The value of the len (1 to 10) digits ending just before end, with no loop:
// the 16 bytes before end, minus '0' and with the bytes ahead of the number
// zeroed, are combined 2, 4 and then 8 digits at a time. (A digit loop here
// cost about 5% more instructions in all.)
inline u32 decode(const char* end, u32 len) {
    __m128i v = _mm_subs_epu8(_mm_loadu_si128((const __m128i*)(end - 16)), _mm_set1_epi8('0'));
    __m128i lane = _mm_set_epi64x(0x0F0E0D0C0B0A0908, 0x0706050403020100);
    v = _mm_and_si128(v, _mm_cmpgt_epi8(lane, _mm_set1_epi8(char(15 - len))));
    v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A));  // pairs (10, 1)
    v = _mm_madd_epi16(v, _mm_set1_epi32(0x00010064));  // pairs (100, 1)
    v = _mm_madd_epi16(_mm_packus_epi32(v, v), _mm_set1_epi32(0x00012710));
    return u32(_mm_cvtsi128_si32(v)) * u64(100000000) + u32(_mm_extract_epi32(v, 1));
}

// Puts the k-th smallest of a[0, n) at a[k], the smaller ones before it,
// without a branch on the data: each step partitions around the median of
// three by Lomuto's scheme with an unconditional swap, the boundary moving
// by the comparison's result. (std::nth_element mispredicted at about every
// other element.) Many equal values could make a round shrink the range by
// only one, so after 40 rounds the library's selection finishes.
inline void select(u32* a, u32 n, u32 k) {
    for (int round = 0; n > 1; ++round) {
        if (round == 40) return std::nth_element(a, a + k, a + n);
        if (k + 1 == n) return std::iter_swap(std::max_element(a, a + n), a + k);
        u32 m = n / 2;  // sort a[0], a[m], a[n - 1], then the median is the pivot
        if (a[m] < a[0]) std::swap(a[m], a[0]);
        if (a[n - 1] < a[m]) std::swap(a[n - 1], a[m]);
        if (a[m] < a[0]) std::swap(a[m], a[0]);
        std::swap(a[m], a[n - 1]);
        u64 pivot = a[n - 1], lt = 0;
        for (u32 i = 0; i + 1 < n; ++i) {
            u32 x = a[i];
            a[i] = a[lt], a[lt] = x, lt += x < pivot;
        }
        std::swap(a[lt], a[n - 1]);
        if (k == lt) return;
        k < lt ? n = lt : (a += lt + 1, k -= lt + 1, n -= lt + 1);
    }
}

// Endpoint names to ids, one table for all threads. Open addressing on a CRC
// of the name's first and last eight bytes (masked when the name is shorter;
// the rest of the line keeps both loads in bounds), and a name's id is its
// slot. A new name is copied into the arena, so the names compared on every
// line sit together, and then published with one compare-and-swap; a thread
// that loses the race for the slot compares against the winner's name.
struct Names {
    static constexpr u32 SLOTS = 1 << 12, ARENA = SLOTS * 128;  // a seed has 1,842
    std::atomic<u64> slot[SLOTS];  // length << 32 | arena offset, 0 if free
    std::atomic<u32> used{0};
    char arena[ARENA + 64];  // 64 spare bytes keep the last compare in bounds

    std::string_view name(u64 v) const { return {arena + u32(v), size_t(v >> 32)}; }

    u32 operator()(std::string_view s) {
        u64 a, b = 0;
        memcpy(&a, s.data(), 8);
        if (s.size() >= 8) memcpy(&b, s.data() + s.size() - 8, 8);
        else a &= (u64(1) << 8 * s.size()) - 1;
        for (u32 i = _mm_crc32_u64(_mm_crc32_u64(s.size(), a), b);; ++i) {
            u64 v = slot[i % SLOTS].load(std::memory_order_acquire);
            if (v == 0) {
                u32 at = used.fetch_add(s.size());
                if (at + s.size() > ARENA) _exit(2);
                memcpy(arena + at, s.data(), s.size());
                if (slot[i % SLOTS].compare_exchange_strong(v, u64(s.size()) << 32 | at)) return i % SLOTS;
            }
            if (same(name(v), s)) return i % SLOTS;
        }
    }

    // 64 bytes can always be read from either name (the rest of the line and
    // the spare page follow one, the arena's spare bytes the other), so a name
    // of up to 64 bytes (94% of lines) is compared in two steps, not memcmp.
    static bool same(std::string_view x, std::string_view s) {
        if (x.size() != s.size() || s.size() > 64) return x == s;
        auto eq = [&](int i) { return u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i*)(x.data() + i)), _mm256_loadu_si256((const __m256i*)(s.data() + i))))); };
        return (~(eq(0) | u64(eq(32)) << 32) & (~u64(0) >> (64 - s.size()))) == 0;
    }
};
static Names endpoints;  // zero pages until written

// One hour's records from one thread, in a chain of 16 KB chunks. Every hour
// has its own stretch of one mapping, from which all threads carve its chunks
// in order, so an hour's chunks are contiguous and go back in one madvise as
// soon as its records are copied out, in whatever order the hours finish. The
// output file's pages count toward peak memory, and by then all are released.
struct Chunk {
    static constexpr u32 CAP = (16384 - 8) / 4;
    Chunk* prev;
    u32 rec[CAP];
};
static Chunk* chunks[HOURS];  // each hour's stretch
static std::atomic<u32> carved[HOURS];

struct Parser {
    Chunk* last[HOURS] = {};
    // Each hour's next free record. Chunks are aligned to their size, so a cursor
    // with no low bits is either null or just past a full chunk.
    u32* to[HOURS] = {};
    // (latency, key) of the long ones.
    std::vector<std::pair<u32, u32>> wide[HOURS];

    void run(const char* p, const char* end) {
        // The line ends of the next 4 KB are listed first, so a line's start does not
        // wait for the parse of the one before. A line is at least 40 bytes, so a
        // 64-byte block holds at most two ends; both are stored (the second only
        // counts if there is one), with no branch on how many lines the block has.
        const char* ends[4096 / 40 + 8];
        for (const char* scan = p; p < end;) {
            u32 n = 0;
            for (const char* stop = std::min(scan + 4096, end); scan < stop; scan += 64) {
                u64 m = eq32(scan, '\n') | u64(eq32(scan + 32, '\n')) << 32;
                ends[n] = scan + _tzcnt_u64(m), ends[n + 1] = scan + _tzcnt_u64(_blsr_u64(m));
                n += __builtin_popcountll(m);
            }
            while (n && ends[n - 1] >= end) --n;  // the last block's may be the next piece's
            for (u32 i = 0; i < n; ++i) {
                const char* eol = ends[i];
                // The file's pages are scattered in memory and the hardware
                // prefetcher stops at each 4 KB page; this reaches into the next ones.
                _mm_prefetch(p + 4096, _MM_HINT_T0);
                // YYYY-MM-DDTHH: the month's last digit at 6 (bit 0) plus the day's at 9
                // (two bits) count the days 0 to 3, the hour is at 11-12. One multiply
                // gathers days * 24 + tens * 10 + units in the top byte.
                u64 t;
                memcpy(&t, p + 6, 8);
                int h = int((t & 0x000F0F0003000001) * 0x18000018000A0100 >> 56) - 18;

                // The method (3 to 7 bytes) ends within 16 bytes of the timestamp, and
                // the 16 bytes before the line's end hold " SSS latency", at most 10 digits.
                const char* name = p + 29 + __builtin_ctz(eq16(p + 28));
                const char* sp = eol - 16 + (31 - __builtin_clz(eq16(eol - 16)));
                u32 key = endpoints(std::string_view(name, sp - 4 - name)) | (sp[-3] == '5' ? ERR : 0);
                u32 latency = decode(eol, eol - sp - 1);
                p = eol + 1;

                if (__builtin_expect(latency >= PACKED_MAX, 0)) {
                    wide[h].push_back({latency, key});
                    continue;
                }
                u32*& w = to[h];
                if (!(uintptr_t(w) & (sizeof(Chunk) - 1))) {
                    Chunk* c = chunks[h] + carved[h]++;
                    c->prev = last[h], last[h] = c, w = c->rec;
                }
                *w++ = latency << KEY_BITS | key;
            }
        }
    }

    // Calls f(endpoint rank, key, latency) for every record of hour h.
    template <class F>
    void each(int h, const u32* rank, F&& f) const {
        u32 n = last[h] ? to[h] - last[h]->rec : 0;  // the newest chunk's; the older ones are full
        for (const Chunk* c = last[h]; c; c = c->prev, n = Chunk::CAP)
            for (u32 i = 0; i < n; ++i) f(rank[c->rec[i] & ID_MASK], c->rec[i] & ERR, c->rec[i] >> KEY_BITS);
        for (auto [latency, key] : wide[h]) f(rank[key & ID_MASK], key & ERR, latency);
    }
};

int main() {
    // With transparent huge pages, a chunk region written only in part could be
    // backed by whole 2 MB pages and count against peak memory.
    prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
    const int nt = std::clamp<int>(std::thread::hardware_concurrency(), 1, 12);
    int in = open("/data/requests.log", O_RDONLY);
    int out = open("/out/summary.txt", O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (in < 0 || out < 0) return perror("open"), 1;

    // Map the log over one spare zero page, so the 64-byte scans stay in bounds.
    size_t size = lseek(in, 0, SEEK_END);
    auto* log = (char*)mmap(nullptr, size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (size && mmap(log, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, in, 0) == MAP_FAILED) return perror("mmap"), 1;
    // Line-aligned pieces, eight per thread, claimed in order: a thread that
    // starts late or shares its core takes fewer.
    // Each hour's stretch has room for every line (a line is at least 40 bytes)
    // plus one part-full chunk per thread; pages are charged only once written.
    const size_t room = size / 40 / Chunk::CAP + 1 + nt;
    auto region = (uintptr_t)mmap(nullptr, (HOURS * room + 1) * sizeof(Chunk), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (region == uintptr_t(MAP_FAILED)) return perror("mmap"), 1;
    for (int h = 0; h < HOURS; ++h) chunks[h] = (Chunk*)((region + sizeof(Chunk) - 1) & -sizeof(Chunk)) + h * room;
    const int pieces = 8 * nt;
    std::vector<size_t> cut(pieces + 1, size);
    cut[0] = 0;
    for (int k = 1; k < pieces; ++k)
        for (cut[k] = std::max(cut[k - 1], size * k / pieces); cut[k] < size && log[cut[k] - 1] != '\n';) ++cut[k];
    std::atomic<int> claim_piece{0}, claim{0}, claim_text{0};

    // Between the phases, endpoints are ranked in unsigned byte order (a
    // proper prefix first), which is the order lines are printed in an hour.
    std::vector<Parser> parsers(nt);
    std::vector<std::string_view> names;
    std::vector<u32> rank(Names::SLOTS);  // endpoint id -> rank
    std::barrier ranked(nt, [&]() noexcept {
        std::vector<std::pair<std::string_view, u32>> by_name;
        for (u32 id = 0; id < Names::SLOTS; ++id)
            if (u64 v = endpoints.slot[id]) by_name.push_back({endpoints.name(v), id});
        std::sort(by_name.begin(), by_name.end());
        for (auto [name, id] : by_name) rank[id] = names.size(), names.push_back(name);
    });

    struct Line {
        u32 e, n, p50, p95, p99, errors;
    };
    auto ppm = [](const Line& l) { return u32(u64(l.errors) * 1000000 / l.n); };
    std::vector<std::vector<Line>> lines(HOURS);
    std::vector<u64> at(HOURS + 1);  // an hour's text size, then its offset
    char* text = nullptr;
    std::barrier sized(nt, [&]() noexcept {
        u64 total = 0;
        for (int h = 0; h < HOURS; ++h) total += std::exchange(at[h], total);
        at[HOURS] = total;
        if (total == 0) return;
        if (ftruncate(out, total) || (text = (char*)mmap(nullptr, total, PROT_READ | PROT_WRITE, MAP_SHARED, out, 0)) == MAP_FAILED) _exit(1);
    });

    std::vector<std::thread> pool;
    for (int t = 0; t < nt; ++t) pool.emplace_back([&, t] {
        for (int k; (k = claim_piece++) < pieces;) {
            parsers[t].run(log + cut[k], log + cut[k + 1]);
            // The piece is not read again: its pages are unmapped now, by this
            // thread, instead of all of the log's at exit by one.
            uintptr_t from = (uintptr_t(log + cut[k]) + 4095) & ~uintptr_t(4095), to = uintptr_t(log + cut[k + 1]) & ~uintptr_t(4095);
            if (to > from) madvise((void*)from, to - from, MADV_DONTNEED);
        }
        ranked.arrive_and_wait();

        {  // the scratch goes before the text phase, where the output counts
        const u32 ne = names.size();
        std::vector<u32> start(ne + 1), errors(ne), latency;
        for (int h; (h = claim++) < HOURS;) {
            // Counting sort of the hour's latencies by endpoint rank: count into
            // start[e + 1], prefix, then place each latency at its group's cursor.
            std::fill(start.begin(), start.end(), 0);
            std::fill(errors.begin(), errors.end(), 0);
            for (auto& ps : parsers) ps.each(h, rank.data(), [&](u32 e, u32 err, u32) { ++start[e + 1], errors[e] += err != 0; });
            for (u32 e = 0; e < ne; ++e) start[e + 1] += start[e];
            latency.resize(start[ne]);
            std::vector<u32> cursor(start.begin(), start.end() - 1);
            for (auto& ps : parsers) ps.each(h, rank.data(), [&](u32 e, u32, u32 l) { latency[cursor[e]++] = l; });
            madvise(chunks[h], carved[h] * sizeof(Chunk), MADV_DONTNEED);

            u64 size = 0;
            for (u32 e = 0; e < ne; ++e) {
                u32 n = start[e + 1] - start[e];
                if (n == 0) continue;
                // Nearest rank p is index ceil(p * n / 100) - 1. A selection
                // leaves only larger values after its rank, so each higher rank
                // is selected among those: half the group for p95, a twentieth
                // for p99. (An equal rank is already in place.)
                u32* a = latency.data() + start[e];
                u32 r50 = (50 * n + 99) / 100 - 1, r95 = (95 * n + 99) / 100 - 1, r99 = (99 * n + 99) / 100 - 1;
                for (u32 from = 0; u32 r : {r50, r95, r99})
                    if (r >= from) select(a + from, n - from, r - from), from = r + 1;
                const Line& l = lines[h].emplace_back(e, n, a[r50], a[r95], a[r99], errors[e]);
                // Label, name, six numbers each after a space, newline.
                size += 21 + names[e].size() + 7;
                for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)}) size += width(x);
            }
            at[h] = size;
        }
        }
        sized.arrive_and_wait();

        for (int h; (h = claim_text++) < HOURS;) {
            char label[24];
            int day = (h + 18) / 24;
            snprintf(label, sizeof label, "2026-%02d-%02dT%02d:00:00Z ", day < 2 ? 10 : 11, day < 2 ? 30 + day : day - 1, (h + 18) % 24);
            char* o = text + at[h];
            for (const Line& l : lines[h]) {
                o = std::copy_n(label, 21, o);
                o = std::copy(names[l.e].begin(), names[l.e].end(), o);
                for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)}) {
                    *o++ = ' ';
                    o = std::to_chars(o, o + 10, x).ptr;
                }
                *o++ = '\n';
            }
            std::vector<Line>().swap(lines[h]);
        }
    });
    for (auto& th : pool) th.join();
    _exit(0);
}
