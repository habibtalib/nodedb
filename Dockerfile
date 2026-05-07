# syntax=docker/dockerfile:1
# Production image for NodeDB Origin server
# Requires Linux kernel >= 5.1 (io_uring)

# ── Stage 1: Chef base (rust + build deps + cargo-chef) ──────────────────────
FROM rust:1.95-bookworm AS chef

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    libclang-dev \
    pkg-config \
    protobuf-compiler \
    perl \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked
WORKDIR /build

# ── Stage 2: Dependency plan ──────────────────────────────────────────────────
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin nodedb

# ── Stage 3: Build dependencies (cached — only reruns if Cargo.lock changes) ──
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin nodedb

# ── Stage 4: Build server binary ─────────────────────────────────────────────
COPY . .
RUN cargo build --release -p nodedb

# ── Stage 5: Minimal runtime (Chainguard glibc-dynamic) ──────────────────────
# Wolfi-based, daily-rebuilt against patched packages — typically 0 CVEs.
# Ships only glibc + libgcc + ca-certificates + tzdata. No shell, no package
# manager, no `curl` — that's why the binary has a built-in `healthcheck`
# subcommand (see ctl/healthcheck.rs).
FROM cgr.dev/chainguard/glibc-dynamic:latest AS runtime

# `nonroot` user (uid/gid 65532) is built into the Chainguard base. The
# declared VOLUME below inherits this ownership, so named volumes work
# out of the box without an entrypoint chown step.
USER nonroot:nonroot

COPY --from=builder --chown=nonroot:nonroot /build/target/release/nodedb /usr/local/bin/nodedb

# nodedb dynamically links libz.so.1 (transitively via crates that default to
# system zlib — e.g. flate2 with its default `zlib` feature). Chainguard's
# glibc-dynamic ships only glibc + libgcc + ca-certs + tzdata, so the runtime
# fails immediately with: "error while loading shared libraries: libz.so.1:
# cannot open shared object file: No such file or directory".
#
# Copy the Debian builder's libz into a default ld.so search path. The glob
# covers both amd64 and arm64 — per-arch builds only see their own multiarch
# directory, so exactly one match per build.
#
# Long-term cleanup options (any of these would let us drop this COPY):
#   * Switch flate2 (and any other zlib-using deps) to the `rust_backend`
#     feature so libz becomes a build-time C lib statically linked.
#   * Or vendor libz via the `libz-sys` `static` feature.
#   * Or move runtime to `cgr.dev/chainguard/wolfi-base` + `apk add zlib`.
COPY --from=builder /lib/*-linux-gnu/libz.so.1 /usr/lib/libz.so.1

# Bind to all interfaces (required for Docker port mapping)
# Point data dir at the declared volume
ENV NODEDB_HOST=0.0.0.0 \
    NODEDB_DATA_DIR=/var/lib/nodedb

WORKDIR /var/lib/nodedb

# pgwire | native protocol | HTTP API | WebSocket sync | OTLP gRPC | OTLP HTTP
EXPOSE 6432 6433 6480 9090 4317 4318

VOLUME ["/var/lib/nodedb"]

# Probe local /health via the binary's built-in subcommand. No curl needed.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s \
    CMD ["/usr/local/bin/nodedb", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/nodedb"]
