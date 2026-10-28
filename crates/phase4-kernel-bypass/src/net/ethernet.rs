#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct EthernetHeader {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub ethertype: u16,
}

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct ArpPacket {
    pub htype: u16,     // Hardware type (1 = Ethernet)
    pub ptype: u16,     // Protocol type (0x0800 = IPv4)
    pub hlen: u8,       // Hardware address length (6)
    pub plen: u8,       // Protocol address length (4)
    pub oper: u16,      // Operation (1 = Request, 2 = Reply)
    pub sha: [u8; 6],   // Sender hardware address (MAC)
    pub spa: [u8; 4],   // Sender protocol address (IP)
    pub tha: [u8; 6],   // Target hardware address (MAC)
    pub tpa: [u8; 4],   // Target protocol address (IP)
}

pub const ARP_OPER_REQUEST: u16 = 1;
pub const ARP_OPER_REPLY: u16 = 2;

impl EthernetHeader {
    #[inline(always)]
    pub fn parse(buf: &[u8]) -> Option<(&Self, &[u8])> {
        let hdr_len = std::mem::size_of::<Self>();
        if buf.len() < hdr_len {
            return None;
        }
        let hdr = unsafe { &*(buf.as_ptr() as *const Self) };
        Some((hdr, &buf[hdr_len..]))
    }
}

impl ArpPacket {
    #[inline(always)]
    pub fn parse(buf: &[u8]) -> Option<&Self> {
        let len = std::mem::size_of::<Self>();
        if buf.len() < len {
            return None;
        }
        Some(unsafe { &*(buf.as_ptr() as *const Self) })
    }

    /// Construct an ARP reply frame given the incoming request, our MAC, and our IP
    pub fn build_reply(
        our_mac: [u8; 6],
        our_ip: [u8; 4],
        target_mac: [u8; 6],
        target_ip: [u8; 4],
        out_buf: &mut [u8],
    ) -> usize {
        let eth_len = std::mem::size_of::<EthernetHeader>();
        let arp_len = std::mem::size_of::<ArpPacket>();
        let total_len = eth_len + arp_len;

        if out_buf.len() < total_len {
            return 0;
        }

        // 1. Ethernet header
        let eth = EthernetHeader {
            dst_mac: target_mac,
            src_mac: our_mac,
            ethertype: ETHERTYPE_ARP.to_be(),
        };

        // 2. ARP packet
        let arp = ArpPacket {
            htype: 1u16.to_be(),
            ptype: ETHERTYPE_IPV4.to_be(),
            hlen: 6,
            plen: 4,
            oper: ARP_OPER_REPLY.to_be(),
            sha: our_mac,
            spa: our_ip,
            tha: target_mac,
            tpa: target_ip,
        };

        unsafe {
            std::ptr::copy_nonoverlapping(
                &eth as *const _ as *const u8,
                out_buf.as_mut_ptr(),
                eth_len,
            );
            std::ptr::copy_nonoverlapping(
                &arp as *const _ as *const u8,
                out_buf.as_mut_ptr().add(eth_len),
                arp_len,
            );
        }

        total_len
    }
}
