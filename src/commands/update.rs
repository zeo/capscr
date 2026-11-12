use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    // "in-place" | "deb" | "rpm" | "external"
    pub install_kind: String,
    // for deb/rpm: a direct link to the matching release asset, so the UI
    // can offer "download .deb" instead of a bare error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

// the linux updater can only swap a running AppImage; deb/rpm installs are
// owned by the package manager and windows/macos self-update in place
fn update_install_kind() -> String {
    #[cfg(target_os = "linux")]
    {
        crate::distro::detect().as_str().to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        "in-place".to_string()
    }
}

// look up the release asset URL matching this install's package format.
// tries the github release API first (exact filenames), then falls back to
// the predictable asset name, then None (the UI drops back to the releases
// page)
#[cfg(target_os = "linux")]
async fn matching_asset_url(version: &str, kind: &str) -> Option<String> {
    let suffix = match kind {
        "deb" => ".deb",
        "rpm" => ".rpm",
        _ => return None,
    };
    let tag = format!("v{version}");
    let api = format!("https://api.github.com/repos/zeo/capscr/releases/tags/{tag}");
    let found = async {
        let client = reqwest::Client::builder()
            .user_agent("capscr-updater")
            .build()
            .ok()?;
        let text = client.get(&api).send().await.ok()?.text().await.ok()?;
        let body: serde_json::Value = serde_json::from_str(&text).ok()?;
        body.get("assets")?
            .as_array()?
            .iter()
            .filter_map(|a| a.get("browser_download_url")?.as_str())
            .find(|url| url.ends_with(suffix))
            .map(str::to_string)
    }
    .await;
    found.or_else(|| {
        // predictable names from the bundler (README install table)
        let name = match kind {
            "deb" => format!("capscr_{version}_amd64.deb"),
            "rpm" => format!("capscr-{version}-1.x86_64.rpm"),
            _ => return None,
        };
        Some(format!(
            "https://github.com/zeo/capscr/releases/download/{tag}/{name}"
        ))
    })
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    #[derive(serde::Deserialize)]
    struct InstallerRelease {
        version: String,
        #[serde(default)]
        notes: String,
    }
    let release = tokio::task::spawn_blocking(|| {
        let installer = shared_installer().ok_or("shared updater is not installed")?;
        let mut status = std::process::Command::new(&installer)
            .args(["status", "capscr", "--json"])
            .output()
            .map_err(|error| format!("read installer status: {error}"))?;
        let mut receipts: Vec<serde_json::Value> = serde_json::from_slice(&status.stdout)
            .map_err(|error| format!("read installer status: {error}"))?;
        if status.status.success() && receipts.is_empty() && installer_is_bundled(&installer) {
            adopt_bundled_installer(&installer, "capscr", env!("CARGO_PKG_VERSION"), false)?;
            status = std::process::Command::new(&installer)
                .args(["status", "capscr", "--json"])
                .output()
                .map_err(|error| format!("read installer status: {error}"))?;
            receipts = serde_json::from_slice(&status.stdout)
                .map_err(|error| format!("read installer status: {error}"))?;
        }
        if !status.status.success() || receipts.is_empty() {
            return Err("capscr is still owned by its legacy or system package".into());
        }
        let output = std::process::Command::new(installer)
            .args(["check", "capscr", "--json"])
            .output()
            .map_err(|error| format!("start updater: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        serde_json::from_slice::<InstallerRelease>(&output.stdout)
            .map_err(|error| format!("read updater response: {error}"))
    })
    .await
    .map_err(|error| error.to_string())??;
    let current = app.package_info().version.to_string();
    if semver::Version::parse(&release.version).map_err(|error| error.to_string())?
        <= semver::Version::parse(&current).map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let kind = update_install_kind();
    #[cfg(target_os = "linux")]
    let download_url = matching_asset_url(&release.version, &kind).await;
    #[cfg(not(target_os = "linux"))]
    let download_url = None;
    Ok(Some(UpdateInfo {
        version: release.version,
        current_version: current,
        notes: (!release.notes.is_empty()).then_some(release.notes),
        install_kind: kind,
        download_url,
    }))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    if update_install_kind() != "in-place" {
        return Err("this install updates through your package manager; \
                    download the new release instead"
            .into());
    }
    // claim the capture gate before launching the updater so a hotkey cannot
    // start new work between the idle check and process exit
    let state = app.state::<AppState>();
    let mut waited = 0u32;
    let _capture_gate = loop {
        match state.capture_in_progress.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => break CaptureGate(&state.capture_in_progress),
            Err(_) if waited < 50 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                waited += 1;
            }
            Err(_) => {
                let recording = *state.recording_state.lock().unwrap();
                let reason = update_blocker(true, recording, 0)
                    .expect("a busy capture always blocks an update");
                return Err(reason.to_string());
            }
        }
    };
    let shutdown_guard = MutationShutdownGuard::claim(&app).map_err(|error| error.to_string())?;
    require_editor_closed(&app, "installing an update")?;
    let mut pending_mutations = state
        .pending_file_mutations
        .load(std::sync::atomic::Ordering::SeqCst);
    let mut waited_for_mutations = 0u32;
    while pending_mutations > 0 && waited_for_mutations < 300 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited_for_mutations += 1;
        pending_mutations = state
            .pending_file_mutations
            .load(std::sync::atomic::Ordering::SeqCst);
    }
    let recording = *state.recording_state.lock().unwrap();
    if let Some(reason) = update_blocker(false, recording, pending_mutations) {
        return Err(reason.to_string());
    }
    let installer =
        shared_installer().ok_or_else(|| "shared updater is not installed".to_string())?;
    std::process::Command::new(installer)
        .args([
            "update",
            "capscr",
            "--wait-pid",
            &std::process::id().to_string(),
        ])
        .spawn()
        .map_err(|error| format!("start updater: {error}"))?;
    app.exit(0);
    shutdown_guard.commit();
    Ok(())
}

