#[cfg(target_os = "linux")]
mod linux_impl {
    use crossbeam_channel::{bounded, Receiver, Sender};
    use io_uring::{cqueue, opcode, types, IoUring};
    use rusqlite::{params, Connection};
    use std::env;
    use std::os::unix::io::RawFd;
    use std::sync::atomic::{fence, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub const MAX_CONNS: usize = 65536;
    pub const BUF_RING_ENTRIES: u16 = 4096;
    pub const BUF_SIZE: usize = 2048;
    pub const WRITE_BUF_SIZE: usize = 512;
    pub const BUF_GROUP_ID: u16 = 1;

    pub const TOKEN_ACCEPT: u64 = 1 << 62;
    pub const TOKEN_EVENTFD: u64 = 2 << 62;
    pub const TOKEN_RECV_MASK: u64 = 3 << 62;
    pub const TOKEN_SEND_MASK: u64 = 4 << 62;
    pub const TOKEN_SLOT_MASK: u64 = 0x3FFFFFFF;

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
    const _: () = assert!(std::mem::align_of::<ConnState>() == 64);

    impl ConnState {
        pub const fn new() -> Self {
            Self {
                fd: -1,
                slot: 0,
                state: 0,
                keep_alive: 1,
                write_len: 0,
                write_offset: 0,
                buf_id: 0,
                generation: 0,
                last_active_ts: 0,
                write_buf_ptr: std::ptr::null_mut(),
                _pad: [0; 24],
            }
        }
    }

    pub enum DbRequest {
        Get {
            slot: u32,
            fd: i32,
            id: i64,
        },
        Post {
            slot: u32,
            fd: i32,
            payload_len: usize,
            payload: [u8; 256],
        },
    }

    pub enum DbResponse {
        GetResult {
            slot: u32,
            fd: i32,
            found: bool,
            id: i64,
            payload_len: usize,
            payload: [u8; 256],
            updated_at: i64,
        },
        PostResult {
            slot: u32,
            fd: i32,
            id: i64,
        },
        NotFound {
            slot: u32,
            fd: i32,
        },
        Error {
            slot: u32,
            fd: i32,
        },
    }

    pub struct ProvidedBufRing {
        pub ring_mem: *mut types::BufRingEntry,
        pub buf_mem: *mut u8,
        pub tail: u16,
    }

    impl ProvidedBufRing {
        pub fn new(submitter: &io_uring::Submitter) -> Self {
            let ring_size = (BUF_RING_ENTRIES as usize) * std::mem::size_of::<types::BufRingEntry>();
            let mut ring_layout_mem: *mut libc::c_void = std::ptr::null_mut();
            let ret = unsafe {
                libc::posix_memalign(&mut ring_layout_mem, 4096, ring_size)
            };
            assert_eq!(ret, 0, "Failed to allocate memory for buffer ring");
            let ring_mem = ring_layout_mem as *mut types::BufRingEntry;

            let buf_total_size = (BUF_RING_ENTRIES as usize) * BUF_SIZE;
            let mut buf_layout_mem: *mut libc::c_void = std::ptr::null_mut();
            let ret2 = unsafe {
                libc::posix_memalign(&mut buf_layout_mem, 4096, buf_total_size)
            };
            assert_eq!(ret2, 0, "Failed to allocate memory for buffer payload pool");
            let buf_mem = buf_layout_mem as *mut u8;

            for i in 0..BUF_RING_ENTRIES {
                unsafe {
                    let entry = ring_mem.add(i as usize);
                    let buf_addr = buf_mem.add((i as usize) * BUF_SIZE) as u64;
                    (*entry).set_addr(buf_addr);
                    (*entry).set_len(BUF_SIZE as u32);
                    (*entry).set_bid(i);
                }
            }

            unsafe {
                let tail_ptr = types::BufRingEntry::tail(ring_mem) as *mut u16;
                std::ptr::write_volatile(tail_ptr, BUF_RING_ENTRIES);
                fence(Ordering::Release);

                submitter
                    .register_buf_ring_with_flags(ring_mem as u64, BUF_RING_ENTRIES, BUF_GROUP_ID, 0)
                    .expect("Failed to register io_uring buffer ring");
            }

            Self {
                ring_mem,
                buf_mem,
                tail: BUF_RING_ENTRIES,
            }
        }

        #[inline(always)]
        pub fn replenish(&mut self, bid: u16) {
            unsafe {
                let mask = (BUF_RING_ENTRIES - 1) as usize;
                let idx = (self.tail as usize) & mask;
                let entry = self.ring_mem.add(idx);
                let buf_addr = self.buf_mem.add((bid as usize) * BUF_SIZE) as u64;
                (*entry).set_addr(buf_addr);
                (*entry).set_len(BUF_SIZE as u32);
                (*entry).set_bid(bid);

                self.tail = self.tail.wrapping_add(1);
                let tail_ptr = types::BufRingEntry::tail(self.ring_mem) as *mut u16;
                std::ptr::write_volatile(tail_ptr, self.tail);
                fence(Ordering::Release);
            }
        }

        #[inline(always)]
        pub fn get_slice(&self, bid: u16, len: usize) -> &[u8] {
            unsafe {
                let ptr = self.buf_mem.add((bid as usize) * BUF_SIZE);
                std::slice::from_raw_parts(ptr, len)
            }
        }
    }

    pub fn start_db_worker(
        db_path: String,
        rx_req: Receiver<DbRequest>,
        tx_resp: Sender<DbResponse>,
        eventfd: RawFd,
    ) {
        thread::spawn(move || {
            let conn = Connection::open(&db_path).expect("Failed to open SQLite database");
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 5000;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA mmap_size = 268435456;
                 PRAGMA cache_size = -64000;
                 CREATE TABLE IF NOT EXISTS records (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     payload TEXT NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_records_updated_at ON records(updated_at);",
            )
            .expect("Database optimization setup failed");

            let mut batch = Vec::with_capacity(1024);

            loop {
                let first = match rx_req.recv() {
                    Ok(req) => req,
                    Err(_) => break,
                };
                batch.clear();
                batch.push(first);

                while batch.len() < 1024 {
                    match rx_req.try_recv() {
                        Ok(req) => batch.push(req),
                        Err(_) => break,
                    }
                }

                let mut has_reads = false;
                let mut has_writes = false;
                for item in &batch {
                    match item {
                        DbRequest::Get { .. } => has_reads = true,
                        DbRequest::Post { .. } => has_writes = true,
                    }
                }

                if has_reads {
                    let _ = conn.execute_batch("BEGIN DEFERRED;");
                    {
                        let mut stmt = conn
                            .prepare_cached("SELECT payload, updated_at FROM records WHERE id = ?1")
                            .expect("Failed to prepare cached read statement");

                        for req in &batch {
                            if let DbRequest::Get { slot, fd, id } = *req {
                                let res = stmt.query_row(params![id], |row| {
                                    let p: String = row.get(0)?;
                                    let u: i64 = row.get(1)?;
                                    Ok((p, u))
                                });

                                match res {
                                    Ok((p, u)) => {
                                        let mut payload_buf = [0u8; 256];
                                        let p_bytes = p.as_bytes();
                                        let len = p_bytes.len().min(256);
                                        payload_buf[..len].copy_from_slice(&p_bytes[..len]);

                                        let _ = tx_resp.send(DbResponse::GetResult {
                                            slot,
                                            fd,
                                            found: true,
                                            id,
                                            payload_len: len,
                                            payload: payload_buf,
                                            updated_at: u,
                                        });
                                    }
                                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                                        let _ = tx_resp.send(DbResponse::NotFound { slot, fd });
                                    }
                                    Err(_) => {
                                        let _ = tx_resp.send(DbResponse::Error { slot, fd });
                                    }
                                }
                            }
                        }
                    }
                    let _ = conn.execute_batch("COMMIT;");
                }

                if has_writes {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    let _ = conn.execute_batch("BEGIN IMMEDIATE;");
                    {
                        let mut stmt = conn
                            .prepare_cached("INSERT INTO records (payload, updated_at) VALUES (?1, ?2)")
                            .expect("Failed to prepare cached insert statement");

                        for req in &batch {
                            if let DbRequest::Post {
                                slot,
                                fd,
                                payload_len,
                                payload,
                            } = *req
                            {
                                if let Ok(s) = std::str::from_utf8(&payload[..payload_len]) {
                                    match stmt.execute(params![s, now]) {
                                        Ok(_) => {
                                            let id = conn.last_insert_rowid();
                                            let _ = tx_resp.send(DbResponse::PostResult { slot, fd, id });
                                        }
                                        Err(_) => {
                                            let _ = tx_resp.send(DbResponse::Error { slot, fd });
                                        }
                                    }
                                } else {
                                    let _ = tx_resp.send(DbResponse::Error { slot, fd });
                                }
                            }
                        }
                    }
                    let _ = conn.execute_batch("COMMIT;");
                }

                let val: u64 = 1;
                unsafe {
                    libc::write(
                        eventfd,
                        &val as *const u64 as *const libc::c_void,
                        std::mem::size_of::<u64>(),
                    );
                }
            }
        });
    }

