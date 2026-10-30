# Track A — Ingest. Two-stage build matching the allowlist's published
# example. Digests confirmed 2026-10-25 by pulling both tags locally
# (they matched the rules page's own example exactly) and build-tested
# end to end with docker build --network=none plus the harness's exact
# docker run flags. Re-check speeddemon.dev/allowlist.json at freeze in
# case either image gets re-tagged before Oct 28.
FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
COPY . /src
WORKDIR /src
# Profile-guided: pgo.profdata was recorded with this exact source on the public
# dataset (rustc 1.99.1, LLVM 23.1.1, the toolchain of this image).
ENV RUSTFLAGS="-C target-cpu=x86-64-v3 -C profile-use=/src/pgo.profdata"
RUN cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/track-a /app/run
ENTRYPOINT ["/app/run"]
