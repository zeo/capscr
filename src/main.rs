#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod clipboard;
mod commands;
mod config;
#[cfg(target_os = "linux")]
mod distro;
mod hotkeys;
#[cfg(windows)]
mod jumplist;
mod marketplace;
mod overlay;
mod plugin;
mod recbar;
mod recording;
mod secret;
#[cfg(target_os = "linux")]
mod shell;
mod sound;
mod state;
mod upload;
#[cfg(windows)]
mod win_darkmode;

use ceri::{AppHandle, MenuItem, TrayConfig, TrayEvent};
use crossbeam_channel as cb;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

use state::{app_state, HotkeyCommand};

// names the profile folder, and the app's own data and cache folders
pub(crate) const APP_ID: &str = "io.rot.capscr";
// where `npm run dev` serves the pages a debug build loads
const DEV_URL: &str = "http://localhost:1420";

static APP: OnceLock<AppHandle> = OnceLock::new();

// the running app, for the commands a page invokes
fn handle() -> AppHandle {
    APP.get()
        .expect("the app handle is kept before the app runs")
        .clone()
}

/// the runtime the async commands and their background work run on
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: LazyLock<tokio::runtime::Runtime> =
        LazyLock::new(|| tokio::runtime::Runtime::new().expect("the async runtime starts"));
    &RUNTIME
}

// the login entry keeps the name earlier builds registered it under
pub fn autostart() -> ceri::autostart::Autostart {
    ceri::autostart::Autostart::new("capscr", "capscr")
}

