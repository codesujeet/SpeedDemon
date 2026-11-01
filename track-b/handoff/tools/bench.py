import subprocess, sys, time, statistics, resource, os, hashlib
OUTS = ['/out/summary.txt', '/out/results.txt', '/out/aggregates.txt']  # a stale output file makes open(O_TRUNC) cost 2-10 ms; the board starts empty
# usage: bench.py reps cpus bin [bin...]   (binaries interleaved)
reps, cpus, bins = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
res = {b: [] for b in bins}; cpu = {b: [] for b in bins}; mem = {b: [] for b in bins}
for _ in range(reps):
    for b in bins:
        [os.path.exists(o) and os.unlink(o) for o in OUTS]
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
        t = time.perf_counter()
        subprocess.run(["taskset", "-c", cpus, b], check=True)
        w = time.perf_counter() - t
        r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        res[b].append(w * 1000); cpu[b].append((r1.ru_utime + r1.ru_stime - r0.ru_utime - r0.ru_stime) * 1000)
for b in bins:
    print(f"{os.path.basename(b):12s} wall med {statistics.median(res[b]):7.1f} min {min(res[b]):7.1f} ms | cpu med {statistics.median(cpu[b]):7.1f} min {min(cpu[b]):7.1f} ms")
