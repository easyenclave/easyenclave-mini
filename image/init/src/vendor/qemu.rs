//! qemu vendor stage — port of `qemu.sh`. virtio/isofs are already loaded by
//! `modules::load_all`; bring up the first interface (best-effort DHCP), then
//! probe a secondary config disk (`/dev/vdb`, `/dev/sdb`) for `/agent.env`.

use crate::env::EnvFile;
use crate::vendor::log_merged;
use crate::{mount, net};
use std::path::Path;

const CONFIG_MNT: &str = "/tmp/ee-config-disk";

pub fn run(env: &EnvFile, newroot: &Path) {
    if let Some(iface) = net::first_iface() {
        net::ifup(&iface, env, newroot);
    }
    probe_config_disk(env);
}

fn probe_config_disk(env: &EnvFile) {
    let _ = mount::mkdir_p(CONFIG_MNT);
    for dev in ["/dev/vdb", "/dev/sdb"] {
        if !Path::new(dev).exists() {
            continue;
        }
        for fstype in ["iso9660", "ext4", "vfat", "ext2"] {
            if mount::mount(dev, CONFIG_MNT, fstype, libc::MS_RDONLY, None).is_ok() {
                eprintln!("vendor:qemu: mounted config disk ({dev}/{fstype})");
                let agent = Path::new(CONFIG_MNT).join("agent.env");
                if let Ok(body) = std::fs::read_to_string(&agent) {
                    match env.append_config(&body) {
                        Ok(()) => log_merged("qemu", "agent.env", env),
                        Err(e) => eprintln!("vendor:qemu: agent.env merge failed: {e}"),
                    }
                }
                let _ = mount::umount(CONFIG_MNT);
                return;
            }
        }
    }
    eprintln!("vendor:qemu: no config disk at /dev/vdb or /dev/sdb");
}
