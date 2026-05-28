#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod clipboard;
mod commands;
mod config;
mod hotkeys;
#[cfg(windows)]
mod jumplist;
mod marketplace;
mod overlay;
mod plugin;
mod recording;
mod secret;
mod sound;
mod state;
mod upload;
#[cfg(windows)]
mod win_darkmode;

use std::time::Duration;
use crossbeam_channel as cb;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tracing_subscriber::EnvFilter;

use state::HotkeyCommand;

pub fn install_hdr_runtime_from_config(config: &config::Config) {
    use capture::TonemapParams;

    capture::install_tonemap_params(TonemapParams {
        sdr_white_nits_override: config.capture.hdr.brightness_nits,
        user_brightness_scale: config.capture.hdr.user_brightness_scale,
        use_p99_max_cll: config.capture.hdr.use_p99_max_cll,
    });
}

#[cfg(windows)]
fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
fn set_dpi_awareness() {}

fn main() {
    // early exit for --version / --help so capscr.exe behaves like a normal
    // CLI when invoked from PowerShell. Done before tracing / DPI / Tauri
    // setup so the process is genuinely transient in those modes.
    if handle_cli_short_circuit(std::env::args()) {
        return;
    }

    set_dpi_awareness();
    #[cfg(windows)]
    jumplist::set_app_user_model_id();
    #[cfg(windows)]
    win_darkmode::enable_dark_menus();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,wry=warn,tao=warn,tauri=warn,hyper=warn,reqwest=warn")),
        )
        .init();

    let config = config::Config::load().unwrap_or_default();
    if let Err(e) = config.ensure_output_dir() {
        // the hub UI isn't up yet — surface this through the OS notification
        // channel so the user knows captures will fail until they fix it.
        let _ = clipboard::show_notification(
            "capscr: captures folder unreachable",
            &format!("{e}. Open Settings → Output to point at a writable path."),
        );
        tracing::error!("ensure_output_dir failed at startup: {e:#}");
    }
    install_hdr_runtime_from_config(&config);

    // pre-warm the Win32 audio subsystem in the background so the first
    // capture cue isn't delayed by waveOut initialisation. Fire-and-forget;
    // the actual user-triggered Sound::play won't race because it serialises
    // through PlaySoundW.
    std::thread::spawn(sound::warm_audio_subsystem);

    // pre-warm D3D11 devices in the background to avoid driver wakeup delays
    // during the first screen capture.
    std::thread::spawn(|| {
        capture::HdrCapture::prewarm();
    });

    let initial_tasks = config.capture_tasks.clone();
    let app_state = state::AppState::new(config);

    let autostart_desired = app_state.config.lock().unwrap().ui.auto_start;
    let initial_jump = parse_jump_arg(std::env::args());

    tauri::Builder::default()
        // single-instance plugin must be the first one — when a second
        // capscr.exe launches (e.g. via a jump list shortcut), it forwards
        // argv to the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let kind = parse_jump_arg(argv.iter().cloned());
            dispatch_jump(app, kind.as_deref());
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // exclude VISIBLE — we manage hub visibility manually via prewarm + tray-click
        .plugin(tauri_plugin_window_state::Builder::default()
            .with_state_flags(
                tauri_plugin_window_state::StateFlags::all()
                    & !tauri_plugin_window_state::StateFlags::VISIBLE
            )
            .build())
        .manage(app_state)
        .setup(move |app| {
            build_tray(app)?;
            sync_autostart(app, autostart_desired);
            #[cfg(windows)]
            if let Err(e) = jumplist::register() {
                tracing::warn!("jumplist register failed: {e}");
            }
            // make sure the asset:// protocol can reach the user's configured
            // output dir even if they moved it off the default $PICTURE/capscr.
            // the static scope in tauri.conf.json is the fallback; this widens
            // it dynamically based on actual config.
            {
                let st = app.state::<state::AppState>();
                let dir = st.config.lock().unwrap().output.directory.clone();
                if let Err(e) = app.asset_protocol_scope().allow_directory(&dir, true) {
                    tracing::warn!("asset scope allow_directory({:?}) failed: {e}", dir);
                }
                if let Ok(dir_can) = std::fs::canonicalize(&dir) {
                    let _ = app.asset_protocol_scope().allow_directory(&dir_can, true);
                }
                if let Some(h_dir) = commands::history_dir() {
                    if let Err(e) = app.asset_protocol_scope().allow_directory(&h_dir, true) {
                        tracing::warn!("asset scope allow_directory({:?}) failed: {e}", h_dir);
                    }
                    if let Ok(h_can) = std::fs::canonicalize(&h_dir) {
                        let _ = app.asset_protocol_scope().allow_directory(&h_can, true);
                    }
                }
            }
            // pre-create the plugins folder so 'Open folder' from the
            // Marketplace tab succeeds on a fresh install without round-
            // tripping through the open_plugins_folder fallback create.
            if let Ok(dirs) = commands::resolve_plugins_dir() {
                let _ = std::fs::create_dir_all(&dirs);
            }
            let (tx, rx) = cb::unbounded::<HotkeyCommand>();
            {
                let st = app.state::<state::AppState>();
                *st.hotkey_tx.lock().unwrap() = Some(tx);
            }
            spawn_hotkey_thread(app.handle().clone(), rx, initial_tasks.clone());
            // warm the hub WebView2 ahead of the first tray click so it shows
            // instantly instead of paying cold-boot cost on demand.
            if let Err(e) = commands::prewarm_hub_window(app) {
                tracing::warn!("hub pre-warm failed: {e}");
            }
            // first-launch jump-list dispatch: if capscr.exe was launched with
            // --jump=<kind>, run that action now. We delay slightly so the tray
            // and webview are fully ready before any capture pipeline fires.
            if let Some(kind) = initial_jump.clone() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    dispatch_jump(&handle, Some(&kind));
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::get_default_config,
            commands::is_hdr_capture,
            commands::set_config,
            commands::take_screenshot,
            commands::list_captures,
            commands::delete_capture,
            commands::copy_capture_to_clipboard,
            commands::reupload_capture,
            commands::open_in_explorer,
            commands::exit_app,
            commands::set_autostart,
            commands::get_autostart,
            commands::list_installed_plugins,
            commands::open_plugins_folder,
            commands::marketplace_browse,
            commands::marketplace_install,
            commands::marketplace_uninstall,
            commands::toggle_plugin_enabled,
            commands::check_for_updates,
            commands::install_update,
            commands::get_editor_image_path,
            commands::open_editor,
            commands::save_edited_image,
            commands::copy_edited_image_to_clipboard,
            commands::upload_edited_image,
            commands::upload_file,
            commands::hotkey_diagnostics,
            commands::set_hotkeys_disabled,
            commands::start_hotkey_capture,
            commands::cancel_hotkey_capture,
            commands::sftp_known_hosts,
            commands::sftp_forget_host,
            commands::test_upload_connection,
            commands::fire_task,
        ])
        .build(tauri::generate_context!())
        .expect("error while building capscr")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = &event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

