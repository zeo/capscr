use super::*;

#[cfg(any(windows, target_os = "linux"))]
static FFMPEG_DOWNLOADING: AtomicBool = AtomicBool::new(false);
#[cfg(any(windows, target_os = "linux"))]
struct FfmpegDownloadGuard {
    _mutation: PendingFileMutation,
}
#[cfg(any(windows, target_os = "linux"))]
impl FfmpegDownloadGuard {
    fn begin(app: &AppHandle) -> Result<Self, &'static str> {
        let mutation = PendingFileMutation::begin(app)?;
        FFMPEG_DOWNLOADING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "FFmpeg is already downloading")?;
        Ok(Self {
            _mutation: mutation,
        })
    }
}
#[cfg(any(windows, target_os = "linux"))]
impl Drop for FfmpegDownloadGuard {
    fn drop(&mut self) {
        FFMPEG_DOWNLOADING.store(false, Ordering::SeqCst);
    }
}
#[cfg(any(windows, target_os = "linux"))]
const MAX_FFMPEG_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(any(windows, target_os = "linux"))]
const MAX_FFMPEG_BINARY_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(any(windows, target_os = "linux"))]
const MAX_FFMPEG_REDIRECTS: usize = 5;

// a repacked static ffmpeg for linux (johnvansickle 7.0.2, md5-verified
// against the vendor before repack), hosted on the same origin capscr
// already trusts for the marketplace registry and pinned to a sha256 that
// ships in this build, which makes the mirror untrusted storage. when the
// hosted zip is replaced, update both constants together; while the hash is
// empty the download offer is hidden and users get package-manager
// instructions (a distro ffmpeg is preferred anyway).
#[cfg(target_os = "linux")]
const LINUX_FFMPEG_URL: &str = "https://rot.lt/capscr/ffmpeg/ffmpeg-linux-x64.zip";
#[cfg(target_os = "linux")]
const LINUX_FFMPEG_SHA256: &str =
    "98aaf9bae2daf2f731aa1b1bc4b22eb458caa3d89061c5b94e1e3c39e41a61e8";

// gyan.dev's versioned essentials build, cross-checked against its published
// sha256 and the GyanD/codexffmpeg release digest. gyan.dev prunes old
// packages, so a 404 here means the pin needs a bump: update both together
#[cfg(windows)]
const WINDOWS_FFMPEG_URL: &str =
    "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip";
#[cfg(windows)]
const WINDOWS_FFMPEG_SHA256: &str =
    "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba";

// download a zip over TLS, verify it against a sha256 pinned in this build,
// extract the ffmpeg binary into data_dir, and mark it executable
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn ffmpeg_redirect_allowed(url: &url::Url, previous: usize) -> bool {
    url.scheme() == "https" && previous < MAX_FFMPEG_REDIRECTS
}

#[cfg(any(windows, target_os = "linux"))]
fn ffmpeg_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if ffmpeg_redirect_allowed(attempt.url(), attempt.previous().len()) {
            attempt.follow()
        } else {
            attempt.error("ffmpeg redirect blocked")
        }
    })
}

