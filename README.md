# Speed Demon 2026 — team Microsecond

Solo entry for [Speed Demon 2026](https://speeddemon.dev), Rust, no
dependencies, all four tracks.

## Layout

- `track-a/` Ingest, `track-b/` Index, `track-c/` Join, `track-d/` Traverse.
  Each folder is its own build context: `Dockerfile`, `.speed-demon.toml`,
  `Cargo.toml`, `.cargo/config.toml`, `src/main.rs`, and the two write-ups
  `README.md` (approach) and `PERF.md` (optimisation log).
- `docs/` working notes: designs, experiments, plan.
- `research/` technique notes that source comments point to.
- `check.py` the starter's local build-and-run check.

## Check a track locally

```
python3 check.py a --data Dataset/a --expected 495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3
```

Expected hashes: A `495ca096…`, B `f86973c2…`, C `d8088b1c…`, D `a23dbdf0…`
(full seed-1 datasets from speeddemon.dev).
