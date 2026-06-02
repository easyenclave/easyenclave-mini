//! Interface bring-up — port of `ee_ifup` from `_lib.sh`. Static `EE_IP` or
//! DHCP; `EE_DNS` override wins; resolv.conf is written to
//! `<newroot>/run/resolv.conf` (the tmpfs the initrd already mounted) so it
//! survives `switch_root` — replacing the old `/tmp/resolv.conf.udhcpc` splice
//! and the udhcpc hook entirely.

pub mod dhcp;
pub mod http;
pub mod rtnl;

use crate::env::EnvFile;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

pub fn loopback_up() {
    if let Some(idx) = if_index("lo") {
        let _ = rtnl::link_up(idx);
    }
}

/// First non-loopback interface (sorted), mirroring `ls /sys/class/net | grep -v lo | head -1`.
pub fn first_iface() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "lo")
        .collect();
    names.sort();
    names.into_iter().next()
}

pub fn if_index(name: &str) -> Option<u32> {
    std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn mac(name: &str) -> Option<[u8; 6]> {
    let s = std::fs::read_to_string(format!("/sys/class/net/{name}/address")).ok()?;
    let parts: Vec<u8> = s
        .trim()
        .split(':')
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect();
    <[u8; 6]>::try_from(parts.as_slice()).ok()
}

/// Bring `iface` up and configure it. `env` supplies static overrides
/// (`EE_IP`, `EE_GATEWAY`, `EE_DNS`); otherwise DHCP.
pub fn ifup(iface: &str, env: &EnvFile, newroot: &Path) {
    let idx = match if_index(iface) {
        Some(i) => i,
        None => {
            eprintln!("ee-init: net: no ifindex for {iface}");
            return;
        }
    };
    if let Err(e) = rtnl::link_up(idx) {
        eprintln!("ee-init: net: link up {iface}: {e}");
    }

    if let Some(static_ip) = env.get("EE_IP") {
        if let Some((ip, prefix)) = parse_cidr(&static_ip) {
            eprintln!("ee-init: net: static ip={static_ip} on {iface}");
            let _ = rtnl::addr_add(idx, ip, prefix);
            if let Some(gw) = env.get("EE_GATEWAY").and_then(|g| g.parse().ok()) {
                add_default(idx, gw);
            }
        } else {
            eprintln!("ee-init: net: bad EE_IP={static_ip}");
        }
    } else {
        let m = mac(iface).unwrap_or([0; 6]);
        eprintln!("ee-init: net: dhcp on {iface}");
        match dhcp::acquire(iface, idx, m, Duration::from_secs(15)) {
            Some(lease) => apply_lease(idx, &lease, newroot),
            None => eprintln!("ee-init: net: dhcp failed on {iface}"),
        }
    }

    if let Some(dns) = env.get("EE_DNS") {
        eprintln!("ee-init: net: static dns={dns}");
        write_resolv(newroot, std::slice::from_ref(&dns));
    }
}

fn apply_lease(idx: u32, lease: &dhcp::Lease, newroot: &Path) {
    eprintln!(
        "ee-init: net: lease ip={}/{} routers={:?}",
        lease.ip, lease.prefix_len, lease.routers
    );
    let _ = rtnl::addr_add(idx, lease.ip, lease.prefix_len);
    for gw in &lease.routers {
        add_default(idx, *gw);
    }
    let dns: Vec<String> = lease.dns.iter().map(|d| d.to_string()).collect();
    if !dns.is_empty() {
        write_resolv(newroot, &dns);
    }
}

/// Add a default route via `gw`, first ensuring `gw` is reachable: GCE hands
/// out an off-subnet /32 gateway, so a plain `default via gw` would be
/// ENETUNREACH. An on-link host route to `gw/32` covers both that case and the
/// normal on-subnet gateway (where it's redundant but harmless).
fn add_default(idx: u32, gw: Ipv4Addr) {
    let _ = rtnl::route_add_onlink(idx, gw, 32);
    if let Err(e) = rtnl::route_add_default(idx, gw) {
        eprintln!("ee-init: net: default via {gw}: {e}");
    }
}

fn write_resolv(newroot: &Path, nameservers: &[String]) {
    let dir = newroot.join("run");
    let _ = std::fs::create_dir_all(&dir);
    let body: String = nameservers
        .iter()
        .map(|n| format!("nameserver {n}\n"))
        .collect();
    if let Err(e) = std::fs::write(dir.join("resolv.conf"), body) {
        eprintln!("ee-init: net: write resolv.conf: {e}");
    }
}

fn parse_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, prefix) = s.split_once('/')?;
    Some((ip.parse().ok()?, prefix.parse().ok()?))
}
