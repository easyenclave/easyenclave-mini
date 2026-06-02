//! Minimal hand-rolled DHCPv4 client (DORA) — replaces busybox udhcpc with no
//! third-party crate (RFC 2131 is a fixed BOOTP header + TLV options). We drive
//! the state machine over a raw broadcast UDP socket bound to the (still
//! address-less) interface via `SO_BINDTODEVICE` + `SO_BROADCAST`, sending from
//! 0.0.0.0:68 to 255.255.255.255:67. We read options 1 (mask), 3 (router),
//! 6 (DNS) and `yiaddr` — enough for slirp (10.0.2.15) and, with the `gw/32`
//! on-link trick in `mod.rs`, GCE's off-subnet gateway. RFC-3442 classless
//! routes (121/249) are a documented follow-up.

use std::ffi::CString;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::unix::io::FromRawFd;
use std::time::{Duration, Instant};

// DHCP message types (option 53).
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;

const MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const OPTS_START: usize = 240; // 236-byte BOOTP header + 4-byte cookie

pub struct Lease {
    pub ip: Ipv4Addr,
    pub prefix_len: u8,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
}

struct Reply {
    yiaddr: Ipv4Addr,
    msg_type: u8,
    mask: Option<Ipv4Addr>,
    routers: Vec<Ipv4Addr>,
    dns: Vec<Ipv4Addr>,
    server_id: Option<Ipv4Addr>,
}

/// Acquire a lease on `iface`. `None` on timeout/failure (non-fatal — the
/// caller logs and continues, matching udhcpc's best-effort use).
pub fn acquire(iface: &str, _ifindex: u32, mac: [u8; 6], timeout: Duration) -> Option<Lease> {
    let sock = match open_socket(iface) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ee-init: dhcp: socket on {iface}: {e}");
            return None;
        }
    };
    // xid need only correlate our request/reply; derive it from the MAC.
    let xid = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]) | 0x0100_0000;
    let deadline = Instant::now() + timeout;

    // DISCOVER -> OFFER, retrying until the deadline.
    let offer = loop {
        if Instant::now() >= deadline {
            return None;
        }
        if let Err(e) = broadcast(&sock, &build_packet(mac, xid, DHCP_DISCOVER, &[])) {
            eprintln!("ee-init: dhcp: send discover: {e}");
        }
        if let Some(r) = recv_of_type(&sock, xid, DHCP_OFFER, &deadline) {
            break r;
        }
    };
    let server_id = offer.server_id.unwrap_or(Ipv4Addr::UNSPECIFIED);

    // REQUEST -> ACK.
    let req = build_packet(
        mac,
        xid,
        DHCP_REQUEST,
        &[(50, &offer.yiaddr.octets()), (54, &server_id.octets())],
    );
    if let Err(e) = broadcast(&sock, &req) {
        eprintln!("ee-init: dhcp: send request: {e}");
        return None;
    }
    let ack = recv_of_type(&sock, xid, DHCP_ACK, &deadline)?;
    Some(Lease {
        ip: ack.yiaddr,
        prefix_len: ack
            .mask
            .map(|m| u32::from(m).count_ones() as u8)
            .unwrap_or(24),
        routers: ack.routers,
        dns: ack.dns,
    })
}

/// Build a BOOTREQUEST with message-type `msg_type` plus any `extra` options,
/// always followed by a parameter-request-list and client-identifier.
fn build_packet(mac: [u8; 6], xid: u32, msg_type: u8, extra: &[(u8, &[u8])]) -> Vec<u8> {
    let mut p = vec![0u8; OPTS_START];
    p[0] = 1; // op: BOOTREQUEST
    p[1] = 1; // htype: Ethernet
    p[2] = 6; // hlen
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // flags: broadcast
    p[28..34].copy_from_slice(&mac); // chaddr (first 6 bytes)
    p[236..240].copy_from_slice(&MAGIC_COOKIE);

    p.extend_from_slice(&[53, 1, msg_type]); // message type
    for (code, data) in extra {
        p.push(*code);
        p.push(data.len() as u8);
        p.extend_from_slice(data);
    }
    p.extend_from_slice(&[55, 4, 1, 3, 6, 15]); // param request: mask,router,dns,domain
    p.push(61); // client identifier
    p.push(7);
    p.push(1); // htype Ethernet
    p.extend_from_slice(&mac);
    p.push(255); // end
    p
}

