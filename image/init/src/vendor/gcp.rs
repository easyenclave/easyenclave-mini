//! gcp vendor stage — port of `gcp.sh`. gve/virtio_net already loaded; bring up
//! the interface, fetch the GCE instance metadata attribute `ee-config`
//! (plain HTTP, `Metadata-Flavor: Google`), merge it.

use crate::env::EnvFile;
use crate::net;
use crate::vendor::log_merged;
use std::path::Path;
use std::time::Duration;

const EE_CONFIG_PATH: &str = "/computeMetadata/v1/instance/attributes/ee-config";

pub fn run(env: &EnvFile, newroot: &Path) {
    match net::first_iface() {
        Some(iface) => net::ifup(&iface, env, newroot),
        None => {
            eprintln!("vendor:gcp: no network interface");
            return;
        }
    }
    match fetch_ee_config() {
        Some(body) if !body.trim().is_empty() => match env.append_config(&body) {
            Ok(()) => log_merged("gcp", "metadata", env),
            Err(e) => eprintln!("vendor:gcp: ee-config merge failed: {e}"),
        },
        _ => eprintln!("vendor:gcp: no ee-config (non-GCE host or unset)"),
    }
}

fn fetch_ee_config() -> Option<String> {
    net::http::imds_get(
        EE_CONFIG_PATH,
        ("Metadata-Flavor", "Google"),
        Duration::from_secs(2),
    )
}
