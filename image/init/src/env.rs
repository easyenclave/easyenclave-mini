//! The `/run/easyenclave/env` merge contract — a faithful Rust port of
//! `image/init-templates/vendors/_lib.sh` (`_ee_json_to_env`,
//! `ee_append_config`, `ee_env_get`). The env file is a KEY=VALUE file, one
//! per line, seeded from `ee.*` cmdline params and then appended to by the
//! vendor stage (config-disk `agent.env`, GCE/Azure metadata) in precedence
//! order — later sources win, which `get()` honors by taking the last match.
//!
//! Portable (std + serde_json only) so the contract tests run on any host.
//! The 10 cases at the bottom mirror `image/ci-test-vendor-lib.sh` exactly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Flatten a flat JSON object `{string:string}` into ordered KEY=VALUE pairs.
///
/// Port of `_ee_json_to_env`: the body must be a JSON object whose every value
/// is a string; anything else (non-object, nested object/array, non-string
/// value) is an error. serde_json's string decoder unescapes `\"` for us — the
/// legacy GCE `EE_BOOT_WORKLOADS` case where the value is a JSON-encoded JSON
/// array. `preserve_order` keeps source order.
pub fn json_to_env(body: &str) -> Result<Vec<(String, String)>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|e| format!("not JSON: {e}"))?;
    let obj = match value {
        serde_json::Value::Object(map) => map,
        _ => return Err("not a JSON object".into()),
    };
    let mut out = Vec::with_capacity(obj.len());
    for (key, val) in obj {
        match val {
            serde_json::Value::String(s) => out.push((key, s)),
            _ => return Err(format!("non-string value for key {key:?}")),
        }
    }
    Ok(out)
}

/// Compute the KEY=VALUE lines a config body contributes.
///
/// Port of `ee_append_config`: a body whose first non-space char is `{` is
/// flattened as JSON (hard error on flatten failure — no partial output);
/// anything else is KEY=VALUE passthrough with comment (`#`) and blank lines
/// filtered. An empty body contributes nothing.
pub fn config_lines(body: &str) -> Result<Vec<String>, String> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    if body.trim_start().starts_with('{') {
        let pairs = json_to_env(body)?;
        Ok(pairs.into_iter().map(|(k, v)| format!("{k}={v}")).collect())
    } else {
        Ok(body
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !(t.is_empty() || t.starts_with('#'))
            })
            .map(str::to_string)
            .collect())
    }
}

/// A handle to the newroot env file at `/mnt/root/run/easyenclave/env`.
pub struct EnvFile {
    path: PathBuf,
}

