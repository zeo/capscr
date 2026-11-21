use super::*;

pub fn get_config(state: &AppState) -> Result<Config, String> {
    Ok(state.config.lock().unwrap().clone())
}

pub fn get_default_config() -> Config {
    Config::default()
}

pub(crate) fn sftp_identity_matches(requested: &Config, saved: &Config) -> bool {
    requested.upload.sftp.host == saved.upload.sftp.host
        && requested.upload.sftp.port == saved.upload.sftp.port
        && requested.upload.sftp.username == saved.upload.sftp.username
        && requested.upload.sftp.private_key_path == saved.upload.sftp.private_key_path
}

pub(crate) fn s3_identity_matches(requested: &Config, saved: &Config) -> bool {
    requested.upload.s3.bucket == saved.upload.s3.bucket
        && requested.upload.s3.region == saved.upload.s3.region
        && requested.upload.s3.endpoint == saved.upload.s3.endpoint
        && requested.upload.s3.access_key_id == saved.upload.s3.access_key_id
}

pub(crate) fn bind_backend_secret_references(
    config: &mut Config,
    stored: &Config,
) -> crate::secret::SecretTransaction {
    let mut retired = crate::secret::SecretTransaction::default();
    config.upload.sftp.password_encrypted.clear();
    config
        .upload
        .sftp
        .private_key_passphrase_encrypted
        .clear();
    config.upload.s3.secret_access_key_encrypted.clear();

    if sftp_identity_matches(config, stored) {
        config.upload.sftp.password_encrypted = stored.upload.sftp.password_encrypted.clone();
        config.upload.sftp.private_key_passphrase_encrypted = stored
            .upload
            .sftp
            .private_key_passphrase_encrypted
            .clone();
    } else {
        retired.retire(&stored.upload.sftp.password_encrypted);
        retired.retire(&stored.upload.sftp.private_key_passphrase_encrypted);
    }
    if s3_identity_matches(config, stored) {
        config.upload.s3.secret_access_key_encrypted =
            stored.upload.s3.secret_access_key_encrypted.clone();
    } else {
        retired.retire(&stored.upload.s3.secret_access_key_encrypted);
    }
    retired
}

/// cheap-ish probe: reads PNG chunks until the `cICP` chunk or `IDAT`. The
/// editor calls this on load to know whether to warn the user that edits
/// will flatten HDR to SDR. Returns false on any read error.
pub fn is_hdr_capture(path: String) -> bool {
    let path = std::path::PathBuf::from(path);
    crate::capture::read_cicp(&path)
        .map(|info| info.is_hdr())
        .unwrap_or(false)
}

pub fn set_config(mut config: Config, app: AppHandle, state: &AppState) -> Result<Config, String> {
    {
        let mut stored = state.config.lock().unwrap();
        // encrypted references are backend-owned and never accepted from ipc
        let retired_secrets = bind_backend_secret_references(&mut config, &stored);
        // the global hotkey kill switch lives in the atomic (the tray and Settings
        // toggle it there); make the persisted config agree with it so this save
        // can't quietly clear the switch and re-enable every hotkey on next launch
        config.hotkeys.disabled_globally = state
            .hotkeys_disabled
            .load(std::sync::atomic::Ordering::SeqCst);
        pin_backend_owned(&mut config, &stored);
        config.validate().map_err(|e| e.to_string())?;
        let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
        let autostart = crate::autostart();
        let previous_autostart = autostart.is_enabled()?;
        apply_autostart_transaction(
            previous_autostart,
            config.ui.auto_start,
            |enabled| {
                if enabled {
                    autostart.enable()
                } else {
                    autostart.disable()
                }
            },
            || {
                config
                    .save_and_migrate_secrets()
                    .map_err(|e| e.to_string())?;
                retired_secrets.commit();
                Ok(())
            },
        )?;
        *stored = config.clone();
    }
    crate::install_hdr_runtime_from_config(&config);
    // respect the tray's Disable-hotkeys toggle: when off, reload with an
    // empty task list so the new config doesn't silently re-register hotkeys
    use std::sync::atomic::Ordering;
    // apply the advanced-input toggle before the reload flush reads it; an
    // explicit enable also starts the evdev readers on demand
    #[cfg(target_os = "linux")]
    if let Some(advanced) = config.hotkeys.advanced_input {
        crate::hotkeys::set_advanced_input(advanced);
        if advanced {
            crate::hotkeys::evdev_linux::start(app.clone());
        }
    }
    let tasks_to_register = if state.hotkeys_disabled.load(Ordering::SeqCst) {
        Vec::new()
    } else {
        config.capture_tasks.clone()
    };
    state.send_hotkey_reload(tasks_to_register);
    #[cfg(windows)]
    crate::jumplist::refresh(config.capture_tasks.clone());
    allow_capture_dirs(&app, &config.output.directory);
    crate::rebuild_tray_menu(&app);
    app.emit("capscr://config-updated", ());
    Ok(config)
}

pub(crate) fn pin_backend_owned(config: &mut Config, stored: &Config) {
    // window growth is recorded by the backend after the hub loaded its copy,
    // so a stale settings draft must not undo it and regrow resized windows
    config.ui.grown_windows = stored.ui.grown_windows.clone();
    // the output folder gates delete, upload and open, so it only changes
    // through pick_output_dir or a confirmed set_output_dir
    config.output.directory = stored.output.directory.clone();
}

