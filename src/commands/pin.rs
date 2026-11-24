use super::*;

// the compositor helpers on linux find pin windows by this title
const PIN_TITLE: &str = "capscr: pinned";

pub fn pin_image(path: String, app: AppHandle, state: &AppState) -> Result<(), String> {
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

    let windows = app.windows().ok_or("the app is not running yet")?;
    // its page shows the window once the image has loaded and sized it
    windows.open(WindowConfig {
        label: label.clone(),
        title: PIN_TITLE.to_string(),
        width: 800,
        height: 600,
        minimum_width: 1,
        minimum_height: 1,
        decorations: false,
        always_on_top: true,
        transparent: true,
        visible: false,
        ..WindowConfig::default()
    });

    // always_on_top is a no-op on wayland; keep the pin above other windows
    // via a compositor helper where available
    #[cfg(target_os = "linux")]
    if crate::capture::gui_is_wayland() {
        if !crate::shell::wayland_globals().plasma_shell && crate::capture::gnome_shell::available()
        {
            let index = state.pinned_images.lock().unwrap().len().saturating_sub(1);
            let base = 48 + (index as i32 % 6) * 36;
            std::thread::spawn(move || {
                // the extension queues the placement if the pin hasn't mapped
                if let Err(e) = crate::capture::gnome_shell::place_above(PIN_TITLE, base, base) {
                    tracing::debug!("companion pin keep-above unavailable: {e:#}");
                }
            });
        } else if crate::shell::wayland_globals().plasma_shell {
            std::thread::spawn(|| {
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(300));
                    match crate::capture::keep_own_windows_above(PIN_TITLE) {
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
        }
    }
    Ok(())
}

// tracked position for manual pin drag when a compositor declines window drag
#[cfg(target_os = "linux")]
static PIN_POSITIONS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (i32, i32)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

// drop a pin's tracked position when its window goes away
#[cfg(target_os = "linux")]
pub(crate) fn forget_pin_position(label: &str) {
    PIN_POSITIONS.lock().unwrap().remove(label);
}

// whether pins need capscr-driven dragging instead of native window drag
pub fn pin_manual_drag() -> bool {
    false
}

// nudge a pin by a pointer delta against its tracked position
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
        if let Some(windows) = app.windows() {
            windows.act(&label, WindowAction::Position(x, y));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (label, dx, dy, app);
        Err("pin_move_by only applies on Linux Wayland".into())
    }
}

pub fn get_pinned_image_path(label: String, state: &AppState) -> Option<String> {
    state.pinned_images.lock().unwrap().get(&label).cloned()
}