#[cfg(any(windows, target_os = "linux"))]
fn download_verified_ffmpeg(
    url: &str,
    expected_sha256: &str,
    bin_name: &str,
) -> anyhow::Result<()> {
    let source = url::Url::parse(url)?;
    if source.scheme() != "https" {
        return Err(anyhow::anyhow!("ffmpeg download URL must use https"));
    }
    let proj_dirs = crate::config::project_dirs()
        .ok_or_else(|| anyhow::anyhow!("failed to locate app data directory"))?;
    let data_dir = proj_dirs.data_dir();
    std::fs::create_dir_all(data_dir)?;
    let dest_path = data_dir.join(bin_name);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .https_only(true)
        .redirect(ffmpeg_redirect_policy())
        .build()?;
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("http error: {}", resp.status()));
    }
    if resp
        .content_length()
        .is_some_and(|length| length > MAX_FFMPEG_ARCHIVE_BYTES)
    {
        return Err(anyhow::anyhow!("ffmpeg archive exceeds download size cap"));
    }
    let mut zip_bytes = Vec::new();
    std::io::Read::take(resp, MAX_FFMPEG_ARCHIVE_BYTES + 1).read_to_end(&mut zip_bytes)?;
    if zip_bytes.len() as u64 > MAX_FFMPEG_ARCHIVE_BYTES {
        return Err(anyhow::anyhow!("ffmpeg archive exceeds download size cap"));
    }

    // the payload is verified against a sha256 pinned in the binary before we
    // write and later execute it, so a truncated, corrupted, or tampered
    // download is rejected and the server is untrusted storage
    let expected = expected_sha256.to_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow::anyhow!(
            "ffmpeg checksum missing or malformed; refusing to install unverified binary"
        ));
    }
    let got = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&zip_bytes));
    if got != expected {
        return Err(anyhow::anyhow!(
            "ffmpeg download failed its integrity check (sha256 mismatch)"
        ));
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.as_slice()))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name()?.into_owned();
        if name.ends_with("bin/ffmpeg.exe")
            || name.ends_with("bin/ffmpeg")
            || name.ends_with("/ffmpeg")
            || name == "ffmpeg"
        {
            if file.size() > MAX_FFMPEG_BINARY_BYTES {
                return Err(anyhow::anyhow!("ffmpeg binary exceeds extraction size cap"));
            }
            let temp_path = dest_path
                .with_extension(format!("download-{}.tmp", uuid::Uuid::new_v4().as_simple()));
            let install_result = (|| -> anyhow::Result<()> {
                let mut out_file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp_path)?;
                let written = std::io::copy(
                    &mut std::io::Read::take(&mut file, MAX_FFMPEG_BINARY_BYTES + 1),
                    &mut out_file,
                )?;
                if written > MAX_FFMPEG_BINARY_BYTES {
                    return Err(anyhow::anyhow!("ffmpeg binary exceeds extraction size cap"));
                }
                drop(out_file);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&temp_path)?.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&temp_path, perms)?;
                }
                std::fs::rename(&temp_path, &dest_path)?;
                Ok(())
            })();
            if let Err(error) = install_result {
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
            return Ok(());
        }
    }
    Err(anyhow::anyhow!("ffmpeg binary not found in zip archive"))
}

#[cfg(windows)]
fn perform_ffmpeg_download() -> anyhow::Result<()> {
    download_verified_ffmpeg(WINDOWS_FFMPEG_URL, WINDOWS_FFMPEG_SHA256, "ffmpeg.exe")
}

// linux prefers a distro ffmpeg (find_ffmpeg checks PATH first and the
// packages list it as a recommend), so this only fires for an AppImage on a
// minimal system. offer the verified download when a build is pinned,
// otherwise fall back to package-manager instructions.
#[cfg(target_os = "linux")]
fn handle_missing_ffmpeg(app: &AppHandle) -> anyhow::Result<()> {
    use tauri_plugin_dialog::DialogExt;
    let instructions = "Recording MP4 requires FFmpeg, which was not found on your system.\n\n\
         Install it with your package manager, e.g.\n\
         sudo apt install ffmpeg   (Debian/Ubuntu)\n\
         sudo dnf install ffmpeg   (Fedora)";

    // the hosted build is x86_64 only; other arches get the instructions
    if LINUX_FFMPEG_SHA256.is_empty() || !cfg!(target_arch = "x86_64") {
        app.dialog()
            .message(instructions)
            .title("FFmpeg Required")
            .kind(tauri_plugin_dialog::MessageDialogKind::Info)
            .blocking_show();
        return Ok(());
    }
    if FFMPEG_DOWNLOADING.load(Ordering::SeqCst) {
        let _ = show_notification(
            "FFmpeg Download",
            "FFmpeg is already downloading in the background. Please wait.",
        );
        return Ok(());
    }
    let download = app
        .dialog()
        .message(
            "Recording MP4 requires FFmpeg, which was not found on your system.\n\n\
             Download a verified static FFmpeg (~30 MB, used only by capscr), or \
             install it with your package manager instead?",
        )
        .title("FFmpeg Required")
        .kind(tauri_plugin_dialog::MessageDialogKind::Info)
        .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
            "Download".into(),
            "Show instructions".into(),
        ))
        .blocking_show();
    if !download {
        app.dialog()
            .message(instructions)
            .title("Install FFmpeg")
            .kind(tauri_plugin_dialog::MessageDialogKind::Info)
            .blocking_show();
        return Ok(());
    }
    let download_guard = match FfmpegDownloadGuard::begin(app) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = show_notification("FFmpeg Download", error);
            return Ok(());
        }
    };
    std::thread::spawn(move || {
        let _download_guard = download_guard;
        let _ = show_notification("FFmpeg Download", "Downloading a verified FFmpeg build...");
        match download_verified_ffmpeg(LINUX_FFMPEG_URL, LINUX_FFMPEG_SHA256, "ffmpeg") {
            Ok(()) => {
                let _ = show_notification(
                    "FFmpeg Configured",
                    "FFmpeg is ready. You can now record MP4 videos.",
                );
            }
            Err(e) => {
                tracing::error!("failed to download ffmpeg: {e}");
                let _ = show_notification("FFmpeg Download Failed", &format!("{e}"));
            }
        }
    });
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn handle_missing_ffmpeg(_app: &AppHandle) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn handle_missing_ffmpeg(app: &AppHandle) -> anyhow::Result<()> {
    if FFMPEG_DOWNLOADING.load(Ordering::SeqCst) {
        let _ = show_notification(
            "FFmpeg Download",
            "FFmpeg is already downloading in the background. Please wait.",
        );
        return Ok(());
    }

    use tauri_plugin_dialog::DialogExt;
    let is_confirmed = app.dialog()
        .message("Recording MP4 requires FFmpeg, which was not found on your system.\n\nWould you like to automatically download and configure it?")
        .title("FFmpeg Required")
        .kind(tauri_plugin_dialog::MessageDialogKind::Info)
        .buttons(tauri_plugin_dialog::MessageDialogButtons::YesNo)
        .blocking_show();

    if is_confirmed {
        let download_guard = match FfmpegDownloadGuard::begin(app) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = show_notification("FFmpeg Download", error);
                return Ok(());
            }
        };
        std::thread::spawn(move || {
            let _download_guard = download_guard;
            let _ = show_notification(
                "FFmpeg Download",
                "Starting FFmpeg download (approx. 90MB)...",
            );
            match perform_ffmpeg_download() {
                Ok(()) => {
                    let _ = show_notification(
                        "FFmpeg Configured",
                        "FFmpeg has been downloaded and configured. You can now record MP4 videos.",
                    );
                }
                Err(e) => {
                    tracing::error!("failed to download ffmpeg: {}", e);
                    let _ = show_notification(
                        "FFmpeg Download Failed",
                        &format!("Could not configure FFmpeg: {}", e),
                    );
                }
            }
        });
    }

    Ok(())
}