pub fn install_hdr_runtime_from_config(config: &config::Config) {
    use capture::TonemapParams;

    capture::install_tonemap_params(TonemapParams {
        sdr_white_nits_override: config.capture.hdr.brightness_nits,
        user_brightness_scale: config.capture.hdr.user_brightness_scale,
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
    // CLI when invoked from PowerShell. Done before tracing / DPI / app
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
                .unwrap_or_else(|_| EnvFilter::new("info,hyper=warn,reqwest=warn")),
        )
        .init();

    let (config, startup_notices) = config::Config::load().unwrap_or_default();
    // ensure the output dir exists off the startup critical path: the capture
    // and save paths create it on demand anyway, so this is only an early
    // warning, and canonicalize on a cold or networked Pictures folder can
    // stall time-to-tray.
    {
        let cfg = config.clone();
        std::thread::spawn(move || {
            if let Err(e) = cfg.ensure_output_dir() {
                // the hub UI isn't up yet — surface this through the OS
                // notification channel so the user knows captures will fail
                // until they fix it.
                let _ = clipboard::show_notification(
                    "capscr: captures folder unreachable",
                    &format!("{e}. Open Settings → Output to point at a writable path."),
                );
                tracing::error!("ensure_output_dir failed at startup: {e:#}");
            }
        });
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

    let mut initial_tasks = config.capture_tasks.clone();
    initial_tasks.extend(state::pause_binding(&config));
    let app_state = state::AppState::new(config);
    *app_state.startup_notices.lock().unwrap() = startup_notices;

    let autostart_desired = app_state.config.lock().unwrap().ui.auto_start;
    let initial_jump = parse_jump_arg(std::env::args());
    state::manage(app_state);

    let app = ceri::App::new()
        .identifier(APP_ID)
        .name("capscr")
        .version(env!("CARGO_PKG_VERSION"))
        .icon(include_bytes!("../icons/icon.png"))
        .assets(ceri::embedded_assets!())
        // a debug build loads the pages from the vite dev server
        .dev_url(DEV_URL)
        .content_security_policy(
            "default-src 'self'; img-src 'self' https://files.ceri.invalid data:; \
             media-src 'self' https://files.ceri.invalid; style-src 'self' 'unsafe-inline'; \
             script-src 'self'; connect-src 'self'",
        )
        .capabilities(ceri::Capabilities::new(vec![
            // every window may invoke the app's own commands
            ceri::Grant {
                windows: vec!["*".into()],
                origins: vec![ceri::APP_ORIGIN.into()],
                commands: vec!["*".into()],
            },
            ceri::Grant {
                windows: vec![commands::HUB_LABEL.into(), commands::EDITOR_LABEL.into()],
                origins: vec![ceri::APP_ORIGIN.into()],
                commands: vec![
                    "dialog:*".into(),
                    "opener:open-url".into(),
                    "opener:reveal".into(),
                ],
            },
        ]))
        // capscr lives in the tray, with or without a window open
        .keep_running()
        // when a second capscr.exe launches (e.g. via a jump list shortcut),
        // it hands its arguments to the running instance and exits
        .single_instance(|app, argv, _cwd| {
            let kind = parse_jump_arg(argv.iter().map(|arg| arg.to_string_lossy().into_owned()));
            dispatch_jump(app, kind.as_deref());
        })
        .tray(
            TrayConfig {
                tooltip: "capscr".into(),
                menu: build_tray_menu(),
                ..TrayConfig::default()
            },
            |app, event| match event {
                TrayEvent::Click | TrayEvent::DoubleClick => commands::open_hub_window(app),
                TrayEvent::Menu(id) => on_tray_menu(app, &id),
            },
        )
        .on_start(move |app| start(app, autostart_desired, initial_tasks, initial_jump))
        .on_window_opened(commands::window_opened)
        .on_close_requested(commands::close_requested)
        .on_closed(|_, label| commands::window_closed(label))
        .on_file_drop(|_, label, paths| commands::files_dropped(label, paths));
    // the tauri builds kept the editor's tool settings in webview2's storage
    #[cfg(windows)]
    let app = match ceri::dirs::local_data() {
        Some(dir) => app.import_local_storage(
            dir.join(APP_ID)
                .join("EBWebView/Default/Local Storage/leveldb"),
            "http://tauri.localhost",
        ),
        None => app,
    };
    let _ = APP.set(app.handle());
    register_commands(app)
        .run()
        .expect("error while running capscr");
}

// runs on the loop's thread once the app is up
fn start(
    app: &AppHandle,
    autostart_desired: bool,
    initial_tasks: Vec<config::CaptureTask>,
    initial_jump: Option<String>,
) {
    // debug builds load every page from the vite dev server; a standalone
    // launch of one yields blank selector and hub windows with no other
    // symptom, so make the misbuild impossible to miss
    if cfg!(debug_assertions) {
        use std::net::ToSocketAddrs;
        let reachable = DEV_URL
            .trim_start_matches("http://")
            .to_socket_addrs()
            .is_ok_and(|mut addrs| {
                addrs.any(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
                })
            });
        if !reachable {
            tracing::error!(
                "debug build but no dev server answers on {DEV_URL}; every window will be \
                 blank. run `npm run dev` in frontend, or build a release (linux/install.sh does)."
            );
            let _ = clipboard::show_notification(
                "capscr misbuilt",
                "debug binary without a dev server: windows will be blank",
            );
        }
    }
    // the linux selector overlay builds its webview through a stored
    // AppHandle; register it before any capture can fire
    #[cfg(target_os = "linux")]
    overlay::linux::init(app);
    // drop annotate-stage files orphaned by editor sessions that
    // closed without saving, so the cache does not fill with them
    commands::cleanup_annotate_stage();
    // load plugins on a background thread so the cranelift JIT compile
    // of each enabled WASM plugin doesn't delay the tray or first
    // capture. dispatch sees zero plugins until the load swaps them in.
    {
        let handle = app.clone();
        std::thread::spawn(move || {
            app_state().load_plugins();
            rebuild_tray_menu(&handle);
        });
    }
    upload::set_plugin_upload(Box::new(|id, data, mime, file_name| {
        app_state()
            .plugin_manager
            .read()
            .unwrap()
            .upload(id, data, mime, file_name)
    }));
    // on a tray-less desktop (vanilla gnome has no StatusNotifier
    // host) the tray icon silently never appears, stranding a
    // tray-first app. detect that and surface the hub with a one-time
    // banner so capscr stays reachable. the check is delayed and
    // repeated because gnome extensions register their host late in
    // the session.
    #[cfg(target_os = "linux")]
    {
        let handle = app.clone();
        let dismissed = app_state().config.lock().unwrap().ui.tray_hint_dismissed;
        if !dismissed {
            std::thread::spawn(move || {
                for wait in [3u64, 12] {
                    std::thread::sleep(Duration::from_secs(wait));
                    if crate::shell::tray_detect::status_notifier_present() {
                        return;
                    }
                    // the companion extension's panel button carries
                    // the same menu, so the session isn't tray-less
                    if capture::gnome_shell::available() {
                        return;
                    }
                }
                tracing::info!("no status-notifier tray host; surfacing the hub");
                commands::open_hub_window(&handle);
                handle.emit("capscr://tray-missing", ());
            });
        }
    }
    sync_autostart(autostart_desired);
    // jump-list registration does synchronous COM (CoCreateInstance per
    // task), so it runs off this thread. it only needs the AUMID (set early
    // in main) and current_exe, neither tied to the loop's thread.
    #[cfg(windows)]
    jumplist::refresh(app_state().config.lock().unwrap().capture_tasks.clone());
    // the folders pages may load files from: the captures and history
    // folders, which follow the config, and the app's own data and cache
    {
        let dir = app_state().config.lock().unwrap().output.directory.clone();
        commands::allow_capture_dirs(app, &dir);
        let own = [
            ceri::dirs::pictures().map(|dir| dir.join("capscr")),
            ceri::dirs::home().map(|dir| dir.join("Pictures").join("capscr")),
            ceri::dirs::data().map(|dir| dir.join(APP_ID)),
            commands::app_cache_dir(),
        ];
        for dir in own.into_iter().flatten() {
            if let Err(e) = app.files().allow(&dir) {
                tracing::warn!("file scope allow({dir:?}) failed: {e}");
            }
        }
    }
    // the plugins folder is created on demand by open_plugins_folder
    // (Marketplace → Open folder) and load_all tolerates its absence, so
    // there's no need to pre-create it synchronously during setup.
    let (tx, rx) = cb::unbounded::<HotkeyCommand>();
    *app_state().hotkey_tx.lock().unwrap() = Some(tx);
    let records_mp4 = initial_tasks
        .iter()
        .any(|task| task.capture_mode == config::TaskCaptureMode::RegionMp4);
    spawn_hotkey_thread(app.clone(), rx, initial_tasks);
    // load the hub ahead of the first tray click so it shows instantly
    commands::prewarm_hub_window(app);
    #[cfg(windows)]
    recbar::prewarm(app);
    // the hardware encoder trial runs off the startup path so the
    // first mp4 recording doesn't wait on it
    if records_mp4 {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
            recording::hardware_encoders();
        });
    }
    // first-launch jump-list dispatch: if capscr.exe was launched with
    // --jump=<kind>, run that action now. We delay slightly so the tray
    // and hub are fully ready before any capture pipeline fires.
    if let Some(kind) = initial_jump {
        let handle = app.clone();
        runtime().spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            dispatch_jump(&handle, Some(&kind));
        });
    }
}

