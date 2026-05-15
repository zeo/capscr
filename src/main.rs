#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod clipboard;
mod commands;
mod config;
mod hotkeys;
mod overlay;
mod plugin;
mod recording;
mod sound;
mod state;
mod upload;

use std::sync::mpsc;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tracing_subscriber::EnvFilter;

use state::HotkeyCommand;

pub fn install_hdr_runtime_from_config(config: &config::Config) {
    use capture::{SkivMode, SkivParams};
    use config::HdrCompressionMode;

    let mode = match config.capture.hdr.mode {
        HdrCompressionMode::MapCllToDisplay => SkivMode::MapCllToDisplay,
        HdrCompressionMode::NormalizeToCll => SkivMode::NormalizeToCll,
    };
    capture::install_skiv_params(SkivParams {
        mode,
        sdr_brightness_nits: config.capture.hdr.brightness_nits,
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
    set_dpi_awareness();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::load().unwrap_or_default();
    let _ = config.ensure_output_dir();
    install_hdr_runtime_from_config(&config);

    let initial_screenshot = config.hotkeys.screenshot.clone();
    let initial_record_gif = config.hotkeys.record_gif.clone();

    let app_state = state::AppState::new(config);

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app_state)
        .setup(move |app| {
            build_tray(app)?;
            let (tx, rx) = mpsc::channel::<HotkeyCommand>();
            {
                let st = app.state::<state::AppState>();
                *st.hotkey_tx.lock().unwrap() = Some(tx);
            }
            spawn_hotkey_thread(
                app.handle().clone(),
                rx,
                initial_screenshot.clone(),
                initial_record_gif.clone(),
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::take_screenshot,
            commands::list_captures,
            commands::delete_capture,
            commands::copy_capture_to_clipboard,
            commands::reupload_capture,
            commands::open_in_explorer,
            commands::exit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building capscr")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                api.prevent_exit();
            }
        });
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let screenshot_item = MenuItem::with_id(
        app,
        "screenshot",
        "Take Screenshot (region)",
        true,
        None::<&str>,
    )?;
    let fullscreen_item = MenuItem::with_id(
        app,
        "fullscreen",
        "Fullscreen Screenshot",
        true,
        None::<&str>,
    )?;
    let record_gif_item =
        MenuItem::with_id(app, "record_gif", "Record GIF (region)", true, None::<&str>)?;
    let separator1 = PredefinedMenuItem::separator(app)?;
    let settings_item = MenuItem::with_id(app, "settings", "Open hub", true, None::<&str>)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let exit_item = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &screenshot_item,
            &fullscreen_item,
            &record_gif_item,
            &separator1,
            &settings_item,
            &separator2,
            &exit_item,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default tray icon".into()))?;

    TrayIconBuilder::with_id("capscr-tray")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("capscr")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "screenshot" => commands::trigger_hotkey(app, hotkeys::HotkeyAction::Screenshot),
            "fullscreen" => {
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Err(e) = commands::run_capture_pipeline(
                        commands::CaptureModeArg::ActiveMonitor,
                        commands::PostActionArg::Clipboard,
                        &app,
                    ) {
                        tracing::warn!("fullscreen capture failed: {e}");
                    }
                });
            }
            "record_gif" => commands::trigger_hotkey(app, hotkeys::HotkeyAction::RecordGif),
            "settings" => {
                let _ = commands::open_hub_window(app);
            }
            "exit" => app.exit(0),
            _ => {}
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
    rx: mpsc::Receiver<HotkeyCommand>,
    initial_screenshot: String,
    initial_record_gif: String,
) {
    std::thread::spawn(move || {
        let mut hm = match hotkeys::HotkeyManager::new() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("hotkey manager init failed: {e}");
                return;
            }
        };
        hm.try_register(hotkeys::HotkeyAction::Screenshot, &initial_screenshot);
        hm.try_register(hotkeys::HotkeyAction::RecordGif, &initial_record_gif);
        for err in hm.take_errors() {
            tracing::warn!(
                "hotkey '{}' for {:?} failed: {}",
                err.hotkey,
                err.action,
                err.reason
            );
        }

        loop {
            while let Ok(cmd) = rx.try_recv() {
                let HotkeyCommand::Reload {
                    screenshot,
                    record_gif,
                } = cmd;
                hm.unregister_all();
                hm.try_register(hotkeys::HotkeyAction::Screenshot, &screenshot);
                hm.try_register(hotkeys::HotkeyAction::RecordGif, &record_gif);
                for err in hm.take_errors() {
                    tracing::warn!(
                        "hotkey '{}' for {:?} failed: {}",
                        err.hotkey,
                        err.action,
                        err.reason
                    );
                }
            }
            if let Some(action) = hm.poll() {
                commands::trigger_hotkey(&app, action);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}
