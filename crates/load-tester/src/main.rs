use clap::Parser;
use hdrhistogram::Histogram;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(author, version, about = "High-concurrency multi-IP load tester")]
struct Args {
    #[arg(short, long, default_value = "http://127.0.0.1:8080")]
    target: String,

    #[arg(short, long, default_value_t = 1000)]
    concurrency: usize,

    #[arg(short, long, default_value_t = 30)]
    duration: u64,

    #[arg(short, long, default_value_t = 5)]
    ramp_up: u64,

    #[arg(long, default_value = "")]
    ips: String,

    #[arg(long, default_value_t = 10)]
    write_ratio: u32,

    #[arg(long, default_value_t = 0)]
    pacing_ms: u64,
}

struct Stats {
    total_requests: AtomicU64,
    total_errors: AtomicU64,
    active_conns: AtomicUsize,
    histograms: Mutex<Histogram<u64>>,
}

#[cfg(target_os = "linux")]
fn apply_no_port_bind(socket: &socket2::Socket) {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    let one: libc::c_int = 1;
    const IP_BIND_ADDRESS_NO_PORT: libc::c_int = 24;
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            IP_BIND_ADDRESS_NO_PORT,
            &one as *const _ as _,
            std::mem::size_of::<libc::c_int>() as u32,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_no_port_bind(_socket: &socket2::Socket) {}

fn read_cgroup_cpu_mem() -> (f64, u64) {
    let mem_used = fs::read_to_string("/sys/fs/cgroup/memory.current")
        .or_else(|_| fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let mem_mb = mem_used / (1024 * 1024);
    (0.0, mem_mb)
}

async fn run_connection_worker(
    target_addr: SocketAddr,
    source_ip: Option<IpAddr>,
    write_ratio: u32,
    pacing_ms: u64,
    end_time: Instant,
    stats: Arc<Stats>,
) {
    while Instant::now() < end_time {
        let socket = match socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        ) {
            Ok(s) => s,
            Err(_) => {
                stats.total_errors.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };

        let _ = socket.set_reuse_address(true);
        let _ = socket.set_nodelay(true);
        let _ = socket.set_nonblocking(true);

        apply_no_port_bind(&socket);

        if let Some(ip) = source_ip {
            let bind_addr = SocketAddr::new(ip, 0);
            let _ = socket.bind(&bind_addr.into());
        }

        let stream_res = socket.connect(&target_addr.into());
        let mut stream = match stream_res {
            Ok(()) => match std::net::TcpStream::from(socket).try_into() {
                Ok(s) => s,
                Err(_) => {
                    stats.total_errors.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {
                let std_stream: std::net::TcpStream = socket.into();
                match TcpStream::from_std(std_stream) {
                    Ok(s) => match s.writable().await {
                        Ok(()) => s,
                        Err(_) => {
                            stats.total_errors.fetch_add(1, Ordering::Relaxed);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    },
                    Err(_) => {
                        stats.total_errors.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                }
            }
            Err(_) => {
                stats.total_errors.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };

        stats.active_conns.fetch_add(1, Ordering::Relaxed);

        let mut read_buf = [0u8; 1024];

        // Keep-alive request loop
        while Instant::now() < end_time {
            let is_write = (rand::random::<u32>() % 100) < write_ratio.min(100);
            let start = Instant::now();

            let write_res = if is_write {
                let payload = b"{\"payload\":\"load_tester_benchmark_record\"}";
                let req = format!(
                    "POST /record HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                    target_addr,
                    payload.len()
                );
                async {
                    stream.write_all(req.as_bytes()).await?;
                    stream.write_all(payload).await
                }
                .await
            } else {
                let id = (rand::random::<u64>() % 999) + 1;
                let req = format!(
                    "GET /record/{} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
                    id, target_addr
                );
                stream.write_all(req.as_bytes()).await
            };

            if write_res.is_err() {
                stats.total_errors.fetch_add(1, Ordering::Relaxed);
                break;
            }

            // Read response
            let read_res = stream.read(&mut read_buf).await;
            match read_res {
                Ok(n) if n > 0 => {
                    let latency_us = start.elapsed().as_micros() as u64;
                    stats.total_requests.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut h) = stats.histograms.try_lock() {
                        let _ = h.record(latency_us);
                    }
                    if pacing_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(pacing_ms)).await;
                    }
                }
                _ => {
                    stats.total_errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
        }

        stats.active_conns.fetch_sub(1, Ordering::Relaxed);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("============================================================");
    println!(" Hyperion High-Concurrency Multi-IP Benchmark Client");
    println!("============================================================");
    println!(" Target URL:      {}", args.target);
    println!(" Target Users:    {}", args.concurrency);
    println!(" Duration:        {}s", args.duration);
    println!(" Ramp-up:         {}s", args.ramp_up);
    println!(" Write Ratio:     {}%", args.write_ratio);

    let parsed_url = args
        .target
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let target_addr: SocketAddr = tokio::net::lookup_host(parsed_url)
        .await?
        .next()
        .expect("Failed to resolve target address");

    println!(" Resolved Target: {}", target_addr);

    let mut source_ips: Vec<IpAddr> = Vec::new();
    if !args.ips.is_empty() {
        for s in args.ips.split(',') {
            let s = s.trim();
            if let Ok(ip) = s.parse::<IpAddr>() {
                source_ips.push(ip);
            }
        }
    }

    if source_ips.is_empty() {
        println!(" Source IPs:      Default (Single Local IP)");
    } else {
        println!(" Bound Source IPs ({} IPs): {:?}", source_ips.len(), source_ips);
    }
    println!("============================================================");

    let stats = Arc::new(Stats {
        total_requests: AtomicU64::new(0),
        total_errors: AtomicU64::new(0),
        active_conns: AtomicUsize::new(0),
        histograms: Mutex::new(Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)?),
    });

    let end_time = Instant::now() + Duration::from_secs(args.duration + args.ramp_up);

    // Spawn live reporter ticker
    let stats_reporter = stats.clone();
    let reporter_end = end_time;
    tokio::spawn(async move {
        let mut prev_requests = 0u64;
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;

        while Instant::now() < reporter_end {
            interval.tick().await;
            let current_requests = stats_reporter.total_requests.load(Ordering::Relaxed);
            let rps = (current_requests.saturating_sub(prev_requests)) / 5;
            prev_requests = current_requests;

            let active = stats_reporter.active_conns.load(Ordering::Relaxed);
            let errors = stats_reporter.total_errors.load(Ordering::Relaxed);

            let (p50, p95, p99) = {
                if let Ok(h) = stats_reporter.histograms.try_lock() {
                    (
                        h.value_at_quantile(0.50) as f64 / 1000.0,
                        h.value_at_quantile(0.95) as f64 / 1000.0,
                        h.value_at_quantile(0.99) as f64 / 1000.0,
                    )
                } else {
                    (0.0, 0.0, 0.0)
                }
            };

            println!(
                "[{:>3}s] Active: {:>6} | Total Req: {:>8} | RPS: {:>7} | P50: {:>6.2}ms | P95: {:>6.2}ms | P99: {:>6.2}ms | Errors: {:>4}",
                Instant::now().elapsed().as_secs(),
                active,
                current_requests,
                rps,
                p50,
                p95,
                p99,
                errors
            );
        }
    });

    // Ramp up connections
    let spawn_delay = if args.ramp_up > 0 && args.concurrency > 0 {
        Duration::from_secs_f64(args.ramp_up as f64 / args.concurrency as f64)
    } else {
        Duration::from_millis(0)
    };

    let start_bench = Instant::now();
    for i in 0..args.concurrency {
        let target = target_addr;
        let sip = if !source_ips.is_empty() {
            Some(source_ips[i % source_ips.len()])
        } else {
            None
        };
        let s = stats.clone();
        let pacing = args.pacing_ms;
        tokio::spawn(async move {
            run_connection_worker(target, sip, args.write_ratio, pacing, end_time, s).await;
        });

        if spawn_delay > Duration::from_micros(10) {
            tokio::time::sleep(spawn_delay).await;
        }
    }

    tokio::time::sleep_until(tokio::time::Instant::now() + Duration::from_secs(args.duration + args.ramp_up)).await;

    let elapsed = start_bench.elapsed().as_secs_f64();
    let total_req = stats.total_requests.load(Ordering::Relaxed);
    let avg_rps = if elapsed > 0.0 {
        (total_req as f64 / elapsed).round() as u64
    } else {
        0
    };
    let total_err = stats.total_errors.load(Ordering::Relaxed);
    let final_active = stats.active_conns.load(Ordering::Relaxed);

    let (p50, p95, p99) = {
        let h = stats.histograms.lock().await;
        (
            h.value_at_quantile(0.50) as f64 / 1000.0,
            h.value_at_quantile(0.95) as f64 / 1000.0,
            h.value_at_quantile(0.99) as f64 / 1000.0,
        )
    };

    let (_cpu, mem_mb) = read_cgroup_cpu_mem();

    println!("\n============================================================");
    println!(" Benchmark Completed: Final Summary Table");
    println!("============================================================");
    println!("| Metric                     | Value                 |");
    println!("| :------------------------- | :-------------------- |");
    println!("| Target Concurrent Users    | {:<21} |", args.concurrency);
    println!("| Active Connected Users     | {:<21} |", final_active);
    println!("| Total Requests             | {:<21} |", total_req);
    println!("| Average RPS                | {:<21} |", avg_rps);
    println!("| P50 Latency (ms)           | {:<21.2} |", p50);
    println!("| P95 Latency (ms)           | {:<21.2} |", p95);
    println!("| P99 Latency (ms)           | {:<21.2} |", p99);
    println!("| Socket/Packet Errors       | {:<21} |", total_err);
    println!("| Benchmark Duration (s)     | {:<21.1} |", elapsed);
    println!("| Container RAM (approx MB)  | {:<21} |", mem_mb);
    println!("============================================================");

    Ok(())
}
