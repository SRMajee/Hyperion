use std::sync::atomic::{AtomicUsize, Ordering};

pub const STATE_FREE: u8 = 0;
pub const STATE_SYN_RECEIVED: u8 = 1;
pub const STATE_ESTABLISHED: u8 = 2;
pub const STATE_CLOSED: u8 = 3;

#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct MicroTcpConn {
    pub client_ip: u32,
    pub client_port: u16,
    pub server_port: u16,
    pub state: u8,
    pub client_mac: [u8; 6],
    pub rcv_nxt: u32,
    pub snd_nxt: u32,
    pub last_active_sec: u32,
    pub _pad: [u8; 39],
}

impl Default for MicroTcpConn {
    fn default() -> Self {
        Self {
            client_ip: 0,
            client_port: 0,
            server_port: 0,
            state: STATE_FREE,
            client_mac: [0; 6],
            rcv_nxt: 0,
            snd_nxt: 1000,
            last_active_sec: 0,
            _pad: [0; 39],
        }
    }
}

pub struct ConnTable {
    slots: Box<[MicroTcpConn]>,
    _capacity: usize,
    mask: usize,
    active_count: AtomicUsize,
}

impl ConnTable {
    pub fn new(capacity_power_of_two: usize) -> Self {
        assert!(capacity_power_of_two.is_power_of_two());
        let mut v = Vec::with_capacity(capacity_power_of_two);
        v.resize_with(capacity_power_of_two, MicroTcpConn::default);
        let slots = v.into_boxed_slice();
        let mask = capacity_power_of_two - 1;

        Self {
            slots,
            _capacity: capacity_power_of_two,
            mask,
            active_count: AtomicUsize::new(0),
        }
    }

    #[inline(always)]
    fn hash_tuple(client_ip: u32, client_port: u16) -> usize {
        let mut h = (client_ip as u64) ^ ((client_port as u64) << 32);
        h = h.wrapping_mul(0x517cc1b727220a95);
        (h ^ (h >> 32)) as usize
    }

    #[inline(always)]
    pub fn find(&mut self, client_ip: u32, client_port: u16) -> Option<&mut MicroTcpConn> {
        let start = Self::hash_tuple(client_ip, client_port) & self.mask;
        let ptr = self.slots.as_mut_ptr();
        for i in 0..64 {
            let idx = (start + i) & self.mask;
            let slot = unsafe { &mut *ptr.add(idx) };
            if slot.state == STATE_FREE {
                return None;
            }
            if slot.client_ip == client_ip && slot.client_port == client_port {
                return Some(slot);
            }
        }
        None
    }

    #[inline(always)]
    pub fn insert_or_get(
        &mut self,
        client_ip: u32,
        client_port: u16,
        client_mac: [u8; 6],
        init_seq: u32,
    ) -> Option<&mut MicroTcpConn> {
        let start = Self::hash_tuple(client_ip, client_port) & self.mask;
        let mut first_free: Option<usize> = None;
        let ptr = self.slots.as_mut_ptr();

        for i in 0..64 {
            let idx = (start + i) & self.mask;
            let slot = unsafe { &*ptr.add(idx) };
            if slot.state == STATE_FREE || slot.state == STATE_CLOSED {
                if first_free.is_none() {
                    first_free = Some(idx);
                }
                if slot.state == STATE_FREE {
                    break;
                }
            } else if slot.client_ip == client_ip && slot.client_port == client_port {
                return Some(unsafe { &mut *ptr.add(idx) });
            }
        }

        if let Some(free_idx) = first_free {
            let slot = unsafe { &mut *ptr.add(free_idx) };
            let was_free = slot.state == STATE_FREE || slot.state == STATE_CLOSED;
            slot.client_ip = client_ip;
            slot.client_port = client_port;
            slot.server_port = 80;
            slot.client_mac = client_mac;
            slot.state = STATE_SYN_RECEIVED;
            slot.rcv_nxt = init_seq.wrapping_add(1);
            slot.snd_nxt = (rand_isn(client_ip, client_port)).wrapping_add(1);
            slot.last_active_sec = 0;
            if was_free {
                self.active_count.fetch_add(1, Ordering::Relaxed);
            }
            Some(slot)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn remove(&mut self, client_ip: u32, client_port: u16) {
        if let Some(conn) = self.find(client_ip, client_port) {
            conn.state = STATE_CLOSED;
            self.active_count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn active_conns(&self) -> usize {
        self.active_count.load(Ordering::Relaxed)
    }
}

#[inline(always)]
fn rand_isn(ip: u32, port: u16) -> u32 {
    let mut h = (ip as u64) ^ ((port as u64) << 16) ^ 0x9e3779b97f4a7c15;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    (h ^ (h >> 32)) as u32
}
