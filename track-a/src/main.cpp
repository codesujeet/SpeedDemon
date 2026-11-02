// Speed Demon 2026, Track A (Ingest).
//
// One pool of threads reads the log and finishes the hours while it is still being read.
//
// Parse. The log is memory-mapped and cut into line-aligned pieces, claimed in order. The
// timestamp is always 27 bytes and the 72-hour span is fixed, so a line's hour index comes from
// its day and hour digits alone. One 64-byte scan after the timestamp finds the method's end,
// the endpoint's end and the line's end. Endpoints are interned in one lock-free table shared
// by all threads, and each line files a 4-byte record (latency, endpoint id, 5xx flag) under its
// hour.
//
// Hours. The log is nearly in time order, so hour h is finished (by whichever thread is free)
// as soon as the pieces parsed from the start hold a line of hour h + 2; a check at the end of
// each piece catches a log where that guess was wrong, and then the run starts over with every
// hour finished after the parse. An hour's ~43k latencies are counting-sorted by endpoint,
// three selections per group give the percentiles, and the text is written with pwrite at the
// offset that the sizes of the earlier hours give. The hour's records are then released, so the
// memory in use is mostly the output file's pages, not the records.
#include <bits/stdc++.h>
#include <fcntl.h>
#include <malloc.h>
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
constexpr u32 KEY_BITS = 13, ERR = 1u << 12, ID_MASK = ERR - 1, PACKED_MAX = (1u << 19) - 1;

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
        // a[0], a[n / 2], a[n - 1] are sorted by minimum and maximum alone, as comparing and
        // swapping mispredicted. That finishes a range of up to 3; otherwise their median
        // goes last as the pivot.
        u32 m = n / 2, lo = std::min(a[0], a[m]), hi = std::max(a[0], a[m]), z = a[n - 1];
        u32 mid = std::min(hi, z), top = std::max(hi, z), med = std::max(lo, mid);
        a[0] = std::min(lo, mid);
        if (n <= 3) {
            a[m] = med, a[n - 1] = top;
            return;
        }
        a[m] = top, a[n - 1] = med;
        u64 pivot = a[n - 1], lt = 0;
        for (u32 i = 0; i + 1 < n; ++i) {
            u32 x = a[i];
            a[i] = a[lt], a[lt] = x, lt += x < pivot;
        }
        std::swap(a[lt], a[n - 1]);
        if (k == lt) return;
        u32 right = -u32(k > lt), skip = (lt + 1) & right;  // all ones if k is above lt
        n = ((n - skip) & right) | (lt & ~right);
        a += skip, k -= skip;
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
    alignas(32) std::atomic<u64> slot[SLOTS];  // tag << 48 | length << 32 | arena offset, 0 if free
    std::atomic<u32> used{0};
    char arena[ARENA + 64];  // 64 spare bytes keep the last compare in bounds

    std::string_view name(u64 v) const { return {arena + u32(v), size_t(v >> 32 & 0xFFFF)}; }

    // Probes four slots at a time, the 16-bit tags compared at once, so a slot taken by
    // another name costs no branch: comparing lengths one slot at a time mispredicted.
    u32 operator()(std::string_view s) {
        u64 a, b = 0;
        memcpy(&a, s.data(), 8);
        if (s.size() >= 8) memcpy(&b, s.data() + s.size() - 8, 8);
        else a &= (u64(1) << 8 * s.size()) - 1;
        u32 h = _mm_crc32_u64(_mm_crc32_u64(s.size(), a), b);
        u64 tag = h >> 12 & 0xFFFF | 1;
        for (u32 i = h & (SLOTS - 4);; i = (i + 4) % SLOTS) {
            __m256i v = _mm256_load_si256((const __m256i*)&slot[i]);
            auto eq = [](__m256i x, u64 y) { return u32(_mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(x, _mm256_set1_epi64x(y))))); };
            for (u32 hit = eq(_mm256_srli_epi64(v, 48), tag); hit; hit &= hit - 1) {
                u32 j = i + __builtin_ctz(hit);
                if (same(name(slot[j].load(std::memory_order_acquire)), s)) return j;
            }
            if (u32 free = eq(v, 0)) return claim(i + __builtin_ctz(free), s, tag);
        }
    }

    // A new name goes in slot j, unless another thread took it first; then the search
    // starts again, as that thread's name may be this one.
    [[gnu::noinline]] u32 claim(u32 j, std::string_view s, u64 tag) {
        u32 at = used.fetch_add(s.size());
        if (at + s.size() > ARENA) _exit(2);
        memcpy(arena + at, s.data(), s.size());
        u64 v = 0;
        return slot[j].compare_exchange_strong(v, tag << 48 | u64(s.size()) << 32 | at) ? j : (*this)(s);
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

                u32*& w = to[h];
                auto put = [&](u32 v) {
                    if (!(uintptr_t(w) & (sizeof(Chunk) - 1))) {
                        Chunk* c = chunks[h] + carved[h]++;
                        c->prev = last[h], last[h] = c, w = c->rec;
                    }
                    *w++ = v;
                };
                if (__builtin_expect(latency >= PACKED_MAX, 0)) {
                    // A marker, then the latency in the next slot, never split between chunks:
                    // with one slot left, a marker alone fills it.
                    if (!((uintptr_t(w) + 4) & (sizeof(Chunk) - 1))) put(~0u);
                    put(PACKED_MAX << KEY_BITS | key), put(latency);
                } else
                    put(latency << KEY_BITS | key);
            }
        }
    }

    // The state after a piece: for each hour, its newest chunk and next free record.
    // Written once by its thread, read by whoever finishes an hour.
    struct Snap {
        Chunk* last[HOURS];
        u32* to[HOURS];
    };
    std::atomic<const Snap*> pub{nullptr};

    // Publishes the state after a piece as s and returns the lowest and highest hour the
    // piece added records to.
    std::pair<int, int> publish(Snap& s) {
        static const Snap none{};
        const Snap* old = pub.load(std::memory_order_relaxed);
        std::copy(last, last + HOURS, s.last), std::copy(to, to + HOURS, s.to);
        int lo = HOURS, hi = -1;
        for (int h = 0; h < HOURS; ++h)
            if (to[h] != (old ? old : &none)->to[h]) lo = std::min(lo, h), hi = h;
        pub.store(&s);
        return {lo, hi};
    }

    // Calls f(endpoint id, error flag, latency) for every record of hour h in a snapshot.
    template <class F>
    static void each(const Snap& s, int h, F&& f) {
        u32 n = s.last[h] ? s.to[h] - s.last[h]->rec : 0;  // the newest chunk's; the older ones are full
        for (const Chunk* c = s.last[h]; c; c = c->prev, n = Chunk::CAP)
            for (u32 i = 0; i < n; ++i) {
                u32 v = c->rec[i], latency = v >> KEY_BITS;
                if (latency == PACKED_MAX) {  // a long latency follows; a marker in the last slot is padding
                    if (i == Chunk::CAP - 1) continue;
                    latency = c->rec[++i];
                }
                f(v & ID_MASK, v & ERR, latency);
            }
    }
};

