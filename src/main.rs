mod attestation;
mod capture;
mod config;
mod init;
mod process;
mod release;
mod socket;
mod workload;

use std::collections::HashMap;
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
        Some("initrd") => {
            run_initrd_probe(args.collect()).unwrap_or_else(|e| {
                eprintln!("easyenclave initrd: {e}");
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

fn run_initrd_probe(args: Vec<String>) -> Result<(), String> {
    for arg in &args {
        match arg.as_str() {
            "--probe-only" => {}
            "--help" | "-h" => {
                print_initrd_usage();
                return Ok(());
            }
            other => return Err(format!("unknown initrd argument {other:?}")),
        }
    }

    let cmdline =
        std::fs::read_to_string("/proc/cmdline").map_err(|e| format!("read /proc/cmdline: {e}"))?;
    let mut root = None;
    let mut roothash = None;
    let mut verity_root_data = None;
    let mut verity_root_hash = None;
    let mut ee_params = 0usize;

    for param in cmdline.split_whitespace() {
        if let Some(value) = param.strip_prefix("root=") {
            root = Some(value);
        } else if let Some(value) = param.strip_prefix("roothash=") {
            roothash = Some(value);
        } else if let Some(value) = param.strip_prefix("systemd.verity_root_data=") {
            verity_root_data = Some(value);
        } else if let Some(value) = param.strip_prefix("systemd.verity_root_hash=") {
            verity_root_hash = Some(value);
        } else if param.starts_with("ee.") {
            ee_params += 1;
        }
    }

    eprintln!("easyenclave initrd: probe-only mode");
    eprintln!("easyenclave initrd: root={}", root.unwrap_or("<unset>"));
    eprintln!(
        "easyenclave initrd: systemd.verity_root_data={}",
        verity_root_data.unwrap_or("<unset>")
    );
    eprintln!(
        "easyenclave initrd: systemd.verity_root_hash={}",
        verity_root_hash.unwrap_or("<unset>")
    );
    eprintln!(
        "easyenclave initrd: roothash={}",
        roothash.unwrap_or("<unset>")
    );
    eprintln!("easyenclave initrd: ee_params={ee_params}");
    Ok(())
}

fn print_usage() {
    eprintln!("usage: easyenclave [initrd [--probe-only]]");
}

fn print_initrd_usage() {
    eprintln!("usage: easyenclave initrd [--probe-only]");
}
