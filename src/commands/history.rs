use super::*;

#[tauri::command]
pub fn list_captures(state: State<AppState>) -> Result<Vec<HistoryEntry>, String> {
    let config = state.config.lock().unwrap().clone();
    let dir = config.output.directory.clone();

    let mut filenames: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dir_entries: Vec<std::fs::DirEntry> = Vec::new();

    if dir.exists() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    filenames.insert(name.to_string());
                }
                dir_entries.push(entry);
            }
        }
    }

    if let Some(h_dir) = history_dir() {
        if h_dir.exists() {
            if let Ok(read) = std::fs::read_dir(&h_dir) {
                for entry in read.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if !filenames.contains(name) {
                            filenames.insert(name.to_string());
                            dir_entries.push(entry);
                        }
                    }
                }
            }
        }
    }

    let mut entries: Vec<HistoryEntry> = Vec::new();
    for entry in &dir_entries {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "avif" | "jxl" | "mp4"
        ) {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        if filename.ends_with(".hdr.png") {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified_unix = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let has_hdr = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| filenames.contains(&format!("{stem}.hdr.png")))
            .unwrap_or(false);

        let path_str = path.to_string_lossy().to_string();
        let path_clean = if let Some(stripped) = path_str.strip_prefix(r"\\?\") {
            stripped.to_string()
        } else {
            path_str
        };

        let is_mp4 = ext == "mp4";
        entries.push(HistoryEntry {
            path: path_clean,
            filename,
            size_bytes: metadata.len(),
            modified_unix,
            is_gif: ext == "gif",
            is_mp4,
            has_hdr,
            uploads: Vec::new(),
        });
    }

    entries.sort_by_key(|e| std::cmp::Reverse(e.modified_unix));
    entries.truncate(1000);
    state.with_upload_records(|records| {
        for entry in &mut entries {
            entry.uploads = records
                .entries(&entry.path)
                .iter()
                .map(|upload| UploadSummary {
                    url: upload.url.clone(),
                    destination: upload.destination.clone(),
                    uploaded_unix: upload.uploaded_unix,
                    deletable: upload.remote != crate::upload::records::RemoteRef::Url,
                })
                .collect();
        }
    });
    tracing::info!(
        "list_captures: {} entries collected (output: {}, history cache)",
        entries.len(),
        dir.display()
    );
    Ok(entries)
}

#[tauri::command]
pub fn delete_capture(
    path: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let _pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    let sidecar = canonical
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| canonical.with_file_name(format!("{stem}.hdr.png")));
    std::fs::remove_file(&canonical).map_err(|e| e.to_string())?;
    if let Some(sidecar) = sidecar {
        if sidecar != canonical {
            let _ = std::fs::remove_file(sidecar);
        }
    }
    Ok(())
}

#[tauri::command]
pub fn copy_capture_to_clipboard(path: String, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    // the image crate decodes neither avif nor jxl here, so those go as files
    if matches!(ext.as_str(), "gif" | "mp4" | "avif" | "jxl") {
        // copy the file itself so pasting inserts the animation instead of a
        // path string; fall back to path text if the file copy fails
        #[cfg(any(windows, target_os = "linux"))]
        if crate::clipboard::copy_file_to_clipboard(&canonical).is_ok() {
            return Ok(());
        }
        let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
        return cb
            .copy_text(&canonical.to_string_lossy())
            .map_err(|e| e.to_string());
    }
    let img = image::open(&canonical).map_err(|e| e.to_string())?;
    let rgba = img.into_rgba8();
    let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
    cb.copy_image(&rgba).map_err(|e| e.to_string())
}

