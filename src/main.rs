mod attestation;
mod capture;
mod config;
mod init;
mod process;
mod release;
mod socket;
mod workload;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    if maybe_run_subcommand() {
        return;
    }

    // 1. PID 1 init (mount filesystems, parse kernel cmdline, reap zombies)
    init::maybe_init();

    // 2. Load config
    let cfg = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("easyenclave: configuration error: {e}");
            std::process::exit(1);
        }
    };

    // Ensure data directories exist
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    let _ = std::fs::create_dir_all(format!("{}/workloads/logs", cfg.data_dir));
    let bin_dir = format!("{}/bin", cfg.data_dir);
    let _ = std::fs::create_dir_all(&bin_dir);

    // 3. Detect attestation backend
    let attestation = attestation::detect().unwrap_or_else(|e| {
        eprintln!("easyenclave: FATAL: {e}");
        std::process::exit(1);
    });
    eprintln!(
        "easyenclave: attestation backend: {}",
        attestation.attestation_type()
    );

    // 4. Pre-fetch all github_release assets before any workload starts.
    // Boot workloads spawn asynchronously, so without this phase a
    // workload could shell out to a tool (e.g. cloudflared) before its
    // download completes. Fail fast if any asset can't be fetched —
    // the VM is useless without its binaries.
    for bw in &cfg.boot_workloads {
        if let Some(gh) = bw.github_release.clone() {
            eprintln!("easyenclave: pre-fetching {} for {}", gh.asset, bw.app_name);
            let bin = bin_dir.clone();
            let res = tokio::task::spawn_blocking(move || release::download(&gh, &bin))
                .await
                .map_err(|e| format!("join: {e}"));
            match res.and_then(|r| r) {
                Ok(path) => eprintln!("easyenclave: fetched {}", path.display()),
                Err(e) => {
                    eprintln!(
                        "easyenclave: FATAL: failed to fetch asset for {}: {e}",
                        bw.app_name
                    );
                    std::process::exit(1);
                }
            }
        }
    }

    // Put the bin dir on PATH so workloads can shell out by name.
    let existing_path = std::env::var("PATH").unwrap_or_default();
    if existing_path.is_empty() {
        std::env::set_var("PATH", &bin_dir);
    } else {
        std::env::set_var("PATH", format!("{bin_dir}:{existing_path}"));
    }

    // 5. Create empty deployments
    let deployments: workload::Deployments = Arc::new(Mutex::new(HashMap::new()));

    // Mint a single per-boot token. Workloads with `inherit_token: true`
    // get it in their env (`EE_TOKEN=<hex>`); the socket server requires
    // it on every request. Local-root-inside-a-compromised-workload
    // stops being enough to drive EE — only the one workload that was
    // explicitly granted the env var can talk to the socket.
    let boot_token = mint_boot_token();

    // 6. Deploy boot workloads from config.
    for bw in &cfg.boot_workloads {
        eprintln!("easyenclave: boot workload: {}", bw.app_name);
        let mut env = bw.env.clone();
        if bw.inherit_token {
            env.get_or_insert_with(Vec::new)
                .push(format!("EE_TOKEN={boot_token}"));
            eprintln!("easyenclave: {} inherits EE_TOKEN", bw.app_name);
        }
        let req = workload::DeployRequest {
            cmd: bw.cmd.clone().unwrap_or_default(),
            env,
            app_name: Some(bw.app_name.clone()),
            tty: bw.tty,
            github_release: bw.github_release.clone(),
        };
        let (id, _status) = workload::execute_deploy(&deployments, req).await;
        eprintln!("easyenclave: boot workload {} -> {id}", bw.app_name);
    }

    let start_time = std::time::Instant::now();

    // 7. Start socket server (with signal handlers for clean shutdown)
    let deployments_shutdown = deployments.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("easyenclave: shutting down (SIGINT)...");
        workload::stop_all(&deployments_shutdown).await;
        std::process::exit(0);
    });

    let deployments_sigterm = deployments.clone();
    tokio::spawn(async move {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        sigterm.recv().await;
        eprintln!("easyenclave: shutting down (SIGTERM)...");
        workload::stop_all(&deployments_sigterm).await;
        std::process::exit(0);
    });

    let server = socket::SocketServer {
        socket_path: cfg.socket_path.clone(),
        deployments,
        attestation: Arc::new(attestation),
        start_time,
        expected_token: Some(boot_token),
    };

    if let Err(e) = server.run().await {
        eprintln!("easyenclave: socket server error: {e}");
        std::process::exit(1);
    }
}

/// Mint a 32-byte random token at boot, hex-encoded. Uses
/// `getrandom(2)` via the kernel; no extra crate dep. `/dev/urandom`
/// is also an option but `getrandom` avoids the need to open a file
/// and is always available on Linux 3.17+.
fn mint_boot_token() -> String {
    let mut buf = [0u8; 32];
    unsafe {
        let mut got = 0usize;
        while got < buf.len() {
            let n = libc::getrandom(buf.as_mut_ptr().add(got) as *mut _, buf.len() - got, 0);
            if n < 0 {
                let err = std::io::Error::last_os_error();
                panic!("easyenclave: FATAL: getrandom failed: {err}");
            }
            got += n as usize;
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn maybe_run_subcommand() -> bool {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("smoke-http") => {
            let opts = parse_smoke_http_args(args.collect()).unwrap_or_else(|e| {
                eprintln!("easyenclave smoke-http: {e}");
                std::process::exit(2);
            });
            run_smoke_http(opts).unwrap_or_else(|e| {
                eprintln!("easyenclave smoke-http: {e}");
                std::process::exit(1);
            });
            true
        }
        Some("--help") | Some("-h") => {
            print_usage();
            true
        }
        _ => false,
    }
}

struct SmokeHttpOptions {
    port: u16,
    body: String,
}

fn parse_smoke_http_args(args: Vec<String>) -> Result<SmokeHttpOptions, String> {
    let mut port = 80u16;
    let mut body = "ok\n".to_string();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                let value = args.get(i).ok_or("--port requires a value")?;
                port = value
                    .parse::<u16>()
                    .map_err(|e| format!("invalid --port value {value:?}: {e}"))?;
            }
            "--body" => {
                i += 1;
                body = args.get(i).ok_or("--body requires a value")?.clone();
            }
            "--help" | "-h" => {
                print_smoke_http_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown smoke-http argument {other:?}")),
        }
        i += 1;
    }
    Ok(SmokeHttpOptions { port, body })
}

fn run_smoke_http(opts: SmokeHttpOptions) -> Result<(), String> {
    let listener = TcpListener::bind(("0.0.0.0", opts.port))
        .map_err(|e| format!("bind 0.0.0.0:{}: {e}", opts.port))?;
    eprintln!("easyenclave smoke-http: listening on 0.0.0.0:{}", opts.port);

    for conn in listener.incoming() {
        let mut stream = match conn {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("easyenclave smoke-http: accept failed: {e}");
                continue;
            }
        };

        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            opts.body.len(),
            opts.body
        );
        if let Err(e) = stream.write_all(response.as_bytes()) {
            eprintln!("easyenclave smoke-http: write failed: {e}");
        }
    }

    Ok(())
}

fn print_usage() {
    eprintln!("usage: easyenclave [smoke-http [--port PORT] [--body BODY]]");
}

fn print_smoke_http_usage() {
    eprintln!("usage: easyenclave smoke-http [--port PORT] [--body BODY]");
}
