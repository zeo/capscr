use super::*;

#[tauri::command]
pub fn open_in_explorer(
    path: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    // accept the history dir too, not just the output dir — the History tab
    // lists both, so "reveal in folder" must reach either
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed capture directories".into());
    }
    reveal_in_file_manager(&app, &canonical);
    Ok(())
}

#[cfg(windows)]
pub(crate) fn reveal_in_file_manager(_app: &AppHandle, path: &std::path::Path) {
    let _ = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .spawn();
}

// the opener plugin talks to the freedesktop FileManager1 dbus interface and
// falls back to plain-opening the parent directory itself
#[cfg(not(windows))]
pub(crate) fn reveal_in_file_manager(app: &AppHandle, path: &std::path::Path) {
    use tauri_plugin_opener::OpenerExt;
    if let Err(e) = app.opener().reveal_item_in_dir(path) {
        tracing::warn!("reveal in file manager failed: {e}");
    }
}

struct ExitWorkerGuard {
    app: AppHandle,
    reset_on_drop: bool,
}

impl ExitWorkerGuard {
    fn claim(app: &AppHandle) -> Option<Self> {
        app.state::<AppState>()
            .exit_worker_running
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .ok()
            .map(|_| Self {
                app: app.clone(),
                reset_on_drop: true,
            })
    }

    fn commit(mut self) {
        self.reset_on_drop = false;
    }
}

impl Drop for ExitWorkerGuard {
    fn drop(&mut self) {
        if self.reset_on_drop {
            self.app
                .state::<AppState>()
                .exit_worker_running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

pub(crate) struct MutationShutdownGuard {
    app: AppHandle,
    reset_on_drop: bool,
}

impl MutationShutdownGuard {
    pub(crate) fn claim(app: &AppHandle) -> Result<Self, &'static str> {
        app.state::<AppState>()
            .shutdown_in_progress
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .map_err(|_| "application shutdown is already in progress")?;
        Ok(Self {
            app: app.clone(),
            reset_on_drop: true,
        })
    }

    pub(crate) fn commit(mut self) {
        self.reset_on_drop = false;
    }
}

impl Drop for MutationShutdownGuard {
    fn drop(&mut self) {
        if self.reset_on_drop {
            self.app
                .state::<AppState>()
                .shutdown_in_progress
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

pub fn exit_app(app: AppHandle) {
    let Some(exit_guard) = ExitWorkerGuard::claim(&app) else {
        return;
    };
    let report_app = app.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("capscr-exit".into())
        .spawn(move || run_exit_worker(app, exit_guard))
    {
        report_app
            .state::<AppState>()
            .exit_worker_running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        emit_error(
            &report_app,
            "exit",
            &format!("couldn't start exit worker: {error}"),
        );
    }
}

fn run_exit_worker(app: AppHandle, exit_guard: ExitWorkerGuard) {
    let state = app.state::<AppState>();
    UnifiedSelector::cancel_active_selection();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let _capture_gate = loop {
        if state
            .capture_in_progress
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            break CaptureGate(&state.capture_in_progress);
        }
        if std::time::Instant::now() >= deadline {
            emit_error(
                &app,
                "exit",
                "capture is still busy; quit again when it finishes",
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let shutdown_guard = match MutationShutdownGuard::claim(&app) {
        Ok(guard) => guard,
        Err(error) => {
            emit_error(&app, "exit", error);
            return;
        }
    };
    if let Err(error) = require_editor_closed(&app, "exiting capscr") {
        emit_error(&app, "exit", &error);
        return;
    }

    // save any active recording before exiting so the user doesn't lose frames
    let cfg = state.config.lock().unwrap().clone();
    let mut recorder_slot = state.gif_recorder.lock().unwrap();
    let mut saved_recording = None;
    if let Some(mut recorder) = recorder_slot.take() {
        recorder.stop();
        // let the capture thread drain its last frames before we save, the same
        // way finalize_gif_recording does, so we don't clip the tail
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(
            recorder.state(),
            crate::recording::RecordingState::Processing
        ) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let is_mp4 = recorder.format() == crate::recording::RecordingFormat::Mp4;
        let mut path = cfg.output_path();
        path.set_extension(if is_mp4 { "mp4" } else { "gif" });
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir on exit: {e}");
        }
        let path = get_unique_filepath(&path);
        let save_result = if is_mp4 {
            recorder.save_mp4(&path).map(|_| ())
        } else {
            recorder.save(&path)
        };
        if let Err(error) = save_result {
            let _ = std::fs::remove_file(&path);
            *recorder_slot = Some(recorder);
            drop(recorder_slot);
            tracing::warn!("recording save on exit failed: {error}");
            let err_type = if is_mp4 { "mp4-save" } else { "gif-save" };
            emit_error(&app, err_type, &error.to_string());
            return;
        }
        saved_recording = Some((is_mp4, path.clone(), recording_save(&recorder, &path, None)));
    }
    drop(recorder_slot);
    if let Some((is_mp4, path, save)) = saved_recording {
        notify_capture_saved(&app, save);
        if cfg.ui.show_notifications {
            let title = if is_mp4 { "Video saved" } else { "GIF saved" };
            let _ = show_notification(title, &path.to_string_lossy());
        }
    }

    let recording_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while matches!(
        *state.recording_state.lock().unwrap(),
        crate::recording::RecordingState::Processing
    ) {
        if std::time::Instant::now() >= recording_deadline {
            emit_error(
                &app,
                "exit",
                "the recording is still being saved; quit again when it finishes",
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while state
        .pending_file_mutations
        .load(std::sync::atomic::Ordering::SeqCst)
        > 0
    {
        if std::time::Instant::now() >= deadline {
            emit_error(
                &app,
                "exit",
                "files are still being updated; quit again when they finish",
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    app.exit(0);
    shutdown_guard.commit();
    exit_guard.commit();
}