int main(int, char** argv) {
    // With transparent huge pages, a chunk region written only in part could be
    // backed by whole 2 MB pages and count against peak memory.
    prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
    // Hour-sized buffers get their own mappings, so freeing them returns the pages.
    mallopt(M_MMAP_THRESHOLD, 1 << 15);
    const int nt = std::clamp<int>(std::thread::hardware_concurrency(), 1, 12);
    int in = open("/data/requests.log", O_RDONLY);
    int out = open("/out/summary.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (in < 0 || out < 0) return perror("open"), 1;

    // Map the log over one spare zero page, so the 64-byte scans stay in bounds.
    size_t size = lseek(in, 0, SEEK_END);
    auto* log = (char*)mmap(nullptr, size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (size && mmap(log, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, in, 0) == MAP_FAILED) return perror("mmap"), 1;
    // Each hour's stretch has room for every line (a line is at least 40 bytes)
    // plus one part-full chunk per thread; pages are charged only once written.
    const size_t room = size / 40 / Chunk::CAP + 1 + nt;
    auto region = (uintptr_t)mmap(nullptr, (HOURS * room + 1) * sizeof(Chunk), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (region == uintptr_t(MAP_FAILED)) return perror("mmap"), 1;
    for (int h = 0; h < HOURS; ++h) chunks[h] = (Chunk*)((region + sizeof(Chunk) - 1) & -sizeof(Chunk)) + h * room;
    // Line-aligned pieces, eight per thread, claimed in order: a thread that
    // starts late or shares its core takes fewer.
    const int pieces = 8 * nt;
    std::vector<size_t> cut(pieces + 1, size);
    cut[0] = 0;
    for (int k = 1; k < pieces; ++k)
        for (cut[k] = std::max(cut[k - 1], size * k / pieces); cut[k] < size && log[cut[k] - 1] != '\n';) ++cut[k];

    // Hours are finished while the log is still being read. The log is nearly in time order (a
    // line is at most ten minutes late), so once the finished pieces from the start of the file
    // hold a line of hour h + 2, hour h should be complete. That is a guess, and it is checked:
    // an hour is claimed with `next_hour` and then reads every thread's newest snapshot; a piece
    // publishes its snapshot and then reads `next_hour`. All four are seq_cst, so either the hour
    // read that snapshot or the piece sees the hour claimed and sets `late`; the program then
    // runs again with the guess off (no hour starts before all pieces are finished). A piece adds
    // records only after its thread's last snapshot, in chunks and slots the snapshot does not
    // include, so a reader never touches memory that a writer is writing.
    const bool safe = getenv("SD_SAFE");
    std::vector<Parser> parsers(nt);
    std::vector<Parser::Snap> snaps(pieces);
    std::vector<int> top(pieces);  // the highest hour of a piece, written before finished[k]
    std::unique_ptr<std::atomic<bool>[]> finished(new std::atomic<bool>[pieces]());
    std::atomic<int> claim_piece{0}, next_hour{0}, texted{0}, progress{0};
    std::atomic<bool> late{false};
    u64 text_end = 0;  // where the next hour's text goes; handed from hour to hour through `texted`
    auto ready = [&](int h) {
        int k = 0, newest = -1;
        for (; k < pieces && finished[k]; ++k) newest = std::max(newest, top[k]);
        return k == pieces || (!safe && newest >= h + 2);
    };

    struct Line {
        u32 id, n, p50, p95, p99, errors;
    };
    auto name = [](u32 id) { return endpoints.name(endpoints.slot[id]); };
    auto ppm = [](const Line& l) { return u32(u64(l.errors) * 1000000 / l.n); };
    // Works out hour h and writes its text. `ids` holds the endpoint ids in the order of their
    // names (a proper prefix first), which is the order lines are printed in an hour.
    auto finish = [&](int h, std::vector<u32>& ids) {
        std::vector<const Parser::Snap*> view;
        for (auto& ps : parsers)
            if (const Parser::Snap* s = ps.pub.load()) view.push_back(s);
        std::vector<u32> count(Names::SLOTS), errors(Names::SLOTS), start(Names::SLOTS);
        u32 total = 0;
        for (auto s : view) Parser::each(*s, h, [&](u32 id, u32 err, u32) { ++count[id], errors[id] += err != 0, ++total; });
        // Counting sort of the hour's latencies by endpoint: the start of each group, in name order.
        // A name that appeared since `ids` was made is missing from it, and the sum falls short.
        auto place = [&] {
            u32 sum = 0;
            for (u32 id : ids) start[id] = sum, sum += count[id];
            return sum == total;
        };
        if (!place()) {
            ids.clear();
            for (u32 id = 0; id < Names::SLOTS; ++id)
                if (endpoints.slot[id]) ids.push_back(id);
            std::sort(ids.begin(), ids.end(), [&](u32 a, u32 b) { return name(a) < name(b); });
            if (!place()) late = true, total = 0, view.clear(), std::fill(count.begin(), count.end(), 0), place();
        }
        auto latency = std::make_unique_for_overwrite<u32[]>(total);
        for (auto s : view) Parser::each(*s, h, [&](u32 id, u32, u32 l) { latency[start[id]++] = l; });
        madvise(chunks[h], carved[h] * sizeof(Chunk), MADV_DONTNEED);

        std::vector<Line> lines;
        u64 bytes = 0;
        for (u32 id : ids) {
            u32 n = count[id];
            if (n == 0) continue;
            // Nearest rank p is index ceil(p * n / 100) - 1. A selection
            // leaves only larger values after its rank, so each higher rank
            // is selected among those: half the group for p95, a twentieth
            // for p99. (An equal rank is already in place.)
            u32* a = latency.get() + start[id] - n;
            u32 r50 = (50 * n + 99) / 100 - 1, r95 = (95 * n + 99) / 100 - 1, r99 = (99 * n + 99) / 100 - 1;
            for (u32 from = 0; u32 r : {r50, r95, r99})
                if (r >= from) select(a + from, n - from, r - from), from = r + 1;
            const Line& l = lines.emplace_back(id, n, a[r50], a[r95], a[r99], errors[id]);
            // Label, name, six numbers each after a space, newline.
            bytes += 21 + name(id).size() + 7;
            for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)}) bytes += width(x);
        }
        latency.reset();
        // This hour's text goes after the earlier hours', once their sizes are known.
        for (int t; (t = texted.load(std::memory_order_acquire)) != h;) texted.wait(t);
        u64 at = text_end;
        text_end += bytes;
        texted.store(h + 1, std::memory_order_release);
        texted.notify_all();

        char label[24], buf[1 << 14];
        int day = (h + 18) / 24;
        snprintf(label, sizeof label, "2026-%02d-%02dT%02d:00:00Z ", day < 2 ? 10 : 11, day < 2 ? 30 + day : day - 1, (h + 18) % 24);
        char* o = buf;
        auto flush = [&] {
            for (char* p = buf; p < o;) {
                ssize_t w = pwrite(out, p, o - p, at);
                if (w <= 0) _exit(1);
                p += w, at += w;
            }
            o = buf;
        };
        for (const Line& l : lines) {
            std::string_view s = name(l.id);
            if (o + s.size() + 128 > buf + sizeof buf) flush();
            o = std::copy_n(label, 21, o);
            o = std::copy(s.begin(), s.end(), o);
            for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)}) {
                *o++ = ' ';
                o = std::to_chars(o, o + 10, x).ptr;
            }
            *o++ = '\n';
        }
        flush();
    };

    std::vector<std::thread> pool;
    for (int t = 0; t < nt; ++t) pool.emplace_back([&, t] {
        std::vector<u32> ids;
        // scratch per hour
        // Claims and finishes the hours that are ready, in order.
        auto drain = [&] {
            for (int h; (h = next_hour) < HOURS && ready(h);)
                if (next_hour.compare_exchange_strong(h, h + 1)) finish(h, ids);
        };
        for (int k; (k = claim_piece++) < pieces;) {
            parsers[t].run(log + cut[k], log + cut[k + 1]);
            // The piece is not read again: its pages are unmapped now, by this
            // thread, instead of all of the log's at exit by one.
            uintptr_t from = (uintptr_t(log + cut[k]) + 4095) & ~uintptr_t(4095), to = uintptr_t(log + cut[k + 1]) & ~uintptr_t(4095);
            if (to > from) madvise((void*)from, to - from, MADV_DONTNEED);
            auto [lo, hi] = parsers[t].publish(snaps[k]);
            if (lo < next_hour) late = true;
            top[k] = hi;
            finished[k] = true;
            ++progress, progress.notify_all();
            drain();
        }
        for (int p = progress; next_hour < HOURS; p = progress) {
            drain();
            if (next_hour < HOURS) progress.wait(p);
        }
        ++progress, progress.notify_all();  // wakes the others to see that all hours are claimed
    });
    for (auto& th : pool) th.join();
    if (late && !safe) {  // the log was not in time order: run again, finishing hours only after the parse
        setenv("SD_SAFE", "1", 1);
        execv("/proc/self/exe", argv);
    }
    _exit(late);
}