// a command's future, run to its end on the command's own thread
fn block<F: std::future::Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

// the commands the pages invoke, each answered on a thread of its own
fn register_commands(app: ceri::App) -> ceri::App {
    use ceri::command;
    use commands::CaptureModeArg;
    use config::{CaptureTask, Config, TaskPostAction};
    type Values = serde_json::Map<String, serde_json::Value>;

    #[cfg(target_os = "linux")]
    let app = app
        .blocking_json_command(
            "selector_context",
            command!(|call| overlay::linux::selector_context(&call.window)),
        )
        .blocking_json_command(
            "selector_frame",
            command!(|call| overlay::linux::selector_frame(&call.window)),
        )
        .blocking_json_command(
            "selector_ready",
            command!(|call| overlay::linux::selector_ready(&call.window)),
        )
        .blocking_json_command(
            "selector_finish",
            command!(|call, outcome: overlay::linux::SelectorOutcome| {
                overlay::linux::selector_finish(&call.window, outcome)
            }),
        );
    app.blocking_json_command("get_config", command!(|| commands::get_config(app_state())))
        .blocking_json_command(
            "get_default_config",
            command!(|| commands::get_default_config()),
        )
        .blocking_json_command(
            "is_hdr_capture",
            command!(|path: String| commands::is_hdr_capture(path)),
        )
        .blocking_json_command(
            "set_config",
            command!(|config: Config| commands::set_config(config, handle(), app_state())),
        )
        .blocking_json_command(
            "pick_output_dir",
            command!(|call| block(commands::pick_output_dir(handle(), call.window.clone()))),
        )
        .blocking_json_command(
            "set_output_dir",
            command!(|call, path: String| block(commands::set_output_dir(
                path,
                handle(),
                call.window.clone()
            ))),
        )
        .blocking_json_command(
            "set_capture_tasks",
            command!(
                |capture_tasks: Vec<CaptureTask>| commands::set_capture_tasks(
                    capture_tasks,
                    handle(),
                    app_state()
                )
            ),
        )
        .blocking_json_command(
            "take_screenshot",
            command!(
                |mode: CaptureModeArg, post: TaskPostAction| commands::take_screenshot(
                    mode,
                    post,
                    handle()
                )
            ),
        )
        .blocking_json_command(
            "list_captures",
            command!(|| commands::list_captures(app_state())),
        )
        .blocking_json_command(
            "history_thumbnail",
            command!(|path: String| block(commands::history_thumbnail(path, app_state()))),
        )
        .blocking_json_command(
            "delete_capture",
            command!(|path: String| commands::delete_capture(path, app_state())),
        )
        .blocking_json_command(
            "copy_capture_to_clipboard",
            command!(|path: String| commands::copy_capture_to_clipboard(path, app_state())),
        )
        .blocking_json_command(
            "reupload_capture",
            command!(|path: String| block(commands::reupload_capture(path, handle(), app_state()))),
        )
        .blocking_json_command(
            "open_in_explorer",
            command!(|path: String| commands::open_in_explorer(path, handle(), app_state())),
        )
        .blocking_json_command(
            "trim_recording",
            command!(
                |path: String, start_secs: f64, end_secs: f64, fast: bool| block(
                    commands::trim_recording(path, start_secs, end_secs, fast, app_state())
                )
            ),
        )
        .blocking_json_command(
            "export_gif",
            command!(
                |path: String, start_secs: f64, end_secs: f64, fps: u32, max_width: u32| block(
                    commands::export_gif(path, start_secs, end_secs, fps, max_width, app_state())
                )
            ),
        )
        .blocking_json_command(
            "gif_duration",
            command!(|path: String| block(commands::gif_duration(path, app_state()))),
        )
        .blocking_json_command(
            "acknowledge_hub_close",
            command!(|request_id: String| commands::acknowledge_hub_close(request_id, app_state())),
        )
        .blocking_json_command(
            "complete_hub_close",
            command!(|force_exit: bool| commands::complete_hub_close(force_exit, handle())),
        )
        .blocking_json_command(
            "set_autostart",
            command!(|enabled: bool| commands::set_autostart(enabled, app_state())),
        )
        .blocking_json_command("get_autostart", command!(|| commands::get_autostart()))
        .blocking_json_command(
            "list_installed_plugins",
            command!(|| commands::list_installed_plugins()),
        )
        .blocking_json_command(
            "plugin_load_errors",
            command!(|| commands::plugin_load_errors(app_state())),
        )
        .blocking_json_command(
            "reload_plugins",
            command!(|| block(commands::reload_plugins(handle()))),
        )
        .blocking_json_command(
            "list_upload_plugins",
            command!(|| commands::list_upload_plugins(app_state())),
        )
        .blocking_json_command(
            "delete_remote_upload",
            command!(
                |call, path: String, url: String| block(commands::delete_remote_upload(
                    path,
                    url,
                    handle(),
                    call.window.clone()
                ))
            ),
        )
        .blocking_json_command(
            "plugin_settings",
            command!(|id: String| block(commands::plugin_settings(id))),
        )
        .blocking_json_command(
            "set_plugin_settings",
            command!(
                |id: String, values: Values| block(commands::set_plugin_settings(
                    id,
                    values,
                    handle()
                ))
            ),
        )
        .blocking_json_command(
            "take_startup_notices",
            command!(|| commands::take_startup_notices(app_state())),
        )
        .blocking_json_command(
            "open_plugins_folder",
            command!(|| commands::open_plugins_folder()),
        )
        .blocking_json_command(
            "marketplace_browse",
            command!(|| block(commands::marketplace_browse(app_state()))),
        )
        .blocking_json_command(
            "marketplace_install",
            command!(|id: String| block(commands::marketplace_install(id, handle()))),
        )
        .blocking_json_command(
            "marketplace_uninstall",
            command!(|id: String| block(commands::marketplace_uninstall(id, handle()))),
        )
        .blocking_json_command(
            "toggle_plugin_enabled",
            command!(
                |id: String, enabled: bool| block(commands::toggle_plugin_enabled(
                    id,
                    enabled,
                    handle()
                ))
            ),
        )
        .blocking_json_command(
            "check_for_updates",
            command!(|| block(commands::check_for_updates())),
        )
        .blocking_json_command(
            "install_update",
            command!(|| block(commands::install_update(handle()))),
        )
        .blocking_json_command(
            "get_editor_image_path",
            command!(|| commands::get_editor_image_path(app_state())),
        )
        .blocking_json_command(
            "open_editor",
            command!(|path: String| commands::open_editor(path, handle(), app_state())),
        )
        .blocking_json_command(
            "set_editor_close_guard",
            command!(|guarded: bool| commands::set_editor_close_guard(guarded, app_state())),
        )
        .blocking_json_command(
            "request_editor_exit",
            command!(|request_id: String| commands::request_editor_exit(request_id, handle())),
        )
        .blocking_json_command(
            "answer_editor_exit",
            command!(
                |request_id: String, allowed: bool| commands::answer_editor_exit(
                    request_id,
                    allowed,
                    handle()
                )
            ),
        )
        .blocking_json_command(
            "save_edited_image",
            command!(|call| commands::save_edited_image(call, handle(), app_state())),
        )
        .blocking_json_command(
            "copy_edited_image_to_clipboard",
            command!(|call| commands::copy_edited_image_to_clipboard(call)),
        )
        .blocking_json_command(
            "upload_edited_image",
            command!(|call| block(commands::upload_edited_image(call, handle(), app_state()))),
        )
        .blocking_json_command(
            "upload_file",
            command!(|path: String| block(commands::upload_file(path, handle(), app_state()))),
        )
        .blocking_json_command(
            "hotkey_diagnostics",
            command!(|| commands::hotkey_diagnostics(app_state())),
        )
        .blocking_json_command("evdev_status", command!(|| commands::evdev_status()))
        .blocking_json_command(
            "gnome_companion_status",
            command!(|| commands::gnome_companion_status()),
        )
        .blocking_json_command(
            "hdr_pipeline_supported",
            command!(|| commands::hdr_pipeline_supported()),
        )
        .blocking_json_command(
            "install_gnome_companion",
            command!(|| commands::install_gnome_companion(handle())),
        )
        .blocking_json_command(
            "portal_rebind_shortcuts",
            command!(|| commands::portal_rebind_shortcuts(handle(), app_state())),
        )
        .blocking_json_command("pin_manual_drag", command!(|| commands::pin_manual_drag()))
        .blocking_json_command(
            "pin_move_by",
            command!(|label: String, dx: i32, dy: i32| commands::pin_move_by(
                label,
                dx,
                dy,
                handle()
            )),
        )
        .blocking_json_command(
            "dismiss_tray_hint",
            command!(|| commands::dismiss_tray_hint(handle(), app_state())),
        )
        .blocking_json_command(
            "set_hotkeys_disabled",
            command!(|disabled: bool| commands::set_hotkeys_disabled(
                disabled,
                handle(),
                app_state()
            )),
        )
        .blocking_json_command(
            "start_hotkey_capture",
            command!(|| commands::start_hotkey_capture()),
        )
        .blocking_json_command(
            "cancel_hotkey_capture",
            command!(|| commands::cancel_hotkey_capture()),
        )
        .blocking_json_command(
            "sftp_known_hosts",
            command!(|| commands::sftp_known_hosts()),
        )
        .blocking_json_command(
            "sftp_forget_host",
            command!(|host_port: String| commands::sftp_forget_host(host_port)),
        )
        .blocking_json_command(
            "test_upload_connection",
            command!(|call, destination: String, config: Config| block(
                commands::test_upload_connection(
                    destination,
                    config,
                    handle(),
                    call.window.clone(),
                    app_state()
                )
            )),
        )
        .blocking_json_command(
            "fire_task",
            command!(|task_id: String| commands::fire_task(task_id, handle())),
        )
        .blocking_json_command("stop_recording", command!(|| commands::stop_recording()))
        .blocking_json_command(
            "video_encoder_support",
            command!(|| block(commands::video_encoder_support())),
        )
        .blocking_json_command("recbar_status", command!(|| recbar::recbar_status()))
        .blocking_json_command(
            "recbar_record",
            command!(|| recbar::recbar_record(handle())),
        )
        .blocking_json_command(
            "recbar_pause",
            command!(|paused: bool| recbar::recbar_pause(handle(), paused)),
        )
        .blocking_json_command(
            "recbar_cancel",
            command!(|| recbar::recbar_cancel(handle())),
        )
        .blocking_json_command(
            "recbar_preset",
            command!(|width: u32, height: u32| recbar::recbar_preset(width, height)),
        )
        .blocking_json_command(
            "run_ocr",
            command!(|path: String| block(commands::run_ocr(path, app_state()))),
        )
        .blocking_json_command(
            "pin_image",
            command!(|path: String| commands::pin_image(path, handle(), app_state())),
        )
        .blocking_json_command(
            "get_pinned_image_path",
            command!(|label: String| commands::get_pinned_image_path(label, app_state())),
        )
}

