#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;

pub struct PacketSocket {
    #[cfg(target_os = "linux")]
    fd: RawFd,
    #[cfg(target_os = "linux")]
    if_index: i32,
    pub our_mac: [u8; 6],
    pub our_ip: [u8; 4],
}

#[cfg(target_os = "linux")]
impl PacketSocket {
    pub fn new(iface_name: &str, our_ip_str: &str) -> std::io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW,
                (libc::ETH_P_ALL as u16).to_be() as i32,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let iface_c = std::ffi::CString::new(iface_name)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let if_index = unsafe { libc::if_nametoindex(iface_c.as_ptr()) as i32 };
        if if_index == 0 {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error());
        }

        let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
        let iface_bytes = iface_name.as_bytes();
        let copy_len = iface_bytes.len().min(libc::IFNAMSIZ - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(
                iface_bytes.as_ptr(),
                ifr.ifr_name.as_mut_ptr() as *mut u8,
                copy_len,
            );
        }

        // Get MAC address
        if unsafe { libc::ioctl(fd, libc::SIOCGIFHWADDR, &mut ifr) } < 0 {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error());
        }
        let mut our_mac = [0u8; 6];
        unsafe {
            let sa_data = ifr.ifr_ifru.ifru_hwaddr.sa_data;
            for i in 0..6 {
                our_mac[i] = sa_data[i] as u8;
            }
        }

        // Bind socket to interface
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
        sll.sll_ifindex = if_index;

        let bind_res = unsafe {
            libc::bind(
                fd,
                &sll as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as u32,
            )
        };
        if bind_res < 0 {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error());
        }

        // Set non-blocking
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

        // Boost socket buffers
        let buf_size: libc::c_int = 16 * 1024 * 1024; // 16MB ring buffer
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUFFORCE,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUFFORCE,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }

        let mut our_ip = [0u8; 4];
        let parts: Vec<&str> = our_ip_str.split('.').collect();
        if parts.len() == 4 {
            for i in 0..4 {
                our_ip[i] = parts[i].parse::<u8>().unwrap_or(0);
            }
        }

        Ok(Self {
            fd,
            if_index,
            our_mac,
            our_ip,
        })
    }

    #[inline(always)]
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, std::io::ErrorKind> {
        let n = unsafe {
            libc::recv(
                self.fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if n > 0 {
            Ok(n as usize)
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EAGAIN) || err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                Err(std::io::ErrorKind::WouldBlock)
            } else {
                Err(std::io::ErrorKind::Other)
            }
        }
    }

    #[inline(always)]
    pub fn send(&self, frame: &[u8]) -> Result<usize, std::io::ErrorKind> {
        let mut sll: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sll.sll_family = libc::AF_PACKET as u16;
        sll.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
        sll.sll_ifindex = self.if_index;
        sll.sll_halen = 6;

        let n = unsafe {
            libc::sendto(
                self.fd,
                frame.as_ptr() as *const libc::c_void,
                frame.len(),
                0,
                &sll as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as u32,
            )
        };
        if n >= 0 {
            Ok(n as usize)
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EAGAIN) || err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                Err(std::io::ErrorKind::WouldBlock)
            } else {
                Err(std::io::ErrorKind::Other)
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PacketSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(not(target_os = "linux"))]
impl PacketSocket {
    pub fn new(_iface_name: &str, _our_ip_str: &str) -> std::io::Result<Self> {
        Ok(Self {
            our_mac: [0x02, 0x42, 0xac, 0x1c, 0x00, 0x02],
            our_ip: [172, 28, 0, 2],
        })
    }

    pub fn recv(&self, _buf: &mut [u8]) -> Result<usize, std::io::ErrorKind> {
        Err(std::io::ErrorKind::WouldBlock)
    }

    pub fn send(&self, _frame: &[u8]) -> Result<usize, std::io::ErrorKind> {
        Ok(0)
    }
}
