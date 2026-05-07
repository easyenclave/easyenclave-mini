//! PID 1 init: load the initrd-written env file, mount attestation, pty,
//! cgroup, and shared-memory filesystems, reap zombies. All vendor-shaped concerns (networking,
//! metadata fetch, config-disk probing, cmdline parsing, DNS, hostname)
//! live in the initrd's per-target vendor stage under
//! `image/init-templates/vendors/<vendor>.sh`, which writes its results
//! to `/run/easyenclave/env` before `switch_root`.
//!
//! `/run` is a tmpfs mounted by the initrd so the env file survives a
//! read-only rootfs (the GCP/Azure disk images mount root RO; squashfs
//! on local-tdx is strictly RO with a tmpfs overlay).

const ENV_FILE: &str = "/run/easyenclave/env";

/// If we are PID 1, load the env file written by the initrd vendor stage,
/// mount the filesystems easyenclave itself needs, and start the zombie
/// reaper. Filesystem prerequisites (/proc, /sys, /dev, tmpfs, /var/lib/
/// easyenclave) are provided by the initrd via `mount --move` before
/// `switch_root`. Everything else is a no-op when not PID 1.
pub fn maybe_init() {
    if std::process::id() != 1 {
        return;
    }

    eprintln!("easyenclave: running as PID 1 -- sealed VM init");

    std::env::set_var(
        "PATH",
        "/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin",
    );

    load_env_file();

    // configfs for TDX attestation (tsm report interface). Kept in Rust
    // because it's attestation-coupled, not vendor-coupled, and the
    // attestation backend detection depends on it existing.
    let _ = std::fs::create_dir_all("/sys/kernel/config");
    if let Err(e) = nix_mount("configfs", "/sys/kernel/config", "configfs") {
        eprintln!("easyenclave: init: mount configfs: {e}");
    }

    // /dev/pts for PTY-backed TTY workloads (attach method).
    let _ = std::fs::create_dir_all("/dev/pts");
    if let Err(e) = nix_mount("devpts", "/dev/pts", "devpts") {
        eprintln!("easyenclave: init: mount devpts: {e}");
    }

    // /sys/fs/cgroup for OCI runtimes. We still run the smoke workload
    // with cgroups disabled, but crun/runc require the mount point to be
    // a real cgroup filesystem while constructing the container spec.
    let _ = std::fs::create_dir_all("/sys/fs/cgroup");
    if let Err(e) = nix_mount_with_data(
        "cgroup2",
        "/sys/fs/cgroup",
        "cgroup2",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        None,
    ) {
        eprintln!("easyenclave: init: mount cgroup2: {e}");
    }

    // /dev/shm for POSIX shared memory. Container runtimes use this for
    // libpod/conmon locks even when the container itself uses host networking.
    let _ = std::fs::create_dir_all("/dev/shm");
    if let Err(e) = nix_mount_with_data(
        "tmpfs",
        "/dev/shm",
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        Some("mode=1777"),
    ) {
        eprintln!("easyenclave: init: mount /dev/shm: {e}");
    }

    let _ = std::fs::create_dir_all("/var/lib/easyenclave/workloads");
    let _ = std::fs::create_dir_all("/var/lib/easyenclave/shared");

    // Zombie reaper: PID 1 must wait() on reparented orphans or they
    // accumulate as zombies and eventually exhaust the PID space.
    std::thread::spawn(|| loop {
        unsafe {
            libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    });

    eprintln!("easyenclave: init complete");
}

/// Load `/run/easyenclave/env` (a KEY=VALUE file, one per line) into the
/// process environment. Written by the initrd vendor stage — carries
/// `ee.*` cmdline params, secondary-disk `agent.env`, and cloud-metadata
/// config, merged in that precedence order. Missing file is not an
/// error: the VM may be configured entirely via `/etc/easyenclave/
/// config.json` or the inherited env.
fn load_env_file() {
    let body = match std::fs::read_to_string(ENV_FILE) {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() {
                continue;
            }
            std::env::set_var(k, v);
            eprintln!("easyenclave: init: env {k}=<redacted>");
        }
    }
}

fn nix_mount(src: &str, target: &str, fstype: &str) -> Result<(), String> {
    nix_mount_with_data(src, target, fstype, 0, None)
}

fn nix_mount_with_data(
    src: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<(), String> {
    use std::ffi::CString;
    let _ = std::fs::create_dir_all(target);
    let src = CString::new(src).unwrap();
    let target_c = CString::new(target).unwrap();
    let fstype = CString::new(fstype).unwrap();
    let data_c = data.map(|s| CString::new(s).unwrap());
    let data_ptr = data_c
        .as_ref()
        .map_or(std::ptr::null(), |s| s.as_ptr() as *const libc::c_void);
    let ret = unsafe {
        libc::mount(
            src.as_ptr(),
            target_c.as_ptr(),
            fstype.as_ptr(),
            flags,
            data_ptr,
        )
    };
    if ret != 0 {
        Err(format!("errno {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
