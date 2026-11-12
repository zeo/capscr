use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_RECORDED_CAPTURES: usize = 2000;

/// where an upload lives on its host, enough to delete it later. credentials
/// are never stored here; a delete takes them from the current config
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteRef {
    Imgur {
        delete_hash: String,
    },
    S3 {
        object_url: String,
        key: String,
    },
    Sftp {
        host: String,
        port: u16,
        username: String,
        remote_path: String,
    },
    /// a host with no delete api (custom http, plugins)
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadEntry {
    pub url: String,
    pub destination: String,
    pub uploaded_unix: u64,
    pub remote: RemoteRef,
}

/// uploads per capture path, persisted as json in the app data dir
#[derive(Debug)]
pub struct UploadRecords {
    file: PathBuf,
    by_capture: HashMap<String, Vec<UploadEntry>>,
}

/// the capture path as list_captures reports it, so records and history rows match
pub fn record_key(path: &Path) -> String {
    let path = path.to_string_lossy();
    path.strip_prefix(r"\\?\").unwrap_or(&path).to_string()
}

impl UploadRecords {
    pub fn load(file: PathBuf) -> Self {
        let by_capture = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(
                    "upload records at {} are unreadable: {error}",
                    file.display()
                );
                if let Err(error) = std::fs::rename(&file, file.with_extension("json.bad")) {
                    tracing::warn!("could not move the bad upload records aside: {error}");
                }
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        Self { file, by_capture }
    }

    pub fn entries(&self, capture: &str) -> &[UploadEntry] {
        self.by_capture.get(capture).map_or(&[], Vec::as_slice)
    }

    pub fn add(&mut self, capture: String, entry: UploadEntry) -> Result<()> {
        let entries = self.by_capture.entry(capture).or_default();
        entries.retain(|e| e.url != entry.url);
        entries.push(entry);
        while self.by_capture.len() > MAX_RECORDED_CAPTURES {
            let oldest = self
                .by_capture
                .iter()
                .min_by_key(|(_, entries)| entries.iter().map(|e| e.uploaded_unix).max())
                .map(|(capture, _)| capture.clone());
            if let Some(oldest) = oldest {
                self.by_capture.remove(&oldest);
            }
        }
        self.save()
    }

    pub fn remove(&mut self, capture: &str, url: &str) -> Result<()> {
        let Some(entries) = self.by_capture.get_mut(capture) else {
            return Ok(());
        };
        entries.retain(|e| e.url != url);
        if entries.is_empty() {
            self.by_capture.remove(capture);
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec(&self.by_capture)?;
        let tmp = self
            .file
            .with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().as_simple()));
        std::fs::write(&tmp, body).map_err(|e| anyhow!("upload records write failed: {e}"))?;
        if let Err(error) = std::fs::rename(&tmp, &self.file) {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow!("upload records rename failed: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, uploaded_unix: u64) -> UploadEntry {
        UploadEntry {
            url: url.into(),
            destination: "imgur".into(),
            uploaded_unix,
            remote: RemoteRef::Imgur {
                delete_hash: "abc123".into(),
            },
        }
    }

    #[test]
    fn records_round_trip_and_dedupe_by_url() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("uploads.json");
        let mut records = UploadRecords::load(file.clone());
        records
            .add("C:/a.png".into(), entry("https://x/1", 1))
            .unwrap();
        records
            .add("C:/a.png".into(), entry("https://x/1", 2))
            .unwrap();
        records
            .add("C:/a.png".into(), entry("https://x/2", 3))
            .unwrap();

        let back = UploadRecords::load(file);
        let entries = back.entries("C:/a.png");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].uploaded_unix, 2);
        assert!(back.entries("C:/b.png").is_empty());
    }

    #[test]
    fn records_drop_the_oldest_capture_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut records = UploadRecords::load(dir.path().join("uploads.json"));
        for i in 0..=MAX_RECORDED_CAPTURES as u64 {
            records
                .by_capture
                .insert(format!("c{i}"), vec![entry("u", i + 10)]);
        }
        records.add("new".into(), entry("u", 99_999)).unwrap();
        assert_eq!(records.by_capture.len(), MAX_RECORDED_CAPTURES);
        assert!(records.entries("c0").is_empty());
        assert!(records.entries("c1").is_empty());
        assert_eq!(records.entries("new").len(), 1);
    }

    #[test]
    fn removing_the_last_upload_drops_the_capture() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("uploads.json");
        let mut records = UploadRecords::load(file.clone());
        records.add("a".into(), entry("https://x/1", 1)).unwrap();
        records.remove("a", "https://x/1").unwrap();
        assert!(UploadRecords::load(file).by_capture.is_empty());
    }

    #[test]
    fn corrupt_records_load_empty_and_move_aside() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("uploads.json");
        std::fs::write(&file, b"{not json").unwrap();
        let records = UploadRecords::load(file.clone());
        assert!(records.by_capture.is_empty());
        assert!(!file.exists());
        assert_eq!(
            std::fs::read(dir.path().join("uploads.json.bad")).unwrap(),
            b"{not json"
        );
    }

    #[test]
    fn remote_refs_round_trip_through_json() {
        for remote in [
            RemoteRef::Imgur {
                delete_hash: "h".into(),
            },
            RemoteRef::S3 {
                object_url: "https://b.s3.us-east-1.amazonaws.com/k.png".into(),
                key: "k.png".into(),
            },
            RemoteRef::Sftp {
                host: "h".into(),
                port: 22,
                username: "u".into(),
                remote_path: "/srv/k.png".into(),
            },
            RemoteRef::Url,
        ] {
            let json = serde_json::to_string(&remote).unwrap();
            assert_eq!(serde_json::from_str::<RemoteRef>(&json).unwrap(), remote);
        }
        assert_eq!(
            serde_json::to_string(&RemoteRef::Url).unwrap(),
            r#"{"kind":"url"}"#
        );
    }

    #[test]
    fn record_key_matches_history_paths() {
        assert_eq!(
            record_key(Path::new(r"\\?\C:\pics\a.png")),
            r"C:\pics\a.png"
        );
        assert_eq!(record_key(Path::new("/home/me/a.png")), "/home/me/a.png");
    }
}
