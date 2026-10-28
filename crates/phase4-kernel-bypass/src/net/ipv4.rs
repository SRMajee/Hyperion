#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Ipv4Header {
    pub ver_ihl: u8,
    pub tos: u8,
    pub total_len: u16,
    pub id: u16,
    pub frag_off: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src_ip: [u8; 4],
    pub dst_ip: [u8; 4],
}

pub const IPPROTO_TCP: u8 = 6;

impl Ipv4Header {
    #[inline(always)]
    pub fn parse(buf: &[u8]) -> Option<(&Self, &[u8])> {
        let min_len = std::mem::size_of::<Self>();
        if buf.len() < min_len {
            return None;
        }
        let hdr = unsafe { &*(buf.as_ptr() as *const Self) };
        let ihl = (hdr.ver_ihl & 0x0F) as usize * 4;
        if ihl < min_len || buf.len() < ihl {
            return None;
        }
        Some((hdr, &buf[ihl..]))
    }

    #[inline(always)]
    pub fn calculate_checksum(hdr_bytes: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        let mut i = 0;
        while i < hdr_bytes.len() - 1 {
            let word = u16::from_be_bytes([hdr_bytes[i], hdr_bytes[i + 1]]);
            sum = sum.wrapping_add(word as u32);
            i += 2;
        }
        if i < hdr_bytes.len() {
            let word = u16::from_be_bytes([hdr_bytes[i], 0]);
            sum = sum.wrapping_add(word as u32);
        }
        while (sum >> 16) > 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !(sum as u16)
    }

    #[inline(always)]
    pub fn build(
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
        protocol: u8,
        payload_len: usize,
        id: u16,
    ) -> Self {
        let total_len = (std::mem::size_of::<Self>() + payload_len) as u16;
        let mut hdr = Self {
            ver_ihl: 0x45, // IPv4, IHL = 5 (20 bytes)
            tos: 0,
            total_len: total_len.to_be(),
            id: id.to_be(),
            frag_off: 0x4000u16.to_be(), // Don't Fragment flag
            ttl: 64,
            protocol,
            checksum: 0,
            src_ip,
            dst_ip,
        };

        let bytes = unsafe {
            std::slice::from_raw_parts(
                &hdr as *const _ as *const u8,
                std::mem::size_of::<Self>(),
            )
        };
        hdr.checksum = Self::calculate_checksum(bytes).to_be();
        hdr
    }
}
