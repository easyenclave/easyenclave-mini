//! Fatal-error handling. The initramfs no longer ships a shell to `exec /bin/sh`
//! into on failure (busybox is gone), so we emit the `FATAL:` token the smoke
//! tests grep for (`ci-tdx-smoke-*-runner.sh`), flush serial, and reboot —
//! rather than hang CI on a wedged boot.

pub fn fatal(msg: &str) -> ! {
    eprintln!("FATAL: ee-init: {msg}");
    unsafe {
        libc::sync();
        libc::sleep(5);
        libc::reboot(libc::RB_AUTOBOOT);
    }
    loop {
        unsafe { libc::pause() };
    }
}
