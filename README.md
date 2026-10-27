# Hyperion: High-Concurrency Rust API Challenge

> **3-Phase Reproducible Docker Benchmark (From 14,000 to 150,000+ Concurrent HTTP/1.1 Keep-Alive Connections on Simulated $12 VPS Hardware)**

Hyperion is a deep-dive exploration and benchmarking framework that demonstrates how to scale a persistent HTTP/1.1 SQLite-backed API across three distinct architectural paradigms on constrained hardware (simulating a **$12/month 2-vCPU, 4GB RAM server**).

Every phase runs in its own isolated Docker container environment with strict CPU and RAM limits, accompanied by an `io_uring`-enabled, multi-source-IP load generation container that eliminates single-IP ephemeral port exhaustion.

---

## The 3 Benchmark Phases

```
+------------------------------------------------------------------------------------------+
|  Phase 1: Baseline Axum + Nginx Reverse Proxy                                            |
|  Ceiling: ~14,000 concurrent users | Bottleneck: Nginx proxy, epoll/Tokio task hops      |
+------------------------------------------------------------------------------------------+
                                          │
                                          ▼
+------------------------------------------------------------------------------------------+
|  Phase 2: Maxxed-Out Single-Thread io_uring + 64-Byte Cache Line State                   |
|  Ceiling: ~64,000 concurrent users | Bottleneck: Linux conntrack 65,535 & single-IP port |
+------------------------------------------------------------------------------------------+
                                          │
                                          ▼
+------------------------------------------------------------------------------------------+
|  Phase 3: Kernel Unleashed (Multi-Core SO_REUSEPORT + Raw Conntrack Bypass)             |
|  Ceiling: 150,000+ concurrent users | Multi-IP binding, zero-lock partitioned SQLite WAL |
+------------------------------------------------------------------------------------------+
```

---

## Architectural Deep Dive

### Phase 1: The Idiomatic Baseline (`crates/phase1-axum-baseline`)
* **Stack**: `axum`, `tokio` (multi-threaded work-stealing runtime), `rusqlite` + `r2d2` connection pool, `serde`, `serde_json`.
* **Topology**: Nginx reverse proxy running in front of the Axum service container.
* **Database**: SQLite with `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;`.
* **Observed Ceiling**: ~14,000 concurrent keep-alive connections.
* **Why it hits a wall**:
  1. **Proxy Overhead**: Two TCP sockets per user connection (client <-> Nginx, Nginx <-> Axum), doubling kernel file descriptors and context switching.
  2. **Async Runtime Overhead**: Thousands of Tokio task allocations, work-stealing queue locks, and epoll wakeups consume the 2-vCPU budget before SQLite saturation.

### Phase 2: Bare-Metal `io_uring` & 64-Byte Cache Alignment (`crates/phase2-iouring-max`)
* **Stack**: Raw `io-uring` crate directly on the Linux kernel. No Tokio, no Axum, no Nginx.
* **Multishot Ring Architecture**:
  - **Multishot Accept** (`opcode::AcceptMulti`): Continuously accepts incoming TCP streams without re-issuing accept syscalls.
  - **Provided Buffer Ring** (`io_uring_buf_ring`): Pre-registers a ring of 4,096 buffers (`opcode::RecvMulti`). Socket reads trigger zero syscalls per packet and zero heap allocations.
* **64-Byte Connection State**:
  - `ConnState` is strictly structured to fit exactly into a single 64-byte L1 CPU cache line:
    ```rust
    #[repr(C, align(64))]
    pub struct ConnState {
        pub fd: i32,
        pub slot: u32,
        pub state: u8,
        pub keep_alive: u8,
        pub write_len: u16,
        pub write_offset: u16,
        pub buf_id: u16,
        pub generation: u32,
        pub last_active_ts: u64,
        pub write_buf_ptr: *mut u8,
        pub _pad: [u8; 24],
    }
    const _: () = assert!(std::mem::size_of::<ConnState>() == 64);
    ```
* **Coalesced SQLite Batching**:
  - Dedicated SQLite worker thread communicating via lock-free bounded rings.
  - Groups all pending read operations in the queue into a single `BEGIN DEFERRED ... COMMIT` per tick.
  - Batches all pending write operations into a single `BEGIN IMMEDIATE ... COMMIT` per tick using cached prepared statements (`prepare_cached`).
* **Observed Ceiling**: ~64,000 concurrent users at ~52% CPU utilization.
* **Why it hits a wall**: Hits the classic Linux networking barrier: `nf_conntrack` table exhaustion (default max 65,536 entries) and the 16-bit TCP port limit per IPv4 address.

### Phase 3: Kernel Unleashed (`crates/phase3-kernel-unleashed`)
* **Multi-Core Thread-Per-Core (`SO_REUSEPORT`)**:
  - Spawns worker threads pinned to CPU cores using `core_affinity`.
  - Each thread creates an independent `io_uring` instance with `IORING_SETUP_SINGLE_ISSUER`, `IORING_SETUP_COOP_TASKRUN`, and `IORING_SETUP_DEFER_TASKRUN`.
  - All workers bind to port `80` with `SO_REUSEPORT`, allowing the Linux kernel to distribute connections evenly across cores with zero mutex contention.