pub(crate) fn run_gif_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    // cancel selection if already active
    if UnifiedSelector::active_selector_active() {
        UnifiedSelector::cancel_active_selection();
        return Ok(());
    }

    let state = app.state::<AppState>();
    let current = *state.recording_state.lock().unwrap();
    let armed_id = crate::recbar::armed_task_id();
    if armed_id.as_deref() == Some(task.id.as_str()) {
        crate::recbar::record(app);
        return Ok(());
    }
    let busy = armed_id.is_some() || matches!(current, RecordingState::Recording);
    let active_id = armed_id.or_else(|| state.recording_task_id.lock().unwrap().clone());

    if busy {
        // same task hotkey re-pressed -> user wants to stop
        // different task hotkey while recording -> reject and tell the user
        if active_id.as_deref() == Some(task.id.as_str()) {
            stop_gif_recording(app);
        } else {
            let active_name = {
                let cfg = state.config.lock().unwrap();
                active_id
                    .as_deref()
                    .and_then(|id| cfg.capture_tasks.iter().find(|t| t.id == id))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "another task".to_string())
            };
            emit_error(
                app,
                "recording",
                &format!(
                    "already recording '{}' — press its hotkey again to stop first",
                    active_name
                ),
            );
        }
        return Ok(());
    }

    if matches!(current, RecordingState::Processing) {
        // mid-save from a previous run; skip
        return Ok(());
    }

    if matches!(task.capture_mode, TaskCaptureMode::RegionMp4)
        && !crate::recording::is_ffmpeg_available()
    {
        handle_missing_ffmpeg(app)?;
        return Ok(());
    }

    // hold the gate until recording state is live so updater startup cannot
    // slip between the selector closing and recorder startup
    use std::sync::atomic::Ordering as OrdGif;
    if state
        .capture_in_progress
        .compare_exchange(false, true, OrdGif::SeqCst, OrdGif::SeqCst)
        .is_err()
    {
        tracing::info!("capture already in progress; dropping gif trigger");
        return Ok(());
    }
    let _capture_gate = CaptureGate(&state.capture_in_progress);
    let selection = UnifiedSelector::select(None);

    let region = match selection {
        SelectionResult::Region(r) => r,
        // the selector hands back frozen pixels with the rect; recording only
        // needs the rect
        #[cfg(target_os = "linux")]
        SelectionResult::FrozenRegion { rect, .. } => rect,
        SelectionResult::Cancelled => return Ok(()),
        _ => {
            tracing::info!("gif task '{}' aborted: needs a region selection", task.id);
            return Ok(());
        }
    };

    #[cfg(windows)]
    {
        crate::recbar::arm(app, task, region);
        Ok(())
    }
    #[cfg(not(windows))]
    start_gif_recording(task, app, region)
}

