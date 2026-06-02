//! Kernel module loading — replaces busybox modprobe/insmod. The initrd ships
//! DECOMPRESSED `.ko` (mkinitrd.sh unzips `.ko.zst`) plus a depmod-generated
//! `modules.dep`, so we use the legacy `init_module(2)` (whole-image) and load
//! in dependency order (deps before dependents — e.g. `hv_vmbus` before
//! `hv_storvsc`). Built-ins have no `.ko`/no dep line and are skipped;
//! already-loaded (`EEXIST`) is tolerated.

use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::path::Path;

pub fn load_all() {
    let kver = match uname_release() {
        Some(k) => k,
        None => {
            eprintln!("ee-init: modules: uname() failed; skipping");
            return;
        }
    };
    let base = format!("/lib/modules/{kver}");
    let dep_path = format!("{base}/modules.dep");
    let dep = match std::fs::read_to_string(&dep_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ee-init: modules: no {dep_path} ({e}); assuming built-in drivers");
            return;
        }
    };
    let graph = parse_modules_dep(&dep);
    let mut loaded = HashSet::new();
    for module in graph.keys() {
        load_with_deps(module, &graph, &base, &mut loaded);
    }
}

/// `modules.dep` line: `kernel/.../foo.ko: kernel/.../bar.ko kernel/.../baz.ko`.
/// Deps after the colon must load first. Paths are relative to the base dir.
fn parse_modules_dep(dep: &str) -> BTreeMap<String, Vec<String>> {
    let mut graph = BTreeMap::new();
    for line in dep.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, rest)) = line.split_once(':') {
            let deps = rest.split_whitespace().map(str::to_string).collect();
            graph.insert(name.trim().to_string(), deps);
        }
    }
    graph
}

fn load_with_deps(
    module: &str,
    graph: &BTreeMap<String, Vec<String>>,
    base: &str,
    loaded: &mut HashSet<String>,
) {
    if !loaded.insert(module.to_string()) {
        return;
    }
    if let Some(deps) = graph.get(module) {
        for d in deps {
            load_with_deps(d, graph, base, loaded);
        }
    }
    insmod(&format!("{base}/{module}"));
}

fn insmod(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return, // dep listed but file absent (built-in / stale dep)
    };
    let params = CString::new("").unwrap();
    let ret = unsafe {
        libc::syscall(
            libc::SYS_init_module,
            data.as_ptr() as *const libc::c_void,
            data.len() as libc::c_ulong,
            params.as_ptr(),
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            let name = Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            eprintln!("ee-init: modules: {name} not loaded ({err}) (may be built-in)");
        }
    }
}

fn uname_release() -> Option<String> {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = uts
        .release
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