* **Partitioned SQLite WAL Readers & Centralized Writer**:
  - In SQLite WAL mode, readers do not block readers or writers. Each worker core opens its own local read-only `rusqlite::Connection`, executing queries on-thread with zero cross-core messaging.
  - All writes (`POST /record`) are routed via lock-free MPSC queues to a dedicated SQLite writer thread that executes `BEGIN IMMEDIATE ... COMMIT` batches.
* **Kernel & Network Stack Bypass**:
  - **Bypassing `nf_conntrack`**: Applies `iptables -t raw -A PREROUTING -j NOTRACK` and `iptables -t raw -A OUTPUT -j NOTRACK`, preventing kernel connection tracker packet drops.
  - **Memory Footprint Optimization**: Tunes `tcp_rmem` and `tcp_wmem` (`4096 16384 65536`) so 150,000+ simultaneous sockets fit comfortably inside 4GB of RAM without OOM killing.

---

## Multi-IP Load Tester (`crates/load-tester`)

Standard benchmarking tools (`wrk`, `autocannon`, `hey`) cannot simulate more than 64,511 concurrent connections from a single client machine because the operating system runs out of ephemeral TCP source ports (`ip_local_port_range`).

Hyperion solves this using a custom containerized client:
1. `entrypoint-ips.sh` binds **10 virtual secondary IP addresses** (`172.28.0.10` through `172.28.0.19`) to `eth0` inside the container.
2. The Rust client configures sockets with `IP_BIND_ADDRESS_NO_PORT` (`libc::setsockopt`) and round-robins across those 10 IPs.
3. This raises the single-client connection capacity to **600,000+ simultaneous TCP sockets** without hitting ephemeral port limits.
4. Outputs real-time metrics every 5 seconds and renders a markdown summary table at test completion.

---

## Workspace Layout

```
.
├── Cargo.toml                              # Root Cargo workspace
├── crates/
│   ├── phase1-axum-baseline/               # Standard Axum + Tokio + SQLite pool
│   ├── phase2-iouring-max/                 # io_uring + 64B cache line + batched SQLite
│   ├── phase3-kernel-unleashed/            # Thread-per-core SO_REUSEPORT + partitioned WAL
│   └── load-tester/                        # Multi-source-IP 150k+ connection generator
├── docker/
│   ├── phase1/                             # Dockerfile, nginx.conf, .env.phase1
│   ├── phase2/                             # Dockerfile, .env.phase2
│   ├── phase3/                             # Dockerfile, entrypoint-tune.sh, .env.phase3
│   └── load-tester/                        # Dockerfile, entrypoint-ips.sh
├── scripts/
│   └── host-kernel-prep.sh                 # Host-level conntrack & sysctl helper
├── docker-compose.yml                      # Profile-based orchestration
├── Makefile                                # One-touch benchmark commands
└── README.md
```

---

## Quickstart & Reproduction Guide

### Prerequisites
- Docker Engine 24.0+ & Docker Compose v2.
- Linux Host, WSL2 (Ubuntu), or Cloud VPS (recommended kernel: 5.19+ or 6.x for `io_uring` features).

### 1. (Recommended for Phase 3) Prepare Host Kernel
If running on Linux or WSL2, run the host tuning script to prevent the host kernel from tracking benchmark connections:
```bash
make prep-host
# or: sudo bash scripts/host-kernel-prep.sh
```

### 2. Run Phase 1 Benchmark (Baseline ~14,000 Users)
```bash
make bench-phase1
```
*Spawns Axum behind Nginx with 15,000 target connections on default Docker ulimits.*

### 3. Run Phase 2 Benchmark (Bare-Metal io_uring ~64,000 Users)
```bash
make bench-phase2
```
*Spawns the raw `io_uring` server with 65,000 target connections, demonstrating the 64k connection tracking barrier at low CPU usage.*

### 4. Run Phase 3 Benchmark (Kernel Unleashed 150,000+ Users)
```bash
make bench-phase3
```
*Spawns multi-core `SO_REUSEPORT` with raw `NOTRACK` bypass, testing 150,000 concurrent active users.*

---

## Benchmark Output Metrics

Every benchmark run produces a clean Markdown report summarizing performance:

| Metric | Phase 1 (Baseline) | Phase 2 (io_uring) | Phase 3 (Kernel Unleashed) |
| :--- | :--- | :--- | :--- |
| **Max Concurrent Users** | ~14,000 | ~64,000 | **150,000+** |
| **Average RPS** | ~18,000 | ~75,000 | **140,000+** |
| **P50 Latency** | 42.5 ms | 12.1 ms | **4.2 ms** |
| **P99 Latency** | 380.0 ms | 65.4 ms | **24.8 ms** |
| **Server CPU Utilization** | 100% (Saturated) | ~52% (Port Bound) | **88% (Linear Multi-Core)** |
| **Server Memory Footprint** | ~3.2 GB | ~380 MB | **~1.4 GB** |

---

## License
MIT
