//! Kernel cmdline parsing — port of the `for param in $(cat /proc/cmdline)`
//! loop in `ext4-label.sh`. Pure string handling so it unit-tests on any host.

/// How the root filesystem is identified on the cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSpec {
    Label(String),
    Uuid(String),
    Path(String),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmdline {
    pub root: RootSpec,
    pub roothash: Option<String>,
    pub verity_data: Option<String>,
    pub verity_hash: Option<String>,
    /// `ee.vendor=<v>` — selects the vendor stage at runtime (one binary,
    /// all three vendors). Pulled out of `ee_lines` so it never leaks into
    /// the newroot env file.
    pub vendor: Option<String>,
    /// Remaining `ee.*` params, prefix stripped, to seed the env file.
    pub ee_lines: Vec<String>,
}

impl Default for Cmdline {
    fn default() -> Self {
        Self {
            root: RootSpec::None,
            roothash: None,
            verity_data: None,
            verity_hash: None,
            vendor: None,
            ee_lines: Vec::new(),
        }
    }
}

fn parse_root_spec(v: &str) -> RootSpec {
    if let Some(l) = v.strip_prefix("LABEL=") {
        RootSpec::Label(l.to_string())
    } else if let Some(u) = v.strip_prefix("UUID=") {
        RootSpec::Uuid(u.to_string())
    } else {
        RootSpec::Path(v.to_string())
    }
}

pub fn parse(cmdline: &str) -> Cmdline {
    let mut c = Cmdline::default();
    for tok in cmdline.split_whitespace() {
        if let Some(v) = tok.strip_prefix("root=") {
            c.root = parse_root_spec(v);
            // ext4-label.sh sets ROOT_DATA from root= too.
            c.verity_data.get_or_insert_with(|| v.to_string());
        } else if let Some(v) = tok.strip_prefix("roothash=") {
            c.roothash = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("systemd.verity_root_data=") {
            c.verity_data = Some(v.to_string());
            if c.root == RootSpec::None {
                c.root = parse_root_spec(v);
            }
        } else if let Some(v) = tok.strip_prefix("systemd.verity_root_hash=") {
            c.verity_hash = Some(v.to_string());
        } else if let Some(rest) = tok.strip_prefix("ee.") {
            if let Some(vendor) = rest.strip_prefix("vendor=") {
                c.vendor = Some(vendor.to_string());
            } else {
                c.ee_lines.push(rest.to_string());
            }
        }
    }
    c
}

/// True when a dm-verity root was requested (roothash + verity hash device).
pub fn has_verity(c: &Cmdline) -> bool {
    c.roothash.is_some() && c.verity_hash.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_root_and_ee_params() {
        let c = parse("root=LABEL=root console=ttyS0,115200 ee.vendor=qemu ee.EE_OWNER=alice");
        assert_eq!(c.root, RootSpec::Label("root".into()));
        assert_eq!(c.vendor.as_deref(), Some("qemu"));
        assert_eq!(c.ee_lines, vec!["EE_OWNER=alice".to_string()]);
        assert!(!has_verity(&c));
    }

    #[test]
    fn uuid_and_path_roots() {
        assert_eq!(
            parse("root=UUID=1234-abcd").root,
            RootSpec::Uuid("1234-abcd".into())
        );
        assert_eq!(
            parse("root=/dev/vda2").root,
            RootSpec::Path("/dev/vda2".into())
        );
    }

    #[test]
    fn verity_detected() {
        let c = parse(
            "roothash=deadbeef systemd.verity_root_data=/dev/vda2 systemd.verity_root_hash=/dev/vda3",
        );
        assert!(has_verity(&c));
        assert_eq!(c.roothash.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn vendor_not_leaked_into_ee_lines() {
        let c = parse("ee.vendor=gcp ee.EE_DNS=8.8.8.8");
        assert_eq!(c.vendor.as_deref(), Some("gcp"));
        assert_eq!(c.ee_lines, vec!["EE_DNS=8.8.8.8".to_string()]);
    }
}