pub(crate) fn start_gif_recording(
    task: &CaptureTask,
    app: &AppHandle,
    region: Rectangle,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();

    let is_mp4 = matches!(task.capture_mode, TaskCaptureMode::RegionMp4);
    let settings = RecordingSettings {
        fps: if is_mp4 {
            cfg.capture.video_fps
        } else {
            cfg.capture.gif_fps
        },
        max_duration: Duration::from_secs(cfg.capture.gif_max_duration_secs as u64),
        quality: cfg.output.quality,
        video_crf: cfg.capture.video_quality.crf(),
        show_cursor: cfg.capture.show_cursor,
        record_audio: cfg.capture.record_audio,
        record_mic: cfg.capture.record_mic,
        format: if is_mp4 {
            crate::recording::RecordingFormat::Mp4
        } else {
            crate::recording::RecordingFormat::Gif
        },
        gif: cfg.capture.gif.clone(),
        video_encoder: cfg.capture.video_encoder,
    };

    // steer the wayland frame grabs; windows composites the cursor per frame
    #[cfg(target_os = "linux")]
    crate::capture::set_include_cursor(cfg.capture.show_cursor);

    // the recording's first grab must not catch the selector on its way
    // out: webview teardown is queued on the main thread, and even after
    // the surfaces are gone the compositor plays a close fade (~150ms on
    // kwin) that a screenshot would composite. drain the queue, then sit
    // out the fade
    #[cfg(target_os = "linux")]
    {
        let (drained_tx, drained_rx) = std::sync::mpsc::channel();
        let _ = app.run_on_main_thread(move || {
            let _ = drained_tx.send(());
        });
        let _ = drained_rx.recv_timeout(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(300));
    }

    let mut recorder = GifRecorder::new(settings).with_region(region);
    recorder.start()?;

    *state.gif_recorder.lock().unwrap() = Some(recorder);
    *state.recording_state.lock().unwrap() = RecordingState::Recording;
    *state.recording_task_id.lock().unwrap() = Some(task.id.clone());

    crate::recbar::begin(app, task, region);
    let _ = app.emit("capscr://recording-started", task.id.clone());
    set_tray_tooltip(app, &format!("capscr · recording '{}'", task.name));

    let app2 = app.clone();
    let task_owned = task.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let st = app2.state::<AppState>();

            let user_stopped = st.recording_task_id.lock().unwrap().is_none();
            let recorder_done = {
                let rec = st.gif_recorder.lock().unwrap();
                match rec.as_ref() {
                    Some(r) => !matches!(r.state(), RecordingState::Recording),
                    None => true,
                }
            };

            if user_stopped || recorder_done {
                break;
            }
        }

        finalize_gif_recording(&task_owned, &app2);
    });

    Ok(())
}

#[derive(Serialize)]
pub struct VideoEncoderSupport {
    ffmpeg: bool,
    hardware: Vec<crate::config::VideoEncoder>,
}

