//! Thin `libc::mount`/`umount2` wrappers — the same `CString` + raw-syscall
//! style as `../../src/init.rs:nix_mount`, with explicit flags/data.

use std::ffi::CString;
use std::io;

/// `mount(2)`. Creates `target` first (mountpoints must exist). `data` is the
/// fs-specific options string (e.g. none for ext4-ro; unused for `MS_MOVE`).
pub fn mount(
    src: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> io::Result<()> {
    let _ = std::fs::create_dir_all(target);
    let src_c = CString::new(src)?;
    let target_c = CString::new(target)?;
    let fstype_c = CString::new(fstype)?;
    let data_c = data.map(CString::new).transpose()?;
    let data_ptr = data_c
        .as_ref()
        .map_or(std::ptr::null(), |d| d.as_ptr() as *const libc::c_void);
    let ret = unsafe {
        libc::mount(
            src_c.as_ptr(),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            flags,
            data_ptr,
        )
    };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `mount --move src target` (`MS_MOVE` ignores fstype/data).
pub fn mount_move(src: &str, target: &str) -> io::Result<()> {
    mount(src, target, "", libc::MS_MOVE, None)
}

/// `umount2(target, MNT_DETACH)` — lazy unmount, matches the config-disk
/// probe's `umount` in `qemu.sh`.
pub fn umount(target: &str) -> io::Result<()> {
    let target_c = CString::new(target)?;
    let ret = unsafe { libc::umount2(target_c.as_ptr(), libc::MNT_DETACH) };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn mkdir_p(path: &str) -> io::Result<()> {
    std::fs::create_dir_all(path)
}
