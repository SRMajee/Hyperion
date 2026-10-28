FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/phase1-axum-baseline ./crates/phase1-axum-baseline
COPY crates/phase2-iouring-max ./crates/phase2-iouring-max
COPY crates/phase3-kernel-unleashed ./crates/phase3-kernel-unleashed
COPY crates/phase4-kernel-bypass ./crates/phase4-kernel-bypass
COPY crates/load-tester ./crates/load-tester

RUN cargo build --release -p load-tester

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    iproute2 \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/load-tester /app/load-tester
COPY docker/load-tester/entrypoint-ips.sh /entrypoint-ips.sh
RUN chmod +x /entrypoint-ips.sh

ENTRYPOINT ["/entrypoint-ips.sh"]
CMD ["/app/load-tester", "--help"]
