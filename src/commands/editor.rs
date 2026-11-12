use super::*;

pub const EDITOR_LABEL: &str = "editor";

pub fn open_editor_window(app: &AppHandle, image_path: &str) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    *state.editor_image_path.lock().unwrap() = Some(image_path.to_string());

    if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
        fit_window_to_work_area(&window, 420.0, 280.0);
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("capscr://editor-load", image_path.to_string());
        // the reused window may be a leftover that never loaded (about:blank);
        // heal it the same way as a fresh one — once the page boots it pulls
        // the current path via get_editor_image_path
        watch_editor_navigation(app, window);
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(app, EDITOR_LABEL, url)
        .title("capscr — edit")
        .inner_size(960.0, 640.0)
        .min_inner_size(420.0, 280.0)
        .resizable(true)
        .decorations(false)
        .visible(false);

    #[cfg(windows)]
    {
        builder = builder.additional_browser_args(WEBVIEW_BROWSER_ARGS);
    }

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    let sized = window.clone();
    // window-state's restore holds its locks across calls the main thread has
    // to answer, so off the main thread it hangs the command; the plugin skips
    // this window's own restore (main.rs) so the growth below sees the saved size
    app.run_on_main_thread(move || {
        let _ = sized.restore_state(WindowStateFlags::all() & !WindowStateFlags::VISIBLE);
        grow_window_once(&sized, (0.85, 0.85), (960.0, 640.0));
        fit_window_to_work_area(&sized, 420.0, 280.0);
        let _ = sized.show();
        let _ = sized.set_focus();
    })?;
    watch_editor_navigation(app, window);
    Ok(())
}

// dynamically created webview windows sometimes come up on about:blank instead
// of loading the app url (tauri#13967). The page never renders, and because the
// editor is undecorated there's no native close button either — the user is
// stuck with a dead white window. Probe the webview shortly after creation and
// navigate explicitly when that happens. The hub window is prewarmed at startup
// and never destroyed, so its url is the canonical app url to copy.
fn watch_editor_navigation(app: &AppHandle, window: tauri::WebviewWindow) {
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
            tracing::warn!("editor webview stuck on about:blank; navigating explicitly");
            if let Some(url) = canonical_app_url(&app) {
                if let Err(e) = window.navigate(url) {
                    tracing::warn!("editor explicit navigation failed: {e}");
                }
            } else {
                tracing::warn!("no canonical app url available for the editor");
            }
        }
    });
}

#[tauri::command]
pub fn get_editor_image_path(state: State<AppState>) -> Option<String> {
    // liveness signal: fires only when the editor frontend actually booted
    tracing::debug!("editor webview alive — frontend requested image path");
    state.editor_image_path.lock().unwrap().clone()
}

// async so the webview window is built off the main thread — a sync command
// runs on the main thread and WebviewWindowBuilder::build deadlocks there on
// windows, leaving a white window that can't be closed
#[tauri::command]
pub async fn open_editor(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _capture_gate = state
        .capture_in_progress
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .map(|_| CaptureGate(&state.capture_in_progress))
        .map_err(|_| "capture or shutdown is already in progress".to_string())?;
    if state
        .shutdown_in_progress
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("application shutdown is already in progress".to_string());
    }
    let buf = PathBuf::from(&path);
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let cfg = state.config.lock().unwrap().clone();
    // history captures live in the history dir; the editor must reach them too
    if !is_path_allowed(&canonical, &cfg) {
        return Err("Path is outside the allowed capture directories".into());
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ext == "gif" || ext == "mp4" {
        return Err("Recordings can't be edited — the editor would flatten the animation".into());
    }
    open_editor_window(&app, &canonical.to_string_lossy()).map_err(|e| e.to_string())
}

/// true when `canonical` lives inside the annotate staging dir. `Path::starts_with`
/// compares whole components, so a sibling like `annotate-stage-evil` never matches.
fn is_annotate_staged_path(stage_dir: &std::path::Path, canonical: &std::path::Path) -> bool {
    canonical.starts_with(stage_dir)
}

/// true for files the annotate flow stages (`annotate-<uuid>.png`). everything
/// else in the cache is left alone by the orphan cleanup.
fn is_annotate_staged_filename(name: &str) -> bool {
    name.starts_with("annotate-") && name.ends_with(".png")
}

#[cfg(test)]
mod annotate_stage_tests {
    use super::*;

    #[test]
    fn staged_paths_match_only_inside_the_stage_dir() {
        use std::path::Path;
        let stage = Path::new("/cache/annotate-stage");
        assert!(is_annotate_staged_path(
            stage,
            Path::new("/cache/annotate-stage/annotate-abc.png")
        ));
        assert!(is_annotate_staged_path(
            stage,
            Path::new("/cache/annotate-stage/sub/annotate-abc.png")
        ));
        assert!(!is_annotate_staged_path(stage, Path::new("/cache/other.png")));
        // component-wise matching: a sibling dir sharing the prefix is not inside
        assert!(!is_annotate_staged_path(
            stage,
            Path::new("/cache/annotate-stage-evil/annotate-abc.png")
        ));
    }

