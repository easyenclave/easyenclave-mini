//! Hand-rolled `NETLINK_ROUTE` — replaces busybox `ip link/addr/route`. Only
//! the four operations the vendor stages need: bring a link up, add an IPv4
//! address, add a default route, and add on-link / via routes (RFC 3442
//! classless static routes from DHCP). Built as raw RTM messages so we avoid
//! the async-only `rtnetlink`/tokio stack in this static binary.

use std::io;
use std::net::Ipv4Addr;

// Kernel ABI constants (stable) — defined locally rather than via libc, whose
// netlink coverage varies across versions/targets.
const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const RTM_NEWROUTE: u16 = 24;

const NLM_F_REQUEST: u16 = 0x001;
const NLM_F_ACK: u16 = 0x004;
const NLM_F_CREATE: u16 = 0x400;
const NLM_F_REPLACE: u16 = 0x100;

const NLMSG_ERROR: u16 = 2;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;

const AF_INET_U8: u8 = 2;
const AF_UNSPEC_U8: u8 = 0;

const IFF_UP: u32 = 1;

const RT_TABLE_MAIN: u8 = 254;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RTPROT_BOOT: u8 = 3;
const RTN_UNICAST: u8 = 1;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Builder for a single netlink message (nlmsghdr + family header + rtattrs).
struct NlMsg {
    buf: Vec<u8>,
}

impl NlMsg {
    fn new(msg_type: u16, flags: u16, seq: u32) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&0u32.to_ne_bytes()); // nlmsg_len (patched in finish)
        buf.extend_from_slice(&msg_type.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes()); // nlmsg_pid (0 = kernel)
        Self { buf }
    }

    /// Append a family header / payload, zero-padded to 4 bytes.
    fn push_aligned(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let pad = align4(bytes.len()) - bytes.len();
        self.buf.extend(std::iter::repeat_n(0u8, pad));
    }

    fn push_attr(&mut self, rta_type: u16, payload: &[u8]) {
        let rta_len = (4 + payload.len()) as u16;
        self.buf.extend_from_slice(&rta_len.to_ne_bytes());
        self.buf.extend_from_slice(&rta_type.to_ne_bytes());
        self.push_aligned(payload);
    }

    fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len() as u32;
        self.buf[0..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }
}

fn nl_socket() -> io::Result<i32> {
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut sa: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    sa.nl_family = libc::AF_NETLINK as u16;
    let ret = unsafe {
        libc::bind(
            fd,
            &sa as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        )
    };
    if ret < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(fd)
}

fn send_and_ack(msg: Vec<u8>) -> io::Result<()> {
    let fd = nl_socket()?;
    let res = (|| {
        let mut dst: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        dst.nl_family = libc::AF_NETLINK as u16;
        let sent = unsafe {
            libc::sendto(
                fd,
                msg.as_ptr() as *const libc::c_void,
                msg.len(),
                0,
                &dst as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        parse_ack(&buf[..n as usize])
    })();
    unsafe { libc::close(fd) };
    res
}

fn parse_ack(buf: &[u8]) -> io::Result<()> {
    if buf.len() < 20 {
        return Ok(()); // no/short ack — treat as success
    }
    let msg_type = u16::from_ne_bytes([buf[4], buf[5]]);
    if msg_type == NLMSG_ERROR {
        // nlmsgerr = { i32 error; nlmsghdr orig }, error at offset 16.
        let err = i32::from_ne_bytes([buf[16], buf[17], buf[18], buf[19]]);
        if err != 0 {
            return Err(io::Error::from_raw_os_error(-err));
        }
    }
    Ok(())
}

fn ifinfomsg(ifindex: u32, flags: u32, change: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(16);
    h.push(AF_UNSPEC_U8); // ifi_family
    h.push(0); // pad
    h.extend_from_slice(&0u16.to_ne_bytes()); // ifi_type
    h.extend_from_slice(&(ifindex as i32).to_ne_bytes()); // ifi_index
    h.extend_from_slice(&flags.to_ne_bytes()); // ifi_flags
    h.extend_from_slice(&change.to_ne_bytes()); // ifi_change
    h
}

fn ifaddrmsg(prefixlen: u8, ifindex: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(8);
    h.push(AF_INET_U8); // ifa_family
    h.push(prefixlen); // ifa_prefixlen
    h.push(0); // ifa_flags
    h.push(RT_SCOPE_UNIVERSE); // ifa_scope
    h.extend_from_slice(&ifindex.to_ne_bytes()); // ifa_index
    h
}

fn rtmsg(dst_len: u8, scope: u8) -> Vec<u8> {
    let mut h = Vec::with_capacity(12);
    h.push(AF_INET_U8); // rtm_family
    h.push(dst_len); // rtm_dst_len
    h.push(0); // rtm_src_len
    h.push(0); // rtm_tos
    h.push(RT_TABLE_MAIN); // rtm_table
    h.push(RTPROT_BOOT); // rtm_protocol
    h.push(scope); // rtm_scope
    h.push(RTN_UNICAST); // rtm_type
    h.extend_from_slice(&0u32.to_ne_bytes()); // rtm_flags
    h
}

pub fn link_up(ifindex: u32) -> io::Result<()> {
    let mut m = NlMsg::new(RTM_NEWLINK, NLM_F_REQUEST | NLM_F_ACK, 1);
    m.push_aligned(&ifinfomsg(ifindex, IFF_UP, IFF_UP));
    send_and_ack(m.finish())
}

pub fn addr_add(ifindex: u32, ip: Ipv4Addr, prefix_len: u8) -> io::Result<()> {
    let mut m = NlMsg::new(
        RTM_NEWADDR,
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE,
        2,
    );
    m.push_aligned(&ifaddrmsg(prefix_len, ifindex));
    m.push_attr(IFA_LOCAL, &ip.octets());
    m.push_attr(IFA_ADDRESS, &ip.octets());
    send_and_ack(m.finish())
}

pub fn route_add_default(ifindex: u32, gw: Ipv4Addr) -> io::Result<()> {
    let mut m = NlMsg::new(
        RTM_NEWROUTE,
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE,
        3,
    );
    m.push_aligned(&rtmsg(0, RT_SCOPE_UNIVERSE));
    m.push_attr(RTA_GATEWAY, &gw.octets());
    m.push_attr(RTA_OIF, &ifindex.to_ne_bytes());
    send_and_ack(m.finish())
}

/// On-link host/subnet route (no gateway). Used for the `gw/32` reachability
/// trick in `mod.rs` (GCE hands out an off-subnet gateway) and, in future, for
/// RFC-3442 classless routes with gateway 0.0.0.0.
pub fn route_add_onlink(ifindex: u32, dst: Ipv4Addr, dst_len: u8) -> io::Result<()> {
    let mut m = NlMsg::new(
        RTM_NEWROUTE,
        NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE,
        4,
    );
    m.push_aligned(&rtmsg(dst_len, RT_SCOPE_LINK));
    m.push_attr(RTA_DST, &dst.octets());
    m.push_attr(RTA_OIF, &ifindex.to_ne_bytes());
    send_and_ack(m.finish())
}
