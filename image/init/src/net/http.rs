//! Minimal HTTP/1.0 GET for cloud metadata (IMDS) — replaces busybox `wget`
//! and keeps ureq's DNS/TLS/URL/icu stack out of the initramfs. IMDS is always
//! plain HTTP to the fixed link-local address 169.254.169.254, so we need
//! neither DNS nor TLS: connect, send a GET with one metadata header, read the
//! response, return the body on HTTP 200.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

const IMDS: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);

/// GET `http://169.254.169.254{path}` with a single header. `None` on any
/// connect/IO error, non-200 status, or timeout (mirrors `wget -T 2 || true`).
pub fn imds_get(path: &str, header: (&str, &str), timeout: Duration) -> Option<String> {
    let addr = SocketAddr::from((IMDS, 80));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {IMDS}\r\n{}: {}\r\nConnection: close\r\n\r\n",
        header.0, header.1
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    // status line: "HTTP/1.x 200 OK"
    let code = head.lines().next()?.split_whitespace().nth(1)?;
    (code == "200").then(|| body.to_string())
}