fn sync_autostart(app: &tauri::App, desired: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    match manager.is_enabled() {
        Ok(current) if current == desired => {}
        Ok(_) => {
            let res = if desired {
                manager.enable()
            } else {
                manager.disable()
            };
            if let Err(e) = res {
                tracing::warn!("autostart sync failed: {e}");
            }
        }
        Err(e) => tracing::warn!("autostart query failed: {e}"),
    }
}

// build the full tray menu fresh from current AppState. Called once at
// startup and again any time the state surfaces in the menu changes —
// new upload, destination switched, hotkeys toggled.
fn build_tray_menu<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
) -> tauri::Result<Menu<R>> {
    use std::sync::atomic::Ordering;

    // --- Capture submenu ---
    let cap_region =
        MenuItem::with_id(app, "cap_region", "Region", true, None::<&str>)?;
    let cap_window =
        MenuItem::with_id(app, "cap_window", "Window", true, None::<&str>)?;
    let cap_fullscreen =
        MenuItem::with_id(app, "cap_fullscreen", "Fullscreen (selector)", true, None::<&str>)?;
    let cap_active =
        MenuItem::with_id(app, "cap_active_monitor", "Active monitor", true, None::<&str>)?;
    let capture_submenu = Submenu::with_items(
        app,
        "Capture",
        true,
        &[&cap_region, &cap_window, &cap_fullscreen, &cap_active],
    )?;

    // --- Record submenu ---
    let rec_region_gif = MenuItem::with_id(
        app,
        "rec_region_gif",
        "Region GIF (toggle)",
        true,
        None::<&str>,
    )?;
    let record_submenu =
        Submenu::with_items(app, "Record", true, &[&rec_region_gif])?;

    // --- Recent uploads submenu (dynamic) ---
    let state = app.state::<state::AppState>();
    let recent: Vec<state::UploadRecord> = state
        .recent_uploads
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    let recent_items: Vec<MenuItem<R>> = recent
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            // truncate long URLs so the menu doesn't sprawl
            let label = if rec.url.len() > 56 {
                format!("{}…", &rec.url[..55])
            } else {
                rec.url.clone()
            };
            MenuItem::with_id(
                app,
                format!("recent_upload_{i}"),
                &label,
                true,
                None::<&str>,
            )
            .expect("recent upload item")
        })
        .collect();
    let recent_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = if recent_items.is_empty() {
        Vec::new()
    } else {
        recent_items
            .iter()
            .map(|m| m as &dyn tauri::menu::IsMenuItem<R>)
            .collect()
    };
    let recent_submenu_enabled = !recent_items.is_empty();
    let recent_submenu = if recent_submenu_enabled {
        Submenu::with_items(app, "Recent uploads (click → copy)", true, &recent_refs)?
    } else {
        Submenu::with_items(
            app,
            "Recent uploads (none yet)",
            false,
            &[] as &[&dyn tauri::menu::IsMenuItem<R>],
        )?
    };

    // --- Upload / utility items ---
    let copy_last_url = MenuItem::with_id(
        app,
        "copy_last_url",
        "Copy last upload URL",
        true,
        None::<&str>,
    )?;
    let open_captures = MenuItem::with_id(
        app,
        "open_captures",
        "Open captures folder",
        true,
        None::<&str>,
    )?;

    // --- Destination switcher ---
    let current_dest = state.config.lock().unwrap().upload.destination;
    let mark = |is_current: bool, label: &str| -> String {
        if is_current {
            format!("● {label}")
        } else {
            format!("○ {label}")
        }
    };
    let dest_imgur = MenuItem::with_id(
        app,
        "dest_imgur",
        mark(current_dest == config::UploadDestination::Imgur, "Imgur"),
        true,
        None::<&str>,
    )?;
    let dest_custom = MenuItem::with_id(
        app,
        "dest_custom",
        mark(current_dest == config::UploadDestination::Custom, "Custom HTTPS"),
        true,
        None::<&str>,
    )?;
    let dest_ftp = MenuItem::with_id(
        app,
        "dest_ftp",
        mark(current_dest == config::UploadDestination::Ftp, "FTP"),
        true,
        None::<&str>,
    )?;
    let dest_sftp = MenuItem::with_id(
        app,
        "dest_sftp",
        mark(current_dest == config::UploadDestination::Sftp, "SFTP"),
        true,
        None::<&str>,
    )?;
    let dest_submenu = Submenu::with_items(
        app,
        "Upload destination",
        true,
        &[&dest_imgur, &dest_custom, &dest_ftp, &dest_sftp],
    )?;

    // --- Open hub (single top-level item, no submenu) ---
    let open_hub = MenuItem::with_id(app, "tab_default", "Open hub", true, None::<&str>)?;

    // --- Hotkey toggle (stateful) ---
    let disabled = state.hotkeys_disabled.load(Ordering::SeqCst);
    let hotkeys_toggle = MenuItem::with_id(
        app,
        "hotkeys_toggle",
        if disabled {
            "Enable all hotkeys"
        } else {
            "Disable all hotkeys"
        },
        true,
        None::<&str>,
    )?;

    let separator1 = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let separator3 = PredefinedMenuItem::separator(app)?;
    let separator4 = PredefinedMenuItem::separator(app)?;
    let separator5 = PredefinedMenuItem::separator(app)?;
    let exit_item = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;

    Menu::with_items(
        app,
        &[
            &capture_submenu,
            &record_submenu,
            &separator1,
            &recent_submenu,
            &copy_last_url,
            &open_captures,
            &dest_submenu,
            &separator2,
            &open_hub,
            &separator3,
            &hotkeys_toggle,
            &separator4,
            &exit_item,
            &separator5,
        ],
    )
}

