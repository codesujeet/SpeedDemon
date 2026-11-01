# Speed Demon 2026 - team Microsecond

Solo entry for [Speed Demon 2026](https://speeddemon.dev), all four tracks, no dependencies in any of them.

| Track | Language | Source |
|---|---|---|
| A Ingest | C++20 (g++ 15) | `track-a/src/main.cpp` |
| B Index | Rust (std only) | `track-b/src/main.rs` |
| C Join | C++20 (g++ 15) | `track-c/src/main.cpp` |
| D Traverse | Rust (std only) | `track-d/src/main.rs` |

Tracks A and C started as Rust and were moved to C++ during the event because the C++ builds were shorter and, with a static link, started faster on the board. The last Rust versions are kept at git tags (`best-19` and earlier).

## How the code was written

The first Rust versions of all four tracks were written and tuned by the team with Claude Code. The later drafts of every track (and the C++ versions of A and C) were drafted with ChatGPT by the team and then reviewed, tested and measured by us: each was checked against the published output hash on the full dataset, repeated 40-150 times back to back, compared with the previous version on other seeds where the input has input-dependent paths, and kept only if it scored better on the live board. Each track's `README.md` says which draft is current and what its assumptions are.

## Layout

- `track-a/` to `track-d/`: each folder is its own build context: `Dockerfile`, `.speed-demon.toml`, the source, and two write-ups, `README.md` (approach, assumptions) and `PERF.md` (optimisation log, with a "Board runs" table of real run IDs copied from the public board feed).
- `docs/EXPERIMENTS.md`: the rehearsal-era experiment log (history; see the note at its top).
- `real-spec/`: our local tooling: the board reader (`board.py`), the table writer (`perf_runs.py`), `board-runs.jsonl` (every scored board run we read from the public feed, the source of the run IDs in the PERF tables), and the local Docker runners (`dock.ps1`, `ab.ps1`, `peak.ps1`). Only these files are in the repo.
- `check.py`: the starter's local build-and-run check.

## Check a track locally

```
python3 check.py a --data Dataset/a --expected 495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3
```

Expected hashes (full seed-1 datasets from speeddemon.dev): A `495ca096...`, B `f86973c2...`, C `d8088b1c...`, D `a23dbdf0...`.

## Next 24 hours

Track A is within about 9 ms of the leaders and is limited by the parse and the per-hour selection; the next step would be overlapping the hour phase with the parse. Tracks B and D are the weakest; the next step is a C++ draft of each, since the C++ builds of A and C gained points mostly from a shorter source.
