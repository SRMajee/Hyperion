mod db;
mod http;
mod net;

use crate::db::{init_db, open_readonly, open_writer};
use crate::http::{format_404_response, format_get_response, format_post_response, parse_http_request, HttpRequest};
use crate::net::conntable::{ConnTable, STATE_ESTABLISHED};
use crate::net::ethernet::{ArpPacket, EthernetHeader, ARP_OPER_REQUEST, ETHERTYPE_ARP, ETHERTYPE_IPV4};
use crate::net::ipv4::{Ipv4Header, IPPROTO_TCP};
use crate::net::packet_io::PacketSocket;
use crate::net::tcp::{TcpHeader, TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN};
use rusqlite::params;
use std::env;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct GlobalStats {
    total_requests: AtomicU64,
    total_syns: AtomicU64,
    active_conns: AtomicUsize,
}

fn build_and_send_tcp(
    socket: &PacketSocket,
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
    packet_id: u16,
    tx_buf: &mut [u8],
) -> bool {
    let eth_len = 14;
    let ip_len = 20;
    let tcp_len = 20;
    let total_frame_len = eth_len + ip_len + tcp_len + payload.len();

    if tx_buf.len() < total_frame_len {
        return false;
    }

    // 1. Ethernet Header
    let eth = EthernetHeader {
        dst_mac,
        src_mac: socket.our_mac,
        ethertype: ETHERTYPE_IPV4.to_be(),
    };
    unsafe {
        std::ptr::copy_nonoverlapping(
            &eth as *const _ as *const u8,
            tx_buf.as_mut_ptr(),
            eth_len,
        );
    }

    // 2. IPv4 Header
    let ip = Ipv4Header::build(
        socket.our_ip,
        dst_ip,
        IPPROTO_TCP,
        tcp_len + payload.len(),
        packet_id,
    );
    unsafe {
        std::ptr::copy_nonoverlapping(
            &ip as *const _ as *const u8,
            tx_buf.as_mut_ptr().add(eth_len),
            ip_len,
        );
    }

    // 3. TCP Header
    let mut tcp = TcpHeader::build(
        src_port,
        dst_port,
        seq,
        ack,
        flags,
        65535,
    );

    // Copy TCP header and payload to compute checksum
    let tcp_offset = eth_len + ip_len;
    unsafe {
        std::ptr::copy_nonoverlapping(
            &tcp as *const _ as *const u8,
            tx_buf.as_mut_ptr().add(tcp_offset),
            tcp_len,
        );
        if !payload.is_empty() {
            std::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                tx_buf.as_mut_ptr().add(tcp_offset + tcp_len),
                payload.len(),
            );
        }
    }

    let tcp_chk = TcpHeader::calculate_checksum(
        socket.our_ip,
        dst_ip,
        &tx_buf[tcp_offset..tcp_offset + tcp_len + payload.len()],
    );
    tcp.checksum = tcp_chk.to_be();

    unsafe {
        std::ptr::copy_nonoverlapping(
            &tcp as *const _ as *const u8,
            tx_buf.as_mut_ptr().add(tcp_offset),
            tcp_len,
        );
    }

    socket.send(&tx_buf[..total_frame_len]).is_ok()
}