// the first call runs the hardware trial encodes, so it stays off the main thread
#[tauri::command]
pub async fn video_encoder_support() -> Result<VideoEncoderSupport, String> {
    tauri::async_runtime::spawn_blocking(|| VideoEncoderSupport {
        ffmpeg: crate::recording::is_ffmpeg_available(),
        hardware: crate::recording::hardware_encoders(),
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn stop_recording(app: AppHandle) {
    let recording = *app.state::<AppState>().recording_state.lock().unwrap();
    if matches!(recording, RecordingState::Recording) {
        stop_gif_recording(&app);
    }
}

fn stop_gif_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    // stop the capture thread first, then clear task_id so the monitor thread
    // doesn't call finalize before the stop signal has been sent to the encoder
    {
        let mut guard = state.gif_recorder.lock().unwrap();
        if let Some(rec) = guard.as_mut() {
            rec.stop();
        }
    }
    *state.recording_task_id.lock().unwrap() = None;
}

pub(crate) fn finalize_gif_recording(task: &CaptureTask, app: &AppHandle) {
    crate::recbar::finish(app);

    let state = app.state::<AppState>();
    *state.recording_state.lock().unwrap() = RecordingState::Processing;

    let cfg = state.config.lock().unwrap().clone();
    let mut recorder = state.gif_recorder.lock().unwrap().take();

    if let Some(ref mut rec) = recorder {
        rec.stop();
        // wait for the capture thread to finish rather than sleeping a fixed
        // duration -- the thread sets state to processing after its last frame
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(rec.state(), crate::recording::RecordingState::Processing) {
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    "gif capture thread did not finish within 5s; saving partial frames"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let is_mp4 = matches!(task.capture_mode, TaskCaptureMode::RegionMp4);

        let mut path = cfg.output_path();
        if is_mp4 {
            path.set_extension("mp4");
        } else {
            path.set_extension("gif");
        }
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }
        let path = get_unique_filepath(&path);

        let save_result = if is_mp4 {
            rec.save_mp4(&path)
        } else {
            rec.save(&path).map(|_| Vec::new())
        };

        match save_result {
            Ok(lost_audio) => {
                *state.last_save.lock().unwrap() = Some(path.clone());
                Sound::Screenshot.play_if_enabled(cfg.post_capture.play_sound);
                if cfg.ui.show_notifications {
                    let title = if is_mp4 { "Video saved" } else { "GIF saved" };
                    let _ = show_notification(title, &path.to_string_lossy());
                }
                if let Some(note) = audio_loss_note(&lost_audio) {
                    emit_error(app, "recording", note);
                }
                // a stop the user didn't ask for deserves an explanation
                let early_stop_note = match rec.stop_reason() {
                    Some(StopReason::MaxDuration) => Some(format!(
                        "hit the {}s max duration — raise it under settings → capture",
                        cfg.capture.gif_max_duration_secs
                    )),
                    Some(StopReason::FrameCap) => {
                        Some("hit the frame-count safety limit — recording saved".to_string())
                    }
                    Some(StopReason::DiskFull) => {
                        Some("stopped early: the disk is nearly full — recording saved".to_string())
                    }
                    Some(StopReason::EncoderFailed) => Some(
                        "stopped early: couldn't write frames to disk — saved what was captured"
                            .to_string(),
                    ),
                    #[cfg(target_os = "linux")]
                    Some(StopReason::SourceLost) => {
                        Some("stopped: screen sharing ended, recording saved".to_string())
                    }
                    _ => None,
                };
                if let Some(note) = early_stop_note {
                    emit_error(app, "recording", &note);
                }
                notify_capture_saved(app, recording_save(rec, &path, Some(&task.id)));
                apply_gif_post_action(task, app, &path, &cfg);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                let err_type = if is_mp4 { "mp4-save" } else { "gif-save" };
                tracing::warn!("{} failed: {e}", err_type);
                emit_error(app, err_type, &e.to_string());
            }
        }
    }

    *state.recording_state.lock().unwrap() = RecordingState::Idle;
    *state.recording_task_id.lock().unwrap() = None;
    let _ = app.emit("capscr://recording-stopped", task.id.clone());
    set_tray_tooltip(app, "capscr");
}

fn set_tray_tooltip(app: &AppHandle, tooltip: &str) {
    if let Some(tray) = app.tray_by_id("capscr-tray") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingKind {
    Mp4,
    Gif,
}

fn recording_kind(path: &std::path::Path) -> Option<RecordingKind> {
    let ext = path.extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("mp4") {
        Some(RecordingKind::Mp4)
    } else if ext.eq_ignore_ascii_case("gif") {
        Some(RecordingKind::Gif)
    } else {
        None
    }
}

const GIF_EXPORT_FPS: std::ops::RangeInclusive<u32> = 5..=30;
const GIF_EXPORT_WIDTH: std::ops::RangeInclusive<u32> = 160..=1920;
// one global palette, ordered dither, and only the changed rectangle per frame
const GIF_PALETTE_GRAPH: &str = "split[a][b];[a]palettegen=stats_mode=diff[p];\
     [b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle";

/// the ffmpeg arguments that cut [start, start + duration) out of a recording.
/// mp4 is stream-copied when `fast` (keyframe-aligned start) and re-encoded
/// otherwise; a gif is always re-encoded so frame delays stay right
fn build_trim_args(
    kind: RecordingKind,
    input: &str,
    output: &str,
    start: f64,
    duration: f64,
    fast: bool,
) -> Vec<String> {
    let start = format!("{start:.3}");
    let duration = format!("{duration:.3}");
    let args: Vec<&str> = match (kind, fast) {
        (RecordingKind::Mp4, true) => vec![
            "-ss", &start, "-i", input, "-t", &duration, "-c", "copy", "-an", "-y", output,
        ],
        (RecordingKind::Mp4, false) => vec![
            "-i", input, "-ss", &start, "-t", &duration, "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-crf", "23", "-an", "-y", output,
        ],
        // the gif demuxer can't seek, so the cut happens after decoding
        (RecordingKind::Gif, _) => vec![
            "-i",
            input,
            "-ss",
            &start,
            "-t",
            &duration,
            "-filter_complex",
            GIF_PALETTE_GRAPH,
            "-loop",
            "0",
            "-y",
            output,
        ],
    };
    args.into_iter().map(String::from).collect()
}

/// the ffmpeg arguments that turn [start, start + duration) of an mp4 into a
/// gif at `fps`, no wider than `max_width`. both are clamped to the export limits
fn build_gif_export_args(
    input: &str,
    output: &str,
    start: f64,
    duration: f64,
    fps: u32,
    max_width: u32,
) -> Vec<String> {
    let fps = fps.clamp(*GIF_EXPORT_FPS.start(), *GIF_EXPORT_FPS.end());
    let width = max_width.clamp(*GIF_EXPORT_WIDTH.start(), *GIF_EXPORT_WIDTH.end());
    let graph = format!("fps={fps},scale='min({width},iw)':-2:flags=lanczos,{GIF_PALETTE_GRAPH}");
    [
        "-ss",
        &format!("{start:.3}"),
        "-i",
        input,
        "-t",
        &format!("{duration:.3}"),
        "-filter_complex",
        &graph,
        "-loop",
        "0",
        "-an",
        "-y",
        output,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// resolve a history path for ffmpeg: canonical, inside the allowed dirs, and
/// without the \\?\ prefix canonicalize adds on windows, which the
/// network-path guard in check_recording_source would otherwise reject
fn resolve_recording_path(path: &str, config: &Config) -> Result<String, String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, config) {
        return Err("Path is outside the allowed directories".into());
    }
    #[cfg(windows)]
    let resolved = {
        let s = canonical.to_string_lossy();
        match s.strip_prefix(r"\\?\") {
            Some(rest) => match rest.strip_prefix(r"UNC\") {
                Some(unc) => format!(r"\\{unc}"),
                None => rest.to_string(),
            },
            None => s.into_owned(),
        }
    };
    #[cfg(not(windows))]
    let resolved = canonical.to_string_lossy().into_owned();
    Ok(resolved)
}

/// the recording kind and clip length for a cut, after the path and range checks
fn check_recording_source(
    path: &str,
    start_secs: f64,
    end_secs: f64,
) -> anyhow::Result<(RecordingKind, f64, f64)> {
    if path.contains("..") {
        anyhow::bail!("path contains directory traversal");
    }
    #[cfg(windows)]
    if path.starts_with("\\\\") {
        anyhow::bail!("network paths not allowed");
    }
    let src = std::path::Path::new(path);
    if !src.exists() {
        anyhow::bail!("file not found");
    }
    let kind = recording_kind(src).ok_or_else(|| anyhow::anyhow!("not an mp4 or gif file"))?;
    let start = start_secs.max(0.0);
    let duration = end_secs - start;
    if !duration.is_finite() || duration <= 0.05 {
        anyhow::bail!("trim must be at least 0.05s and end after start");
    }
    Ok((kind, start, duration))
}

/// run ffmpeg with `args` writing `out`, removing a failed or empty output
fn run_ffmpeg_to(out: &std::path::Path, args: &[String]) -> anyhow::Result<()> {
    let output = crate::recording::ffmpeg_command()
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| {
            let _ = std::fs::remove_file(out);
            anyhow::anyhow!("failed to launch ffmpeg: {e}")
        })?;
    if !output.status.success() {
        let _ = std::fs::remove_file(out);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.trim().lines().last().unwrap_or("ffmpeg error");
        anyhow::bail!("ffmpeg failed: {tail}");
    }
    if std::fs::metadata(out).map_or(true, |m| m.len() == 0) {
        let _ = std::fs::remove_file(out);
        anyhow::bail!("ffmpeg produced no output file");
    }
    Ok(())
}

/// `<stem><suffix>.<ext>` next to the source, numbered when that name is taken
fn sibling_output(src: &str, suffix: &str, ext: &str) -> std::path::PathBuf {
    let src = std::path::Path::new(src);
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    let parent = src.parent().unwrap_or_else(|| std::path::Path::new("."));
    crate::clipboard::get_unique_filepath(&parent.join(format!("{stem}{suffix}.{ext}")))
}

async fn run_recording_job(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
    job: impl FnOnce(&str) -> anyhow::Result<String> + Send + 'static,
) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let resolved = resolve_recording_path(&path, &config)?;
    let pending_mutation = PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _pending_mutation = pending_mutation;
        job(&resolved).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// trim an mp4 or gif recording to [start_secs, end_secs) into a new file next
// to the source and return its path. recordings are video-only, so audio is dropped
#[tauri::command]
pub async fn trim_recording(
    path: String,
    start_secs: f64,
    end_secs: f64,
    fast: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    run_recording_job(path, app, state, move |src| {
        let (kind, start, duration) = check_recording_source(src, start_secs, end_secs)?;
        let ext = match kind {
            RecordingKind::Mp4 => "mp4",
            RecordingKind::Gif => "gif",
        };
        let out = sibling_output(src, "_trim", ext);
        let out_str = out.to_string_lossy().into_owned();
        run_ffmpeg_to(
            &out,
            &build_trim_args(kind, src, &out_str, start, duration, fast),
        )?;
        Ok(out_str)
    })
    .await
}

// convert [start_secs, end_secs) of an mp4 recording to a gif next to it
#[tauri::command]
pub async fn export_gif(
    path: String,
    start_secs: f64,
    end_secs: f64,
    fps: u32,
    max_width: u32,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    run_recording_job(path, app, state, move |src| {
        let (kind, start, duration) = check_recording_source(src, start_secs, end_secs)?;
        if kind != RecordingKind::Mp4 {
            anyhow::bail!("only an mp4 recording can be exported as a gif");
        }
        let out = sibling_output(src, "", "gif");
        let out_str = out.to_string_lossy().into_owned();
        run_ffmpeg_to(
            &out,
            &build_gif_export_args(src, &out_str, start, duration, fps, max_width),
        )?;
        Ok(out_str)
    })
    .await
}

/// a gif's play time in seconds, counting delays the way ffmpeg's gif demuxer
/// does so the trim range lines up with what ffmpeg cuts
fn gif_duration_secs(reader: impl std::io::Read) -> anyhow::Result<f64> {
    let mut decoder = gif::DecodeOptions::new().read_info(reader)?;
    let mut hundredths = 0u64;
    while let Some(frame) = decoder.next_frame_info()? {
        hundredths += match frame.delay {
            0 | 1 => 10,
            delay => u64::from(delay),
        };
    }
    Ok(hundredths as f64 / 100.0)
}

#[tauri::command]
pub async fn gif_duration(path: String, state: State<'_, AppState>) -> Result<f64, String> {
    let config = state.config.lock().unwrap().clone();
    let resolved = resolve_recording_path(&path, &config)?;
    tauri::async_runtime::spawn_blocking(move || {
        let file = std::fs::File::open(&resolved).map_err(|e| e.to_string())?;
        gif_duration_secs(std::io::BufReader::new(file)).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// what to tell the user when requested audio tracks are missing from a recording
fn audio_loss_note(lost: &[crate::recording::AudioSource]) -> Option<&'static str> {
    use crate::recording::AudioSource;
    match lost {
        [] => None,
        [AudioSource::System] => {
            Some("saved without system audio: the track couldn't be captured or muxed")
        }
        [AudioSource::Mic] => Some(
            "saved without narration: the microphone was unavailable or its track couldn't be muxed",
        ),
        _ => Some("saved without audio: neither system audio nor the microphone made it into the file"),
    }
}

fn apply_gif_post_action(
    task: &CaptureTask,
    app: &AppHandle,
    path: &std::path::Path,
    cfg: &Config,
) {
    match task.post_action {
        TaskPostAction::Clipboard | TaskPostAction::SaveAndClipboard => {
            // animated gif/mp4 can't go on the clipboard as pixels without
            // flattening, so copy the saved file itself (CF_HDROP) — pasting
            // into explorer/discord/slack then inserts the actual file
            #[cfg(any(windows, target_os = "linux"))]
            let copied = match crate::clipboard::copy_file_to_clipboard(path) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("file clipboard copy failed: {e}; falling back to path text");
                    false
                }
            };
            #[cfg(not(any(windows, target_os = "linux")))]
            let copied = false;
            if !copied {
                if let Ok(mut cb) = ClipboardManager::new() {
                    let _ = cb.copy_text(&path.to_string_lossy());
                }
            }
        }
        TaskPostAction::OpenEditor | TaskPostAction::Annotate | TaskPostAction::Prompt => {
            // recordings can't be annotated without flattening their frames, so
            // reveal the saved file in the file manager instead of opening the editor
            reveal_in_file_manager(app, path);
        }
        TaskPostAction::Upload => {
            let app2 = app.clone();
            let path = path.to_path_buf();
            let cfg = cfg.clone();
            let target_override = task.target_destination;
            let pending_mutation = match PendingFileMutation::begin(&app2) {
                Ok(mutation) => mutation,
                Err(error) => {
                    emit_error(&app2, "upload", error);
                    return;
                }
            };
            std::thread::spawn(move || {
                let _pending_mutation = pending_mutation;
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        emit_error(&app2, "upload", &e.to_string());
                        return;
                    }
                };
                let uploader = match crate::upload::shared_uploader() {
                    Ok(u) => u,
                    Err(e) => {
                        emit_error(&app2, "upload", &e.to_string());
                        return;
                    }
                };
                let service = build_upload_service_for_target(&cfg, target_override);
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("gif");
                let mime = crate::upload::mime_for(ext).unwrap_or("image/gif");
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("capture.gif");
                match uploader.upload_raw(&bytes, mime, file_name, &service) {
                    Ok(result) => {
                        finish_upload(&app2, &cfg, &result, service.label(), Some(&path));
                        Sound::Upload.play_if_enabled(cfg.post_capture.play_sound);
                    }
                    Err(e) => emit_error(&app2, "upload", &e.to_string()),
                }
            });
        }
        // CopyText (OCR) is a still-only action, filtered out of the recording
        // post-action list in the UI; if one reaches here the file is saved and
        // there's nothing sensible to OCR from a recording
        TaskPostAction::SaveFile | TaskPostAction::DoNothing | TaskPostAction::CopyText => {
            // already saved to disk; nothing further
        }
    }
}