impl EnvFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Seed (overwrite) the file with `ee.*`-derived KEY=VALUE lines. Mirrors
    /// `cat /tmp/ee-cmdline.env > .../env` in `ext4-label.sh`.
    pub fn seed(&self, lines: &[String]) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut body = lines.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        fs::write(&self.path, body)
    }

    /// Append a config body. `Err` (and nothing written) on JSON-flatten
    /// failure, matching the shell's loud-fail-no-partial-write semantics.
    pub fn append_config(&self, body: &str) -> Result<(), String> {
        let lines = config_lines(body)?;
        if lines.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open {}: {e}", self.path.display()))?;
        for line in &lines {
            writeln!(f, "{line}").map_err(|e| format!("write: {e}"))?;
        }
        Ok(())
    }

    /// Last value for `key` (port of `ee_env_get`: `grep ^KEY= | tail -1 | cut -d= -f2-`).
    pub fn get(&self, key: &str) -> Option<String> {
        let body = fs::read_to_string(&self.path).ok()?;
        let prefix = format!("{key}=");
        body.lines()
            .rfind(|l| l.starts_with(&prefix))
            .map(|l| l[prefix.len()..].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- _ee_json_to_env (ci-test-vendor-lib.sh cases 1-5) ----

    #[test]
    fn flat_object() {
        assert_eq!(
            json_to_env(r#"{"A":"1","B":"2"}"#).unwrap(),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
    }

    #[test]
    fn whitespace_tolerated() {
        assert_eq!(
            json_to_env(r#"{ "A" : "1" , "B": "2" }"#).unwrap(),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
    }

    #[test]
    fn escaped_quote_values_unescape() {
        // legacy GCE ee-config: a JSON-string-encoded JSON array value.
        let payload = r#"{"EE_BOOT_WORKLOADS":"[{\"app_name\":\"foo\"}]","EE_OWNER":"alice"}"#;
        assert_eq!(
            json_to_env(payload).unwrap(),
            vec![
                ("EE_BOOT_WORKLOADS".into(), r#"[{"app_name":"foo"}]"#.into()),
                ("EE_OWNER".into(), "alice".into()),
            ]
        );
    }

    #[test]
    fn empty_object_no_output() {
        assert_eq!(json_to_env("{}").unwrap(), Vec::<(String, String)>::new());
    }

    #[test]
    fn non_object_rejected() {
        assert!(json_to_env("not json").is_err());
        assert!(json_to_env(r#""just a string""#).is_err());
        assert!(json_to_env("[1,2]").is_err());
        // nested / non-string values are rejected too
        assert!(json_to_env(r#"{"A":{"nested":"x"}}"#).is_err());
        assert!(json_to_env(r#"{"A":1}"#).is_err());
    }

    // ---- ee_append_config (cases 6-10), exercised via config_lines ----

    #[test]
    fn keyvalue_passthrough() {
        assert_eq!(
            config_lines("EE_OWNER=alice\nEE_DATA_DIR=/var/x").unwrap(),
            vec!["EE_OWNER=alice", "EE_DATA_DIR=/var/x"]
        );
    }

    #[test]
    fn comments_and_blanks_filtered() {
        let body = "# header comment\n\nEE_OWNER=alice\n# inline\nEE_DATA_DIR=/var/x\n";
        assert_eq!(
            config_lines(body).unwrap(),
            vec!["EE_OWNER=alice", "EE_DATA_DIR=/var/x"]
        );
    }

    #[test]
    fn json_body_auto_flattened() {
        let body = r#"{"EE_BOOT_WORKLOADS":"[{\"app_name\":\"foo\"}]","EE_OWNER":"alice"}"#;
        assert_eq!(
            config_lines(body).unwrap(),
            vec![
                r#"EE_BOOT_WORKLOADS=[{"app_name":"foo"}]"#,
                "EE_OWNER=alice"
            ]
        );
    }

    #[test]
    fn leading_spaces_then_json() {
        assert_eq!(config_lines(r#"   {"A":"1"}"#).unwrap(), vec!["A=1"]);
    }

    #[test]
    fn empty_body_is_noop() {
        assert_eq!(config_lines("").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn json_flatten_failure_is_error() {
        // body looks like JSON (leading `{`) but isn't a flat string object.
        assert!(config_lines(r#"{"A":["nested"]}"#).is_err());
    }

    // ---- EnvFile file round-trips (seed / append / get precedence) ----

    #[test]
    fn append_and_get_last_wins() {
        let dir = tempfile::tempdir().unwrap();
        let env = EnvFile::new(dir.path().join("env"));
        env.seed(&["EE_OWNER=seed".into()]).unwrap();
        env.append_config("EE_OWNER=disk\nEE_DATA_DIR=/var/x")
            .unwrap();
        env.append_config(r#"{"EE_OWNER":"meta"}"#).unwrap();
        // later append wins
        assert_eq!(env.get("EE_OWNER").as_deref(), Some("meta"));
        assert_eq!(env.get("EE_DATA_DIR").as_deref(), Some("/var/x"));
        assert_eq!(env.get("MISSING"), None);
    }

    #[test]
    fn append_config_json_failure_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let env = EnvFile::new(dir.path().join("env"));
        env.seed(&["EE_OWNER=seed".into()]).unwrap();
        assert!(env.append_config(r#"{"A":["nested"]}"#).is_err());
        // file unchanged — no partial write
        assert_eq!(env.get("EE_OWNER").as_deref(), Some("seed"));
        assert_eq!(env.get("A"), None);
    }
}
