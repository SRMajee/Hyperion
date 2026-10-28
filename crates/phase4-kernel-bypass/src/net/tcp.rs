#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset_res: u8, // data offset (4 bits) + reserved (4 bits)
    pub flags: u8,
    pub window_size: u16,
    pub checksum: u16,
    pub urgent_ptr: u16,
}

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

impl TcpHeader {
    #[inline(always)]
    pub fn parse(buf: &[u8]) -> Option<(&Self, &[u8])> {
        let min_len = std::mem::size_of::<Self>();
        if buf.len() < min_len {
            return None;
        }
        let hdr = unsafe { &*(buf.as_ptr() as *const Self) };
        let offset = ((hdr.data_offset_res >> 4) as usize) * 4;
        if offset < min_len || buf.len() < offset {
            return None;
        }
        Some((hdr, &buf[offset..]))
    }

    #[inline(always)]
    pub fn calculate_checksum(
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
        tcp_hdr_and_payload: &[u8],
    ) -> u16 {
        let mut sum: u32 = 0;

        // 1. Pseudo-header: Src IP & Dst IP
        sum = sum.wrapping_add(u16::from_be_bytes([src_ip[0], src_ip[1]]) as u32);
        sum = sum.wrapping_add(u16::from_be_bytes([src_ip[2], src_ip[3]]) as u32);
        sum = sum.wrapping_add(u16::from_be_bytes([dst_ip[0], dst_ip[1]]) as u32);
        sum = sum.wrapping_add(u16::from_be_bytes([dst_ip[2], dst_ip[3]]) as u32);

        // Protocol (6) and TCP Length
        sum = sum.wrapping_add(6u32);
        sum = sum.wrapping_add(tcp_hdr_and_payload.len() as u32);

        // 2. TCP Header + Payload
        let mut i = 0;
        while i < tcp_hdr_and_payload.len() - 1 {
            let word = u16::from_be_bytes([tcp_hdr_and_payload[i], tcp_hdr_and_payload[i + 1]]);
            sum = sum.wrapping_add(word as u32);
            i += 2;
        }
        if i < tcp_hdr_and_payload.len() {
            let word = u16::from_be_bytes([tcp_hdr_and_payload[i], 0]);
            sum = sum.wrapping_add(word as u32);
        }

        while (sum >> 16) > 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !(sum as u16)
    }

    #[inline(always)]
    pub fn build(
        src_port: u16,
        dst_port: u16,
        seq_num: u32,
        ack_num: u32,
        flags: u8,
        window_size: u16,
    ) -> Self {
        Self {
            src_port: src_port.to_be(),
            dst_port: dst_port.to_be(),
            seq_num: seq_num.to_be(),
            ack_num: ack_num.to_be(),
            data_offset_res: (5 << 4), // 20 bytes header
            flags,
            window_size: window_size.to_be(),
            checksum: 0,
            urgent_ptr: 0,
        }
    }
}
