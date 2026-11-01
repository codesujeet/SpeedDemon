# Speed Demon 2026 - team Microsecond

Solo entry for [Speed Demon 2026](https://speeddemon.dev), a performance contest with four tracks. Each track is a small batch program that reads files from `/data`, writes one output file to `/out`, and is scored on the live board by wall time (T), peak memory (M) and source size (S, formatted code lines). Every track here is one source file with no third-party dependencies.

## Tracks and board numbers

Numbers are copied from `real-spec/board-runs.jsonl` (the scored runs read from the public board feed), run IDs exactly as stored. "Final" is the last tagged commit that has stored board runs, tag `best-47` (commit `748922e`). The current tag `best-49` (commit `218638a`) is later than the last stored board run, so its board numbers are not in the log yet; see each `PERF.md`.

| Track | Task | Language | Source | Final (`best-47`): T / M / S / points, run | Best row in the log |
|---|---|---|---|---|---|
| A Ingest | log lines to hourly per-endpoint percentiles | C++20, g++ 15 | `track-a/src/main.cpp` | 0.0199 s / 19.0 MiB / 318 / 85.12, `run-20d4a9f79d87` | 85.32, `run-8f79d074469a` (`best-43`) |
| B Index | boolean and phrase queries over a text corpus | Rust, std only | `track-b/src/main.rs` | 0.1203 s / 43.4 MiB / 642 / 77.25, `run-09ecd8747b3e` | same row |
| C Join | two CSV files joined and aggregated per country | C++20, g++ 15 | `track-c/src/main.cpp` | 0.0243 s / 8.5 MiB / 374 / 81.44, `run-c636f7b1f0f0` | 81.66, `run-ce287149e4e9` (`best-45`) |
| D Traverse | shortest paths on a road-like graph | Rust, std only | `track-d/src/main.rs` | 0.1725 s / 65.8 MiB / 518 / 68.63, `run-cd014768facf` | 69.02, `run-1e5e115c0722` (commit `2a7cc01`) |

Board readings of identical code differ by 1-2 ms for A and C and by several ms for B and D, so a "best row" is partly a lucky reading. Each `PERF.md` starts with a Summary (reference, first logged board run, best, final, then what moved the score and what did not).

The task specs (inputs, output formats, hashes) are in the [contest repository](https://github.com/hackathon-raptors/speed-demon-2026/tree/main/specs). Tracks A and C started as Rust and were moved to C++ during the event because the C++ builds had a shorter source and, linked statically, started faster on the board; the last Rust versions are at git tags `best-19` and earlier.

## Build and check locally

Needs Docker. Builds are offline and use each track's own `Dockerfile` (allowlisted base images).

```
python3 check.py a --data Dataset/a --expected 495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3
```

`check.py` is the starter's build-and-run check: it builds the track image with `--network=none`, runs it with the board's sandbox flags and compares the SHA-256 of the output. The time it prints includes container start-up and is not the board's time.

`real-spec/dock.ps1` (PowerShell) is our closer replica of the board run: it puts the data on a Docker volume, pre-warms it, runs with the board's flags, reads the container's `memory.peak`, and prints wall ms, peak MB and MATCH or MISMATCH against the published hash (for example `.\real-spec\dock.ps1 -Track a -Scale full -Runs 20`). It expects the datasets under `real-spec/data/<track dir>/<scale>/`, which are not in the repo.

Published SHA-256 of the full seed-1 outputs (full values are in `real-spec/dock.ps1`):

| Track | Output file | SHA-256 (start) |
|---|---|---|
| A | `summary.txt` | `495ca096...` |
| B | `results.txt` | `f86973c2...` |
| C | `aggregates.txt` | `d8088b1c...` |
| D | `distances.txt` | `a23dbdf0...` |

## Layout

- `track-a/` to `track-d/`: one build context each: `Dockerfile`, `.speed-demon.toml`, `src/`, `README.md` (approach, assumptions, limits) and `PERF.md` (summary on top, then the long optimisation log with a "Board runs" table). `track-c/pgo/` holds a committed gcc profile that C's `Dockerfile` uses.
- `real-spec/`: local tooling: `board.py` (reads the public board feed), `perf_runs.py` (writes the "Board runs" tables), `board-runs.jsonl` (every scored run we read; the source of the run IDs in the PERF files), and the Docker runners `dock.ps1`, `ab.ps1`, `peak.ps1`.
- `check.py`: the starter's local build-and-run check.
- `docs/EXPERIMENTS.md`: the rehearsal-era experiment log (history; see the note at its top).
- `tests/track_*.rs`: unit tests of the earlier Rust implementations; they are not compiled into the current sources.

## How the code was produced

Claude Code was used throughout: the first Rust versions of all four tracks, the measurement scripts, the optimisation experiments (some run as parallel agent or cloud sessions, named in commit messages) and these docs. The current full source of each track started as a draft written with ChatGPT, which we then reviewed, tested and tuned. Nothing was accepted on reading alone: each version was run against the published output hash on the full data, repeated 40-150 times back to back, compared with the previous version on other generated seeds, and kept only if it scored better on the live board. Laptop timings were used only to choose between variants; the board numbers above are the ones that count.

## What we would do next

From the diagnostics in the `PERF.md` files.

- **A:** board diagnostics put the parse at about 9 ms and the hours phase (selection) at about 3 ms of roughly 20 ms. The remaining lever is fewer instructions per line in the parse; the last cuts went from 805 M to 738 M instructions.
- **B:** term lookups were the largest single cost (about 73 ms of about 172 ms, measured before the four-slot bucket change); the answer phase is about 25 ms. B follows its branch mispredicts more than its instruction count.
- **C:** the record loop is about 18 of 24 ms and is bound by instructions per record. A block-mask pass for the field ends cut mispredicts but added 33% instructions and was not kept.
- **D:** there is no board-based phase profile of the current draft (the phase numbers in `track-d/PERF.md` belong to earlier code), so profiling it on the board the way B and C were is the first step.
