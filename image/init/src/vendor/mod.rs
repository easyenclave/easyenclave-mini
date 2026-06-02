//! Vendor stages — port of `image/init-templates/vendors/{qemu,gcp,azure}.sh`.
//! One binary carries all three; the active one is selected by the baked
//! `ee.vendor=<v>` cmdline param (DMI autodetect + qemu fallback).

pub mod azure;
pub mod gcp;
pub mod qemu;

use crate::cmdline::Cmdline;
use crate::env::EnvFile;
use crate::net;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub enum Vendor {
    Qemu,
    Gcp,
    Azure,
}

pub fn run_stage(cl: &Cmdline, newroot: &str, env_path: &str) {
    let which = select(cl);
    eprintln!("ee-init: vendor stage = {which:?}");
    let env = EnvFile::new(env_path);
    let root = Path::new(newroot);
    net::loopback_up();
    match which {
        Vendor::Qemu => qemu::run(&env, root),
        Vendor::Gcp => gcp::run(&env, root),
        Vendor::Azure => azure::run(&env, root),
    }
}

fn select(cl: &Cmdline) -> Vendor {
    if let Some(v) = cl.vendor.as_deref() {
        match v {
            "gcp" => return Vendor::Gcp,
            "azure" => return Vendor::Azure,
            "qemu" => return Vendor::Qemu,
            other => eprintln!("ee-init: unknown ee.vendor={other}; autodetecting"),
        }
    }
    autodetect()
}

fn autodetect() -> Vendor {
    let vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
    let v = vendor.trim();
    if v.contains("Google") {
        Vendor::Gcp
    } else if v.contains("Microsoft") {
        Vendor::Azure
    } else {
        Vendor::Qemu
    }
}

/// Uniform merge log. The local smoke test greps for
/// `vendor:qemu: merged .* config into`, so every vendor follows that shape.
pub fn log_merged(vendor: &str, detail: &str, env: &EnvFile) {
    eprintln!(
        "vendor:{vendor}: merged {detail} config into {}",
        env.path().display()
    );
}
