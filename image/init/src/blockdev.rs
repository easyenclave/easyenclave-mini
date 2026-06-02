//! Root-device resolution — replaces busybox `findfs LABEL=root`. devtmpfs
//! creates the `/dev` nodes; we identify the right one by reading the ext4
//! superblock (`s_volume_name` / `s_uuid`) directly, so the same UKI boots
//! `vda2` (qemu) and `nvme0n1p2` (GCP) by label rather than a fixed path.

use crate::cmdline::RootSpec;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

// ext4 superblock: starts 1024 B into the partition; 16-bit magic at +0x38,
// 16-byte UUID at +0x68, 16-byte volume label at +0x78.
const SB_OFFSET: u64 = 1024;
const MAGIC_OFFSET: u64 = SB_OFFSET + 0x38;
const UUID_OFFSET: u64 = SB_OFFSET + 0x68;
const LABEL_OFFSET: u64 = SB_OFFSET + 0x78;
const EXT_MAGIC: u16 = 0xEF53;

/// Resolve the root device, retrying for `timeout` while async virtio/VMBus
/// probe enumerates partitions.
pub fn resolve_root(spec: &RootSpec, timeout: Duration) -> Option<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(p) = try_resolve(spec) {
            return Some(p);
        }
        if Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_secs(1));
    }
}

fn try_resolve(spec: &RootSpec) -> Option<PathBuf> {
    match spec {
        RootSpec::Path(p) => {
            let pb = PathBuf::from(p);
            pb.exists().then_some(pb)
        }
        RootSpec::Label(want) => scan_block_devices()
            .into_iter()
            .find(|dev| ext4_label(dev).as_deref() == Some(want.as_str())),
        RootSpec::Uuid(want) => {
            let want = normalize_uuid(want);
            scan_block_devices()
                .into_iter()
                .find(|dev| ext4_uuid(dev).map(|u| normalize_uuid(&u)).as_deref() == Some(&want))
        }
        RootSpec::None => None,
    }
}

/// Candidate `/dev/<name>` nodes from `/sys/class/block` (partitions + disks).
fn scan_block_devices() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/block") {
        for e in entries.flatten() {
            let dev = Path::new("/dev").join(e.file_name());
            if dev.exists() {
                out.push(dev);
            }
        }
    }
    out
}

fn read_at(dev: &Path, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut f = File::open(dev).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn is_ext(dev: &Path) -> bool {
    match read_at(dev, MAGIC_OFFSET, 2) {
        Some(b) => u16::from_le_bytes([b[0], b[1]]) == EXT_MAGIC,
        None => false,
    }
}

fn ext4_label(dev: &Path) -> Option<String> {
    if !is_ext(dev) {
        return None;
    }
    read_at(dev, LABEL_OFFSET, 16).map(|raw| cstr_to_string(&raw))
}

fn ext4_uuid(dev: &Path) -> Option<String> {
    if !is_ext(dev) {
        return None;
    }
    let b = read_at(dev, UUID_OFFSET, 16)?;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

fn cstr_to_string(raw: &[u8]) -> String {
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

fn normalize_uuid(u: &str) -> String {
    u.trim().to_ascii_lowercase()
}

/// Diagnostic dump on resolution failure (replaces the shell's `ls /dev/...`).
pub fn dump_candidates() {
    let mut line = String::from("ee-init: block devices:");
    for d in scan_block_devices() {
        let label = ext4_label(&d).unwrap_or_default();
        line.push_str(&format!(" {}[{}]", d.display(), label));
    }
    eprintln!("{line}");
}
