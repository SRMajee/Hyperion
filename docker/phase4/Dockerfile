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

ENV LIBSQLITE3_FLAGS="-DSQLITE_DEFAULT_MEMSTATUS=0 -DSQLITE_OMIT_DEPRECATED -DSQLITE_OMIT_SHARED_CACHE -DSQLITE_THREADSAFE=2 -DSQLITE_TEMP_STORE=3 -DSQLITE_MMAP_SIZE=268435456"

RUN cargo build --release -p phase4-kernel-bypass

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    iptables \
    iproute2 \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/phase4-kernel-bypass /app/phase4-kernel-bypass
COPY docker/phase4/entrypoint-tune.sh /entrypoint-tune.sh
RUN chmod +x /entrypoint-tune.sh && mkdir -p /data

EXPOSE 80
ENTRYPOINT ["/entrypoint-tune.sh"]
CMD ["/app/phase4-kernel-bypass"]