// lets pages load files from the output folder and the history folder
pub fn allow_capture_dirs(app: &AppHandle, output_dir: &std::path::Path) {
    for dir in [Some(output_dir.to_path_buf()), history_dir()]
        .into_iter()
        .flatten()
    {
        if let Err(e) = app.files().allow(&dir) {
            tracing::warn!("file scope allow({dir:?}) failed: {e}");
        }
    }
}

fn apply_output_dir(app: &AppHandle, directory: PathBuf) -> Result<Config, String> {
    let state = app_state();
    let saved = {
        let mut stored = state.config.lock().unwrap();
        let mut config = stored.clone();
        config.output.directory = directory;
        config.validate().map_err(|e| e.to_string())?;
        config.ensure_output_dir().map_err(|e| e.to_string())?;
        let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
        config
            .save_and_migrate_secrets()
            .map_err(|e| e.to_string())?;
        *stored = config.clone();
        config
    };
    allow_capture_dirs(app, &saved.output.directory);
    crate::rebuild_tray_menu(app);
    app.emit("capscr://config-updated", ());
    Ok(saved)
}

pub async fn pick_output_dir(app: AppHandle, window: String) -> Result<Option<Config>, String> {
    tokio::task::spawn_blocking(move || {
        let current = app_state().config.lock().unwrap().output.directory.clone();
        // the user's own pick in the native picker is the consent
        let picked = ceri::FileDialog {
            title: Some("Pick output directory".to_string()),
            default_path: Some(current),
            owner: window_handle(&app, &window),
            ..ceri::FileDialog::default()
        }
        .open_folders()?;
        let Some(directory) = picked.and_then(|folders| folders.into_iter().next()) else {
            return Ok(None);
        };
        apply_output_dir(&app, directory).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub async fn set_output_dir(
    path: String,
    app: AppHandle,
    window: String,
) -> Result<Option<Config>, String> {
    let directory = PathBuf::from(path.trim());
    if !directory.is_absolute() {
        return Err("the capture folder must be an absolute path".into());
    }
    let current = app_state().config.lock().unwrap().output.directory.clone();
    let same = directory == current
        || matches!(
            (std::fs::canonicalize(&directory), std::fs::canonicalize(&current)),
            (Ok(a), Ok(b)) if a == b
        );
    if same {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let confirmed = confirm(
            &app,
            &window,
            "change capture folder",
            format!(
                "capscr will save, list, upload and delete captures in {}.",
                directory.display()
            ),
            ("use this folder", "keep current"),
        );
        if !confirmed {
            return Ok(None);
        }
        apply_output_dir(&app, directory).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn set_capture_tasks(
    capture_tasks: Vec<CaptureTask>,
    app: AppHandle,
    state: &AppState,
) -> Result<Vec<CaptureTask>, String> {
    let saved_tasks = {
        let mut stored = state.config.lock().unwrap();
        let mut config = stored.clone();
        config.capture_tasks = capture_tasks;
        let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
        config.validate().map_err(|e| e.to_string())?;
        config
            .save_and_migrate_secrets()
            .map_err(|e| e.to_string())?;
        let saved_tasks = config.capture_tasks.clone();
        *stored = config;
        saved_tasks
    };
    let tasks_to_register = if state
        .hotkeys_disabled
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        Vec::new()
    } else {
        saved_tasks.clone()
    };
    state.send_hotkey_reload(tasks_to_register);
    #[cfg(windows)]
    crate::jumplist::refresh(saved_tasks.clone());
    crate::rebuild_tray_menu(&app);
    app.emit("capscr://config-updated", ());
    Ok(saved_tasks)
}

fn apply_autostart_transaction(
    previous: bool,
    desired: bool,
    mut set_enabled: impl FnMut(bool) -> Result<(), String>,
    persist: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let changed = previous != desired;
    if changed {
        set_enabled(desired)
            .map_err(|error| format!("couldn't update system autostart: {error}"))?;
    }
    if let Err(error) = persist() {
        if changed {
            if let Err(rollback_error) = set_enabled(previous) {
                return Err(format!(
                    "couldn't save settings: {error}; restoring system autostart failed: \
                     {rollback_error}"
                ));
            }
        }
        return Err(format!("couldn't save settings: {error}"));
    }
    Ok(())
}

pub fn set_autostart(enabled: bool, state: &AppState) -> Result<(), String> {
    let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
    let manager = crate::autostart();
    let previous = manager.is_enabled()?;
    apply_autostart_transaction(
        previous,
        enabled,
        |target| {
            if target {
                manager.enable()
            } else {
                manager.disable()
            }
        },
        || {
            let mut stored = state.config.lock().unwrap();
            let mut config = stored.clone();
            config.ui.auto_start = enabled;
            config
                .save_and_migrate_secrets()
                .map_err(|e| e.to_string())?;
            *stored = config;
            Ok(())
        },
    )
}

pub fn get_autostart() -> Result<bool, String> {
    crate::autostart().is_enabled()
}

#[cfg(test)]
mod autostart_transaction_tests {
    use super::apply_autostart_transaction;
    use std::cell::RefCell;

    #[test]
    fn failed_persistence_restores_the_previous_system_state() {
        let changes = RefCell::new(Vec::new());
        let error = apply_autostart_transaction(
            false,
            true,
            |enabled| {
                changes.borrow_mut().push(enabled);
                Ok(())
            },
            || Err("disk full".to_string()),
        )
        .expect_err("persistence should fail");

        assert_eq!(*changes.borrow(), vec![true, false]);
        assert!(error.contains("disk full"));
    }
}
