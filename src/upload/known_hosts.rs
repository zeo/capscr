// trust-on-first-use store for sftp server fingerprints
// corrupt state and failed persistence reject the connection
// the process lock covers each complete load-check-save cycle

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustDecision {
    Trusted,
    FirstSeen,
    Mismatch { stored: String },
}

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

    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(body) => toml::from_str::<KnownHosts>(&body)
                .map_err(|e| anyhow!("ssh_known_hosts parse failed: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KnownHosts::default()),
            Err(e) => Err(anyhow!("ssh_known_hosts read failed: {e}")),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("ssh_known_hosts parent dir create failed: {e}"))?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| anyhow!("ssh_known_hosts serialize failed: {e}"))?;
        let tmp = path.with_extension(format!("toml.{}.tmp", uuid::Uuid::new_v4().as_simple()));
        std::fs::write(&tmp, body.as_bytes())
            .map_err(|e| anyhow!("ssh_known_hosts temp write failed: {e}"))?;
        if let Err(error) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow!("ssh_known_hosts atomic rename failed: {error}"));
        }
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

pub fn load_locked(path: &Path) -> Result<KnownHosts> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| anyhow!("ssh_known_hosts lock poisoned"))?;
    KnownHosts::load(path)
}

pub fn verify_or_trust(path: &Path, host_port: &str, fingerprint: &str) -> Result<TrustDecision> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| anyhow!("ssh_known_hosts lock poisoned"))?;
    let mut store = KnownHosts::load(path)?;
    match store.lookup(host_port) {
        Some(entry) if entry.fingerprint == fingerprint => Ok(TrustDecision::Trusted),
        Some(entry) => Ok(TrustDecision::Mismatch {
            stored: entry.fingerprint.clone(),
        }),
        None => {
            store.insert(host_port.to_string(), fingerprint.to_string());
            store.save(path)?;
            Ok(TrustDecision::FirstSeen)
        }
    }
}

pub fn forget_locked(path: &Path, host_port: &str) -> Result<bool> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| anyhow!("ssh_known_hosts lock poisoned"))?;
    let mut store = KnownHosts::load(path)?;
    let removed = store.forget(host_port);
    if removed {
        store.save(path)?;
    }
    Ok(removed)
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

        let loaded = load_locked(&path).expect("load");
        assert_eq!(loaded.hosts.len(), 2);
        assert_eq!(
            loaded
                .lookup("sftp.example.com:22")
                .map(|e| e.fingerprint.as_str()),
            Some("SHA256:abc123")
        );
        assert_eq!(
            loaded
                .lookup("other.host:2222")
                .map(|e| e.fingerprint.as_str()),
            Some("SHA256:def456")
        );
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.toml");
        let kh = load_locked(&path).expect("load");
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
    fn corrupt_file_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_known_hosts.toml");
        std::fs::write(&path, "this is not valid toml }}}").unwrap();
        assert!(load_locked(&path).is_err());
        assert!(verify_or_trust(&path, "host:22", "SHA256:new").is_err());
    }

    #[test]
    fn first_seen_requires_persistence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocked_parent = dir.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"file").expect("write blocker");
        let path = blocked_parent.join("ssh_known_hosts.toml");
        assert!(verify_or_trust(&path, "host:22", "SHA256:new").is_err());
    }

    #[test]
    fn concurrent_updates_preserve_distinct_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = std::sync::Arc::new(dir.path().join("ssh_known_hosts.toml"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for (host, fingerprint) in [
            ("one.example:22", "SHA256:one"),
            ("two.example:22", "SHA256:two"),
        ] {
            let path = path.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                verify_or_trust(&path, host, fingerprint)
            }));
        }
        barrier.wait();
        for thread in threads {
            assert_eq!(
                thread.join().expect("thread").expect("trust"),
                TrustDecision::FirstSeen
            );
        }
        let stored = load_locked(&path).expect("load");
        assert_eq!(stored.hosts.len(), 2);
    }

    #[test]
    fn concurrent_conflicting_first_keys_admit_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = std::sync::Arc::new(dir.path().join("ssh_known_hosts.toml"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for fingerprint in ["SHA256:one", "SHA256:two"] {
            let path = path.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                verify_or_trust(&path, "same.example:22", fingerprint)
            }));
        }
        barrier.wait();
        let decisions: Vec<TrustDecision> = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread").expect("decision"))
            .collect();
        assert_eq!(
            decisions
                .iter()
                .filter(|decision| matches!(decision, TrustDecision::FirstSeen))
                .count(),
            1
        );
        assert_eq!(
            decisions
                .iter()
                .filter(|decision| matches!(decision, TrustDecision::Mismatch { .. }))
                .count(),
            1
        );
    }
}