fn run_worker(
    worker_id: usize,
    db_path: String,
    iface: String,
    our_ip_str: String,
    stats: Arc<GlobalStats>,
) {
    #[cfg(target_os = "linux")]
    if let Some(core_ids) = core_affinity::get_core_ids() {
        if let Some(core) = core_ids.get(worker_id % core_ids.len()) {
            let _ = core_affinity::set_for_current(*core);
        }
    }

    println!("[Worker {}] Initializing Packet Socket on {} ({})", worker_id, iface, our_ip_str);
    let socket = match PacketSocket::new(&iface, &our_ip_str) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[Worker {}] Failed to create packet socket: {}", worker_id, e);
            return;
        }
    };

    println!("[Worker {}] MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        worker_id,
        socket.our_mac[0], socket.our_mac[1], socket.our_mac[2],
        socket.our_mac[3], socket.our_mac[4], socket.our_mac[5]
    );

    // Pre-allocate 1,048,576 slots (64MB) per worker core
    let mut conntable = ConnTable::new(1048576);

    let read_conn = open_readonly(&db_path).expect("Failed to open readonly SQLite connection");
    let mut read_stmt = read_conn
        .prepare_cached("SELECT payload, updated_at FROM records WHERE id = ?1")
        .expect("Failed to prepare cached read query");

    let write_conn = open_writer(&db_path).expect("Failed to open writer SQLite connection");
    let mut write_stmt = write_conn
        .prepare_cached("INSERT INTO records (payload, updated_at) VALUES (?1, ?2)")
        .expect("Failed to prepare cached write query");

    let mut rx_buf = [0u8; 2048];
    let mut tx_buf = [0u8; 2048];
    let mut http_body_buf = [0u8; 1024];
    let mut packet_id: u16 = (worker_id as u16) * 10000;

    let mut idle_spins = 0u32;

    loop {
        match socket.recv(&mut rx_buf) {
            Ok(frame_len) => {
                idle_spins = 0;
                let frame = &rx_buf[..frame_len];
                let (eth_hdr, eth_payload) = match EthernetHeader::parse(frame) {
                    Some(res) => res,
                    None => continue,
                };

                let ethertype = u16::from_be(eth_hdr.ethertype);

                // 1. ARP Handling
                if ethertype == ETHERTYPE_ARP {
                    if let Some(arp) = ArpPacket::parse(eth_payload) {
                        if u16::from_be(arp.oper) == ARP_OPER_REQUEST && arp.tpa == socket.our_ip {
                            let reply_len = ArpPacket::build_reply(
                                socket.our_mac,
                                socket.our_ip,
                                arp.sha,
                                arp.spa,
                                &mut tx_buf,
                            );
                            if reply_len > 0 {
                                let _ = socket.send(&tx_buf[..reply_len]);
                            }
                        }
                    }
                    continue;
                }

                // 2. IPv4 Handling
                if ethertype != ETHERTYPE_IPV4 {
                    continue;
                }

                let (ip_hdr, ip_payload) = match Ipv4Header::parse(eth_payload) {
                    Some(res) => res,
                    None => continue,
                };

                if ip_hdr.protocol != IPPROTO_TCP || ip_hdr.dst_ip != socket.our_ip {
                    continue;
                }

                // 3. TCP Handling
                let (tcp_hdr, tcp_payload) = match TcpHeader::parse(ip_payload) {
                    Some(res) => res,
                    None => continue,
                };

                let dst_port = u16::from_be(tcp_hdr.dst_port);
                if dst_port != 80 {
                    continue;
                }

                let src_port = u16::from_be(tcp_hdr.src_port);
                let client_ip = u32::from_be_bytes(ip_hdr.src_ip);
                let client_seq = u32::from_be(tcp_hdr.seq_num);
                let flags = tcp_hdr.flags;

                packet_id = packet_id.wrapping_add(1);

                // Case A: SYN Packet (New Connection Handshake)
                if (flags & TCP_SYN) != 0 && (flags & TCP_ACK) == 0 {
                    let handshake_info = if let Some(conn) = conntable.insert_or_get(
                        client_ip,
                        src_port,
                        eth_hdr.src_mac,
                        client_seq,
                    ) {
                        Some((conn.snd_nxt.wrapping_sub(1), conn.rcv_nxt))
                    } else {
                        None
                    };

                    if let Some((isn, rcv_nxt)) = handshake_info {
                        stats.total_syns.fetch_add(1, Ordering::Relaxed);
                        stats.active_conns.store(conntable.active_conns(), Ordering::Relaxed);

                        build_and_send_tcp(
                            &socket,
                            eth_hdr.src_mac,
                            ip_hdr.src_ip,
                            80,
                            src_port,
                            isn,
                            rcv_nxt,
                            TCP_SYN | TCP_ACK,
                            &[],
                            packet_id,
                            &mut tx_buf,
                        );
                    }
                    continue;
                }

                // Case B: RST Packet (Connection Reset)
                if (flags & TCP_RST) != 0 {
                    conntable.remove(client_ip, src_port);
                    stats.active_conns.store(conntable.active_conns(), Ordering::Relaxed);
                    continue;
                }

                // Case C: FIN Packet (Connection Close)
                if (flags & TCP_FIN) != 0 {
                    if let Some(conn) = conntable.find(client_ip, src_port) {
                        conn.rcv_nxt = client_seq.wrapping_add(1);
                        build_and_send_tcp(
                            &socket,
                            eth_hdr.src_mac,
                            ip_hdr.src_ip,
                            80,
                            src_port,
                            conn.snd_nxt,
                            conn.rcv_nxt,
                            TCP_FIN | TCP_ACK,
                            &[],
                            packet_id,
                            &mut tx_buf,
                        );
                        conntable.remove(client_ip, src_port);
                        stats.active_conns.store(conntable.active_conns(), Ordering::Relaxed);
                    }
                    continue;
                }

                // Case D: ACK / PSH (Established Connection Data or Handshake ACK)
                if (flags & TCP_ACK) != 0 {
                    if let Some(conn) = conntable.find(client_ip, src_port) {
                        conn.state = STATE_ESTABLISHED;

                        if !tcp_payload.is_empty() {
                            // Update receive sequence
                            conn.rcv_nxt = client_seq.wrapping_add(tcp_payload.len() as u32);

                            // Parse and execute HTTP request
                            let response_len = match parse_http_request(tcp_payload) {
                                HttpRequest::Get(id) => {
                                    let mut row_res: Option<(String, i64)> = None;
                                    if let Ok(mut rows) = read_stmt.query(params![id]) {
                                        if let Ok(Some(row)) = rows.next() {
                                            if let (Ok(p), Ok(ts)) = (row.get(0), row.get(1)) {
                                                row_res = Some((p, ts));
                                            }
                                        }
                                    }
                                    if let Some((payload, ts)) = row_res {
                                        format_get_response(id, &payload, ts, &mut http_body_buf)
                                    } else {
                                        format_404_response(&mut http_body_buf)
                                    }
                                }
                                HttpRequest::Post(payload) => {
                                    let now = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs() as i64;
                                    let _ = write_stmt.execute(params![payload, now]);
                                    let id = write_conn.last_insert_rowid();
                                    format_post_response(id, &mut http_body_buf)
                                }
                                HttpRequest::Other => format_404_response(&mut http_body_buf),
                            };

                            let resp_payload = &http_body_buf[..response_len];
                            build_and_send_tcp(
                                &socket,
                                eth_hdr.src_mac,
                                ip_hdr.src_ip,
                                80,
                                src_port,
                                conn.snd_nxt,
                                conn.rcv_nxt,
                                TCP_ACK | TCP_PSH,
                                resp_payload,
                                packet_id,
                                &mut tx_buf,
                            );

                            conn.snd_nxt = conn.snd_nxt.wrapping_add(response_len as u32);
                            stats.total_requests.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
            Err(std::io::ErrorKind::WouldBlock) => {
                idle_spins = idle_spins.wrapping_add(1);
                if idle_spins > 500 {
                    std::thread::yield_now();
                    idle_spins = 0;
                } else {
                    std::hint::spin_loop();
                }
            }
            Err(_) => {
                std::thread::yield_now();
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("============================================================");
    println!(" Hyperion Phase 4: Kernel Bypass User-Space Micro-TCP Server");
    println!("============================================================");

    let db_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "/data/hyperion_phase4.db".to_string());
    let iface = env::var("IFACE").unwrap_or_else(|_| "eth0".to_string());
    let our_ip = env::var("SERVER_IP").unwrap_or_else(|_| "172.28.0.2".to_string());

    println!(" Database:         {}", db_path);
    println!(" Interface:        {}", iface);
    println!(" Server IP:        {}:80", our_ip);
    println!(" Micro-TCP Conn:   64-byte zero-copy cache-aligned structs");
    println!(" Max Table Slots:  1,048,576 slots per CPU core (~64MB RAM/core)");
    println!(" Target Envelope:  2 vCPUs | 4GB RAM");
    println!("============================================================");

    init_db(&db_path)?;
    println!("[DB] SQLite database initialized and seeded with 1000 records.");

    let stats = Arc::new(GlobalStats {
        total_requests: AtomicU64::new(0),
        total_syns: AtomicU64::new(0),
        active_conns: AtomicUsize::new(0),
    });

    // Spawn 5s stats ticker
    let ticker_stats = stats.clone();
    std::thread::spawn(move || {
        let mut prev_requests = 0u64;
        let mut prev_syns = 0u64;
        let start = Instant::now();

        loop {
            std::thread::sleep(Duration::from_secs(5));
            let total_req = ticker_stats.total_requests.load(Ordering::Relaxed);
            let total_syn = ticker_stats.total_syns.load(Ordering::Relaxed);
            let active = ticker_stats.active_conns.load(Ordering::Relaxed);

            let rps = (total_req.saturating_sub(prev_requests)) / 5;
            let syn_rate = (total_syn.saturating_sub(prev_syns)) / 5;
            prev_requests = total_req;
            prev_syns = total_syn;

            println!(
                "[{:>3}s] Active Conns: {:>7} | Total Req: {:>9} | RPS: {:>7} | Handshakes/s: {:>6}",
                start.elapsed().as_secs(),
                active,
                total_req,
                rps,
                syn_rate
            );
        }
    });

    // Spawn 2 pinned worker threads (matching 2 vCPU budget)
    let mut handles = Vec::new();
    for worker_id in 0..2 {
        let p = db_path.clone();
        let i = iface.clone();
        let ip = our_ip.clone();
        let s = stats.clone();
        let h = std::thread::Builder::new()
            .name(format!("phase4-worker-{}", worker_id))
            .spawn(move || {
                run_worker(worker_id, p, i, ip, s);
            })?;
        handles.push(h);
    }

    for h in handles {
        let _ = h.join();
    }

    Ok(())
}