fn sync_autostart(desired: bool) {
    let manager = autostart();
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
// startup and again any time the state surfaces in the menu changes:
// new upload, destination switched, hotkeys toggled.
fn build_tray_menu() -> Vec<MenuItem> {
    use std::sync::atomic::Ordering;

    let capture_submenu = MenuItem::submenu(
        "Capture",
        vec![
            MenuItem::new("cap_region", "Region"),
            MenuItem::new("cap_region_editor", "Region into editor"),
            MenuItem::new("cap_window", "Window"),
            MenuItem::new("cap_fullscreen", "Fullscreen (selector)"),
            MenuItem::new("cap_active_monitor", "Active monitor"),
        ],
    );
    let record_submenu = MenuItem::submenu(
        "Record",
        vec![
            MenuItem::new("rec_region_gif", "Region GIF (toggle)"),
            MenuItem::new("rec_region_mp4", "Region MP4 (toggle)"),
        ],
    );

    // recent uploads, newest first
    let state = app_state();
    let recent_items: Vec<MenuItem> = state
        .recent_uploads
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            // truncate long URLs so the menu doesn't sprawl. count/slice by
            // char, not byte: a custom uploader's url comes from arbitrary
            // server json and can hold multibyte utf-8, where a byte slice
            // would panic on a non-boundary cut
            let label = if rec.url.chars().count() > 56 {
                format!("{}…", rec.url.chars().take(55).collect::<String>())
            } else {
                rec.url.clone()
            };
            MenuItem::new(format!("recent_upload_{i}"), label)
        })
        .collect();
    let recent_submenu = if recent_items.is_empty() {
        MenuItem::submenu("Recent uploads (none yet)", Vec::new()).disabled()
    } else {
        MenuItem::submenu("Recent uploads (click → copy)", recent_items)
    };

    // destination switcher
    let (current_dest, current_plugin) = {
        let config = state.config.lock().unwrap();
        (config.upload.destination, config.upload.plugin.clone())
    };
    let mark = |is_current: bool, label: &str| -> String {
        if is_current {
            format!("● {label}")
        } else {
            format!("○ {label}")
        }
    };
    let mut dest_items: Vec<MenuItem> = config::UploadDestination::all()
        .iter()
        .map(|d| MenuItem::new(d.menu_id(), mark(current_dest == *d, d.display_name())))
        .collect();
    for plugin in state.plugin_manager.read().unwrap().upload_destinations() {
        let current =
            current_dest == config::UploadDestination::Plugin && current_plugin == plugin.id;
        dest_items.push(MenuItem::new(
            format!("{}{}", config::PLUGIN_MENU_PREFIX, plugin.id),
            mark(current, &plugin.name),
        ));
    }

    let disabled = state.hotkeys_disabled.load(Ordering::SeqCst);
    let hotkeys_toggle = if disabled {
        "Enable all hotkeys"
    } else {
        "Disable all hotkeys"
    };

    vec![
        capture_submenu,
        record_submenu,
        MenuItem::Separator,
        recent_submenu,
        MenuItem::new("copy_last_url", "Copy last upload URL"),
        MenuItem::new("open_captures", "Open captures folder"),
        MenuItem::submenu("Upload destination", dest_items),
        MenuItem::Separator,
        MenuItem::new("tab_default", "Open hub"),
        MenuItem::Separator,
        MenuItem::new("hotkeys_toggle", hotkeys_toggle),
        MenuItem::Separator,
        MenuItem::new("exit", "Exit"),
        MenuItem::Separator,
    ]
}