    #[inline(always)]
    pub fn parse_get_record_id(slice: &[u8]) -> Option<i64> {
        let prefix = b"GET /record/";
        if !slice.starts_with(prefix) {
            return None;
        }
        let rest = &slice[prefix.len()..];
        let mut id: i64 = 0;
        let mut idx = 0;
        while idx < rest.len() {
            let b = rest[idx];
            if b.is_ascii_digit() {
                id = id * 10 + (b - b'0') as i64;
                idx += 1;
            } else {
                break;
            }
        }
        if idx > 0 {
            Some(id)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn parse_post_payload(slice: &[u8]) -> Option<&[u8]> {
        if !slice.starts_with(b"POST /record") {
            return None;
        }
        let marker = b"\r\n\r\n";
        let body_pos = slice.windows(marker.len()).position(|w| w == marker)?;
        let body = &slice[body_pos + marker.len()..];

        let key = b"\"payload\"";
        let key_pos = body.windows(key.len()).position(|w| w == key)?;
        let after_key = &body[key_pos + key.len()..];

        let colon_pos = after_key.iter().position(|&b| b == b':')?;
        let after_colon = &after_key[colon_pos + 1..];

        let start_quote = after_colon.iter().position(|&b| b == b'"')?;
        let str_start = &after_colon[start_quote + 1..];
        let end_quote = str_start.iter().position(|&b| b == b'"')?;

        Some(&str_start[..end_quote])
    }

    pub fn run_server() -> Result<(), Box<dyn std::error::Error>> {
        let port: u16 = env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .unwrap_or(8080);
        let db_path = env::var("DATABASE_URL").unwrap_or_else(|_| "phase2.db".to_string());

        let (tx_db, rx_db) = bounded::<DbRequest>(32768);
        let (tx_resp, rx_resp) = bounded::<DbResponse>(32768);

        let efd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if efd < 0 {
            panic!("Failed to create eventfd");
        }

        start_db_worker(db_path, rx_db, tx_resp, efd);

        let mut ring: IoUring<io_uring::squeue::Entry, io_uring::cqueue::Entry> =
            IoUring::builder().build(4096)?;
        let mut buf_ring = {
            let submitter = ring.submitter();
            ProvidedBufRing::new(&submitter)
        };

        let (submitter, mut sq, mut cq) = ring.split();

        let mut conns: Vec<ConnState> = (0..MAX_CONNS).map(|_| ConnState::new()).collect();
        let mut write_buffers = vec![0u8; MAX_CONNS * WRITE_BUF_SIZE];
        for i in 0..MAX_CONNS {
            conns[i].slot = i as u32;
            conns[i].write_buf_ptr = unsafe { write_buffers.as_mut_ptr().add(i * WRITE_BUF_SIZE) };
        }

        let listener_fd = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0);
            assert!(fd >= 0, "Socket creation failed");

            let one: libc::c_int = 1;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as _, 4);
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, &one as *const _ as _, 4);

