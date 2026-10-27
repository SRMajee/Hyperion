# Project Objective: High-Concurrency Rust API Challenge (3-Phase Reproducible Docker Benchmark)

We are building and benchmarking a three-stage progression of a high-concurrency Rust API designed to maximize concurrent connected users on constrained hardware (simulating a $12/month 2-vCPU, 4GB RAM server). Every phase must run in its own isolated, reproducible Docker container environment so any developer can recreate the exact performance ceilings and kernel bottlenecks on their own PC.

## Core Functional Requirements (Strict Parity Across All 3 Phases)
1. **Database Schema**: SQLite database with a `records` table (`id INTEGER PRIMARY KEY, payload TEXT NOT NULL, updated_at INTEGER NOT NULL`).
2. **Endpoints**:
   - `GET /record/:id` -> Queries SQLite for the exact record by ID and returns JSON (`{"id": ..., "payload": ..., "updated_at": ...}`).
   - `POST /record` -> Accepts JSON (`{"payload": "..."}`), inserts a new record into SQLite, and returns the created ID (`201 Created`).
3. **Hard Constraints**:
   - **NO application-level caching** of database query results (every read/write must actually hit SQLite).
   - Persistent HTTP/1.1 `keep-alive` connections must be maintained for every simulated concurrent user.

---

## Workspace & Directory Structure
Create a Cargo workspace and Docker orchestration layout with this exact structure:
- `Cargo.toml` (Workspace root)
- `crates/phase1-axum-baseline`
- `crates/phase2-iouring-max`
- `crates/phase3-kernel-unleashed`
- `crates/load-tester` (Custom multi-IP `io_uring` benchmark client)
- `docker/`
  - `phase1/` (`Dockerfile`, `nginx.conf`, `.env.phase1`)
  - `phase2/` (`Dockerfile`, `.env.phase2`)
  - `phase3/` (`Dockerfile`, `entrypoint-tune.sh`, `.env.phase3`)
  - `load-tester/` (`Dockerfile`, `entrypoint-ips.sh`)
- `scripts/host-kernel-prep.sh` (Optional host-level sysctl/conntrack helper for Phase 3)
- `docker-compose.yml` (Profile-based orchestration for isolated phase runs)
- `Makefile` (One-command runners for each phase)
- `README.md` (Instructions for reproducing all 3 phases locally)

---