fn parse_reply(buf: &[u8], xid: u32) -> Option<Reply> {
    if buf.len() < OPTS_START
        || buf[0] != 2 // BOOTREPLY
        || u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != xid
        || buf[236..240] != MAGIC_COOKIE
    {
        return None;
    }
    let mut r = Reply {
        yiaddr: Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]),
        msg_type: 0,
        mask: None,
        routers: Vec::new(),
        dns: Vec::new(),
        server_id: None,
    };
    let mut i = OPTS_START;
    while i < buf.len() {
        let code = buf[i];
        if code == 0 {
            i += 1;
            continue; // pad
        }
        if code == 255 || i + 1 >= buf.len() {
            break; // end / truncated
        }
        let len = buf[i + 1] as usize;
        let start = i + 2;
        let end = start + len;
        if end > buf.len() {
            break;
        }
        let data = &buf[start..end];
        match code {
            53 if len >= 1 => r.msg_type = data[0],
            1 if len >= 4 => r.mask = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3])),
            3 => r.routers.extend(parse_ips(data)),
            6 => r.dns.extend(parse_ips(data)),
            54 if len >= 4 => r.server_id = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3])),
            _ => {}
        }
        i = end;
    }
    Some(r)
}

fn parse_ips(data: &[u8]) -> Vec<Ipv4Addr> {
    data.chunks_exact(4)
        .map(|c| Ipv4Addr::new(c[0], c[1], c[2], c[3]))
        .collect()
}

fn broadcast(sock: &UdpSocket, data: &[u8]) -> io::Result<()> {
    sock.send_to(data, (Ipv4Addr::BROADCAST, 67)).map(|_| ())
}

fn recv_of_type(sock: &UdpSocket, xid: u32, want: u8, deadline: &Instant) -> Option<Reply> {
    let mut buf = [0u8; 1500];
    loop {
        if Instant::now() >= *deadline {
            return None;
        }
        let n = match sock.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(_) => return None, // read timeout
        };
        if let Some(r) = parse_reply(&buf[..n], xid) {
            if r.msg_type == want {
                return Some(r);
            }
        }
    }
}

fn open_socket(iface: &str) -> io::Result<UdpSocket> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let one: libc::c_int = 1;
    let setopt = |opt: libc::c_int, val: *const libc::c_void, len: libc::socklen_t| unsafe {
        libc::setsockopt(fd, libc::SOL_SOCKET, opt, val, len)
    };
    setopt(
        libc::SO_BROADCAST,
        &one as *const _ as *const libc::c_void,
        4,
    );
    setopt(
        libc::SO_REUSEADDR,
        &one as *const _ as *const libc::c_void,
        4,
    );
    if let Ok(cif) = CString::new(iface) {
        setopt(
            libc::SO_BINDTODEVICE,
            cif.as_ptr() as *const libc::c_void,
            iface.len() as libc::socklen_t,
        );
    }
    // bind 0.0.0.0:68
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = 68u16.to_be();
    addr.sin_addr.s_addr = 0; // INADDR_ANY
    let r = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if r < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    let sock = unsafe { UdpSocket::from_raw_fd(fd) };
    sock.set_read_timeout(Some(Duration::from_secs(3)))?;
    Ok(sock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_packet_shape() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let xid: u32 = 0x01abcdef;
        let p = build_packet(mac, xid, DHCP_DISCOVER, &[]);
        assert_eq!(p[0], 1); // BOOTREQUEST
        assert_eq!(&p[4..8], &xid.to_be_bytes());
        assert_eq!(&p[236..240], &MAGIC_COOKIE);
        assert_eq!(&p[28..34], &mac);
        assert_eq!(&p[240..243], &[53, 1, DHCP_DISCOVER]);
        assert_eq!(*p.last().unwrap(), 255);
    }

    #[test]
    fn parse_round_trips_offer() {
        // Build a minimal BOOTREPLY with yiaddr + mask + router + dns + server-id.
        let xid: u32 = 0x01abcdef;
        let mut buf = vec![0u8; OPTS_START];
        buf[0] = 2; // BOOTREPLY
        buf[4..8].copy_from_slice(&xid.to_be_bytes());
        buf[16..20].copy_from_slice(&[10, 0, 2, 15]); // yiaddr
        buf[236..240].copy_from_slice(&MAGIC_COOKIE);
        buf.extend_from_slice(&[53, 1, DHCP_OFFER]);
        buf.extend_from_slice(&[1, 4, 255, 255, 255, 0]); // mask /24
        buf.extend_from_slice(&[3, 4, 10, 0, 2, 2]); // router
        buf.extend_from_slice(&[6, 4, 10, 0, 2, 3]); // dns
        buf.extend_from_slice(&[54, 4, 10, 0, 2, 2]); // server id
        buf.push(255);

        let r = parse_reply(&buf, xid).expect("parse");
        assert_eq!(r.msg_type, DHCP_OFFER);
        assert_eq!(r.yiaddr, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(r.mask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(r.routers, vec![Ipv4Addr::new(10, 0, 2, 2)]);
        assert_eq!(r.dns, vec![Ipv4Addr::new(10, 0, 2, 3)]);
        assert_eq!(r.server_id, Some(Ipv4Addr::new(10, 0, 2, 2)));
        // wrong xid rejected
        assert!(parse_reply(&buf, xid ^ 1).is_none());
    }
}
