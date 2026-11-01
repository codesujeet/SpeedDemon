# Own generator for Track A, written from the spec (not the organizers' code).
import random, sys
seed, n, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
r = random.Random(seed)
fixed = ["/", "/health", "/healthz", "/v2/checkout", "/v2/orders", "/v2/orders-export", "/v2/orders.csv",
 "/v2/orders/{id}", "/v2/orders/{id}/items", "/v2/ordersByDate", "/v2/orders_legacy", "/v2/orderstatus",
 "/internal/v1/tenants/{tenant_id}/projects/{project_id}/pipelines/{pipeline_id}/runs/{run_id}/artifacts/{artifact_id}:download"]
voc = ["users","orders","items","cart","pay","search","api","v1","v2","v3","{id}","regions","ab_tests","products","admin","beta","feed","auth","token","media","Upload.x","a-b","c_d"]
eps = set(fixed)
while len(eps) < 1842:
    eps.add("/" + "/".join(r.choice(voc) for _ in range(r.randint(1, 5))) + (str(r.randint(0, 99)) if r.random() < .3 else ""))
eps = list(eps); r.shuffle(eps)
w = [1.0 / (i + 1) for i in range(len(eps))]
cum = []; t = 0
for x in w: t += x; cum.append(t)
import bisect
M = ["GET"] * 8 + ["POST"] * 3 + ["PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
S = [200]*60 + [201, 204, 301, 304, 400, 401, 404, 429, 499] + [500, 502, 503, 504] + [599]
base = 1793000400 - 0  # unused; compute timestamps by hand
import datetime
t0 = datetime.datetime(2026, 10, 30, 18)
lines = []
for i in range(n):
    sec = r.random() * 72 * 3600
    d = t0 + datetime.timedelta(seconds=int(sec))
    us = int((sec % 1) * 1e6)
    e = eps[bisect.bisect_left(cum, r.random() * t)]
    lat = int(r.lognormvariate(8.5, 1.4))
    if r.random() < 0.03: lat = r.randint(1 << 19, 1 << 26)
    if r.random() < 0.0001: lat = r.choice([0, 4294967295])
    lines.append((sec, f"{d:%Y-%m-%dT%H:%M:%S}.{us:06d}Z {r.choice(M)} {e} {r.choice(S)} {lat}\n"))
# interleave: 8 shards, chunks arrive out of order
lines.sort()
chunks = []
for k in range(0, n, 3000):
    c = lines[k:k + 3000]
    chunks.append((c[-1][0] + r.random() * 600, c))
chunks.sort(key=lambda x: x[0])
with open(out + "/requests.log", "w") as f:
    for _, c in chunks:
        f.writelines(l for _, l in c)
