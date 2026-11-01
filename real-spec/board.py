"""Board watcher (local tool, not part of any track).

  py real-spec/board.py            -> print Microsecond's latest and best per track + overall + rank
  py real-spec/board.py --wait SHA -> poll the public feed until every track has a result for commit SHA

Prints one line per track: latest run vs the best run so far, so a regression is obvious.
"""
import json, sys, time, urllib.request

FEED = "https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json"
TEAM = "Microsecond"


def feed():
    for _ in range(6):
        try:
            with urllib.request.urlopen(urllib.request.Request(FEED, headers={"User-Agent": "board-watch"}), timeout=30) as r:
                return json.load(r)
        except Exception:
            time.sleep(4)
    raise SystemExit("feed unreadable")


LOG = "real-spec/board-runs.jsonl"


def log_run(track, r):
    """Append each scored run once (run id, commit, T, M, S, points): the source for PERF.md run IDs."""
    if r.get("stage") != "scored" or not r.get("run_id"):
        return
    try:
        seen = {json.loads(l)["run_id"] for l in open(LOG)}
    except OSError:
        seen = set()
    if r["run_id"] not in seen:
        row = {"track": track, "run_id": r["run_id"], "commit": r["commit"][:7], "T_s": r.get("T_s"),
               "M_bytes": r.get("M_bytes"), "S": r.get("S"), "points": r.get("points"), "ended_at": r.get("ended_at")}
        open(LOG, "a").write(json.dumps(row) + chr(10))


def show(d, sha=None):
    me = next(t for t in d["teams"] if t["team"] == TEAM)
    print("feed", d["generated_at"], "| head", me["watch"]["head"][:7], "| queue", me["queue"])
    best, done = {}, True
    for k, v in sorted(me["tracks"].items()):
        r, b = v["latest"], v.get("best")
        if b:
            best[k] = b["points"]
        log_run(k, r)
        fresh = sha is None or r["commit"].startswith(sha)
        if sha and not (fresh and r["stage"] in ("scored", "failed", "unchanged")):
            done = False
        print(f'{k.upper()} {r["stage"]:9s} {r["commit"][:7]} T {r.get("T_s") or 0:.4f}s M {(r.get("M_bytes") or 0) / 2**20:5.1f}MiB '
              f'S {r.get("S")} pts {r.get("points") or 0:6.2f} | best {b["points"] if b else 0:6.2f}', r.get("failure") or "")
    top = sorted(best.values(), reverse=True)[:3]
    if len(top) == 3:
        print("overall (best 3):", round((top[0] * top[1] * top[2]) ** (1 / 3), 2))
    rows = []
    for t in d["teams"]:
        pts = sorted((v["best"]["points"] for v in t["tracks"].values() if v.get("best") and v["best"].get("points")), reverse=True)[:3]
        if len(pts) == 3:
            rows.append(((pts[0] * pts[1] * pts[2]) ** (1 / 3), t["team"]))
    rows.sort(reverse=True)
    print("rank", [i + 1 for i, (_, n) in enumerate(rows) if n == TEAM], "of", len(rows), [(n, round(g, 1)) for g, n in rows[:3]])
    return done


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "--wait":
        sha = sys.argv[2]
        for _ in range(60):
            if show(feed(), sha):
                break
            print("-- waiting --")
            time.sleep(20)
    else:
        show(feed())
