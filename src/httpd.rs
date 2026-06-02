//! `httpd` applet — a minimal static-file HTTP server replacing
//! `busybox httpd -f -p <port> -h <dir>`. Serves GET/HEAD for files under a
//! root directory, one thread per connection, foreground. Just enough for
//! workloads (and the CI smoke test) to serve files without busybox.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

pub fn main(args: Vec<String>) -> i32 {
    let mut port: u16 = 80;
    let mut root = PathBuf::from(".");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    port = p.parse().unwrap_or(80);
                }
            }
            "-h" => {
                i += 1;
                if let Some(h) = args.get(i) {
                    root = PathBuf::from(h);
                }
            }
            "-f" => {} // foreground is the only mode
            _ => {}
        }
        i += 1;
    }

    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("httpd: bind :{port}: {e}");
            return 1;
        }
    };
    eprintln!("httpd: serving {} on 0.0.0.0:{port}", root.display());
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let root = root.clone();
                std::thread::spawn(move || handle(stream, &root));
            }
            Err(e) => eprintln!("httpd: accept: {e}"),
        }
    }
    0
}

fn handle(mut stream: TcpStream, root: &Path) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    match parse_target(&req) {
        Some(Target::Get(path)) => serve(&mut stream, root, &path, true),
        Some(Target::Head(path)) => serve(&mut stream, root, &path, false),
        Some(Target::BadMethod) => {
            let _ = respond(&mut stream, 405, "text/plain", b"method not allowed", true);
        }
        None => {
            let _ = respond(&mut stream, 400, "text/plain", b"bad request", true);
        }
    }
}

enum Target {
    Get(String),
    Head(String),
    BadMethod,
}

fn parse_target(req: &str) -> Option<Target> {
    let line = req.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let raw = parts.next()?;
    let path = raw.split('?').next().unwrap_or("/").to_string();
    match method {
        "GET" => Some(Target::Get(path)),
        "HEAD" => Some(Target::Head(path)),
        _ => Some(Target::BadMethod),
    }
}

fn serve(stream: &mut TcpStream, root: &Path, urlpath: &str, with_body: bool) {
    let rel = urlpath.trim_start_matches('/');
    let mut full = safe_join(root, rel);
    if full.is_dir() {
        full = full.join("index.html");
    }
    match std::fs::read(&full) {
        Ok(body) => {
            let _ = respond(stream, 200, content_type(&full), &body, with_body);
        }
        Err(_) => {
            let _ = respond(stream, 404, "text/plain", b"not found", with_body);
        }
    }
}

/// Join `rel` under `root`, dropping any `..`/root components so a request
/// can't escape the served directory.
fn safe_join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for comp in Path::new(rel).components() {
        if let Component::Normal(c) = comp {
            p.push(c);
        }
    }
    p
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        _ => "application/octet-stream",
    }
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    ct: &str,
    body: &[u8],
    with_body: bool,
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.0 {code} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if with_body {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_traversal() {
        let root = Path::new("/srv");
        assert_eq!(safe_join(root, "a/b.txt"), Path::new("/srv/a/b.txt"));
        assert_eq!(
            safe_join(root, "../../etc/passwd"),
            Path::new("/srv/etc/passwd")
        );
        assert_eq!(safe_join(root, "/abs"), Path::new("/srv/abs"));
    }

    #[test]
    fn parse_target_methods() {
        assert!(matches!(
            parse_target("GET /index.html HTTP/1.1\r\n"),
            Some(Target::Get(p)) if p == "/index.html"
        ));
        assert!(matches!(
            parse_target("GET /x?y=1 HTTP/1.0\r\n"),
            Some(Target::Get(p)) if p == "/x"
        ));
        assert!(matches!(
            parse_target("POST / HTTP/1.1\r\n"),
            Some(Target::BadMethod)
        ));
    }
}
