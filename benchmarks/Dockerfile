# The comparison, on a Linux kernel.
#
# Why this exists: on macOS the loopback path saturates around 45-50k small
# round-trips a second, which is below what any of these servers can answer. Run
# there, every framework reports the same number and the benchmark measures the
# host instead of the software — see benchmarks/README.md for the evidence. This
# image puts the same five apps on a Linux loopback, where that ceiling is
# roughly an order of magnitude higher, so an ordinary keep-alive load is
# actually bounded by the server.
#
# Build from the repository root:
#   docker build -f benchmarks/Dockerfile -t churust-bench .
#
# Each app is built in its own stage so that a Go toolchain, a JDK and a Rust
# toolchain never have to coexist in the image that runs the measurement — the
# thing being measured should share its machine with as little as possible.

# --- the three Rust apps -----------------------------------------------------
FROM rust:1-bookworm AS rust-build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY churust-core churust-core
COPY churust churust
COPY churust-macros churust-macros
COPY churust-json churust-json
COPY churust-logging churust-logging
COPY churust-cors churust-cors
COPY churust-auth churust-auth
COPY churust-ratelimit churust-ratelimit
COPY churust-compression churust-compression
COPY churust-templates churust-templates
COPY churust-redis churust-redis
COPY churust-client churust-client
COPY churust-openapi churust-openapi
COPY churust-lab churust-lab
COPY examples examples
COPY benchmarks/bench-churust benchmarks/bench-churust
COPY benchmarks/bench-axum benchmarks/bench-axum
COPY benchmarks/bench-actix benchmarks/bench-actix
RUN cargo build --release --manifest-path benchmarks/bench-churust/Cargo.toml \
 && cargo build --release --manifest-path benchmarks/bench-axum/Cargo.toml \
 && cargo build --release --manifest-path benchmarks/bench-actix/Cargo.toml

# --- the Go app --------------------------------------------------------------
# Its own stage because Debian's packaged Go is too old for the 1.22+ pattern
# mux the app routes with.
FROM golang:1-bookworm AS go-build
WORKDIR /src
COPY benchmarks/bench-go .
RUN go build -o bench-go .

# --- the Ktor app ------------------------------------------------------------
FROM gradle:jdk21 AS ktor-build
WORKDIR /src
COPY benchmarks/bench-ktor .
RUN gradle fatJar --no-daemon --quiet

# --- wrk ---------------------------------------------------------------------
# Debian does not package it, and it is the only load generator here that can
# pipeline.
FROM debian:bookworm-slim AS wrk-build
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential libssl-dev git ca-certificates unzip \
 && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch 4.2.0 https://github.com/wg/wrk /wrk \
 && make -C /wrk -j"$(nproc)"

# --- the image that actually runs the measurement ----------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl python3 procps lsof \
 && rm -rf /var/lib/apt/lists/*

# A JRE rather than a JDK: Ktor needs to run here, not compile.
COPY --from=eclipse-temurin:21-jre /opt/java/openjdk /opt/java/openjdk
ENV JAVA_HOME=/opt/java/openjdk
ENV PATH="/opt/java/openjdk/bin:${PATH}"

COPY --from=wrk-build /wrk/wrk /usr/local/bin/wrk
COPY --from=rust-build /src/benchmarks/bench-churust/target/release/bench-churust /apps/
COPY --from=rust-build /src/benchmarks/bench-axum/target/release/bench-axum /apps/
COPY --from=rust-build /src/benchmarks/bench-actix/target/release/bench-actix /apps/
COPY --from=go-build /src/bench-go /apps/
COPY --from=ktor-build /src/build/libs/bench-ktor.jar /apps/

COPY benchmarks/pipeline.lua benchmarks/run-linux.sh /bench/
WORKDIR /bench
ENTRYPOINT ["/bin/bash", "/bench/run-linux.sh"]
