FROM rust:1.80-slim-bookworm AS builder

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
COPY crates/load-tester ./crates/load-tester

RUN cargo build --release -p phase1-axum-baseline

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/phase1-axum-baseline /app/phase1-axum-baseline
RUN mkdir -p /data

EXPOSE 3000
CMD ["/app/phase1-axum-baseline"]
