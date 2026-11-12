use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyStatusEntry {
    pub task_id: String,
    pub status: String, // "live" or "failed"
    pub reason: Option<String>,
    // the desktop's own description of what actually triggers this shortcut
    // (portal backend only) — it may differ from the configured chord when
    // the compositor remapped it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_trigger: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyDiagnostics {
    pub disabled_globally: bool,
    // which mechanism owns keyboard hotkeys right now:
    // "ll_hook" (windows), "x11", "portal", "evdev", or "none"
    pub backend: String,
    pub statuses: Vec<HotkeyStatusEntry>,
    #[cfg(windows)]
    pub hook: HookTelemetrySnapshot,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize)]
pub struct HookTelemetrySnapshot {
    pub installed: bool,
    pub enabled: bool,
    pub registered_count: usize,
    pub registered: Vec<HookBindingSnapshot>,
    pub calls_total: u64,
    pub keydown_calls: u64,
    pub matched_calls: u64,
    pub dispatch_sent: u64,
    pub dispatch_dropped: u64,
    pub last_vk: u32,
    pub last_mods: u8,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize)]
pub struct HookBindingSnapshot {
    pub task_id: String,
    pub vk: u32,
    pub mods: u8,
}

// called from the hotkey thread after every register/reload to push per-task
// status into AppState. the hub Tasks view + Settings panel surface this so
// silent registration failures (risky-bare, parse fail) don't hide.
pub fn record_hotkey_status(
    app: &AppHandle,
    live_ids: &[String],
    errors: &[crate::hotkeys::HotkeyRegistrationError],
) {
    let state = app.state::<AppState>();
    let mut status = state.hotkey_status.lock().unwrap();
    status.clear();
    for id in live_ids {
        status.insert(id.clone(), HotkeyStatus::Live);
    }
    for err in errors {
        status.insert(
            err.task_id.clone(),
            HotkeyStatus::Failed {
                reason: err.reason.clone(),
            },
        );
    }
    drop(status);
    let _ = app.emit("capscr://hotkey-status", ());
}