/// rebuild the tray menu from current state and apply it to the running tray.
/// safe to call from any thread. silently no-ops if the tray hasn't been built
/// yet (startup race).
pub fn rebuild_tray_menu(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("capscr-tray") {
        match build_tray_menu(app) {
            Ok(menu) => {
                if let Err(e) = tray.set_menu(Some(menu)) {
                    tracing::warn!("tray set_menu failed: {e}");
                }
            }
            Err(e) => tracing::warn!("rebuild_tray_menu construct failed: {e}"),
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default tray icon".into()))?;

    TrayIconBuilder::with_id("capscr-tray")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("capscr")
        .on_menu_event(|app, event| {
            use commands::{CaptureModeArg, PostActionArg};
            let id = event.id.as_ref();
            let spawn_capture = |mode: CaptureModeArg, post: PostActionArg| {
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Err(e) = commands::run_capture_pipeline(mode, post, &app) {
                        tracing::warn!("tray capture failed: {e}");
                        commands::emit_error(&app, "capture", &e.to_string());
                    }
                });
            };
            // route by id. dynamic-id items (recent_upload_*) are matched by
            // prefix below so the static-arm part stays compact.
            match id {
                "cap_region" => spawn_capture(CaptureModeArg::Region, PostActionArg::Clipboard),
                "cap_window" => spawn_capture(CaptureModeArg::Window, PostActionArg::Clipboard),
                "cap_fullscreen" => {
                    spawn_capture(CaptureModeArg::Fullscreen, PostActionArg::Clipboard)
                }
                "cap_active_monitor" => {
                    spawn_capture(CaptureModeArg::ActiveMonitor, PostActionArg::Clipboard)
                }
                "rec_region_gif" => {
                    // synthesize a tray-driven gif task so run_gif_task's start/stop
                    // toggle (keyed off the task id in AppState) works the same as
                    // a real hotkey-bound task.
                    let app = app.clone();
                    std::thread::spawn(move || {
                        let task = config::CaptureTask {
                            id: "__tray_gif".into(),
                            name: "Tray GIF".into(),
                            hotkey: String::new(),
                            capture_mode: config::TaskCaptureMode::RegionGif,
                            post_action: config::TaskPostAction::SaveFile,
                            target_destination: None,
                        };
                        if let Err(e) = commands::run_task(&task, &app) {
                            tracing::warn!("tray gif failed: {e}");
                            commands::emit_error(&app, "gif", &e.to_string());
                        }
                    });
                }
                "copy_last_url" => {
                    let st = app.state::<state::AppState>();
                    let last = st.last_upload.lock().unwrap().clone();
                    match last {
                        Some(rec) => {
                            if let Err(e) = crate::upload::copy_url_to_clipboard(&rec.url) {
                                tracing::warn!("copy last url failed: {e}");
                            } else if st.config.lock().unwrap().ui.show_notifications {
                                let _ = crate::clipboard::show_notification(
                                    "Copied",
                                    &rec.url,
                                );
                            }
                        }
                        None => {
                            let _ = crate::clipboard::show_notification(
                                "No uploads yet",
                                "Upload something first and the URL will land here.",
                            );
                        }
                    }
                }
                "open_captures" => {
                    let st = app.state::<state::AppState>();
                    let dir = st.config.lock().unwrap().output.directory.clone();
                    let _ = std::fs::create_dir_all(&dir);
                    use tauri_plugin_opener::OpenerExt;
                    let _ = app
                        .opener()
                        .open_path(dir.to_string_lossy().to_string(), None::<&str>);
                }
                "tab_default" => {
                    let _ = commands::open_hub_window(app);
                }
                "dest_imgur" | "dest_custom" | "dest_ftp" | "dest_sftp" => {
                    let st = app.state::<state::AppState>();
                    let new_dest = match id {
                        "dest_imgur" => config::UploadDestination::Imgur,
                        "dest_custom" => config::UploadDestination::Custom,
                        "dest_ftp" => config::UploadDestination::Ftp,
                        _ => config::UploadDestination::Sftp,
                    };
                    {
                        let mut cfg = st.config.lock().unwrap();
                        if cfg.upload.destination != new_dest {
                            cfg.upload.destination = new_dest;
                            if let Err(e) = cfg.save() {
                                tracing::warn!(
                                    "save after destination switch failed: {e}"
                                );
                            }
                        }
                    }
                    rebuild_tray_menu(app);
                    let _ = app.emit("capscr://config-updated", ());
                    let _ = crate::clipboard::show_notification(
                        "Upload destination",
                        &format!("Switched to {:?}", new_dest),
                    );
                }
                "hotkeys_toggle" => {
                    use std::sync::atomic::Ordering;
                    let st = app.state::<state::AppState>();
                    let was_disabled = st.hotkeys_disabled.load(Ordering::SeqCst);
                    let next = !was_disabled;
                    if let Err(e) = commands::set_hotkeys_disabled(
                        next,
                        app.clone(),
                        st,
                    ) {
                        tracing::warn!("tray hotkey toggle failed: {e}");
                    }
                    let _ = crate::clipboard::show_notification(
                        if next { "Hotkeys disabled" } else { "Hotkeys enabled" },
                        if next {
                            "All capscr hotkeys are off. Re-enable from the tray or Settings → Hotkeys."
                        } else {
                            "All task hotkeys are live again."
                        },
                    );
                }
                "exit" => commands::exit_app(app.clone()),
                other if other.starts_with("recent_upload_") => {
                    let idx: usize = other
                        .trim_start_matches("recent_upload_")
                        .parse()
                        .unwrap_or(usize::MAX);
                    let st = app.state::<state::AppState>();
                    let url = st
                        .recent_uploads
                        .lock()
                        .unwrap()
                        .get(idx)
                        .map(|r| r.url.clone());
                    if let Some(url) = url {
                        if let Err(e) = crate::upload::copy_url_to_clipboard(&url) {
                            tracing::warn!("copy recent url failed: {e}");
                        } else if st.config.lock().unwrap().ui.show_notifications {
                            let _ = crate::clipboard::show_notification("Copied", &url);
                        }
                    }
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = commands::open_hub_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn spawn_hotkey_thread(
    app: tauri::AppHandle,
    rx: cb::Receiver<HotkeyCommand>,
    initial_tasks: Vec<config::CaptureTask>,
) {
    #[cfg(windows)]
    {
        use crate::hotkeys::ll_hook;
        let (hook_tx, hook_rx) = cb::unbounded::<ll_hook::HookEvent>();
        ll_hook::init(hook_tx);
        if let Err(e) = ll_hook::spawn_hook_thread() {
            tracing::error!("LL keyboard hook thread spawn failed: {e}");
        }

        // dispatcher: consumes HookEvents off the LL hook channel and turns
        // them into task triggers. lives on its own thread so the hook
        // callback returns instantly (Windows kills LL hooks that exceed
        // LowLevelHooksTimeout, default 300ms).
        let app_dispatch = app.clone();
        std::thread::Builder::new()
            .name("capscr-hotkey-dispatch".into())
            .spawn(move || {
                let mut last_fire: std::collections::HashMap<String, std::time::Instant> =
                    std::collections::HashMap::new();
                while let Ok(ev) = hook_rx.recv() {
                    match ev {
                        ll_hook::HookEvent::Fire { task_id } => {
                            // dedupe auto-repeat: ignore repeats within 250ms of
                            // the previous fire for the same task. WH_KEYBOARD_LL
                            // forwards held-key auto-repeat presses; users only
                            // expect one capture per press-release cycle.
                            let now = std::time::Instant::now();
                            let last = last_fire.get(&task_id).copied();
                            let allow = match last {
                                None => true,
                                Some(t) => now.duration_since(t).as_millis() > 250,
                            };
                            if !allow {
                                continue;
                            }
                            last_fire.insert(task_id.clone(), now);
                            commands::trigger_task(&app_dispatch, &task_id);
                        }
                        ll_hook::HookEvent::Captured { vk, mods } => {
                            let hotkey = hotkeys::format_vk_mods(vk, mods);
                            let payload = serde_json::json!({
                                "vk": vk,
                                "mods": mods,
                                "hotkey": hotkey,
                            });
                            let _ = app_dispatch.emit("capscr://hotkey-captured", payload);
                        }
                    }
                }
            })
            .ok();
    }

    std::thread::spawn(move || {
        let mut hm = match hotkeys::HotkeyManager::new() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("hotkey manager init failed: {e}");
                return;
            }
        };
        // honour the persisted kill switch when present — restored from config
        // at startup via the AppState constructor.
        #[cfg(windows)]
        {
            use std::sync::atomic::Ordering;
            let st = app.state::<state::AppState>();
            let disabled = st.hotkeys_disabled.load(Ordering::SeqCst);
            crate::hotkeys::ll_hook::set_enabled(!disabled);
        }
        for task in &initial_tasks {
            hm.try_register(task.id.clone(), &task.hotkey);
        }
        hm.flush_to_hook();
        let startup_errors = hm.take_errors();
        for err in &startup_errors {
            tracing::warn!(
                "hotkey '{}' for task '{}' failed: {}",
                err.hotkey,
                err.task_id,
                err.reason
            );
        }
        // record startup registration outcomes so the hub Tasks view can show
        // a per-task status chip even before the user has opened it.
        commands::record_hotkey_status(&app, &hm.registered_task_ids(), &startup_errors);
        if !startup_errors.is_empty() {
            let summary = startup_errors
                .iter()
                .map(|e| format!("'{}' ({}): {}", e.hotkey, e.task_id, e.reason))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = clipboard::show_notification("capscr: hotkey conflicts", &summary);
        }

        // reload loop: hotkey re-registration is driven by the Reload command
        // channel, which is sent on config save, tray toggle, and any other
        // path that mutates the binding set.
        while let Ok(HotkeyCommand::Reload { tasks }) = rx.recv() {
            hm.unregister_all();
            for task in &tasks {
                hm.try_register(task.id.clone(), &task.hotkey);
            }
            hm.flush_to_hook();
            let errs = hm.take_errors();
            commands::record_hotkey_status(&app, &hm.registered_task_ids(), &errs);
            for err in &errs {
                tracing::warn!(
                    "hotkey '{}' for task '{}' failed: {}",
                    err.hotkey,
                    err.task_id,
                    err.reason
                );
                let msg = format!("'{}' ({}) — {}", err.hotkey, err.task_id, err.reason);
                commands::emit_error(&app, "hotkey", &msg);
                let _ = crate::clipboard::show_notification("Hotkey conflict", &msg);
            }
        }
    });
}

fn parse_jump_arg<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter()
        .find_map(|a| a.strip_prefix("--jump=").map(String::from))
}

/// returns true when the process should exit immediately after writing to the
/// parent console (--version / --help). Tauri normally builds the GUI window
/// subsystem with no attached console, so on Windows we hop onto the parent's
/// console via AttachConsole before printing.
fn handle_cli_short_circuit<I: IntoIterator<Item = String>>(args: I) -> bool {
    let mut want_version = false;
    let mut want_help = false;
    for a in args.into_iter().skip(1) {
        match a.as_str() {
            "--version" | "-V" => want_version = true,
            "--help" | "-h" => want_help = true,
            _ => {}
        }
    }
    if !want_version && !want_help {
        return false;
    }
    attach_parent_console();
    if want_version {
        println!("capscr {}", env!("CARGO_PKG_VERSION"));
    } else {
        print_help();
    }
    true
}

fn print_help() {
    println!(
        "capscr {} — modern HDR-aware screen capture\n\
        \n\
        Usage:\n  \
          capscr [--jump=<kind>]\n  \
          capscr --version | -V\n  \
          capscr --help | -h\n\
        \n\
        Options:\n  \
          --jump=<kind>   Trigger a one-shot action and exit. kinds: region, window, fullscreen, captures, hub\n  \
          --version       Print version and exit\n  \
          --help          Print this help and exit\n\
        \n\
        With no flags, capscr runs in the tray. Click the icon or press a configured hotkey to capture.\n\
        Hub UI: <hotkeys / tasks / settings / history / marketplace> via tray menu or jump list.\n\
        Repo:   https://github.com/lintowe/capscr",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn dispatch_jump(app: &tauri::AppHandle, kind: Option<&str>) {
    use commands::{CaptureModeArg, PostActionArg};
    let Some(kind) = kind else {
        // bare second launch (no --jump=) — just surface the hub.
        let _ = commands::open_hub_window(app);
        return;
    };
    let app_clone = app.clone();
    let spawn_capture = move |mode: CaptureModeArg| {
        std::thread::spawn(move || {
            if let Err(e) =
                commands::run_capture_pipeline(mode, PostActionArg::Clipboard, &app_clone)
            {
                tracing::warn!("jump-list capture failed: {e}");
                commands::emit_error(&app_clone, "capture", &e.to_string());
            }
        });
    };
    match kind {
        "region" => spawn_capture(CaptureModeArg::Region),
        "window" => spawn_capture(CaptureModeArg::Window),
        "fullscreen" => spawn_capture(CaptureModeArg::Fullscreen),
        "captures" => {
            let st = app.state::<state::AppState>();
            let dir = st.config.lock().unwrap().output.directory.clone();
            let _ = std::fs::create_dir_all(&dir);
            use tauri_plugin_opener::OpenerExt;
            let _ = app
                .opener()
                .open_path(dir.to_string_lossy().to_string(), None::<&str>);
        }
        "hub" => {
            let _ = commands::open_hub_window(app);
        }
        other => {
            tracing::warn!("unknown --jump= kind: {other}");
        }
    }
}