/// rebuild the tray menu from current state and apply it to the running tray.
/// safe to call from any thread. silently no-ops if the tray hasn't been built
/// yet (startup race).
pub fn rebuild_tray_menu(app: &AppHandle) {
    if let Some(tray) = app.tray() {
        tray.set_menu(build_tray_menu());
    }
}

// what a tray menu item does, by its id; runs on the loop's thread on windows
fn on_tray_menu(app: &AppHandle, id: &str) {
    use commands::CaptureModeArg;
    use config::TaskPostAction;
    let spawn_capture = |mode: CaptureModeArg, post: TaskPostAction| {
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
        "cap_region" => spawn_capture(CaptureModeArg::Region, TaskPostAction::Clipboard),
        "cap_region_editor" => spawn_capture(CaptureModeArg::Region, TaskPostAction::OpenEditor),
        "cap_window" => spawn_capture(CaptureModeArg::Window, TaskPostAction::Clipboard),
        "cap_fullscreen" => spawn_capture(CaptureModeArg::Fullscreen, TaskPostAction::Clipboard),
        "cap_active_monitor" => {
            spawn_capture(CaptureModeArg::ActiveMonitor, TaskPostAction::Clipboard)
        }
        "rec_region_gif" => {
            // synthesize a tray-driven gif task so run_gif_task's start/stop
            // toggle (keyed off the task id in AppState) works the same as
            // a real hotkey-bound task
            let app = app.clone();
            std::thread::spawn(move || {
                let task = config::CaptureTask {
                    id: "__tray_gif".into(),
                    name: "Tray GIF".into(),
                    hotkey: String::new(),
                    capture_mode: config::TaskCaptureMode::RegionGif,
                    post_action: config::TaskPostAction::SaveFile,
                    target_destination: None,
                    delay_ms: None,
                };
                if let Err(e) = commands::run_task(&task, &app) {
                    tracing::warn!("tray gif failed: {e}");
                    commands::emit_error(&app, "gif", &e.to_string());
                }
            });
        }
        "rec_region_mp4" => {
            // synthesize a tray-driven mp4 task so run_gif_task's start/stop
            // toggle (keyed off the task id in AppState) works the same as
            // a real hotkey-bound task
            let app = app.clone();
            std::thread::spawn(move || {
                let task = config::CaptureTask {
                    id: "__tray_mp4".into(),
                    name: "Tray MP4".into(),
                    hotkey: String::new(),
                    capture_mode: config::TaskCaptureMode::RegionMp4,
                    post_action: config::TaskPostAction::SaveFile,
                    target_destination: None,
                    delay_ms: None,
                };
                if let Err(e) = commands::run_task(&task, &app) {
                    tracing::warn!("tray mp4 failed: {e}");
                    commands::emit_error(&app, "mp4", &e.to_string());
                }
            });
        }
        "copy_last_url" => {
            let st = app_state();
            let last = st.last_upload.lock().unwrap().clone();
            match last {
                Some(rec) => {
                    if let Err(e) = crate::upload::copy_url_to_clipboard(&rec.url) {
                        tracing::warn!("copy last url failed: {e}");
                    } else if st.config.lock().unwrap().ui.show_notifications {
                        let _ = crate::clipboard::show_notification("Copied", &rec.url);
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
            let st = app_state();
            let dir = st.config.lock().unwrap().output.directory.clone();
            let _pending_mutation = match commands::PendingFileMutation::begin() {
                Ok(mutation) => mutation,
                Err(error) => {
                    tracing::warn!("open captures blocked: {error}");
                    return;
                }
            };
            let _ = std::fs::create_dir_all(&dir);
            let _ = ceri::opener::open_path(&dir);
        }
        "tab_default" => {
            commands::open_hub_window(app);
        }
        dest if dest.starts_with("dest_") => {
            let new_plugin = dest.strip_prefix(config::PLUGIN_MENU_PREFIX);
            let Some(new_dest) = new_plugin
                .map(|_| config::UploadDestination::Plugin)
                .or_else(|| config::UploadDestination::from_menu_id(dest))
            else {
                return;
            };
            let st = app_state();
            let saved = {
                let mut stored = st.config.lock().unwrap();
                if stored.upload.destination == new_dest
                    && new_plugin.is_none_or(|id| stored.upload.plugin == id)
                {
                    Ok(false)
                } else {
                    let _pending_mutation = match commands::PendingFileMutation::begin() {
                        Ok(mutation) => mutation,
                        Err(error) => {
                            tracing::warn!("destination switch blocked: {error}");
                            return;
                        }
                    };
                    let mut config = stored.clone();
                    config.upload.destination = new_dest;
                    if let Some(id) = new_plugin {
                        config.upload.plugin = id.to_string();
                    }
                    match config.save_and_migrate_secrets() {
                        Ok(()) => {
                            *stored = config;
                            Ok(true)
                        }
                        Err(error) => Err(error.to_string()),
                    }
                }
            };
            match saved {
                Ok(false) => return,
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!("save after destination switch failed: {error}");
                    commands::emit_error(app, "settings", &error);
                    return;
                }
            }
            rebuild_tray_menu(app);
            app.emit("capscr://config-updated", ());
            let _ = crate::clipboard::show_notification(
                "Upload destination",
                &format!(
                    "Switched to {}",
                    new_plugin.unwrap_or(new_dest.display_name())
                ),
            );
        }
        "hotkeys_toggle" => {
            use std::sync::atomic::Ordering;
            let st = app_state();
            let was_disabled = st.hotkeys_disabled.load(Ordering::SeqCst);
            let next = !was_disabled;
            if let Err(e) = commands::set_hotkeys_disabled(next, app.clone(), st) {
                tracing::warn!("tray hotkey toggle failed: {e}");
                commands::emit_error(app, "settings", &e);
                let _ = crate::clipboard::show_notification("Hotkey change failed", &e);
                return;
            }
            let _ = crate::clipboard::show_notification(
                if next {
                    "Hotkeys disabled"
                } else {
                    "Hotkeys enabled"
                },
                if next {
                    "All capscr hotkeys are off. Re-enable from the tray or Settings → Hotkeys."
                } else {
                    "All task hotkeys are live again."
                },
            );
        }
        "exit" => commands::request_hub_close(app, true),
        other if other.starts_with("recent_upload_") => {
            let idx: usize = other
                .trim_start_matches("recent_upload_")
                .parse()
                .unwrap_or(usize::MAX);
            let st = app_state();
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
}

fn spawn_hotkey_thread(
    app: AppHandle,
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
                            app_dispatch.emit("capscr://hotkey-captured", payload);
                        }
                    }
                }
            })
            .ok();
    }

    // linux dispatcher: global-hotkey (X11 grabs) delivers events on a static
    // channel; route pressed events to their task with the same auto-repeat
    // dedupe the windows LL-hook dispatcher applies
    #[cfg(target_os = "linux")]
    {
        use std::sync::atomic::Ordering;
        let app_dispatch = app.clone();
        std::thread::Builder::new()
            .name("capscr-hotkey-dispatch".into())
            .spawn(move || {
                use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
                let rx = GlobalHotKeyEvent::receiver();
                let mut last_fire: std::collections::HashMap<String, std::time::Instant> =
                    std::collections::HashMap::new();
                while let Ok(ev) = rx.recv() {
                    if ev.state() != HotKeyState::Pressed {
                        continue;
                    }
                    let Some(task_id) = hotkeys::task_for_hotkey_id(ev.id()) else {
                        continue;
                    };
                    let st = app_state();
                    if st.hotkeys_disabled.load(Ordering::SeqCst) {
                        continue;
                    }
                    let now = std::time::Instant::now();
                    let allow = match last_fire.get(&task_id) {
                        None => true,
                        Some(t) => now.duration_since(*t).as_millis() > 250,
                    };
                    if !allow {
                        continue;
                    }
                    last_fire.insert(task_id.clone(), now);
                    commands::trigger_task(&app_dispatch, &task_id);
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
            let st = app_state();
            let disabled = st.hotkeys_disabled.load(Ordering::SeqCst);
            crate::hotkeys::ll_hook::set_enabled(!disabled);
        }
        // resolve the advanced-input opt-in before the first flush: an unset
        // value migrates to enabled when mouse-button bindings already exist,
        // so established Mouse4/Mouse5 setups keep working untouched
        #[cfg(target_os = "linux")]
        {
            let configured = {
                let st = app_state();
                let cfg = st.config.lock().unwrap();
                cfg.hotkeys.advanced_input
            };
            let advanced = configured.unwrap_or_else(|| {
                initial_tasks
                    .iter()
                    .any(|t| hotkeys::evdev_linux::parse_mouse_binding(&t.hotkey).is_some())
            });
            hotkeys::set_advanced_input(advanced);
            hotkeys::portal_linux::start(app.clone());
        }
        for task in &initial_tasks {
            hm.try_register_labeled(task.id.clone(), &task.hotkey, &task.name);
        }
        hm.flush_to_hook();
        #[cfg(target_os = "linux")]
        if hotkeys::advanced_input_enabled() {
            hotkeys::evdev_linux::start(app.clone());
        }
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
                hm.try_register_labeled(task.id.clone(), &task.hotkey, &task.name);
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

/// the first --jump=<kind> or --task=<id>, with a task id as the "task:<id>" kind
fn parse_jump_arg<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().find_map(|a| {
        if let Some(kind) = a.strip_prefix("--jump=") {
            return Some(kind.to_string());
        }
        a.strip_prefix("--task=")
            .filter(|id| !id.is_empty())
            .map(|id| format!("task:{id}"))
    })
}

/// one "<id>\t<name>" line per task, with control characters in names blanked
/// so every task stays on its own line
fn format_task_list(tasks: &[config::CaptureTask]) -> String {
    tasks
        .iter()
        .map(|task| {
            let name: String = task
                .name
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            format!("{}\t{name}\n", task.id)
        })
        .collect()
}

/// returns true when the process should exit immediately after writing to the
/// parent console (--version / --help). A release build uses the GUI window
/// subsystem with no attached console, so on Windows we hop onto the parent's
/// console via AttachConsole before printing.
fn handle_cli_short_circuit<I: IntoIterator<Item = String>>(args: I) -> bool {
    let mut want_version = false;
    let mut want_help = false;
    let mut want_task_list = false;
    let mut want_wayland_diag = false;
    let mut sweep_dir: Option<String> = None;
    for a in args.into_iter().skip(1) {
        match a.as_str() {
            "--version" | "-V" => want_version = true,
            "--help" | "-h" => want_help = true,
            "--list-tasks" => want_task_list = true,
            "--wayland-diag" => want_wayland_diag = true,
            s if s.starts_with("--d2d-sweep=") => {
                sweep_dir = Some(s.trim_start_matches("--d2d-sweep=").to_string());
            }
            _ => {}
        }
    }
    // the wayland diag is the linux counterpart of the d2d sweep
    #[cfg(not(target_os = "linux"))]
    if want_wayland_diag {
        eprintln!("--wayland-diag is only available on Linux");
        return true;
    }
    #[cfg(target_os = "linux")]
    if want_wayland_diag {
        capture::wayland_diagnostic();
        return true;
    }
    // the d2d sweep is a windows-only HDR diagnostic
    #[cfg(not(windows))]
    if sweep_dir.is_some() {
        eprintln!("--d2d-sweep is only available on Windows");
        return true;
    }
    #[cfg(windows)]
    if let Some(dir) = sweep_dir {
        attach_parent_console();
        unsafe {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
            use windows::Win32::UI::HiDpi::{
                SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            };
            // DuplicateOutput1 (HDR format list) requires per-monitor DPI
            // awareness or it returns DXGI_ERROR_UNSUPPORTED
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match crate::capture::capture_hdr_to_sdr_sweep(None, &dir) {
            Ok(()) => println!("d2d sweep done -> {dir}"),
            Err(e) => eprintln!("d2d sweep failed: {e:#}"),
        }
        return true;
    }
    if !want_version && !want_help && !want_task_list {
        return false;
    }
    attach_parent_console();
    if want_task_list {
        match config::Config::peek_tasks() {
            Ok(tasks) => print!("{}", format_task_list(&tasks)),
            Err(e) => eprintln!("could not read the saved tasks: {e:#}"),
        }
    } else if want_version {
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
          capscr [--jump=<kind> | --task=<id>]\n  \
          capscr --list-tasks\n  \
          capscr --version | -V\n  \
          capscr --help | -h\n\
        \n\
        Options:\n  \
          --jump=<kind>   Trigger a one-shot action and exit. kinds: region, window, fullscreen, captures, hub\n  \
          --task=<id>     Run a capture task by id, as its hotkey would\n  \
          --list-tasks    Print each saved task as <id><tab><name> and exit\n  \
          --version       Print version and exit\n  \
          --help          Print this help and exit\n\
        \n\
        With no flags, capscr runs in the tray. Click the icon or press a configured hotkey to capture.\n\
        Hub UI: <hotkeys / tasks / settings / history / marketplace> via tray menu or jump list.\n\
        Repo:   https://github.com/zeo/capscr",
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

fn dispatch_jump(app: &AppHandle, kind: Option<&str>) {
    use commands::CaptureModeArg;
    use config::TaskPostAction;
    let Some(kind) = kind else {
        // bare second launch (no --jump=) — just surface the hub.
        commands::open_hub_window(app);
        return;
    };
    let app_clone = app.clone();
    let spawn_capture = move |mode: CaptureModeArg| {
        std::thread::spawn(move || {
            if let Err(e) =
                commands::run_capture_pipeline(mode, TaskPostAction::Clipboard, &app_clone)
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
            let st = app_state();
            let dir = st.config.lock().unwrap().output.directory.clone();
            let _pending_mutation = match commands::PendingFileMutation::begin() {
                Ok(mutation) => mutation,
                Err(error) => {
                    tracing::warn!("open captures blocked: {error}");
                    return;
                }
            };
            let _ = std::fs::create_dir_all(&dir);
            let _ = ceri::opener::open_path(&dir);
        }
        "hub" => {
            commands::open_hub_window(app);
        }
        // fire any configured task by id, same entry the global hotkeys use
        other if other.starts_with("task:") => {
            let task_id = other.trim_start_matches("task:").to_string();
            let app = app.clone();
            std::thread::spawn(move || commands::trigger_task(&app, &task_id));
        }
        other => {
            tracing::warn!("unknown --jump= kind: {other}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jump(args: &[&str]) -> Option<String> {
        parse_jump_arg(
            std::iter::once("capscr")
                .chain(args.iter().copied())
                .map(String::from),
        )
    }

    #[test]
    fn task_arg_becomes_a_task_jump() {
        assert_eq!(jump(&["--task=gif-1"]).as_deref(), Some("task:gif-1"));
        assert_eq!(jump(&["--jump=hub"]).as_deref(), Some("hub"));
        assert_eq!(jump(&[]), None);
        assert_eq!(jump(&["--task="]), None);
        assert_eq!(
            jump(&["--task=", "--task=b", "--jump=hub"]).as_deref(),
            Some("task:b")
        );
    }

    #[test]
    fn task_list_is_one_line_per_task() {
        let tasks = [
            config::CaptureTask {
                id: "a".into(),
                name: "Region\tto clipboard".into(),
                ..Default::default()
            },
            config::CaptureTask {
                id: "b".into(),
                name: "two\nlines".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            format_task_list(&tasks),
            "a\tRegion to clipboard\nb\ttwo lines\n"
        );
    }
}