            let mut sin: libc::sockaddr_in = std::mem::zeroed();
            sin.sin_family = libc::AF_INET as u16;
            sin.sin_port = port.to_be();
            sin.sin_addr.s_addr = libc::INADDR_ANY.to_be();

            let ret = libc::bind(fd, &sin as *const _ as _, std::mem::size_of::<libc::sockaddr_in>() as u32);
            assert_eq!(ret, 0, "Bind failed");

            let ret = libc::listen(fd, 65535);
            assert_eq!(ret, 0, "Listen failed");
            fd
        };

        println!("Phase 2 io_uring server listening on 0.0.0.0:{}", port);

        unsafe {
            let accept_sqe = opcode::AcceptMulti::new(types::Fd(listener_fd))
                .build()
                .user_data(TOKEN_ACCEPT);
            let _ = sq.push(&accept_sqe);
        }

        let mut efd_buf = [0u8; 8];
        unsafe {
            let efd_sqe = opcode::Read::new(types::Fd(efd), efd_buf.as_mut_ptr(), 8)
                .build()
                .user_data(TOKEN_EVENTFD);
            let _ = sq.push(&efd_sqe);
        }

        submitter.submit()?;

        loop {
            submitter.submit_and_wait(1)?;

            let mut cqe_rearm_accept = false;
            let mut cqe_rearm_eventfd = false;

            while let Some(cqe) = cq.next() {
                let udata = cqe.user_data();
                let res = cqe.result();
                let flags = cqe.flags();

                if udata == TOKEN_ACCEPT {
                    if res >= 0 {
                        let client_fd = res;
                        if (client_fd as usize) < MAX_CONNS {
                            let slot = client_fd as usize;
                            conns[slot].fd = client_fd;
                            conns[slot].state = 1;

                            unsafe {
                                let one: libc::c_int = 1;
                                libc::setsockopt(
                                    client_fd,
                                    libc::IPPROTO_TCP,
                                    libc::TCP_NODELAY,
                                    &one as *const _ as _,
                                    4,
                                );

                                let recv_sqe = opcode::RecvMulti::new(
                                    types::Fd(client_fd),
                                    BUF_GROUP_ID,
                                )
                                .build()
                                .user_data(TOKEN_RECV_MASK | (slot as u64));

                                let _ = sq.push(&recv_sqe);
                            }
                        } else {
                            unsafe { libc::close(client_fd); }
                        }
                    }
                    if !cqueue::more(flags) {
                        cqe_rearm_accept = true;
                    }
                } else if udata == TOKEN_EVENTFD {
                    cqe_rearm_eventfd = true;
                } else if (udata & TOKEN_RECV_MASK) == TOKEN_RECV_MASK {
                    let slot = (udata & TOKEN_SLOT_MASK) as usize;
                    if res <= 0 {
                        if conns[slot].fd >= 0 {
                            unsafe { libc::close(conns[slot].fd); }
                            conns[slot].fd = -1;
                            conns[slot].state = 0;
                        }
                    } else {
                        if let Some(bid) = cqueue::buffer_select(flags) {
                            let slice = buf_ring.get_slice(bid, res as usize);

                            if let Some(id) = parse_get_record_id(slice) {
                                let _ = tx_db.send(DbRequest::Get {
                                    slot: slot as u32,
                                    fd: conns[slot].fd,
                                    id,
                                });
                            } else if let Some(payload_slice) = parse_post_payload(slice) {
                                let mut pbuf = [0u8; 256];
                                let len = payload_slice.len().min(256);
                                pbuf[..len].copy_from_slice(&payload_slice[..len]);
                                let _ = tx_db.send(DbRequest::Post {
                                    slot: slot as u32,
                                    fd: conns[slot].fd,
                                    payload_len: len,
                                    payload: pbuf,
                                });
                            } else {
                                let ptr = conns[slot].write_buf_ptr;
                                let resp = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
                                unsafe {
                                    std::ptr::copy_nonoverlapping(resp.as_ptr(), ptr, resp.len());
                                    let send_sqe = opcode::Send::new(
                                        types::Fd(conns[slot].fd),
                                        ptr,
                                        resp.len() as u32,
                                    )
                                    .build()
                                    .user_data(TOKEN_SEND_MASK | (slot as u64));
                                    let _ = sq.push(&send_sqe);
                                }
                            }

                            buf_ring.replenish(bid);
                        }

                        if !cqueue::more(flags) && conns[slot].fd >= 0 {
                            unsafe {
                                let recv_sqe = opcode::RecvMulti::new(
                                    types::Fd(conns[slot].fd),
                                    BUF_GROUP_ID,
                                )
                                .build()
                                .user_data(TOKEN_RECV_MASK | (slot as u64));
                                let _ = sq.push(&recv_sqe);
                            }
                        }
                    }
                } else if (udata & TOKEN_SEND_MASK) == TOKEN_SEND_MASK {
                    // Send completed
                }
            }

            while let Ok(resp) = rx_resp.try_recv() {
                match resp {
                    DbResponse::GetResult {
                        slot,
                        fd,
                        found: _,
                        id,
                        payload_len,
                        payload,
                        updated_at,
                    } => {
                        let s = slot as usize;
                        if conns[s].fd == fd && fd >= 0 {
                            let buf = conns[s].write_buf_ptr;
                            let body_start_idx = 128;
                            unsafe {
                                let body_slice = std::slice::from_raw_parts_mut(buf.add(body_start_idx), 384);
                                let mut pos = 0;
                                let prefix = b"{\"id\":";
                                body_slice[pos..pos + prefix.len()].copy_from_slice(prefix);
                                pos += prefix.len();

                                let mut itoa_b = itoa::Buffer::new();
                                let id_str = itoa_b.format(id);
                                body_slice[pos..pos + id_str.len()].copy_from_slice(id_str.as_bytes());
                                pos += id_str.len();

                                let p_mid = b",\"payload\":\"";
                                body_slice[pos..pos + p_mid.len()].copy_from_slice(p_mid);
                                pos += p_mid.len();

                                body_slice[pos..pos + payload_len].copy_from_slice(&payload[..payload_len]);
                                pos += payload_len;

                                let u_mid = b"\",\"updated_at\":";
                                body_slice[pos..pos + u_mid.len()].copy_from_slice(u_mid);
                                pos += u_mid.len();

                                let mut itoa_b2 = itoa::Buffer::new();
                                let u_str = itoa_b2.format(updated_at);
                                body_slice[pos..pos + u_str.len()].copy_from_slice(u_str.as_bytes());
                                pos += u_str.len();

                                body_slice[pos] = b'}';
                                pos += 1;
                                let body_len = pos;

                                let mut h_pos = 0;
                                let header_prefix = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: keep-alive\r\nContent-Length: ";
                                let h_slice = std::slice::from_raw_parts_mut(buf, 128);
                                h_slice[h_pos..h_pos + header_prefix.len()].copy_from_slice(header_prefix);
                                h_pos += header_prefix.len();

                                let mut itoa_b3 = itoa::Buffer::new();
                                let bl_str = itoa_b3.format(body_len);
                                h_slice[h_pos..h_pos + bl_str.len()].copy_from_slice(bl_str.as_bytes());
                                h_pos += bl_str.len();

                                h_slice[h_pos..h_pos + 4].copy_from_slice(b"\r\n\r\n");
                                h_pos += 4;

                                std::ptr::copy(buf.add(body_start_idx), buf.add(h_pos), body_len);
                                let total_len = h_pos + body_len;

                                let send_sqe = opcode::Send::new(
                                    types::Fd(fd),
                                    buf,
                                    total_len as u32,
                                )
                                .build()
                                .user_data(TOKEN_SEND_MASK | (slot as u64));
                                let _ = sq.push(&send_sqe);
                            }
                        }
                    }
                    DbResponse::PostResult { slot, fd, id } => {
                        let s = slot as usize;
                        if conns[s].fd == fd && fd >= 0 {
                            let buf = conns[s].write_buf_ptr;
                            unsafe {
                                let body_slice = std::slice::from_raw_parts_mut(buf.add(128), 128);
                                let mut pos = 0;
                                let prefix = b"{\"id\":";
                                body_slice[pos..pos + prefix.len()].copy_from_slice(prefix);
                                pos += prefix.len();

                                let mut itoa_b = itoa::Buffer::new();
                                let id_str = itoa_b.format(id);
                                body_slice[pos..pos + id_str.len()].copy_from_slice(id_str.as_bytes());
                                pos += id_str.len();

                                body_slice[pos] = b'}';
                                pos += 1;
                                let body_len = pos;

                                let mut h_pos = 0;
                                let header_prefix = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: keep-alive\r\nContent-Length: ";
                                let h_slice = std::slice::from_raw_parts_mut(buf, 128);
                                h_slice[h_pos..h_pos + header_prefix.len()].copy_from_slice(header_prefix);
                                h_pos += header_prefix.len();

                                let mut itoa_b2 = itoa::Buffer::new();
                                let bl_str = itoa_b2.format(body_len);
                                h_slice[h_pos..h_pos + bl_str.len()].copy_from_slice(bl_str.as_bytes());
                                h_pos += bl_str.len();

                                h_slice[h_pos..h_pos + 4].copy_from_slice(b"\r\n\r\n");
                                h_pos += 4;

                                std::ptr::copy(buf.add(128), buf.add(h_pos), body_len);
                                let total_len = h_pos + body_len;

                                let send_sqe = opcode::Send::new(
                                    types::Fd(fd),
                                    buf,
                                    total_len as u32,
                                )
                                .build()
                                .user_data(TOKEN_SEND_MASK | (slot as u64));
                                let _ = sq.push(&send_sqe);
                            }
                        }
                    }
                    DbResponse::NotFound { slot, fd } => {
                        let s = slot as usize;
                        if conns[s].fd == fd && fd >= 0 {
                            let ptr = conns[s].write_buf_ptr;
                            let resp = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
                            unsafe {
                                std::ptr::copy_nonoverlapping(resp.as_ptr(), ptr, resp.len());
                                let send_sqe = opcode::Send::new(
                                    types::Fd(fd),
                                    ptr,
                                    resp.len() as u32,
                                )
                                .build()
                                .user_data(TOKEN_SEND_MASK | (slot as u64));
                                let _ = sq.push(&send_sqe);
                            }
                        }
                    }
                    DbResponse::Error { slot, fd } => {
                        let s = slot as usize;
                        if conns[s].fd == fd && fd >= 0 {
                            let ptr = conns[s].write_buf_ptr;
                            let resp = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
                            unsafe {
                                std::ptr::copy_nonoverlapping(resp.as_ptr(), ptr, resp.len());
                                let send_sqe = opcode::Send::new(
                                    types::Fd(fd),
                                    ptr,
                                    resp.len() as u32,
                                )
                                .build()
                                .user_data(TOKEN_SEND_MASK | (slot as u64));
                                let _ = sq.push(&send_sqe);
                            }
                        }
                    }
                }
            }

            if cqe_rearm_accept {
                unsafe {
                    let accept_sqe = opcode::AcceptMulti::new(types::Fd(listener_fd))
                        .build()
                        .user_data(TOKEN_ACCEPT);
                    let _ = sq.push(&accept_sqe);
                }
            }

            if cqe_rearm_eventfd {
                unsafe {
                    let efd_sqe = opcode::Read::new(types::Fd(efd), efd_buf.as_mut_ptr(), 8)
                        .build()
                        .user_data(TOKEN_EVENTFD);
                    let _ = sq.push(&efd_sqe);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux_impl::run_server()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("Phase 2 requires Linux (io_uring). Build and run inside the Docker container.");
}