#[cfg(test)]
mod recording_edit_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recording_kind_reads_the_extension() {
        assert_eq!(recording_kind(Path::new("a.mp4")), Some(RecordingKind::Mp4));
        assert_eq!(recording_kind(Path::new("a.MP4")), Some(RecordingKind::Mp4));
        assert_eq!(recording_kind(Path::new("a.gif")), Some(RecordingKind::Gif));
        assert_eq!(recording_kind(Path::new("a.png")), None);
        assert_eq!(recording_kind(Path::new("mp4")), None);
    }

    #[test]
    fn trim_args_per_kind() {
        let fast = build_trim_args(RecordingKind::Mp4, "in.mp4", "out.mp4", 1.0, 2.5, true);
        assert_eq!(
            fast,
            ["-ss", "1.000", "-i", "in.mp4", "-t", "2.500", "-c", "copy", "-an", "-y", "out.mp4"]
        );
        let exact = build_trim_args(RecordingKind::Mp4, "in.mp4", "out.mp4", 1.0, 2.5, false);
        assert_eq!(&exact[..6], ["-i", "in.mp4", "-ss", "1.000", "-t", "2.500"]);
        assert!(exact.iter().any(|a| a == "libx264"));
        let gif = build_trim_args(RecordingKind::Gif, "in.gif", "out.gif", 0.0, 3.0, true);
        assert_eq!(&gif[..6], ["-i", "in.gif", "-ss", "0.000", "-t", "3.000"]);
        assert!(gif.iter().any(|a| a.contains("palettegen")));
        assert!(!gif.iter().any(|a| a == "copy"));
        assert_eq!(gif.last().unwrap(), "out.gif");
    }

    #[test]
    fn gif_export_clamps_fps_and_width() {
        let graph = |fps, width| {
            build_gif_export_args("in.mp4", "out.gif", 0.0, 1.0, fps, width)
                .into_iter()
                .skip_while(|a| a != "-filter_complex")
                .nth(1)
                .unwrap()
        };
        assert!(graph(15, 960).starts_with("fps=15,scale='min(960,iw)':-2:flags=lanczos,split"));
        assert!(graph(1, 10).starts_with("fps=5,scale='min(160,iw)'"));
        assert!(graph(120, 8000).starts_with("fps=30,scale='min(1920,iw)'"));
    }

    #[test]
    fn recording_source_checks() {
        let dir = tempfile::tempdir().unwrap();
        let mp4 = dir.path().join("clip.mp4");
        let png = dir.path().join("shot.png");
        std::fs::write(&mp4, b"x").unwrap();
        std::fs::write(&png, b"x").unwrap();
        let mp4 = mp4.to_string_lossy().into_owned();
        assert_eq!(
            check_recording_source(&mp4, -1.0, 2.0).unwrap(),
            (RecordingKind::Mp4, 0.0, 2.0)
        );
        assert!(check_recording_source(&mp4, 1.0, 1.01).is_err());
        assert!(check_recording_source(&mp4, 2.0, 1.0).is_err());
        assert!(check_recording_source(&mp4, 0.0, f64::NAN).is_err());
        assert!(check_recording_source(&png.to_string_lossy(), 0.0, 1.0).is_err());
        let missing = dir.path().join("gone.gif");
        assert!(check_recording_source(&missing.to_string_lossy(), 0.0, 1.0).is_err());
        let traversal = format!("{}/../clip.mp4", dir.path().display());
        assert!(check_recording_source(&traversal, 0.0, 1.0).is_err());
    }

    #[test]
    fn gif_duration_counts_short_delays_like_ffmpeg() {
        let mut bytes = Vec::new();
        {
            let mut encoder =
                gif::Encoder::new(&mut bytes, 1, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
            for delay in [0, 5, 20] {
                let mut frame = gif::Frame::from_indexed_pixels(1, 1, vec![0], None);
                frame.delay = delay;
                encoder.write_frame(&frame).unwrap();
            }
        }
        let secs = gif_duration_secs(std::io::Cursor::new(bytes)).unwrap();
        assert!((secs - 0.35).abs() < 1e-9, "{secs}");
    }
}