// persist the "no system tray" banner dismissal so it never reappears
#[tauri::command]
pub fn dismiss_tray_hint(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    {
        let mut stored = state.config.lock().unwrap();
        let mut config = stored.clone();
        let _pending_mutation =
            PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
        config.ui.tray_hint_dismissed = true;
        config
            .save_and_migrate_secrets()
            .map_err(|e| e.to_string())?;
        *stored = config;
    }
    let _ = app.emit("capscr://config-updated", ());
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct EvdevStatus {
    pub enabled: bool,
    pub readable_device_count: usize,
    pub dev_input_exists: bool,
}

// settings backing for the advanced-input section: whether the opt-in is on
// and whether /dev/input is actually readable (input-group membership)
#[tauri::command]
pub fn evdev_status() -> EvdevStatus {
    #[cfg(target_os = "linux")]
    {
        EvdevStatus {
            enabled: crate::hotkeys::advanced_input_enabled(),
            readable_device_count: crate::hotkeys::evdev_linux::readable_devices().len(),
            dev_input_exists: std::path::Path::new("/dev/input").exists(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        EvdevStatus {
            enabled: false,
            readable_device_count: 0,
            dev_input_exists: false,
        }
    }
}

#[derive(Serialize)]
pub struct GnomeCompanionStatus {
    pub on_gnome: bool,
    // the extension answers on the bus (installed, enabled, session current)
    pub active: bool,
    // files are on disk; inactive means a re-login (or enable) is pending
    pub installed: bool,
}

#[cfg(target_os = "linux")]
const GNOME_EXTENSION_UUID: &str = "capscr@rot.lt";

#[cfg(target_os = "linux")]
fn gnome_extension_dir() -> Option<std::path::PathBuf> {
    Some(
        directories::BaseDirs::new()?
            .data_dir()
            .join("gnome-shell/extensions")
            .join(GNOME_EXTENSION_UUID),
    )
}

// settings backing for the gnome section: install state of the companion
// shell extension that supplies the window picker, keep-above, and the
// panel indicator on gnome
#[tauri::command]
pub fn gnome_companion_status() -> GnomeCompanionStatus {
    #[cfg(target_os = "linux")]
    {
        GnomeCompanionStatus {
            on_gnome: crate::shell::desktop() == crate::shell::DesktopEnv::Gnome,
            active: crate::capture::gnome_shell::available(),
            installed: gnome_extension_dir().is_some_and(|dir| dir.join("extension.js").exists()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        GnomeCompanionStatus {
            on_gnome: false,
            active: false,
            installed: false,
        }
    }
}

// copy the bundled extension into the user's gnome-shell extensions dir and
// flag it enabled; gnome-shell only scans that dir at session start, so the
// caller tells the user to log out and back in when active stays false
#[tauri::command]
pub fn install_gnome_companion(app: AppHandle) -> Result<GnomeCompanionStatus, String> {
    #[cfg(target_os = "linux")]
    {
        use tauri::path::BaseDirectory;
        let target = gnome_extension_dir().ok_or("no home directory")?;
        let _pending_mutation =
            PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        for file in ["extension.js", "metadata.json"] {
            let bundled = app
                .path()
                .resolve(format!("gnome-extension/{file}"), BaseDirectory::Resource)
                .map_err(|e| e.to_string())?;
            std::fs::copy(&bundled, target.join(file))
                .map_err(|e| format!("copying {} failed: {e}", bundled.display()))?;
        }
        // records the enabled flag against the freshly copied files; the
        // running shell picks it up at the next login
        let _ = std::process::Command::new("gnome-extensions")
            .args(["enable", GNOME_EXTENSION_UUID])
            .status();
        Ok(gnome_companion_status())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("the companion extension only exists on Linux".to_string())
    }
}

// re-run the portal bind for the current set even when unchanged, so a user
// who dismissed the desktop's approval dialog can bring it back
#[tauri::command]
pub fn portal_rebind_shortcuts(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::hotkeys::portal_linux::rebind().map_err(|e| format!("{e:#}"))?;
        // re-flush so statuses reflect the fresh outcome
        let tasks = state.config.lock().unwrap().capture_tasks.clone();
        state.send_hotkey_reload(tasks);
        let _ = app.emit("capscr://hotkey-status", ());
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        Err("the shortcuts portal only exists on Linux".to_string())
    }
}

fn hotkey_backend_name() -> String {
    #[cfg(windows)]
    {
        "ll_hook".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        if !crate::capture::is_wayland_session() {
            "x11".to_string()
        } else if crate::hotkeys::portal_linux::available() {
            "portal".to_string()
        } else if crate::hotkeys::advanced_input_enabled() {
            "evdev".to_string()
        } else {
            "none".to_string()
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "none".to_string()
    }
}

#[tauri::command]
pub fn hotkey_diagnostics(state: State<AppState>) -> HotkeyDiagnostics {
    use std::sync::atomic::Ordering;
    let disabled = state.hotkeys_disabled.load(Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    let effective = crate::hotkeys::portal_linux::effective_triggers();
    #[cfg(not(target_os = "linux"))]
    let effective: std::collections::HashMap<String, String> = Default::default();
    let status = state.hotkey_status.lock().unwrap();
    let statuses = status
        .iter()
        .map(|(task_id, st)| match st {
            HotkeyStatus::Live => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "live".to_string(),
                reason: None,
                effective_trigger: effective.get(task_id).cloned(),
            },
            HotkeyStatus::Failed { reason } => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "failed".to_string(),
                reason: Some(reason.clone()),
                effective_trigger: None,
            },
        })
        .collect();
    HotkeyDiagnostics {
        disabled_globally: disabled,
        backend: hotkey_backend_name(),
        statuses,
        #[cfg(windows)]
        hook: {
            let t = crate::hotkeys::ll_hook::snapshot_telemetry();
            HookTelemetrySnapshot {
                installed: t.installed,
                enabled: t.enabled,
                registered_count: t.registered_count,
                registered: t
                    .registered
                    .into_iter()
                    .map(|(b, id)| HookBindingSnapshot {
                        task_id: id,
                        vk: b.vk,
                        mods: b.mods,
                    })
                    .collect(),
                calls_total: t.calls_total,
                keydown_calls: t.keydown_calls,
                matched_calls: t.matched_calls,
                dispatch_sent: t.dispatch_sent,
                dispatch_dropped: t.dispatch_dropped,
                last_vk: t.last_vk,
                last_mods: t.last_mods,
            }
        },
    }
}

/// Arm the LL hook to capture the next non-modifier keydown as a hotkey.
/// On press, the backend emits `capscr://hotkey-captured` with the vk +
/// mods + canonical hotkey string and clears the arm. UI cancels via
/// `cancel_hotkey_capture` if the user backs out.
#[tauri::command]
#[cfg(windows)]
pub fn start_hotkey_capture() -> Result<(), String> {
    crate::hotkeys::ll_hook::begin_capture();
    Ok(())
}

#[tauri::command]
#[cfg(not(windows))]
pub fn start_hotkey_capture() -> Result<(), String> {
    // no global hook to arm here — the hub window has focus while binding, so
    // HotkeyInput's browser-side keydown capture records the chord itself
    Ok(())
}

#[tauri::command]
#[cfg(windows)]
pub fn cancel_hotkey_capture() -> Result<(), String> {
    crate::hotkeys::ll_hook::cancel_capture();
    Ok(())
}

#[tauri::command]
#[cfg(not(windows))]
pub fn cancel_hotkey_capture() -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub fn set_hotkeys_disabled(
    disabled: bool,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let _pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    let tasks = {
        let mut stored = state.config.lock().unwrap();
        let was = state.hotkeys_disabled.load(Ordering::SeqCst);
        if was == disabled {
            return Ok(());
        }
        let mut config = stored.clone();
        config.hotkeys.disabled_globally = disabled;
        if let Err(error) = config.save_and_migrate_secrets() {
            return Err(error.to_string());
        }
        let tasks = if disabled {
            Vec::new()
        } else {
            config.capture_tasks.clone()
        };
        state.hotkeys_disabled.store(disabled, Ordering::SeqCst);
        *stored = config;
        tasks
    };
    #[cfg(windows)]
    crate::hotkeys::ll_hook::set_enabled(!disabled);
    state.send_hotkey_reload(tasks);
    crate::rebuild_tray_menu(&app);
    let _ = app.emit("capscr://hotkey-status", ());
    // the store mirrors hotkeys.disabled_globally too; nudge it to refetch so a
    // later Settings save doesn't persist a stale (enabled) value over the switch
    let _ = app.emit("capscr://config-updated", ());
    Ok(())
}