    #[test]
    fn staged_filenames_match_only_staged_captures() {
        assert!(is_annotate_staged_filename("annotate-abc123.png"));
        assert!(!is_annotate_staged_filename("annotate-abc123.jpg"));
        assert!(!is_annotate_staged_filename("capture.png"));
        assert!(!is_annotate_staged_filename("annotate-abc.png.bak"));
    }
}

/// finish an annotate-before-save session: write the edited pixels as a real
/// capture, surface it to History, and drop the staging file.
fn save_staged_annotation(
    app: &AppHandle,
    state: &State<AppState>,
    bytes: Vec<u8>,
    staged: &std::path::Path,
) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("Image data is empty".into());
    }
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("Image too large to save".into());
    }
    let config = state.config.lock().unwrap().clone();
    let _pending_mutation =
        PendingFileMutation::begin(app).map_err(|error| error.to_string())?;
    if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
        tracing::warn!("failed to create output dir: {e}");
    }
    let dest = get_unique_filepath(&config.output_path());
    // re-encode into the configured output format (the editor always exports
    // PNG, so writing its bytes verbatim would mislabel non-PNG captures),
    // via a sibling temp that is renamed over the claimed slot for atomicity.
    let stem = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("capture");
    let tmp = dest.with_file_name(format!(
        ".{stem}.{}.capture.tmp",
        uuid::Uuid::new_v4().as_simple()
    ));
    let encode_result: Result<(u32, u32), String> = (|| {
        let img = image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        crate::clipboard::save_image(&img, &tmp, config.output.format, config.output.quality)
            .map_err(|e| format!("{e:#}"))?;
        std::fs::rename(&tmp, &dest).map_err(|e| format!("rename failed: {e}"))?;
        Ok(img.dimensions())
    })();
    let size = match encode_result {
        Ok(size) => size,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&dest);
            return Err(format!("save failed: {e}"));
        }
    };
    let _ = std::fs::remove_file(staged);
    *state.last_save.lock().unwrap() = Some(dest.clone());
    notify_capture_saved(app, sized_save(&dest, Some(size)));
    Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
    if config.ui.show_notifications {
        let _ = show_notification("Capture saved", &dest.to_string_lossy());
    }
    Ok(dest.to_string_lossy().to_string())
}

/// drop annotate-stage files orphaned by editor sessions that closed without
/// saving. runs at launch; the save path removes its own file on success.
pub fn cleanup_annotate_stage(app: &AppHandle) {
    let Ok(cache_dir) = app.path().app_cache_dir() else {
        return;
    };
    let stage = cache_dir.join("annotate-stage");
    let Ok(entries) = std::fs::read_dir(&stage) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let staged = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(is_annotate_staged_filename)
            .unwrap_or(false);
        if staged {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_dir(&stage);
}

const EDITOR_TARGET_HEADER: &str = "x-capscr-target";

// the custom-protocol ipc delivers the editor's Uint8Array untouched; tauri's
// postMessage fallback turns the same body into a json number array
fn editor_image_bytes(body: &tauri::ipc::InvokeBody) -> Result<Vec<u8>, String> {
    match body {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        tauri::ipc::InvokeBody::Json(value) => serde_json::from_value(value.clone())
            .map_err(|_| "expected the image bytes as the request body".to_string()),
    }
}

// header values are ascii-only, so the editor percent-encodes the path with
// encodeURIComponent, which also escapes '+', '&' and '=' and leaves one key
fn editor_target_path(headers: &tauri::http::HeaderMap) -> Result<String, String> {
    let raw = headers
        .get(EDITOR_TARGET_HEADER)
        .ok_or_else(|| "missing target path".to_string())?
        .to_str()
        .map_err(|_| "target path header is not ascii".to_string())?;
    let path = url::form_urlencoded::parse(raw.as_bytes())
        .next()
        .map(|(key, _)| key.into_owned())
        .unwrap_or_default();
    if path.is_empty() || path.contains('\u{FFFD}') {
        return Err("target path is not valid percent-encoded utf-8".into());
    }
    Ok(path)
}

#[cfg(test)]
mod editor_ipc_tests {
    use super::*;
    use tauri::http::{HeaderMap, HeaderValue};

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(EDITOR_TARGET_HEADER, HeaderValue::from_str(value).unwrap());
        map
    }

    #[test]
    fn target_path_round_trips_encode_uri_component() {
        // encodeURIComponent("C:\\Users\\müller\\shot +1 &=.png")
        let encoded = "C%3A%5CUsers%5Cm%C3%BCller%5Cshot%20%2B1%20%26%3D.png";
        assert_eq!(
            editor_target_path(&headers(encoded)).unwrap(),
            "C:\\Users\\müller\\shot +1 &=.png"
        );
    }

    #[test]
    fn target_path_rejects_missing_empty_and_invalid_utf8() {
        assert!(editor_target_path(&HeaderMap::new()).is_err());
        assert!(editor_target_path(&headers("")).is_err());
        assert!(editor_target_path(&headers("%FF%FE.png")).is_err());
    }

    #[test]
    fn image_bytes_accept_raw_and_postmessage_fallback() {
        let raw = tauri::ipc::InvokeBody::Raw(vec![137, 80, 78, 71]);
        assert_eq!(editor_image_bytes(&raw).unwrap(), vec![137, 80, 78, 71]);
        let json = tauri::ipc::InvokeBody::Json(serde_json::json!([137, 80, 78, 71]));
        assert_eq!(editor_image_bytes(&json).unwrap(), vec![137, 80, 78, 71]);
        let wrong = tauri::ipc::InvokeBody::Json(serde_json::json!({ "bytes": [1] }));
        assert!(editor_image_bytes(&wrong).is_err());
    }
}