/// static grid thumbnail for the history view. full-size animated gifs
/// dropped straight into <img> tags decode to gigabytes across a grid, and
/// files outside the asset-protocol scope render blank; a cached first-frame
/// jpeg in the app cache dir solves both. keyed by path + size + mtime.
#[tauri::command]
pub async fn history_thumbnail(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("thumbs");
    let (sender, receiver) = claim_thumbnail_job(&state, &canonical);
    if let Some(sender) = sender {
        let worker_app = app.clone();
        let worker_path = canonical.clone();
        let gate = state.thumbnail_decode_gate.clone();
        tauri::async_runtime::spawn(async move {
            let result = async {
                let _permit = gate
                    .acquire_owned()
                    .await
                    .map_err(|_| "thumbnail worker is unavailable".to_string())?;
                let _pending_mutation = PendingFileMutation::begin(&worker_app)
                    .map_err(|error| error.to_string())?;
                tokio::task::spawn_blocking(move || {
                    thumbnail_for(&cache_dir, &worker_path)
                        .map(|thumb| thumb.to_string_lossy().to_string())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())?
            }
            .await;
            let _ = sender.send(Some(result));
            worker_app
                .state::<AppState>()
                .thumbnail_jobs
                .lock()
                .unwrap()
                .remove(&canonical);
        });
    }
    await_thumbnail(receiver).await
}

fn claim_thumbnail_job(
    state: &AppState,
    path: &std::path::Path,
) -> (
    Option<tokio::sync::watch::Sender<Option<crate::state::ThumbnailResult>>>,
    crate::state::ThumbnailReceiver,
) {
    let mut jobs = state.thumbnail_jobs.lock().unwrap();
    if let Some(receiver) = jobs.get(path) {
        return (None, receiver.clone());
    }
    let (sender, receiver) =
        tokio::sync::watch::channel::<Option<crate::state::ThumbnailResult>>(None);
    jobs.insert(path.to_path_buf(), receiver.clone());
    (Some(sender), receiver)
}

async fn await_thumbnail(
    mut receiver: crate::state::ThumbnailReceiver,
) -> Result<String, String> {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        receiver
            .changed()
            .await
            .map_err(|_| "thumbnail worker stopped before completing".to_string())?;
    }
}

fn thumbnail_for(cache_dir: &std::path::Path, path: &std::path::Path) -> anyhow::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    const THUMB_WIDTH: u32 = 480;
    let meta = std::fs::metadata(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    if let Ok(modified) = meta.modified() {
        if let Ok(age) = modified.duration_since(std::time::UNIX_EPOCH) {
            age.hash(&mut hasher);
        }
    }
    std::fs::create_dir_all(cache_dir)?;
    let thumb = cache_dir.join(format!("{:016x}.jpg", hasher.finish()));
    if thumb.exists() {
        return Ok(thumb);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let frame: RgbaImage = if ext == "gif" {
        // first frame only; the grid shows a still, the viewer animates
        use image::AnimationDecoder;
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
        let decoder = image::codecs::gif::GifDecoder::new(file)?;
        decoder
            .into_frames()
            .next()
            .ok_or_else(|| anyhow::anyhow!("gif has no frames"))??
            .into_buffer()
    } else {
        image::open(path)?.to_rgba8()
    };
    let scale = (THUMB_WIDTH as f32 / frame.width() as f32).min(1.0);
    let width = ((frame.width() as f32 * scale) as u32).max(1);
    let height = ((frame.height() as f32 * scale) as u32).max(1);
    let small = image::imageops::thumbnail(&frame, width, height);
    let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
    // atomic-ish: encode to a temp name then rename, so a torn write never
    // caches as a valid thumb
    let staging =
        thumb.with_extension(format!("jpg.{}.tmp", uuid::Uuid::new_v4().as_simple()));
    if let Err(error) = rgb.save_with_format(&staging, image::ImageFormat::Jpeg) {
        let _ = std::fs::remove_file(&staging);
        return Err(error.into());
    }
    if let Err(error) = std::fs::rename(&staging, &thumb) {
        let _ = std::fs::remove_file(&staging);
        return Err(error.into());
    }
    Ok(thumb)
}

#[cfg(test)]
mod thumbnail_request_tests {
    use super::*;

    #[tokio::test]
    async fn same_path_reuses_the_active_thumbnail_job() {
        let state = AppState::new(Config::default());
        let path = std::path::Path::new("same-capture.png");

        let (owner, first) = claim_thumbnail_job(&state, path);
        let (duplicate_owner, second) = claim_thumbnail_job(&state, path);

        assert!(duplicate_owner.is_none());
        owner
            .expect("first request owns the job")
            .send(Some(Ok("cached-thumbnail.jpg".to_string())))
            .expect("publish result");
        assert_eq!(
            await_thumbnail(first).await.expect("first result"),
            "cached-thumbnail.jpg"
        );
        assert_eq!(
            await_thumbnail(second).await.expect("second result"),
            "cached-thumbnail.jpg"
        );
        assert_eq!(state.thumbnail_decode_gate.available_permits(), 2);
    }
}