## Phase 1: The Baseline (`crates/phase1-axum-baseline`)
Build a standard, idiomatic production Rust web server:
- **Stack**: `axum`, `tokio` (multi-thread), `rusqlite` or `sqlx` (with standard connection pool), `serde`, `serde_json`.
- **Proxy**: Put an Nginx reverse proxy (`docker/phase1/nginx.conf`) in front of the Axum server container.
- **SQLite Config**: Standard WAL mode enabled (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`).
- **Expected Ceiling**: ~14,000 concurrent users before latency spikes or CPU/proxy bottlenecks hit.

---

## Phase 2: The Maxxed-Out `io_uring` Server (`crates/phase2-iouring-max`)
Strip away Nginx, Tokio, Axum, and ORMs. Build a bare-metal, zero-allocation custom HTTP server directly on top of Linux `io_uring`:
1. **Custom `io_uring` Event Loop**:
   - Use the `io-uring` crate directly (zero async runtime overhead).
   - Implement **multishot accept** (`opcode::AcceptMulti`) and **multishot recv** (`opcode::RecvMulti`) using provided buffer rings (`io_uring_buf_ring`) so socket reads require zero syscalls per packet and zero dynamic allocations.
2. **64-Byte Connection State**:
   - Represent each connection state in a `#[repr(C, align(64))]` struct of exactly 64 bytes (fitting a single CPU cache line). Include a compile-time assertion `const _: () = assert!(std::mem::size_of::<ConnState>() == 64);`.
   - Pre-allocate a fixed slab/arena of connection states and reuse ring buffers so request handling performs **zero heap allocations** (`malloc`/`free`) on the hot path.
3. **Zero-Copy HTTP Parser & Serializer**:
   - Write a minimal byte-slice HTTP parser for `GET /record/:id` and `POST /record` operating directly on the pre-allocated `io_uring` buffer ring slices, and format responses into pre-allocated write buffers using `itoa`.
4. **SQLite Extreme Optimization**:
   - Compile SQLite from source via `rusqlite` (`bundled` feature with custom `LIBSQLITE3_FLAGS`: `SQLITE_DEFAULT_MEMSTATUS=0`, `SQLITE_OMIT_DEPRECATED`, `SQLITE_OMIT_SHARED_CACHE`, `SQLITE_THREADSAFE=2`, `SQLITE_TEMP_STORE=3`, `SQLITE_MMAP_SIZE=268435456`).
   - Implement a dedicated database worker thread communicating with the `io_uring` thread via a lock-free ring buffer (`crossbeam-channel` bounded or custom SPSC/MPSC ring).
   - **Group Reads & Batch Writes**: Coalesce all pending read requests in the queue into a single shared read transaction (`BEGIN DEFERRED ... COMMIT`) per tick, and batch all pending writes into a single `BEGIN IMMEDIATE ... COMMIT` transaction per tick using cached prepared statements (`prepare_cached`).
5. **Expected Ceiling**: Supports ~64,000 concurrent users at ~200ms P99 latency and ~52% CPU utilization before hitting the Linux 65,535 connection tracking / port cap.

---

## Phase 3: Overcoming the 65k Linux Kernel & Ephemeral Port Limits (`crates/phase3-kernel-unleashed`)
Break past the ~65,535 connection wall where Linux `nf_conntrack`, file descriptors, and ephemeral ports drop packets:
1. **Multi-Core Thread-Per-Core Architecture (`SO_REUSEPORT`)**:
   - Spawn $N$ pinned worker threads (matching the container's 2 CPU cores via `core_affinity`).
   - Each thread initializes its own independent `io_uring` instance (`IORING_SETUP_SINGLE_ISSUER`, `IORING_SETUP_COOP_TASKRUN`, `IORING_SETUP_DEFER_TASKRUN`, with fallback if kernel < 6.1) and binds to the listening port using `SO_REUSEPORT` so the kernel distributes incoming connections across queues with zero lock contention.
   - Partition read workers per core (using SQLite read-only connections in WAL mode) while routing batched writes to a dedicated writer ring.
2. **Container & Kernel Network Stack Bypass (`docker/phase3/entrypoint-tune.sh` & `scripts/host-kernel-prep.sh`)**:
   - Configure the Phase 3 environment for 150,000+ concurrent connections:
     - **Bypass `nf_conntrack`**: Add `iptables -t raw -A PREROUTING -j NOTRACK` and `iptables -t raw -A OUTPUT -j NOTRACK` inside the container namespace (and provide host-level `nf_conntrack_max` / `NOTRACK` instructions in `scripts/host-kernel-prep.sh`) so the kernel never drops packets at 65,536 tracked connections.
     - **File Descriptors**: Configure `ulimit -n 1048576` (`fs.file-max = 2097152`, `fs.nr_open = 2097152`).
     - **Socket Backlogs & Queues**: Tune `net.core.somaxconn = 65535`, `net.ipv4.tcp_max_syn_backlog = 65535`, `net.core.netdev_max_backlog = 65535`.
     - **TCP Memory & Ephemeral Ports**: Tune `net.ipv4.ip_local_port_range = 1024 65535`, `net.ipv4.tcp_tw_reuse = 1`, `net.ipv4.tcp_fin_timeout = 15`, and minimize per-socket TCP buffer minimums (`net.ipv4.tcp_rmem = 4096 16384 65536`, `net.ipv4.tcp_wmem = 4096 16384 65536`) so 150k+ open sockets fit comfortably inside the 4GB RAM limit.

---

## Reproducible Docker & Isolated Environment Architecture
Every phase must run in its own isolated Docker container with dedicated `.env` files and `docker-compose` profiles so any user can reproduce the exact benchmarks on their own PC.

### 1. Simulated $12 Server Hardware Limits
All server containers (`phase1`, `phase2`, `phase3`) must enforce identical hardware constraints in `docker-compose.yml`:
- `cpus: "2.0"` and `mem_limit: "4g"` (simulating a $12/mo 2-vCPU, 4GB RAM VPS).
- Isolated volumes per phase so SQLite state never collides across phases.
- Separate `.env.phase1`, `.env.phase2`, and `.env.phase3` files controlling port bindings, worker counts, SQLite batch sizes, and ring buffer depths.

### 2. Phase-Specific Container Profiles
- **Phase 1 Profile (`docker compose --profile phase1 up`)**:
  - Runs `phase1-nginx` + `phase1-axum` + `load-tester`.
  - Uses default Docker sysctls and ulimits to naturally reproduce the ~14,000 user baseline ceiling.
- **Phase 2 Profile (`docker compose --profile phase2 up`)**:
  - Runs `phase2-iouring` + `load-tester`.
  - Sets `security_opt: [seccomp:unconfined]` and `ulimits.memlock: -1` so `io_uring` ring buffers and multishot opcodes run without Docker seccomp blocks, while keeping a `65535` ulimit/conntrack ceiling so users can observe the exact ~64,000 connection wall at ~52% CPU.
- **Phase 3 Profile (`docker compose --profile phase3 up`)**:
  - Runs `phase3-unleashed` + `load-tester`.
  - Grants `cap_add: [NET_ADMIN, SYS_ADMIN, SYS_RESOURCE]`, `security_opt: [seccomp:unconfined]`, `ulimits.memlock: -1`, and `ulimits.nofile: 1048576`, running `entrypoint-tune.sh` before starting the binary.

### 3. Containerized Multi-IP Load Tester (`crates/load-tester` & `docker/load-tester/`)
- Runs in a dedicated container on a custom Docker bridge network (`172.28.0.0/16`) with unthrottled host CPU resources so the load generator is never the bottleneck.
- `docker/load-tester/entrypoint-ips.sh` automatically assigns 10 virtual Secondary IPs (`172.28.0.10` through `172.28.0.19`) to `eth0` inside the load-tester container using `ip addr add`.
- The Rust `load-tester` binary uses `io_uring` (or high-concurrency non-blocking epoll/tokio tasks) and round-robins across those 10 local source IPs with `IP_BIND_ADDRESS_NO_PORT` (`libc::setsockopt`). This allows a single user PC to open 150,000+ simultaneous TCP connections to the target server container without hitting the 64,511 single-IP ephemeral port limit.
- Outputs live progress every 5 seconds and a final Markdown summary table reporting: **Target Concurrent Users, Active Connected Users, Total Requests, RPS, P50/P95/P99 Latency (ms), Socket/Packet Errors, and Server CPU/RAM Utilization**.

---

## Execution Plan
1. Scaffold the Cargo workspace and verify shared types/models.
2. Implement **Phase 1 (`phase1-axum-baseline`)** and verify `cargo check`.
3. Implement **Phase 2 (`phase2-iouring-max`)** with the 64-byte connection state, provided buffer rings, multishot accept/recv, and batched SQLite worker thread, and verify `cargo check`.
4. Implement **Phase 3 (`phase3-kernel-unleashed`)** with thread-per-core `SO_REUSEPORT` `io_uring` rings and partitioned SQLite readers/batched writer, and verify `cargo check`.
5. Implement **`crates/load-tester`** with multi-source-IP binding (`IP_BIND_ADDRESS_NO_PORT`), ramp-up control, persistent connection hold, and latency histogram tracking (`hdrhistogram`).
6. Create all `docker/` files, multi-stage `Dockerfile`s, `.env` files, `scripts/host-kernel-prep.sh`, `docker-compose.yml`, `Makefile` (`make bench-phase1`, `make bench-phase2`, `make bench-phase3`), and `README.md`.