fn update_blocker(
    capture_busy: bool,
    recording: crate::recording::RecordingState,
    pending_mutations: usize,
) -> Option<&'static str> {
    if capture_busy {
        return Some("capture is still busy; try the update again when it finishes");
    }
    match recording {
        crate::recording::RecordingState::Idle if pending_mutations == 0 => None,
        crate::recording::RecordingState::Idle => {
            Some("files are still being updated; try the update again when they finish")
        }
        crate::recording::RecordingState::Recording => {
            Some("stop the active recording before installing an update")
        }
        crate::recording::RecordingState::Processing => {
            Some("wait for the recording to finish saving before installing an update")
        }
    }
}

fn shared_installer() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "rot-installer.exe"
    } else {
        "rot-installer"
    };
    let mut candidates = Vec::new();
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            candidates.push(parent.join(name));
            candidates.push(parent.join("installer").join(name));
        }
    }
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("ProgramFiles") {
            candidates.push(
                std::path::PathBuf::from(root)
                    .join("rot")
                    .join("installer")
                    .join(name),
            );
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                std::path::PathBuf::from(root)
                    .join("Programs")
                    .join("rot-installer")
                    .join(name),
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        candidates.push(std::path::PathBuf::from("/opt/rot/installer").join(name));
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            candidates.push(
                std::path::PathBuf::from(data)
                    .join("rot/apps/installer")
                    .join(name),
            );
        } else if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                std::path::PathBuf::from(home)
                    .join(".local/share/rot/apps/installer")
                    .join(name),
            );
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn installer_is_bundled(installer: &std::path::Path) -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .is_some_and(|parent| installer.parent() == Some(parent.as_path()))
}

fn adopt_bundled_installer(
    installer: &std::path::Path,
    app: &str,
    version: &str,
    machine_only: bool,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let root = executable
        .parent()
        .ok_or_else(|| "application path has no parent".to_string())?;
    let machine = machine_only
        || std::env::var_os("ProgramFiles")
            .is_some_and(|program_files| executable.starts_with(program_files));
    let scope = if machine { "machine" } else { "user" };
    let status = std::process::Command::new(installer)
        .args(["adopt", app, "--scope", scope, "--root"])
        .arg(root)
        .args(["--version", version])
        .status()
        .map_err(|error| format!("start installer migration: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("installer migration exited with {status}"))
}

#[cfg(test)]
mod update_tests {
    #[cfg(any(windows, target_os = "linux"))]
    use super::ffmpeg_redirect_allowed;
    use super::update_blocker;
    use crate::recording::RecordingState;

    #[test]
    fn update_requires_capture_and_recording_to_be_idle() {
        assert_eq!(update_blocker(false, RecordingState::Idle, 0), None);
        assert!(update_blocker(true, RecordingState::Idle, 0).is_some());
        assert!(update_blocker(false, RecordingState::Recording, 0).is_some());
        assert!(update_blocker(false, RecordingState::Processing, 0).is_some());
        assert!(update_blocker(false, RecordingState::Idle, 1).is_some());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn ffmpeg_redirects_stay_on_https() {
        let https = url::Url::parse("https://cdn.example/ffmpeg.zip").expect("https url");
        let http = url::Url::parse("http://cdn.example/ffmpeg.zip").expect("http url");
        assert!(ffmpeg_redirect_allowed(&https, 4));
        assert!(!ffmpeg_redirect_allowed(&http, 0));
        assert!(!ffmpeg_redirect_allowed(&https, 5));
    }
}
