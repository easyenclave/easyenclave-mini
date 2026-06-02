//! ee-init — the static initramfs PID 1 for easyenclave sealed VMs.
//!
//! Replaces the old busybox + shell initrd (`ext4-label.sh` + the per-vendor
//! `vendors/*.sh`). Boot sequence (mirrors `ext4-label.sh` step-for-step):
//!
//!   1. mount /proc /sys /dev (devtmpfs)
//!   2. parse /proc/cmdline (root=, roothash=, ee.*)
//!   3. load kernel modules from /lib/modules/$KVER (init_module, dep order)
//!   4. resolve root by LABEL/UUID (ext4 superblock scan), 30s retry
//!   5. mount root read-only at /mnt/root
//!   6. tmpfs overlays + seed /run/easyenclave/env from ee.* params
//!   7. vendor stage: net up + DHCP + cloud metadata -> append env
//!   8. mount --move /proc /sys /dev into newroot; switch_root /sbin/init
//!
//! The post-switch seam is unchanged: `easyenclave` (the rootfs PID 1) still
//! owns configfs/tsm + /dev/pts in `src/init.rs:maybe_init`.
//!
//! Pure-logic modules (`env`, `cmdline`) are portable and unit-tested on any
//! host; the syscall/network modules are `cfg(target_os = "linux")`.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod cmdline;
mod env;

#[cfg(target_os = "linux")]
mod blockdev;
#[cfg(target_os = "linux")]
mod fail;
#[cfg(target_os = "linux")]
mod modules;
#[cfg(target_os = "linux")]
mod mount;
#[cfg(target_os = "linux")]
mod net;
#[cfg(target_os = "linux")]
mod switch;
#[cfg(target_os = "linux")]
mod vendor;

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ee-init runs only as PID 1 on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::cmdline;
    use crate::{blockdev, env, fail, modules, mount, switch, vendor};
    use std::time::Duration;

    const NEWROOT: &str = "/mnt/root";
    const ENV_PATH: &str = "/mnt/root/run/easyenclave/env";

    pub fn run() {
        eprintln!("ee-init: starting (initramfs PID 1)");

        // 1. Virtual filesystems. devtmpfs auto-populates block nodes.
        for (src, target, fstype) in [
            ("proc", "/proc", "proc"),
            ("sysfs", "/sys", "sysfs"),
            ("devtmpfs", "/dev", "devtmpfs"),
        ] {
            if let Err(e) = mount::mount(src, target, fstype, 0, None) {
                eprintln!("ee-init: mount {target}: {e}");
            }
        }

        // 2. Kernel cmdline.
        let raw = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
        let cl = cmdline::parse(&raw);

        // 3. Load every module shipped in the initrd tree (mkinitrd curated the
        //    set per TARGET_INITRD_MODULES), in modules.dep dependency order.
        modules::load_all();

        // 4. Resolve the root device (by label/uuid), waiting for async probe.
        let root_dev =
            blockdev::resolve_root(&cl.root, Duration::from_secs(30)).unwrap_or_else(|| {
                blockdev::dump_candidates();
                fail::fatal("root device not found after 30s");
            });
        eprintln!("ee-init: root resolved to {}", root_dev.display());

        // 5. Mount root read-only. dm-verity is deferred (no target sets
        //    roothash=); fail loudly if one ever does.
        if cmdline::has_verity(&cl) {
            fail::fatal("dm-verity (roothash=) requested but not supported in ee-init v1");
        }
        if let Err(e) = mount::mount(
            &root_dev.to_string_lossy(),
            NEWROOT,
            "ext4",
            libc::MS_RDONLY,
            None,
        ) {
            fail::fatal(&format!("mount root {}: {e}", root_dev.display()));
        }

        // 6. Writable tmpfs overlays + seed env from ee.* params.
        setup_overlays_and_env(&cl);

        // 7. Vendor stage: networking + cloud metadata -> append env.
        vendor::run_stage(&cl, NEWROOT, ENV_PATH);

        // 8. Hand off to the rootfs PID 1.
        switch::switch_root(NEWROOT, "/sbin/init");
    }

    fn setup_overlays_and_env(cl: &cmdline::Cmdline) {
        // /run and /tmp must be writable on the RO rootfs; tmpfs or mkdir.
        for sub in ["run", "tmp"] {
            let t = format!("{NEWROOT}/{sub}");
            if mount::mount("tmpfs", &t, "tmpfs", 0, None).is_err() {
                let _ = mount::mkdir_p(&t);
            }
        }
        let vlib = format!("{NEWROOT}/var/lib/easyenclave");
        let _ = mount::mkdir_p(&vlib);
        let _ = mount::mount("tmpfs", &vlib, "tmpfs", 0, None);
        for d in [
            format!("{NEWROOT}/run/easyenclave"),
            format!("{vlib}/workloads"),
            format!("{vlib}/shared"),
        ] {
            let _ = mount::mkdir_p(&d);
        }

        let envf = env::EnvFile::new(ENV_PATH);
        if let Err(e) = envf.seed(&cl.ee_lines) {
            eprintln!("ee-init: seed env: {e}");
        }
    }
}
