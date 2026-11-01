"""Write the 'Board runs' table into each track's PERF.md from real-spec/board-runs.jsonl.

  py real-spec/perf_runs.py     (run after `py real-spec/board.py`, which logs each scored run)

Only runs read from the public feed are listed, with the commit they scored. The section sits between two
markers, so it can be refreshed in place. Nothing is invented: a run the feed never showed is not listed.
"""
import json, subprocess

NL = chr(10)
BEGIN, END = "<!-- board-runs:begin -->", "<!-- board-runs:end -->"
REPO = [l for l in subprocess.run(["git", "log", "--format=%h", "--all"], capture_output=True, text=True).stdout.split()]

rows = {}
for line in open("real-spec/board-runs.jsonl"):
    r = json.loads(line)
    rows.setdefault(r["track"], []).append(r)

for t in "abcd":
    path = f"track-{t}/PERF.md"
    text = open(path, newline="").read()
    crlf = "\r\n" in text
    text = text.replace("\r\n", NL)
    body = [BEGIN, "## Board runs", "",
            "Each row is one scored run of the named commit on the live board, copied from the public board feed "
            "(`board.json`). The feed keeps only each track's latest and best run, so runs before these cannot be "
            "listed; earlier steps in the table above stay local measurements. Board timing varies by about 1-2 ms "
            "between runs of identical code: every push re-runs all four tracks, so a row whose change names another "
            "track measured this track's unchanged code and shows that noise.", "",
            "| Commit | Change | Run ID | T (s) | M (MiB) | S | Points |", "|---|---|---|---|---|---|---|"]
    for r in rows.get(t, []):
        subj = subprocess.run(["git", "log", "-1", "--format=%s", r["commit"]], capture_output=True, text=True).stdout.strip()
        subj = subj.replace("|", "/").replace("Co-Authored-By", "")[:90]
        body.append(f"| `{r['commit']}` | {subj} | `{r['run_id']}` | {r['T_s']:.4f} | {r['M_bytes'] / 2**20:.1f} | {r['S']} | {r['points']:.2f} |")
    body += ["", END]
    section = NL.join(body)
    if BEGIN in text:
        head, rest = text.split(BEGIN, 1)
        text = head + section + rest.split(END, 1)[1]
    else:
        text = text.rstrip(NL) + NL + NL + section + NL
    open(path, "w", newline="").write(text.replace(NL, "\r\n") if crlf else text)
    print(t, len(rows.get(t, [])), "runs")
