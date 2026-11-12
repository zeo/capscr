use super::*;

fn build_s3_service(config: &Config) -> UploadService {
    UploadService::S3(s3_target(config))
}

fn s3_target(config: &Config) -> crate::upload::S3Target {
    crate::upload::S3Target {
        bucket: config.upload.s3.bucket.clone(),
        region: config.upload.s3.region.clone(),
        endpoint: config.upload.s3.endpoint.clone(),
        access_key_id: config.upload.s3.access_key_id.clone(),
        secret_access_key: config.upload.s3.secret_access_key_plaintext(),
        public_url_template: config.upload.s3.public_url_template.clone(),
    }
}

fn build_sftp_service(config: &Config) -> UploadService {
    UploadService::Sftp(sftp_target(config))
}

fn sftp_target(config: &Config) -> crate::upload::SftpTarget {
    crate::upload::SftpTarget {
        host: config.upload.sftp.host.clone(),
        port: config.upload.sftp.port,
        username: config.upload.sftp.username.clone(),
        password: config.upload.sftp.password_plaintext(),
        remote_dir: config.upload.sftp.remote_dir.clone(),
        public_url_template: config.upload.sftp.public_url_template.clone(),
        private_key_path: config.upload.sftp.private_key_path.clone(),
        private_key_passphrase: config.upload.sftp.private_key_passphrase_plaintext(),
    }
}

fn build_imgur_service(config: &Config) -> UploadService {
    let cid = config.upload.imgur_client_id.trim();
    if cid.is_empty() || cid == crate::upload::BUILTIN_IMGUR_CLIENT_ID {
        UploadService::Imgur
    } else {
        UploadService::ImgurWithClientId(cid.to_string())
    }
}

// build an upload service, optionally overriding the global destination with a
// per-task target. `target_override = None` uses the global config destination.
pub(crate) fn build_upload_service(config: &Config) -> UploadService {
    build_upload_service_for_target(config, None)
}

pub(crate) fn build_upload_service_for_target(
    config: &Config,
    target_override: Option<crate::config::TaskUploadTarget>,
) -> UploadService {
    use crate::config::TaskUploadTarget;
    match target_override {
        None => match config.upload.destination {
            UploadDestination::Imgur => build_imgur_service(config),
            UploadDestination::Custom => UploadService::Custom(CustomUploader {
                name: "Custom".to_string(),
                request_url: config.upload.custom_url.clone(),
                file_form_name: config.upload.custom_form_name.clone(),
                response_url_path: config.upload.custom_response_path.clone(),
            }),
            UploadDestination::Sftp => build_sftp_service(config),
            UploadDestination::S3 => build_s3_service(config),
            UploadDestination::Plugin => UploadService::Plugin(config.upload.plugin.clone()),
        },
        Some(TaskUploadTarget::Imgur) => build_imgur_service(config),
        Some(TaskUploadTarget::Custom) => UploadService::Custom(CustomUploader {
            name: "Custom".to_string(),
            request_url: config.upload.custom_url.clone(),
            file_form_name: config.upload.custom_form_name.clone(),
            response_url_path: config.upload.custom_response_path.clone(),
        }),
        Some(TaskUploadTarget::Sftp) => build_sftp_service(config),
        Some(TaskUploadTarget::S3) => build_s3_service(config),
    }
}

#[tauri::command]
pub async fn reupload_capture(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UploadResponse, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    upload_path(app, config, canonical).await
}

