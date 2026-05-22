// trust-on-first-use known-host store for SFTP. capscr records the SHA256
// fingerprint of each SSH server's public key on the first connect to a given
// host:port and refuses to upload on subsequent mismatches. mismatch can mean
// legitimate key rotation OR an active MITM — either way the user must
// explicitly forget the stored fingerprint via Settings → SSH before capscr
// will re-trust the new key.
//
// stored as TOML at <config_dir>/ssh_known_hosts.toml. atomic write via the
// rename-temp pattern so a crash mid-save can't truncate the file. concurrent
// callers serialise on the on-disk file via OS atomic-rename semantics —
// there's no in-process mutex because the only writer is the russh hook
// thread invoked once per upload.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownHosts {
    // map of "host:port" → SHA256 fingerprint (as returned by ssh-key's
    // PublicKey::fingerprint(HashAlg::Sha256).to_string())
    #[serde(default)]
    pub hosts: HashMap<String, KnownHostEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHostEntry {
    pub fingerprint: String,
    // unix seconds of first observation. used only for the Settings list so
    // the user can tell at a glance how long they've been talking to a host
    #[serde(default)]
    pub first_seen_unix: u64,
}

impl KnownHosts {
    pub fn default_path() -> Option<PathBuf> {
        crate::config::Config::config_dir().map(|d| d.join("ssh_known_hosts.toml"))
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(body) => match toml::from_str::<KnownHosts>(&body) {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(
                        "ssh_known_hosts at {:?} failed to parse ({e}); treating as empty",
                        path
                    );
                    KnownHosts::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => KnownHosts::default(),
            Err(e) => {
                tracing::warn!("ssh_known_hosts read failed: {e}; treating as empty");
                KnownHosts::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("ssh_known_hosts parent dir create failed: {e}"))?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| anyhow!("ssh_known_hosts serialize failed: {e}"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body.as_bytes())
            .map_err(|e| anyhow!("ssh_known_hosts temp write failed: {e}"))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| anyhow!("ssh_known_hosts atomic rename failed: {e}"))?;
        Ok(())
    }

    pub fn lookup(&self, host_port: &str) -> Option<&KnownHostEntry> {
        self.hosts.get(host_port)
    }

    pub fn insert(&mut self, host_port: String, fingerprint: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.hosts.insert(
            host_port,
            KnownHostEntry {
                fingerprint,
                first_seen_unix: now,
            },
        );
    }

    pub fn forget(&mut self, host_port: &str) -> bool {
        self.hosts.remove(host_port).is_some()
    }
}

pub fn host_key(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_known_hosts.toml");

        let mut kh = KnownHosts::default();
        kh.insert("sftp.example.com:22".into(), "SHA256:abc123".into());
        kh.insert("other.host:2222".into(), "SHA256:def456".into());
        kh.save(&path).expect("save");

        let loaded = KnownHosts::load(&path);
        assert_eq!(loaded.hosts.len(), 2);
        assert_eq!(
            loaded.lookup("sftp.example.com:22").map(|e| e.fingerprint.as_str()),
            Some("SHA256:abc123")
        );
        assert_eq!(
            loaded.lookup("other.host:2222").map(|e| e.fingerprint.as_str()),
            Some("SHA256:def456")
        );
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.toml");
        let kh = KnownHosts::load(&path);
        assert!(kh.hosts.is_empty());
    }

    #[test]
    fn forget_removes_entry() {
        let mut kh = KnownHosts::default();
        kh.insert("host:22".into(), "fp".into());
        assert!(kh.forget("host:22"));
        assert!(!kh.forget("host:22"));
        assert!(kh.hosts.is_empty());
    }

    #[test]
    fn corrupt_file_falls_back_to_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_known_hosts.toml");
        std::fs::write(&path, "this is not valid toml }}}").unwrap();
        let kh = KnownHosts::load(&path);
        assert!(kh.hosts.is_empty());
    }
}
