//! `switch_root` in Rust — replaces busybox `switch_root /mnt/root /sbin/init`
//! plus the `mount --move /proc /sys /dev` carry-forward in `ext4-label.sh`.
//!
//! The util-linux algorithm: move the virtual filesystems into the new root,
//! `chdir` there, `MS_MOVE` it onto `/`, `chroot .`, then `execv` the new init
//! as PID 1. We deliberately skip the recursive initramfs unlink (the most
//! error-prone step; negligible RAM on a multi-GB VM) — see the TODO.

use crate::fail;
use crate::mount;
use std::ffi::CString;

pub fn switch_root(newroot: &str, init: &str) -> ! {
    // Carry /proc /sys /dev forward so the new PID 1 inherits them.
    for fs in ["proc", "sys", "dev"] {
        let src = format!("/{fs}");
        let dst = format!("{newroot}/{fs}");
        if let Err(e) = mount::mount_move(&src, &dst) {
            eprintln!("ee-init: mount --move {src} -> {dst}: {e}");
        }
    }

    // TODO: free initramfs RAM by recursively unlinking the old root before
    // MS_MOVE (guarded by an fstatfs ramfs/tmpfs magic check). Skipped in v1.

    let newroot_c = CString::new(newroot).unwrap();
    if unsafe { libc::chdir(newroot_c.as_ptr()) } != 0 {
        fail::fatal(&format!(
            "switch_root: chdir {newroot}: {}",
            std::io::Error::last_os_error()
        ));
    }
    if let Err(e) = mount::mount(".", "/", "", libc::MS_MOVE, None) {
        fail::fatal(&format!("switch_root: MS_MOVE . /: {e}"));
    }
    let dot = CString::new(".").unwrap();
    let slash = CString::new("/").unwrap();
    if unsafe { libc::chroot(dot.as_ptr()) } != 0 {
        fail::fatal(&format!(
            "switch_root: chroot: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::chdir(slash.as_ptr()) } != 0 {
        fail::fatal(&format!(
            "switch_root: chdir /: {}",
            std::io::Error::last_os_error()
        ));
    }

    let init_c = CString::new(init).unwrap();
    let argv = [init_c.as_ptr(), std::ptr::null()];
    unsafe { libc::execv(init_c.as_ptr(), argv.as_ptr()) };
    // execv only returns on failure.
    fail::fatal(&format!(
        "switch_root: exec {init}: {}",
        std::io::Error::last_os_error()
    ));
}