#[tauri::command(async)]
pub fn save_edited_image(
    request: tauri::ipc::Request<'_>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let target_path = editor_target_path(request.headers())?;
    let bytes = editor_image_bytes(request.body())?;
    let buf = PathBuf::from(&target_path);
    // staged pre-save annotations live under the app cache; saving one mints
    // the real capture in the output dir instead of overwriting the stage.
    if let Ok(cache_dir) = app.path().app_cache_dir() {
        // canonicalize both sides: AppData can sit behind a symlink or
        // junction, and a raw prefix check would then miss the stage
        let stage_dir = std::fs::canonicalize(cache_dir.join("annotate-stage"))
            .unwrap_or_else(|_| cache_dir.join("annotate-stage"));
        if let Ok(canonical) = std::fs::canonicalize(&buf) {
            if is_annotate_staged_path(&stage_dir, &canonical) {
                return save_staged_annotation(&app, &state, bytes, &canonical);
            }
        }
    }
    let parent = buf
        .parent()
        .ok_or_else(|| "target_path has no parent".to_string())?;
    let config = state.config.lock().unwrap().clone();
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    // allow writing edited images back into the history dir too, matching where
    // open_editor is now allowed to read from
    if !is_path_allowed(&canonical_parent, &config) {
        return Err("Path is outside the allowed capture directories".into());
    }
    if bytes.is_empty() {
        return Err("Image data is empty".into());
    }
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("Image too large to save".into());
    }
    let _pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    // atomic write: stage to a sibling temp file, then rename. A disk-full
    // or permission-denied mid-write would otherwise truncate the original
    // — the user would lose the un-edited capture too.
    let stem = buf.file_name().and_then(|s| s.to_str()).unwrap_or("edited");
    let tmp = buf.with_file_name(format!(
        ".{stem}.{}.editing.tmp",
        uuid::Uuid::new_v4().as_simple()
    ));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write failed: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &buf) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename failed: {e}"));
    }
    // surface the edit to the History tab so its tile picks up the new mtime
    let size = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    notify_capture_saved(&app, sized_save(&buf, size));
    Ok(target_path)
}

#[tauri::command(async)]
pub fn copy_edited_image_to_clipboard(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let bytes = editor_image_bytes(request.body())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.into_rgba8();
    let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
    cb.copy_image(&rgba).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upload_file(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UploadResponse, String> {
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !canonical.is_file() {
        return Err("not a regular file".into());
    }
    // only upload a path the user actually dropped, or one of capscr's own
    // captures; refuse an arbitrary path a compromised webview might supply
    let config = state.config.lock().unwrap().clone();
    if !state.was_dropped(&canonical) && !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    upload_path(app, config, canonical).await
}

#[tauri::command]
pub async fn upload_edited_image(
    request: tauri::ipc::Request<'_>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UploadResponse, String> {
    let bytes = editor_image_bytes(request.body())?;
    let config = state.config.lock().unwrap().clone();
    let service = build_upload_service(&config);
    let destination = service.label();
    let pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    // reqwest::blocking panics on a tokio worker, which would leave the
    // editor's invoke pending forever
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _pending_mutation = pending_mutation;
        let rgba = image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        crate::upload::shared_uploader()
            .and_then(|uploader| uploader.upload(&rgba, &service))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    finish_upload(&app, &config, &result, destination, None);
    Ok(UploadResponse {
        url: result.url,
        delete_url: result.delete_url,
    })
}
