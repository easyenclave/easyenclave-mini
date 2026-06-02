//! azure vendor stage — port of `azure.sh`. hv_* modules already loaded; bring
//! up the interface, fetch IMDS `userData` (fall back to `customData`),
//! base64-decode, merge.

use crate::env::EnvFile;
use crate::net;
use crate::vendor::log_merged;
use base64::Engine;
use std::path::Path;
use std::time::Duration;

pub fn run(env: &EnvFile, newroot: &Path) {
    match net::first_iface() {
        Some(iface) => net::ifup(&iface, env, newroot),
        None => {
            eprintln!("vendor:azure: no network interface");
            return;
        }
    }

    let blob = fetch_imds("userData", "2021-01-01")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| fetch_imds("customData", "2021-02-01"));
    let Some(b64) = blob else {
        eprintln!("vendor:azure: no userData/customData");
        return;
    };

    let b64 = b64.trim().trim_matches('"');
    match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(bytes) => {
            let body = String::from_utf8_lossy(&bytes);
            if body.trim().is_empty() {
                eprintln!("vendor:azure: empty customData");
                return;
            }
            match env.append_config(&body) {
                Ok(()) => log_merged("azure", "customData", env),
                Err(e) => eprintln!("vendor:azure: merge failed: {e}"),
            }
        }
        Err(e) => eprintln!("vendor:azure: base64 decode failed: {e}"),
    }
}

fn fetch_imds(field: &str, api_version: &str) -> Option<String> {
    let path = format!("/metadata/instance/compute/{field}?api-version={api_version}&format=text");
    net::http::imds_get(&path, ("Metadata", "true"), Duration::from_secs(2))
}
