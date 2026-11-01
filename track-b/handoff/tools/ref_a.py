import sys
g = {}
for line in open(sys.argv[1] + "/requests.log", "rb"):
    ts, _, ep, st, lat = line.split()
    k = (ts[:13], ep); a = g.setdefault(k, [[], 0]); a[0].append(int(lat)); a[1] += st[0] == 53
out = open(sys.argv[2], "wb")
for (h, ep), (ls, er) in sorted(g.items()):
    ls.sort(); n = len(ls); p = lambda q: ls[(q * n + 99) // 100 - 1]
    out.write(h + b":00:00Z " + ep + b" %d %d %d %d %d %d\n" % (n, p(50), p(95), p(99), er, er * 1000000 // n))
