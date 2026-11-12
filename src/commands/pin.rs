use super::*;

#[tauri::command]
pub async fn pin_image(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use uuid::Uuid;
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let path = canonical.to_string_lossy().to_string();
    let label = format!("pin_{}", Uuid::new_v4());

    state
        .pinned_images
        .lock()
        .unwrap()
        .insert(label.clone(), path);

    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(&app, &label, url)
        .title("capscr — pinned")
        .decorations(false)
        .resizable(true)
        .always_on_top(true)
        .transparent(true)
        .visible(false);

    #[cfg(windows)]
    {
        builder = builder.additional_browser_args(WEBVIEW_BROWSER_ARGS);
    }

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon).map_err(|e| e.to_string())?;
    }

    let window = builder.build().map_err(|e| e.to_string())?;
    watch_pin_navigation(&app, window.clone());

    // drop the pin's tracked layer-shell position when its window goes away
    #[cfg(target_os = "linux")]
    {
        let tracked = label.clone();
        window.on_window_event(move |event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                PIN_POSITIONS.lock().unwrap().remove(&tracked);
            }
        });
    }

    // always_on_top is a no-op on wayland; keep the pin above other windows
    // via a compositor role. kde uses kwin scripting (keeps the pin a normal
    // draggable toplevel); gnome goes through the companion extension, which
    // also cascades the pin since mutter ignores client positioning; wlroots
    // compositors use layer-shell, which has no compositor-driven move, so
    // those pins drag through pin_set_position.
    #[cfg(target_os = "linux")]
    if crate::capture::gui_is_wayland() {
        if !crate::shell::wayland_globals().plasma_shell && crate::capture::gnome_shell::available()
        {
            let index = state.pinned_images.lock().unwrap().len().saturating_sub(1);
            let base = 48 + (index as i32 % 6) * 36;
            std::thread::spawn(move || {
                // the extension queues the placement if the pin hasn't mapped
                if let Err(e) = crate::capture::gnome_shell::place_above("capscr — pinned", base, base) {
                    tracing::debug!("companion pin keep-above unavailable: {e:#}");
                }
            });
        } else if crate::shell::wayland_globals().plasma_shell {
            std::thread::spawn(|| {
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(300));
                    match crate::capture::keep_own_windows_above("capscr — pinned") {
                        Ok(count) if count > 0 => {
                            tracing::debug!("flagged {count} pinned window(s) keep-above");
                            return;
                        }
                        Ok(_) => continue,
                        Err(e) => {
                            tracing::debug!("keep-above unavailable: {e:#}");
                            return;
                        }
                    }
                }
                tracing::warn!("pin window never appeared in the window list");
            });
        } else {
            // cascade each new pin down-right from a base offset so stacked
            // pins don't land exactly on top of one another
            let index = state.pinned_images.lock().unwrap().len().saturating_sub(1);
            let base = 48 + (index as i32 % 6) * 36;
            PIN_POSITIONS
                .lock()
                .unwrap()
                .insert(label.clone(), (base, base));
            let app2 = app.clone();
            let label2 = label.clone();
            let _ = app.run_on_main_thread(move || {
                use gtk::prelude::Cast;
                let Some(win) = app2.get_webview_window(&label2) else {
                    return;
                };
                if let Ok(gtk_window) = win.gtk_window() {
                    let gtk_win: &gtk::Window = gtk_window.upcast_ref();
                    let pinned = crate::shell::layer_shell::monitor_at(gtk_win, base, base)
                        .map(|monitor| {
                            crate::shell::layer_shell::pin_at(
                                gtk_win,
                                &monitor,
                                crate::shell::layer_shell::LAYER_TOP,
                                base,
                                base,
                                true,
                            )
                        })
                        .unwrap_or(false);
                    if pinned {
                        PIN_MANUAL_DRAG.store(true, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        PIN_POSITIONS.lock().unwrap().remove(&label2);
                        tracing::warn!("pin keep-above unavailable on this compositor");
                    }
                }
            });
        }
    }
    Ok(())
}

// true once any pin is placed via layer-shell, where the compositor won't
// move the surface and PinView must drive dragging through pin_move_by
#[cfg(target_os = "linux")]
static PIN_MANUAL_DRAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// layer-shell pins have no compositor-reported position, so capscr tracks
// each pin's global logical top-left here and applies drag deltas against it
#[cfg(target_os = "linux")]
static PIN_POSITIONS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (i32, i32)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

// whether pins need capscr-driven dragging (layer-shell path). the frontend
// switches its drag region to pointer-delta + pin_move_by when true
#[tauri::command]
pub fn pin_manual_drag() -> bool {
    #[cfg(target_os = "linux")]
    {
        PIN_MANUAL_DRAG.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

// nudge a layer-shell pin by a pointer delta. accumulates against the
// tracked position and updates margins only (no surface re-init).
#[tauri::command]
pub fn pin_move_by(label: String, dx: i32, dy: i32, app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let (x, y) = {
            let mut positions = PIN_POSITIONS.lock().unwrap();
            let entry = positions.entry(label.clone()).or_insert((48, 48));
            entry.0 = (entry.0 + dx).max(0);
            entry.1 = (entry.1 + dy).max(0);
            *entry
        };
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            use gtk::prelude::Cast;
            let Some(win) = app2.get_webview_window(&label) else {
                return;
            };
            if let Ok(gtk_window) = win.gtk_window() {
                let gtk_win: &gtk::Window = gtk_window.upcast_ref();
                if let Some(monitor) = crate::shell::layer_shell::monitor_at(gtk_win, x, y) {
                    crate::shell::layer_shell::set_position(gtk_win, &monitor, x, y);
                }
            }
        });
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (label, dx, dy, app);
        Err("pin_move_by only applies on Linux Wayland".into())
    }
}

fn watch_pin_navigation(app: &AppHandle, window: tauri::WebviewWindow) {
    let app = app.clone();
    std::thread::spawn(move || {
        for wait_ms in [500u64, 1500, 3000] {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            match window.url() {
                Ok(url) if url.scheme() != "about" => {
                    remember_canonical_url(&app, &url);
                    return;
                }
                Err(_) => return,
                _ => {}
            }
            tracing::warn!("pin webview stuck on about:blank; navigating explicitly");
            if let Some(url) = canonical_app_url(&app) {
                if let Err(e) = window.navigate(url) {
                    tracing::warn!("pin explicit navigation failed: {e}");
                }
            }
        }
    });
}

#[tauri::command]
pub fn get_pinned_image_path(label: String, state: State<'_, AppState>) -> Option<String> {
    state.pinned_images.lock().unwrap().get(&label).cloned()
}
