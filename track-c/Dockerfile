# Track C - Join, in C++. Two-stage build on the allowlisted gcc image, then the
# allowlisted debian runtime (both digests from speeddemon.dev/allowlist.json).
FROM gcc:15@sha256:eb5e64286ad47187b32bda75c6b6916166ecb41da56559b8b09e0001748d2cce AS build
COPY src/main.cpp pgo/ /src/
RUN g++ -O3 -march=x86-64-v3 -mpclmul -mbmi2 -std=c++20 -pthread -static -fprofile-use=/src -Wno-error=coverage-mismatch -ffunction-sections -fdata-sections -Wl,--gc-sections,-z,noseparate-code -o /sdrun /src/main.cpp

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /sdrun /app/run
ENTRYPOINT ["/app/run"]