fn read_upload_file(path: &std::path::Path) -> Result<(Vec<u8>, &'static str, String), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let mime = match crate::upload::mime_for(ext) {
        Some(mime) => mime,
        None if ext.is_empty() => return Err("file has no extension; cannot detect type".into()),
        None => return Err(format!("unsupported file type: .{}", ext.to_lowercase())),
    };
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > 100 * 1024 * 1024 {
        return Err("file too large to upload (>100 MB)".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload")
        .to_string();
    Ok((bytes, mime, file_name))
}

// uploads the file bytes as they are, so gif animation and video survive
pub(crate) async fn upload_path(
    app: AppHandle,
    config: Config,
    path: PathBuf,
) -> Result<UploadResponse, String> {
    let service = build_upload_service(&config);
    let destination = service.label();
    let pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    let source = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _pending_mutation = pending_mutation;
        let (bytes, mime, file_name) = read_upload_file(&source)?;
        crate::upload::shared_uploader()
            .and_then(|uploader| uploader.upload_raw(&bytes, mime, &file_name, &service))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    finish_upload(&app, &config, &result, destination, Some(&path));
    Ok(UploadResponse {
        url: result.url,
        delete_url: result.delete_url,
    })
}

pub(crate) fn finish_upload(
    app: &AppHandle,
    config: &Config,
    result: &crate::upload::UploadResult,
    destination: &str,
    path: Option<&std::path::Path>,
) {
    app.state::<AppState>().record_upload(UploadRecord {
        url: result.url.clone(),
    });
    if let Some(path) = path {
        let entry = crate::upload::records::UploadEntry {
            url: result.url.clone(),
            destination: destination.to_string(),
            uploaded_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            remote: result.remote.clone(),
        };
        let key = crate::upload::records::record_key(path);
        if let Some(Err(error)) = app
            .state::<AppState>()
            .with_upload_records(|records| records.add(key, entry))
        {
            tracing::warn!("upload record not saved: {error}");
        }
    }
    crate::rebuild_tray_menu(app);
    if config.upload.copy_url_to_clipboard {
        let _ = crate::upload::copy_url_to_clipboard(&result.url);
    }
    emit_upload_success(
        app,
        crate::plugin::UploadInfo {
            url: result.url.clone(),
            path: path.map(std::path::Path::to_path_buf),
            destination: destination.to_string(),
            delete_url: result.delete_url.clone(),
        },
    );
}

/// delete one recorded upload from its host after a native confirm. Ok(false)
/// when the user keeps it
#[tauri::command]
pub async fn delete_remote_upload(
    path: String,
    url: String,
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<bool, String> {
    use crate::upload::records::RemoteRef;
    let key = crate::upload::records::record_key(std::path::Path::new(&path));
    let entry = app
        .state::<AppState>()
        .with_upload_records(|records| {
            records
                .entries(&key)
                .iter()
                .find(|entry| entry.url == url)
                .cloned()
        })
        .flatten()
        .ok_or("this capture has no record of that upload")?;
    if entry.remote == RemoteRef::Url {
        return Err(format!(
            "{} has no delete api; remove the upload there by hand",
            entry.destination
        ));
    }
    let config = app.state::<AppState>().config.lock().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        let host = url::Url::parse(&entry.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| entry.destination.clone());
        let confirmed = app
            .dialog()
            .message(format!(
                "Delete {} from {host}? This can't be undone.",
                entry.url
            ))
            .title("delete from host")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "delete".into(),
                "keep".into(),
            ))
            .parent(&window)
            .blocking_show();
        if !confirmed {
            return Ok(false);
        }
        match &entry.remote {
            RemoteRef::Imgur { delete_hash } => {
                let client_id = match config.upload.imgur_client_id.trim() {
                    "" => crate::upload::BUILTIN_IMGUR_CLIENT_ID,
                    id => id,
                };
                crate::upload::shared_uploader()
                    .and_then(|uploader| uploader.delete_imgur(delete_hash, client_id))
            }
            RemoteRef::S3 { .. } => crate::upload::delete_s3(&s3_target(&config), &entry.remote),
            RemoteRef::Sftp { .. } => {
                crate::upload::delete_sftp(&sftp_target(&config), &entry.remote)
            }
            RemoteRef::Url => Err(anyhow::anyhow!("this host has no delete api")),
        }
        .map_err(|e| e.to_string())?;
        if let Some(Err(error)) = app
            .state::<AppState>()
            .with_upload_records(|records| records.remove(&key, &entry.url))
        {
            tracing::warn!("upload record not updated after delete: {error}");
        }
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResponse {
    pub url: String,
    pub delete_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SftpKnownHost {
    pub host_port: String,
    pub fingerprint: String,
    pub first_seen_unix: u64,
}

#[tauri::command]
pub fn sftp_known_hosts() -> Result<Vec<SftpKnownHost>, String> {
    let path = crate::upload::known_hosts::KnownHosts::default_path()
        .ok_or_else(|| "config dir unresolvable".to_string())?;
    let kh = crate::upload::known_hosts::load_locked(&path).map_err(|e| e.to_string())?;
    let mut out: Vec<SftpKnownHost> = kh
        .hosts
        .into_iter()
        .map(|(host_port, entry)| SftpKnownHost {
            host_port,
            fingerprint: entry.fingerprint,
            first_seen_unix: entry.first_seen_unix,
        })
        .collect();
    out.sort_by(|a, b| a.host_port.cmp(&b.host_port));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTestReport {
    pub destination: String,
    pub overall_ok: bool,
    pub steps: Vec<crate::upload::TestStep>,
}

fn connection_test_config(
    destination: &str,
    mut requested: Config,
    saved: &Config,
) -> Result<Config, String> {
    requested.upload.sftp.password_encrypted.clear();
    requested
        .upload
        .sftp
        .private_key_passphrase_encrypted
        .clear();
    requested.upload.s3.secret_access_key_encrypted.clear();

    match destination {
        "Sftp" | "sftp" => {
            let same_identity = sftp_identity_matches(&requested, saved);
            if same_identity {
                if requested.upload.sftp.password.is_empty() {
                    requested.upload.sftp.password_encrypted =
                        saved.upload.sftp.password_encrypted.clone();
                }
                if requested.upload.sftp.private_key_passphrase.is_empty() {
                    requested
                        .upload
                        .sftp
                        .private_key_passphrase_encrypted = saved
                        .upload
                        .sftp
                        .private_key_passphrase_encrypted
                        .clone();
                }
            }
            if requested.upload.sftp.private_key_path.is_empty()
                && requested.upload.sftp.password.is_empty()
                && requested.upload.sftp.password_encrypted.is_empty()
            {
                return Err(
                    "enter an SFTP password or choose a private key before testing".to_string(),
                );
            }
        }
        "S3" | "s3" => {
            let same_identity = s3_identity_matches(&requested, saved);
            if requested.upload.s3.secret_access_key.is_empty() && same_identity {
                requested.upload.s3.secret_access_key_encrypted =
                    saved.upload.s3.secret_access_key_encrypted.clone();
            }
            if requested.upload.s3.secret_access_key.is_empty()
                && requested.upload.s3.secret_access_key_encrypted.is_empty()
            {
                return Err(
                    "enter the S3 secret access key before testing a different connection"
                        .to_string(),
                );
            }
        }
        _ => {}
    }

    Ok(requested)
}

#[tauri::command]
pub async fn test_upload_connection(
    destination: String,
    config: Config,
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> Result<ConnectionTestReport, String> {
    let mut cfg = {
        let saved = state.config.lock().unwrap();
        connection_test_config(&destination, config, &saved)?
    };
    cfg.upload.destination = match destination.as_str() {
        "Sftp" | "sftp" => UploadDestination::Sftp,
        "Imgur" | "imgur" => UploadDestination::Imgur,
        "Custom" | "custom" => UploadDestination::Custom,
        "S3" | "s3" => UploadDestination::S3,
        other => return Err(format!("'{other}' has no test-connection probe")),
    };
    let pending_sftp_trust = if cfg.upload.destination == UploadDestination::Sftp {
        Some(PendingFileMutation::begin(&app).map_err(|error| error.to_string())?)
    } else {
        None
    };
    let service = build_upload_service(&cfg);
    let steps = tauri::async_runtime::spawn_blocking(move || {
        let _pending_sftp_trust = pending_sftp_trust;
        match &service {
            UploadService::Sftp(target) => {
                crate::upload::test_connection_sftp(target, &|host_port, fingerprint| {
                    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
                    app.dialog()
                        .message(format!(
                            "{host_port} presented the host key\n\n{fingerprint}\n\nCompare it \
                             with the server's own fingerprint (ssh-keygen -lf on its host key) \
                             before trusting it. Uploads only connect to trusted keys."
                        ))
                        .title("trust this sftp server?")
                        .kind(MessageDialogKind::Warning)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "trust key".into(),
                            "cancel".into(),
                        ))
                        .parent(&window)
                        .blocking_show()
                })
            }
            UploadService::Imgur | UploadService::ImgurWithClientId(_) => {
                crate::upload::test_connection_imgur(&cfg.upload.imgur_client_id)
            }
            UploadService::Custom(uploader) => crate::upload::test_connection_custom(uploader),
            UploadService::S3(target) => crate::upload::test_connection_s3(target),
            UploadService::Plugin(_) => Err(anyhow::anyhow!(
                "plugin destinations have no connection test"
            )),
        }
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let overall_ok = !steps.is_empty() && steps.iter().all(|s| s.ok);
    Ok(ConnectionTestReport {
        destination,
        overall_ok,
        steps,
    })
}

#[cfg(test)]
mod read_upload_file_tests {
    use super::read_upload_file;

    #[test]
    fn every_capture_kind_reads_with_its_mime() {
        let dir = tempfile::tempdir().unwrap();
        for (name, mime) in [
            ("a.avif", "image/avif"),
            ("b.JXL", "image/jxl"),
            ("c.mp4", "video/mp4"),
            ("d.gif", "image/gif"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"bytes").unwrap();
            let (bytes, got, file_name) = read_upload_file(&path).unwrap();
            assert_eq!(
                (bytes.as_slice(), got, file_name.as_str()),
                (&b"bytes"[..], mime, name)
            );
        }
    }

    #[test]
    fn unknown_or_missing_extensions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["notes.txt", "noext"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            assert!(read_upload_file(&path).is_err(), "{name}");
        }
    }
}

#[cfg(test)]
mod connection_test_config_tests {
    use super::*;

    fn saved_config() -> Config {
        let mut config = Config::default();
        config.upload.sftp.host = "sftp.example.com".into();
        config.upload.sftp.port = 2222;
        config.upload.sftp.username = "sftp-user".into();
        config.upload.sftp.private_key_path = "C:\\keys\\capture".into();
        config.upload.sftp.password_encrypted = "keyring:saved-sftp".into();
        config.upload.sftp.private_key_passphrase_encrypted = "keyring:saved-key".into();

        config.upload.s3.bucket = "captures".into();
        config.upload.s3.region = "eu-central-1".into();
        config.upload.s3.endpoint = "https://s3.example.com".into();
        config.upload.s3.access_key_id = "saved-access-key".into();
        config.upload.s3.secret_access_key_encrypted = "keyring:saved-s3".into();
        config
    }

    #[test]
    fn probes_take_references_from_backend_state_for_saved_identities() {
        let saved = saved_config();
        let mut requested = saved.clone();
        requested.upload.sftp.password_encrypted = "keyring:client-sftp".into();
        requested.upload.sftp.private_key_passphrase_encrypted = "keyring:client-key".into();
        requested.upload.s3.secret_access_key_encrypted = "keyring:client-s3".into();

        let sftp = connection_test_config("Sftp", requested.clone(), &saved).unwrap();
        assert_eq!(
            sftp.upload.sftp.password_encrypted,
            "keyring:saved-sftp"
        );
        assert_eq!(
            sftp.upload.sftp.private_key_passphrase_encrypted,
            "keyring:saved-key"
        );
        assert!(sftp.upload.s3.secret_access_key_encrypted.is_empty());

        let s3 = connection_test_config("S3", requested, &saved).unwrap();
        assert_eq!(
            s3.upload.s3.secret_access_key_encrypted,
            "keyring:saved-s3"
        );
    }

    #[test]
    fn settings_restore_backend_references_for_saved_identities() {
        let saved = saved_config();
        let mut requested = saved.clone();
        requested.upload.sftp.password_encrypted = "keyring:client-sftp".into();
        requested.upload.sftp.private_key_passphrase_encrypted = "keyring:client-key".into();
        requested.upload.s3.secret_access_key_encrypted = "keyring:client-s3".into();

        let _retired = bind_backend_secret_references(&mut requested, &saved);

        assert_eq!(
            requested.upload.sftp.password_encrypted,
            "keyring:saved-sftp"
        );
        assert_eq!(
            requested
                .upload
                .sftp
                .private_key_passphrase_encrypted,
            "keyring:saved-key"
        );
        assert_eq!(
            requested.upload.s3.secret_access_key_encrypted,
            "keyring:saved-s3"
        );
    }

    #[test]
    fn settings_do_not_rebind_secrets_after_identity_changes() {
        let saved = saved_config();
        let mut requested = saved.clone();
        requested.upload.sftp.username = "other-sftp-user".into();
        requested.upload.s3.access_key_id = "other-access-key".into();
        requested.upload.sftp.password_encrypted = "keyring:client-sftp".into();
        requested.upload.sftp.private_key_passphrase_encrypted = "keyring:client-key".into();
        requested.upload.s3.secret_access_key_encrypted = "keyring:client-s3".into();

        let _retired = bind_backend_secret_references(&mut requested, &saved);

        assert!(requested.upload.sftp.password_encrypted.is_empty());
        assert!(
            requested
                .upload
                .sftp
                .private_key_passphrase_encrypted
                .is_empty()
        );
        assert!(requested.upload.s3.secret_access_key_encrypted.is_empty());
    }

    #[test]
    fn probes_reject_stored_references_for_changed_identities() {
        let saved = saved_config();

        let mut sftp = saved.clone();
        sftp.upload.sftp.private_key_path = "C:\\keys\\other".into();
        let sftp = connection_test_config("Sftp", sftp, &saved).unwrap();
        assert!(sftp.upload.sftp.password_encrypted.is_empty());
        assert!(
            sftp.upload
                .sftp
                .private_key_passphrase_encrypted
                .is_empty()
        );

        let mut s3 = saved.clone();
        s3.upload.s3.bucket = "other-bucket".into();
        s3.upload.s3.secret_access_key_encrypted = "keyring:saved-s3".into();
        assert!(connection_test_config("S3", s3.clone(), &saved).is_err());
        s3.upload.s3.secret_access_key = "fresh secret".into();
        let s3 = connection_test_config("S3", s3, &saved).unwrap();
        assert!(s3.upload.s3.secret_access_key_encrypted.is_empty());
    }
}

#[tauri::command]
pub fn sftp_forget_host(host_port: String, app: AppHandle) -> Result<bool, String> {
    let path = crate::upload::known_hosts::KnownHosts::default_path()
        .ok_or_else(|| "config dir unresolvable".to_string())?;
    let _pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    crate::upload::known_hosts::forget_locked(&path, &host_port).map_err(|e| e.to_string())
}
