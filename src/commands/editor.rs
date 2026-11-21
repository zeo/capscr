use super::*;

pub const EDITOR_LABEL: &str = "editor";
pub(crate) const EDITOR_MIN_SIZE: (u32, u32) = (420, 280);

pub fn open_editor_window(app: &AppHandle, image_path: &str) {
    let state = app_state();
    *state.editor_image_path.lock().unwrap() = Some(image_path.to_string());

    let Some(windows) = app.windows() else {
        return;
    };
    if window_is_open(app, EDITOR_LABEL) {
        fit_window_to_work_area(app, EDITOR_LABEL, EDITOR_MIN_SIZE);
        windows.act(EDITOR_LABEL, WindowAction::Focus);
        app.emit_to(EDITOR_LABEL, "capscr://editor-load", image_path.to_string());
        return;
    }
    // opened hidden; window_opened sizes it for its monitor and shows it
    windows.open(WindowConfig {
        label: EDITOR_LABEL.to_string(),
        title: "capscr: edit".to_string(),
        width: 960,
        height: 640,
        minimum_width: EDITOR_MIN_SIZE.0,
        minimum_height: EDITOR_MIN_SIZE.1,
        geometry_path: window_geometry_path(EDITOR_LABEL),
        decorations: false,
        visible: false,
        background: WINDOW_BACKGROUND,
        ..WindowConfig::default()
    });
}

// while on, a close of the editor window is refused and its page is asked
// instead, so unsaved annotations are not discarded silently
pub fn set_editor_close_guard(guarded: bool, state: &AppState) {
    state
        .editor_close_guarded
        .store(guarded, std::sync::atomic::Ordering::SeqCst);
}

// the hub asks the editor page whether the app may close it
pub fn request_editor_exit(request_id: String, app: AppHandle) {
    app.emit_to(
        EDITOR_LABEL,
        "capscr://editor-exit-request",
        serde_json::json!({ "requestId": request_id }),
    );
}

pub fn answer_editor_exit(request_id: String, allowed: bool, app: AppHandle) {
    app.emit_to(
        HUB_LABEL,
        "capscr://editor-exit-decision",
        serde_json::json!({ "requestId": request_id, "allowed": allowed }),
    );
}

pub fn get_editor_image_path(state: &AppState) -> Option<String> {
    // liveness signal: fires only when the editor frontend actually booted
    tracing::debug!("editor webview alive — frontend requested image path");
    state.editor_image_path.lock().unwrap().clone()
}

pub fn open_editor(path: String, app: AppHandle, state: &AppState) -> Result<(), String> {
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
    open_editor_window(&app, &canonical.to_string_lossy());
    Ok(())
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
    state: &AppState,
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
    let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
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
pub fn cleanup_annotate_stage() {
    let Some(cache_dir) = app_cache_dir() else {
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

// the editor sends its Uint8Array as the call's raw body
fn editor_image_bytes(call: &CommandCall) -> Result<Vec<u8>, String> {
    call.body
        .clone()
        .ok_or_else(|| "expected the image bytes as the request body".to_string())
}

// header values are ascii-only, so the editor percent-encodes the path with
// encodeURIComponent, which also escapes '+', '&' and '=' and leaves one key
fn editor_target_path(call: &CommandCall) -> Result<String, String> {
    let raw = call
        .header(EDITOR_TARGET_HEADER)
        .ok_or_else(|| "missing target path".to_string())?;
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

    fn headers(value: &str) -> CommandCall {
        CommandCall {
            headers: vec![(EDITOR_TARGET_HEADER.to_string(), value.to_string())],
            ..CommandCall::default()
        }
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
        assert!(editor_target_path(&CommandCall::default()).is_err());
        assert!(editor_target_path(&headers("")).is_err());
        assert!(editor_target_path(&headers("%FF%FE.png")).is_err());
    }

    #[test]
    fn image_bytes_are_the_raw_body() {
        let raw = CommandCall {
            body: Some(vec![137, 80, 78, 71]),
            ..CommandCall::default()
        };
        assert_eq!(editor_image_bytes(&raw).unwrap(), vec![137, 80, 78, 71]);
        let json = CommandCall {
            payload: "[137,80,78,71]".to_string(),
            ..CommandCall::default()
        };
        assert!(editor_image_bytes(&json).is_err());
    }
}

pub fn save_edited_image(
    call: &CommandCall,
    app: AppHandle,
    state: &AppState,
) -> Result<String, String> {
    let target_path = editor_target_path(call)?;
    let bytes = editor_image_bytes(call)?;
    let buf = PathBuf::from(&target_path);
    // staged pre-save annotations live under the app cache; saving one mints
    // the real capture in the output dir instead of overwriting the stage.
    if let Some(cache_dir) = app_cache_dir() {
        // canonicalize both sides: AppData can sit behind a symlink or
        // junction, and a raw prefix check would then miss the stage
        let stage_dir = std::fs::canonicalize(cache_dir.join("annotate-stage"))
            .unwrap_or_else(|_| cache_dir.join("annotate-stage"));
        if let Ok(canonical) = std::fs::canonicalize(&buf) {
            if is_annotate_staged_path(&stage_dir, &canonical) {
                return save_staged_annotation(&app, state, bytes, &canonical);
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
    let _pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
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

pub fn copy_edited_image_to_clipboard(call: &CommandCall) -> Result<(), String> {
    let bytes = editor_image_bytes(call)?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.into_rgba8();
    let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
    cb.copy_image(&rgba).map_err(|e| e.to_string())
}

pub async fn upload_file(
    path: String,
    app: AppHandle,
    state: &AppState,
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

pub async fn upload_edited_image(
    call: &CommandCall,
    app: AppHandle,
    state: &AppState,
) -> Result<UploadResponse, String> {
    let bytes = editor_image_bytes(call)?;
    let config = state.config.lock().unwrap().clone();
    let service = build_upload_service(&config);
    let destination = service.label();
    let pending_mutation = PendingFileMutation::begin().map_err(|error| error.to_string())?;
    // reqwest::blocking panics on a tokio worker, which would leave the
    // editor's invoke pending forever
    let result = tokio::task::spawn_blocking(move || {
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
