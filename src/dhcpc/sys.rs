//! The client's sockets and the interface's ioctls, on Linux 2.6.31.
//!
//! Sending is a UDP socket bound to 0.0.0.0:68 on the interface
//! (SO_BINDTODEVICE) with SO_BROADCAST: before there is an address, the
//! kernel sends from 0.0.0.0. Receiving before the address is set is a
//! packet socket, as udhcpc does it: the kernel's own UDP delivery of a
//! broadcast to an interface without an address depends on rp_filter (on
//! 2.6.31, `fib_validate_source` refuses it when rp_filter is on and there
//! is no route back), and a server that ignores the BROADCAST flag sends to
//! an address the kernel doesn't have yet. Once the address is on, replies
//! come through the UDP socket and the packet socket is closed.
use super::apply::Interface;
use super::wire;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::ffi::CString;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

const ETH_P_IP: u16 = 0x0800;

pub struct Sockets {
    interface: CString,
    udp: Socket,
    raw: Option<Socket>,
}

/// What [`Sockets::wait`] found.
#[derive(Debug, Default)]
pub struct Ready {
    pub udp: bool,
    pub raw: bool,
}

impl Sockets {
    pub fn open(interface: &str) -> io::Result<Self> {
        let udp = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        udp.set_reuse_address(true)?;
        udp.set_broadcast(true)?;
        udp.bind_device(Some(interface.as_bytes()))?;
        udp.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, wire::CLIENT_PORT).into())?;
        udp.set_nonblocking(true)?;
        Ok(Self {
            interface: CString::new(interface)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            udp,
            raw: None,
        })
    }

    /// The packet socket open (true) or closed.
    pub fn listen_raw(&mut self, on: bool) -> io::Result<()> {
        match (on, self.raw.is_some()) {
            (true, false) => self.raw = Some(self.open_raw()?),
            (false, true) => self.raw = None,
            _ => {}
        }
        Ok(())
    }

    fn open_raw(&self) -> io::Result<Socket> {
        let protocol = Protocol::from(i32::from(ETH_P_IP.to_be()));
        let raw = Socket::new(Domain::PACKET, Type::DGRAM, Some(protocol))?;
        let filter: Vec<socket2::SockFilter> = wire::FILTER
            .iter()
            .map(|&(code, jt, jf, k)| socket2::SockFilter::new(code, jt, jf, k))
            .collect();
        raw.attach_filter(&filter)?;
        // Whether each packet's UDP checksum is filled in (as udhcpc asks).
        let on: libc::c_int = 1;
        // SAFETY: a plain int option.
        let r = unsafe {
            libc::setsockopt(
                raw.as_raw_fd(),
                libc::SOL_PACKET,
                libc::PACKET_AUXDATA,
                &on as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        let ifindex = index(&self.interface)?;
        let mut storage = socket2::SockAddrStorage::zeroed();
        // SAFETY: sockaddr_ll is one of the platform's sockaddr types, and
        // the length given is its size.
        let address = unsafe {
            let ll = storage.view_as::<libc::sockaddr_ll>();
            ll.sll_family = libc::AF_PACKET as u16;
            ll.sll_protocol = ETH_P_IP.to_be();
            ll.sll_ifindex = ifindex;
            SockAddr::new(
                storage,
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        raw.bind(&address)?;
        raw.set_nonblocking(true)?;
        Ok(raw)
    }

    pub fn send(&self, to: Ipv4Addr, packet: &[u8]) -> io::Result<()> {
        self.udp
            .send_to(packet, &SocketAddrV4::new(to, wire::SERVER_PORT).into())
            .map(drop)
    }

    /// Waits up to `timeout` for a datagram, or for `wake` to be readable.
    pub fn wait(&self, wake: RawFd, timeout: Duration) -> io::Result<Ready> {
        let mut fds = vec![
            pollfd(wake),
            pollfd(self.udp.as_raw_fd()),
            pollfd(self.raw.as_ref().map_or(-1, |raw| raw.as_raw_fd())),
        ];
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `fds` is a valid array of pollfd for its length.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(Ready::default());
            }
            return Err(error);
        }
        let ready = |fd: &libc::pollfd| fd.revents & (libc::POLLIN | libc::POLLERR) != 0;
        // The wake-up itself (fds[0]) needs nothing but the next turn.
        Ok(Ready {
            udp: ready(&fds[1]),
            raw: ready(&fds[2]),
        })
    }

    /// The next DHCP message on the UDP socket, from port 67.
    pub fn recv_udp(&self, buf: &mut [u8]) -> Option<usize> {
        let (n, from) = self.udp.recv_from(as_uninit(buf)).ok()?;
        let port = from.as_socket_ipv4().map(|a| a.port());
        (port == Some(wire::SERVER_PORT)).then_some(n)
    }

    /// The next datagram on the packet socket, whole, and whether its UDP
    /// checksum is filled in.
    pub fn recv_raw(&self, buf: &mut [u8]) -> Option<(usize, bool)> {
        let raw = self.raw.as_ref()?;
        // Room for one cmsg of tpacket_auxdata, aligned.
        let mut control = [0u64; 8];
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };
        // SAFETY: a zeroed msghdr is valid; it points at `iov` and
        // `control`, which outlive the call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&control) as _;
        // SAFETY: `msg` is set up as above.
        let n = unsafe { libc::recvmsg(raw.as_raw_fd(), &mut msg, libc::MSG_DONTWAIT) };
        if n < 0 {
            return None;
        }
        let mut summed = true;
        // SAFETY: the CMSG macros walk the control data the kernel wrote.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_PACKET
                    && (*cmsg).cmsg_type == libc::PACKET_AUXDATA
                {
                    let aux = std::ptr::read_unaligned(
                        libc::CMSG_DATA(cmsg) as *const libc::tpacket_auxdata
                    );
                    summed = aux.tp_status & libc::TP_STATUS_CSUMNOTREADY == 0;
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        Some((n as usize, summed))
    }
}

fn as_uninit(buf: &mut [u8]) -> &mut [std::mem::MaybeUninit<u8>] {
    // SAFETY: initialised bytes are valid MaybeUninit<u8>; the socket only
    // writes into them.
    unsafe { &mut *(buf as *mut [u8] as *mut [std::mem::MaybeUninit<u8>]) }
}

fn pollfd(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

fn index(interface: &CString) -> io::Result<i32> {
    // SAFETY: a NUL-terminated name.
    match unsafe { libc::if_nametoindex(interface.as_ptr()) } {
        0 => Err(io::Error::last_os_error()),
        n => Ok(n as i32),
    }
}

/// The interface's address, routes and hardware address, by ioctl on a
/// socket of its own.
pub struct Ioctls {
    name: CString,
    fd: Socket,
}

impl Ioctls {
    pub fn open(interface: &str) -> io::Result<Self> {
        Ok(Self {
            name: CString::new(interface)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            fd: Socket::new(Domain::IPV4, Type::DGRAM, None)?,
        })
    }

    fn ifreq(&self) -> libc::ifreq {
        // SAFETY: a zeroed ifreq is valid.
        let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
        for (to, from) in ifr
            .ifr_name
            .iter_mut()
            .zip(self.name.as_bytes().iter().take(libc::IFNAMSIZ - 1))
        {
            *to = *from as libc::c_char;
        }
        ifr
    }

    fn ioctl<T>(&self, request: libc::c_ulong, arg: &mut T) -> io::Result<()> {
        // SAFETY: `arg` is the struct the request reads or fills.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, arg as *mut T) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn set(&self, request: libc::c_ulong, address: Ipv4Addr) -> io::Result<()> {
        let mut ifr = self.ifreq();
        ifr.ifr_ifru.ifru_addr = sockaddr(address);
        self.ioctl(request, &mut ifr)
    }

    /// The hardware address.
    pub fn mac(&self) -> io::Result<[u8; 6]> {
        let mut ifr = self.ifreq();
        self.ioctl(libc::SIOCGIFHWADDR, &mut ifr)?;
        // SAFETY: SIOCGIFHWADDR filled the hardware address.
        let data = unsafe { ifr.ifr_ifru.ifru_hwaddr.sa_data };
        let mut mac = [0; 6];
        for (to, from) in mac.iter_mut().zip(data.iter()) {
            // c_char: u8 on ARM, i8 elsewhere.
            #[allow(clippy::unnecessary_cast)]
            {
                *to = *from as u8;
            }
        }
        Ok(mac)
    }

    /// The interface's IPv4 address, if it has one.
    pub fn address(&self) -> Option<Ipv4Addr> {
        let mut ifr = self.ifreq();
        self.ioctl(libc::SIOCGIFADDR, &mut ifr).ok()?;
        // SAFETY: SIOCGIFADDR filled an AF_INET address.
        let address = unsafe {
            let sin = &ifr.ifr_ifru.ifru_addr as *const libc::sockaddr as *const libc::sockaddr_in;
            (*sin).sin_addr.s_addr
        };
        Some(Ipv4Addr::from(u32::from_be(address))).filter(|a| !a.is_unspecified())
    }

    fn route(&self, request: libc::c_ulong, gateway: Option<Ipv4Addr>) -> io::Result<()> {
        // SAFETY: a zeroed rtentry is valid; rt_dev points at our name,
        // which outlives the call.
        let mut route: libc::rtentry = unsafe { std::mem::zeroed() };
        route.rt_dst = sockaddr(Ipv4Addr::UNSPECIFIED);
        route.rt_genmask = sockaddr(Ipv4Addr::UNSPECIFIED);
        route.rt_flags = libc::RTF_UP;
        if let Some(gateway) = gateway {
            route.rt_gateway = sockaddr(gateway);
            route.rt_flags |= libc::RTF_GATEWAY;
        }
        route.rt_dev = self.name.as_ptr() as *mut libc::c_char;
        self.ioctl(request, &mut route)
    }
}

impl Interface for Ioctls {
    fn set_address(&mut self, ip: Ipv4Addr) -> io::Result<()> {
        self.set(libc::SIOCSIFADDR, ip)
    }

    fn set_netmask(&mut self, mask: Ipv4Addr) -> io::Result<()> {
        self.set(libc::SIOCSIFNETMASK, mask)
    }

    fn set_broadcast(&mut self, broadcast: Ipv4Addr) -> io::Result<()> {
        self.set(libc::SIOCSIFBRDADDR, broadcast)
    }

    fn delete_default_route(&mut self) -> io::Result<()> {
        self.route(libc::SIOCDELRT, None)
    }

    fn add_default_route(&mut self, gateway: Ipv4Addr) -> io::Result<()> {
        self.route(libc::SIOCADDRT, Some(gateway))
    }
}

fn sockaddr(address: Ipv4Addr) -> libc::sockaddr {
    // SAFETY: sockaddr_in fits in sockaddr; zeroed is valid for both.
    unsafe {
        let mut sin: libc::sockaddr_in = std::mem::zeroed();
        sin.sin_family = libc::AF_INET as libc::sa_family_t;
        sin.sin_addr.s_addr = u32::from(address).to_be();
        std::mem::transmute::<libc::sockaddr_in, libc::sockaddr>(sin)
    }
}
