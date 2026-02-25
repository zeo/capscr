pub mod components;
pub mod style;
pub mod views;

use iced::{Color, Element, Point, Task, Theme};
use image::RgbaImage;
use std::sync::Arc;
use std::time::Duration;

use crate::capture::{Capture, Rectangle, RegionCapture, ScreenCapture, WindowCapture};
use crate::clipboard::{get_unique_filepath, save_image, show_notification, ClipboardManager};
use crate::config::{Config, ImageFormat, PostCaptureAction, RendererBackend, UploadDestination};
use crate::hotkeys::{HotkeyAction, HotkeyManager};
use crate::overlay::{RecordingOverlay, SelectionResult, UnifiedSelector};
use crate::plugin::{CaptureType, PluginEvent, PluginManager, PluginResponse};
use crate::recording::{GifRecorder, RecordingSettings, RecordingState};
use crate::sound::Sound;
use crate::tray::{TrayAction, TrayManager};
use crate::upload::{CustomUploader, UploadService};

use self::style::MonochromeTheme;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    SetFormat(ImageFormat),
    HotkeyTriggered(HotkeyAction),
    TrayAction(TrayAction),
    CaptureComplete(Result<String, String>),
    GifSaved(Result<String, String>),
    Tick,
    ImageCaptured(CapturedImage),
    PostCaptureAction(PostCaptureAction),
    SaveAs,
    SaveAsPath(Option<std::path::PathBuf>),
    UploadComplete(Result<(String, Option<String>), String>),
    CopyToClipboard,
    DismissPostCapture,
    OpenEditor,
    EditorStartStroke(Point),
    EditorAddPoint(Point),
    EditorEndStroke,
    EditorSetTool(views::DrawTool),
    EditorSetColor(Color),
    EditorClear,
    EditorDone,
    EditorCancel,
    OpenSettings,
    CloseSettings,
    SettingChanged(SettingChange),
    TriggerScreenshot,
    TriggerGifRecording,
    SelectionComplete(SelectionResult),
    GifSelectionComplete(SelectionResult),
    ExitApp,
    StartHotkeyRecording(HotkeyTarget),
    CancelHotkeyRecording,
    KeyPressed(iced::keyboard::Key, iced::keyboard::Modifiers),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyTarget {
    Screenshot,
    RecordGif,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SettingChange {
    OutputDirectory(std::path::PathBuf),
    Format(ImageFormat),
    Quality(u8),
    ScreenshotHotkey(String),
    GifHotkey(String),
    GifFps(u32),
    GifMaxDuration(u32),
    ShowNotifications(bool),
    PostCaptureAction(PostCaptureAction),
    Theme(crate::config::Theme),
    PlaySound(bool),
    UploadDestination(UploadDestination),
    CustomUploadUrl(String),
    CopyUrlToClipboard(bool),
    TickIntervalMs(u32),
    RendererBackend(RendererBackend),
    LazyInitUpload(bool),
    LazyInitPlugins(bool),
}

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub image: Arc<RgbaImage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Hidden,
    PostCapture,
    Editor,
    Settings,
}

pub struct App {
    config: Config,
    theme: MonochromeTheme,
    view: View,
    recording_state: RecordingState,
    recording_region: Option<Rectangle>,
    gif_recorder: Option<GifRecorder>,
    clipboard: Option<ClipboardManager>,
    hotkey_manager: Option<HotkeyManager>,
    tray_manager: Option<TrayManager>,
    plugin_manager: PluginManager,
    pending_image: Option<Arc<RgbaImage>>,
    last_upload_url: Option<String>,
    last_delete_url: Option<String>,
    last_save_path: Option<std::path::PathBuf>,
    editor_state: Option<views::EditorState>,
    settings_state: views::SettingsState,
    recording_hotkey: Option<HotkeyTarget>,
}

const ICON_DATA: &[u8] = include_bytes!("../../icon.ico");

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let config = Config::load().unwrap_or_default();
        let theme = match config.ui.theme {
            crate::config::Theme::Dark => MonochromeTheme::dark(),
            crate::config::Theme::Light => MonochromeTheme::light(),
        };

        let clipboard = ClipboardManager::new().ok();

        let mut hotkey_manager = HotkeyManager::new().ok();
        if let Some(ref mut hm) = hotkey_manager {
            hm.try_register(HotkeyAction::Screenshot, &config.hotkeys.screenshot);
            hm.try_register(HotkeyAction::RecordGif, &config.hotkeys.record_gif);

            for err in hm.take_errors() {
                let msg = format!(
                    "Failed to register {} hotkey ({}): {}",
                    err.action.display_name(),
                    err.hotkey,
                    err.reason
                );
                tracing::warn!("{}", msg);
                if config.ui.show_notifications {
                    let _ = crate::clipboard::show_notification("Hotkey Error", &msg);
                }
            }
        }

        let tray_manager = TrayManager::new(ICON_DATA).ok();

        let mut plugin_manager = PluginManager::new();
        plugin_manager.set_lazy_loading(config.performance.lazy_init_plugins);
        let plugin_errors = plugin_manager.load_all();
        for err in plugin_errors {
            tracing::warn!("Plugin load error: {}", err);
        }

        if !config.performance.lazy_init_upload {
            if let Err(e) = crate::upload::shared_uploader() {
                tracing::warn!("Upload client init error: {}", e);
            }
        }

        let settings_state = views::SettingsState::from_config(&config);

        let app = Self {
            config,
            theme,
            view: View::Hidden,
            recording_state: RecordingState::Idle,
            recording_region: None,
            gif_recorder: None,
            clipboard,
            hotkey_manager,
            tray_manager,
            plugin_manager,
            pending_image: None,
            last_upload_url: None,
            last_delete_url: None,
            last_save_path: None,
            editor_state: None,
            settings_state,
            recording_hotkey: None,
        };

        (app, Task::none())
    }

    pub fn title(&self) -> String {
        match self.view {
            View::Hidden => String::from("capscr"),
            View::PostCapture => String::from("capscr - Capture Complete"),
            View::Editor => String::from("capscr - Editor"),
            View::Settings => String::from("capscr - Settings"),
        }
    }

    pub fn theme(&self) -> Theme {
        if self.theme.is_dark {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SetFormat(format) => {
                self.config.output.format = format;
                let _ = self.config.save();
            }
            Message::HotkeyTriggered(action) => match action {
                HotkeyAction::Screenshot => {
                    tracing::info!("flow=screenshot trigger=hotkey");
                    return Task::done(Message::TriggerScreenshot);
                }
                HotkeyAction::RecordGif => {
                    tracing::info!("flow=gif trigger=hotkey");
                    return Task::done(Message::TriggerGifRecording);
                }
            },
            Message::TrayAction(action) => match action {
                TrayAction::Screenshot => {
                    tracing::info!("flow=screenshot trigger=tray");
                    return Task::done(Message::TriggerScreenshot);
                }
                TrayAction::RecordGif => {
                    tracing::info!("flow=gif trigger=tray");
                    return Task::done(Message::TriggerGifRecording);
                }
                TrayAction::Settings => {
                    return Task::done(Message::OpenSettings);
                }
                TrayAction::Exit => {
                    return Task::done(Message::ExitApp);
                }
            },
            Message::TriggerScreenshot => {
                tracing::info!("flow=screenshot trigger=app");
                return Task::perform(
                    async move { UnifiedSelector::select() },
                    Message::SelectionComplete,
                );
            }
            Message::TriggerGifRecording => {
                if self.recording_state == RecordingState::Recording {
                    tracing::info!("flow=gif action=stop-requested");
                    return self.stop_gif_recording();
                } else if self.recording_state == RecordingState::Idle {
                    tracing::info!("flow=gif action=start-requested");
                    return Task::perform(
                        async move { UnifiedSelector::select() },
                        Message::GifSelectionComplete,
                    );
                }
            }
            Message::SelectionComplete(result) => match result {
                SelectionResult::Region(rect) => {
                    return self.capture_region(rect);
                }
                SelectionResult::Window(hwnd) => {
                    return self.capture_window(hwnd);
                }
                SelectionResult::FullScreen => {
                    return self.capture_fullscreen();
                }
                SelectionResult::Cancelled => {}
            },
            Message::GifSelectionComplete(result) => {
                let region = match result {
                    SelectionResult::Region(rect) => Some(rect),
                    SelectionResult::Window(hwnd) => self.get_window_rect(hwnd),
                    SelectionResult::FullScreen => None,
                    SelectionResult::Cancelled => return Task::none(),
                };
                return self.start_gif_recording(region);
            }
            Message::CaptureComplete(result) => match result {
                Ok(path) => {
                    Sound::Screenshot.play_if_enabled(self.config.post_capture.play_sound);
                    self.last_save_path = Some(std::path::PathBuf::from(&path));
                    if let Some(ref mut cb) = self.clipboard {
                        let _ = cb.copy_file_path(&path);
                    }
                    if self.config.ui.show_notifications {
                        let _ =
                            show_notification("Capture Complete", &format!("Saved to {}", path));
                    }
                }
                Err(e) => {
                    if self.config.ui.show_notifications {
                        let _ = show_notification("Capture Failed", &e);
                    }
                }
            },
            Message::GifSaved(result) => {
                self.recording_state = RecordingState::Idle;
                self.recording_region = None;
                if let Some(ref mut recorder) = self.gif_recorder {
                    recorder.reset();
                }
                self.gif_recorder = None;
                if let Some(ref mut tray) = self.tray_manager {
                    tray.set_recording(false);
                }
                match result {
                    Ok(path) => {
                        Sound::Screenshot.play_if_enabled(self.config.post_capture.play_sound);
                        if self.config.ui.show_notifications {
                            let _ = show_notification("GIF Saved", &format!("Saved to {}", path));
                        }
                    }
                    Err(e) => {
                        if self.config.ui.show_notifications {
                            let _ = show_notification("GIF Save Failed", &e);
                        }
                    }
                }
            }
            Message::Tick => {
                if let Some(ref hm) = self.hotkey_manager {
                    if let Some(action) = hm.poll() {
                        return Task::done(Message::HotkeyTriggered(action));
                    }
                }
                if let Some(ref mut tray) = self.tray_manager {
                    if !tray.is_valid() {
                        tray.try_recreate();
                    }
                    if let Some(action) = tray.poll() {
                        return Task::done(Message::TrayAction(action));
                    }
                }
            }
            Message::ImageCaptured(captured) => {
                let mut image = captured.image;

                let event = PluginEvent::PostCapture {
                    image: image.clone(),
                    mode: CaptureType::FullScreen,
                };
                tracing::info!("flow=plugin stage=post-capture dispatch=true");
                match self.plugin_manager.dispatch(&event) {
                    PluginResponse::Cancel => {
                        tracing::info!("flow=plugin stage=post-capture result=cancel");
                        return Task::none();
                    }
                    PluginResponse::ModifiedImage(modified) => {
                        tracing::info!("flow=plugin stage=post-capture result=modified");
                        image = modified;
                    }
                    PluginResponse::Continue => {
                        tracing::info!("flow=plugin stage=post-capture result=continue");
                    }
                }

                self.pending_image = Some(image);
                match self.config.post_capture.action {
                    PostCaptureAction::PromptUser => {
                        self.view = View::PostCapture;
                    }
                    PostCaptureAction::SaveToFile => {
                        return self.save_pending_image();
                    }
                    PostCaptureAction::CopyToClipboard => {
                        return self.copy_pending_to_clipboard();
                    }
                    PostCaptureAction::SaveAndCopy => {
                        let copy_task = self.copy_pending_to_clipboard();
                        let save_task = self.save_pending_image();
                        return Task::batch([copy_task, save_task]);
                    }
                    PostCaptureAction::Upload => {
                        return self.upload_pending_image();
                    }
                }
            }
            Message::PostCaptureAction(action) => {
                self.view = View::Hidden;
                match action {
                    PostCaptureAction::SaveToFile => {
                        return self.save_pending_image();
                    }
                    PostCaptureAction::CopyToClipboard => {
                        return self.copy_pending_to_clipboard();
                    }
                    PostCaptureAction::SaveAndCopy => {
                        let copy_task = self.copy_pending_to_clipboard();
                        let save_task = self.save_pending_image();
                        return Task::batch([copy_task, save_task]);
                    }
                    PostCaptureAction::Upload => {
                        return self.upload_pending_image();
                    }
                    PostCaptureAction::PromptUser => {}
                }
            }
            Message::SaveAs => {
                let format = self.config.output.format;
                return Task::perform(
                    async move {
                        let filter_name = format.display_name();
                        let filter_ext = format.extension();
                        let dialog = rfd::AsyncFileDialog::new()
                            .add_filter(filter_name, &[filter_ext])
                            .set_file_name(format!("capture.{}", filter_ext));
                        dialog.save_file().await.map(|h| h.path().to_path_buf())
                    },
                    Message::SaveAsPath,
                );
            }
            Message::SaveAsPath(path_opt) => {
                self.view = View::Hidden;
                if let Some(path) = path_opt {
                    if let Err(e) = Self::validate_save_path(&path) {
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Save Failed", &e);
                        }
                        self.pending_image = None;
                        return Task::none();
                    }

                    if let Some(ref image) = self.pending_image {
                        let image = image.clone();
                        let format = self.config.output.format;
                        let quality = self.config.output.quality;
                        self.pending_image = None;
                        return Task::perform(
                            async move {
                                match save_image(&image, &path, format, quality) {
                                    Ok(()) => Ok(path.to_string_lossy().to_string()),
                                    Err(e) => Err(e.to_string()),
                                }
                            },
                            Message::CaptureComplete,
                        );
                    }
                }
                self.pending_image = None;
            }
            Message::UploadComplete(result) => {
                self.view = View::Hidden;
                self.pending_image = None;
                match result {
                    Ok((url, delete_url)) => {
                        tracing::info!("flow=upload result=success");
                        Sound::Upload.play_if_enabled(self.config.post_capture.play_sound);
                        self.last_upload_url = Some(url.clone());
                        self.last_delete_url = delete_url.clone();
                        if self.config.upload.copy_url_to_clipboard {
                            let _ = crate::upload::copy_url_to_clipboard(&url);
                        }
                        let msg = if let Some(ref del) = delete_url {
                            format!("{}\nDelete: {}", url, del)
                        } else {
                            url.clone()
                        };
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Upload Complete", &msg);
                        }
                    }
                    Err(e) => {
                        tracing::info!("flow=upload result=failure");
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Upload Failed", &e);
                        }
                    }
                }
            }
            Message::CopyToClipboard => {
                return self.copy_pending_to_clipboard();
            }
            Message::DismissPostCapture => {
                self.view = View::Hidden;
                self.pending_image = None;
            }
            Message::OpenEditor => {
                if let Some(ref image) = self.pending_image {
                    let (w, h) = (image.width(), image.height());
                    self.editor_state = Some(views::EditorState::new(w, h));
                    self.view = View::Editor;
                }
            }
            Message::EditorStartStroke(pos) => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.start_stroke(pos);
                }
            }
            Message::EditorAddPoint(pos) => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.add_point(pos);
                }
            }
            Message::EditorEndStroke => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.end_stroke();
                }
            }
            Message::EditorSetTool(tool) => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.set_tool(tool);
                }
            }
            Message::EditorSetColor(color) => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.set_color(color);
                }
            }
            Message::EditorClear => {
                if let Some(ref mut editor) = self.editor_state {
                    editor.clear();
                }
            }
            Message::EditorDone => {
                if let (Some(editor), Some(image)) = (&self.editor_state, &self.pending_image) {
                    let edited_image = editor.apply_to_image(image);
                    self.pending_image = Some(Arc::new(edited_image));
                }
                self.editor_state = None;
                self.view = View::PostCapture;
            }
            Message::EditorCancel => {
                self.editor_state = None;
                self.view = View::PostCapture;
            }
            Message::OpenSettings => {
                self.settings_state = views::SettingsState::from_config(&self.config);
                self.view = View::Settings;
            }
            Message::CloseSettings => {
                self.view = View::Hidden;
            }
            Message::SettingChanged(change) => {
                self.apply_setting_change(change);
            }
            Message::ExitApp => {
                std::process::exit(0);
            }
            Message::StartHotkeyRecording(target) => {
                self.recording_hotkey = Some(target);
            }
            Message::CancelHotkeyRecording => {
                self.recording_hotkey = None;
            }
            Message::KeyPressed(key, modifiers) => {
                if let Some(target) = self.recording_hotkey {
                    if let iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) = key {
                        self.recording_hotkey = None;
                    } else if let Some(hotkey_str) = self.build_hotkey_string(&key, &modifiers) {
                        match target {
                            HotkeyTarget::Screenshot => {
                                if let Some(ref mut hm) = self.hotkey_manager {
                                    let _ = hm.unregister(HotkeyAction::Screenshot);
                                    let _ = hm.register(HotkeyAction::Screenshot, &hotkey_str);
                                }
                                self.config.hotkeys.screenshot = hotkey_str;
                            }
                            HotkeyTarget::RecordGif => {
                                if let Some(ref mut hm) = self.hotkey_manager {
                                    let _ = hm.unregister(HotkeyAction::RecordGif);
                                    let _ = hm.register(HotkeyAction::RecordGif, &hotkey_str);
                                }
                                self.config.hotkeys.record_gif = hotkey_str;
                            }
                        }
                        let _ = self.config.save();
                        self.settings_state = views::SettingsState::from_config(&self.config);
                        self.recording_hotkey = None;
                    }
                }
            }
        }
        Task::none()
    }

    fn capture_fullscreen(&mut self) -> Task<Message> {
        Task::perform(
            async move {
                use crate::capture::list_monitors;
                let monitors = list_monitors().unwrap_or_default();
                let capture = if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
                    ScreenCapture::with_monitor(primary.id)
                } else {
                    ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())
                };
                capture.capture()
            },
            |result| match result {
                Ok(image) => Message::ImageCaptured(CapturedImage {
                    image: Arc::new(image),
                }),
                Err(e) => Message::CaptureComplete(Err(e.to_string())),
            },
        )
    }

    fn capture_window(&mut self, hwnd: u32) -> Task<Message> {
        Task::perform(
            async move {
                let capture = WindowCapture::new(hwnd);
                capture.capture()
            },
            |result| match result {
                Ok(image) => Message::ImageCaptured(CapturedImage {
                    image: Arc::new(image),
                }),
                Err(e) => Message::CaptureComplete(Err(e.to_string())),
            },
        )
    }

    fn capture_region(&mut self, rect: Rectangle) -> Task<Message> {
        Task::perform(
            async move {
                let capture = RegionCapture::new(rect);
                capture.capture()
            },
            |result| match result {
                Ok(image) => Message::ImageCaptured(CapturedImage {
                    image: Arc::new(image),
                }),
                Err(e) => Message::CaptureComplete(Err(e.to_string())),
            },
        )
    }

    #[allow(unused_variables)]
    fn get_window_rect(&self, hwnd: u32) -> Option<Rectangle> {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Foundation::RECT;
            use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

            unsafe {
                let mut rect = RECT::default();
                if GetWindowRect(HWND(hwnd as *mut _), &mut rect).is_ok() {
                    let width = (rect.right - rect.left) as u32;
                    let height = (rect.bottom - rect.top) as u32;
                    return Some(Rectangle::new(rect.left, rect.top, width, height));
                }
            }
        }
        None
    }

    fn start_gif_recording(&mut self, region: Option<Rectangle>) -> Task<Message> {
        let settings = RecordingSettings {
            fps: self.config.capture.gif_fps,
            max_duration: std::time::Duration::from_secs(
                self.config.capture.gif_max_duration_secs as u64,
            ),
            quality: self.config.output.quality,
        };

        let actual_region = region.unwrap_or_else(|| {
            use crate::capture::list_monitors;
            if let Ok(monitors) = list_monitors() {
                if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
                    return Rectangle::new(0, 0, primary.width, primary.height);
                }
            }
            Rectangle::new(0, 0, 1920, 1080)
        });
        let mut recorder = GifRecorder::new(settings).with_region(actual_region);

        if recorder.start().is_ok() {
            tracing::info!("flow=gif state=recording-started");
            self.recording_state = RecordingState::Recording;
            self.recording_region = Some(actual_region);
            self.gif_recorder = Some(recorder);
            if let Some(ref mut tray) = self.tray_manager {
                tray.set_recording(true);
            }
            RecordingOverlay::start(actual_region);
            if self.config.ui.show_notifications {
                let _ = show_notification("Recording Started", "Press Ctrl+Shift+G to stop");
            }
        }
        Task::none()
    }

    fn stop_gif_recording(&mut self) -> Task<Message> {
        RecordingOverlay::stop();
        if let Some(ref mut recorder) = self.gif_recorder {
            recorder.stop();
            self.recording_state = RecordingState::Processing;
            tracing::info!("flow=gif state=processing");

            let output_dir = self.config.output.directory.clone();
            let filename = format!(
                "recording_{}.gif",
                chrono::Local::now().format("%Y%m%d_%H%M%S")
            );
            let output_path = output_dir.join(filename);

            let recorder = self.gif_recorder.take();
            return Task::perform(
                async move {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Some(rec) = recorder {
                        std::fs::create_dir_all(&output_dir).ok();
                        match rec.save(&output_path) {
                            Ok(()) => Ok(output_path.to_string_lossy().to_string()),
                            Err(e) => Err(e.to_string()),
                        }
                    } else {
                        Err("No recorder available".to_string())
                    }
                },
                Message::GifSaved,
            );
        }
        Task::none()
    }

    fn save_pending_image(&mut self) -> Task<Message> {
        if let Some(ref image) = self.pending_image {
            let image = image.clone();
            let format = self.config.output.format;
            let quality = self.config.output.quality;
            let base_path = self.config.output_path();
            let output_path = get_unique_filepath(&base_path);
            self.pending_image = None;
            return Task::perform(
                async move {
                    match save_image(&image, &output_path, format, quality) {
                        Ok(()) => Ok(output_path.to_string_lossy().to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                },
                Message::CaptureComplete,
            );
        }
        Task::none()
    }

    fn copy_pending_to_clipboard(&mut self) -> Task<Message> {
        if let Some(ref image) = self.pending_image {
            if let Some(ref mut clipboard) = self.clipboard {
                match clipboard.copy_image(image) {
                    Ok(()) => {
                        Sound::Screenshot.play_if_enabled(self.config.post_capture.play_sound);
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Copied", "Image copied to clipboard");
                        }
                    }
                    Err(e) => {
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Copy Failed", &e.to_string());
                        }
                    }
                }
            }
        }
        self.pending_image = None;
        self.view = View::Hidden;
        Task::none()
    }

    fn validate_save_path(path: &std::path::Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();

        if path_str.contains("..") {
            return Err("Path contains directory traversal".to_string());
        }

        #[cfg(windows)]
        {
            if path_str.starts_with("\\\\") {
                return Err("Network paths are not allowed".to_string());
            }

            let dangerous_prefixes = ["C:\\Windows", "C:\\Program Files", "C:\\System"];
            let path_lower = path_str.to_lowercase();
            for prefix in &dangerous_prefixes {
                if path_lower.starts_with(&prefix.to_lowercase()) {
                    return Err("Cannot save to system directories".to_string());
                }
            }
        }

        #[cfg(unix)]
        {
            let dangerous_prefixes = ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/etc", "/boot"];
            for prefix in &dangerous_prefixes {
                if path_str.starts_with(prefix) {
                    return Err("Cannot save to system directories".to_string());
                }
            }
        }

        if let Some(filename) = path.file_name() {
            let name = filename.to_string_lossy();
            if name.starts_with('.') && !name.chars().skip(1).any(|c| c == '.') {
                return Err("Cannot create hidden files".to_string());
            }
        }

        Ok(())
    }

    fn upload_pending_image(&mut self) -> Task<Message> {
        if let Some(ref image) = self.pending_image {
            tracing::info!("flow=upload action=start");
            let image = image.clone();
            let destination = self.config.upload.destination;
            let custom_url = self.config.upload.custom_url.clone();
            let custom_form_name = self.config.upload.custom_form_name.clone();
            let custom_response_path = self.config.upload.custom_response_path.clone();
            self.pending_image = None;

            return Task::perform(
                async move {
                    let uploader = match crate::upload::shared_uploader() {
                        Ok(u) => u,
                        Err(e) => return Err(e.to_string()),
                    };

                    let service = match destination {
                        UploadDestination::Imgur => UploadService::Imgur,
                        UploadDestination::Custom => UploadService::Custom(CustomUploader {
                            name: "Custom".to_string(),
                            request_url: custom_url,
                            file_form_name: custom_form_name,
                            response_url_path: custom_response_path,
                        }),
                    };

                    match uploader.upload(&image, &service) {
                        Ok(result) => Ok((result.url, result.delete_url)),
                        Err(e) => Err(e.to_string()),
                    }
                },
                Message::UploadComplete,
            );
        }
        Task::none()
    }

    fn apply_setting_change(&mut self, change: SettingChange) {
        match change {
            SettingChange::OutputDirectory(path) => {
                self.config.output.directory = path;
            }
            SettingChange::Format(format) => {
                self.config.output.format = format;
            }
            SettingChange::Quality(quality) => {
                self.config.output.quality = quality;
            }
            SettingChange::ScreenshotHotkey(hotkey) => {
                if let Some(ref mut hm) = self.hotkey_manager {
                    let _ = hm.unregister(HotkeyAction::Screenshot);
                    let _ = hm.register(HotkeyAction::Screenshot, &hotkey);
                }
                self.config.hotkeys.screenshot = hotkey;
            }
            SettingChange::GifHotkey(hotkey) => {
                if let Some(ref mut hm) = self.hotkey_manager {
                    let _ = hm.unregister(HotkeyAction::RecordGif);
                    let _ = hm.register(HotkeyAction::RecordGif, &hotkey);
                }
                self.config.hotkeys.record_gif = hotkey;
            }
            SettingChange::GifFps(fps) => {
                self.config.capture.gif_fps = fps;
            }
            SettingChange::GifMaxDuration(duration) => {
                self.config.capture.gif_max_duration_secs = duration;
            }
            SettingChange::ShowNotifications(show) => {
                self.config.ui.show_notifications = show;
            }
            SettingChange::PostCaptureAction(action) => {
                self.config.post_capture.action = action;
            }
            SettingChange::Theme(theme) => {
                self.config.ui.theme = theme;
                self.theme = style::MonochromeTheme::from_config_theme(theme);
            }
            SettingChange::PlaySound(play) => {
                self.config.post_capture.play_sound = play;
            }
            SettingChange::UploadDestination(dest) => {
                self.config.upload.destination = dest;
            }
            SettingChange::CustomUploadUrl(url) => {
                self.config.upload.custom_url = url;
            }
            SettingChange::CopyUrlToClipboard(copy) => {
                self.config.upload.copy_url_to_clipboard = copy;
            }
            SettingChange::TickIntervalMs(ms) => {
                self.config.performance.tick_interval_ms = ms;
            }
            SettingChange::RendererBackend(renderer) => {
                self.config.performance.renderer = renderer;
            }
            SettingChange::LazyInitUpload(lazy) => {
                self.config.performance.lazy_init_upload = lazy;
                if !lazy {
                    if let Err(e) = crate::upload::shared_uploader() {
                        tracing::warn!("Upload client init error: {}", e);
                    }
                }
            }
            SettingChange::LazyInitPlugins(lazy) => {
                self.config.performance.lazy_init_plugins = lazy;
                self.plugin_manager.set_lazy_loading(lazy);
                if !lazy {
                    for err in self.plugin_manager.load_pending() {
                        tracing::warn!("Plugin load error: {}", err);
                    }
                }
            }
        }
        let _ = self.config.save();
        self.settings_state = views::SettingsState::from_config(&self.config);
    }

    fn build_hotkey_string(
        &self,
        key: &iced::keyboard::Key,
        modifiers: &iced::keyboard::Modifiers,
    ) -> Option<String> {
        let mut parts = Vec::new();

        if modifiers.control() {
            parts.push("Ctrl");
        }
        if modifiers.alt() {
            parts.push("Alt");
        }
        if modifiers.shift() {
            parts.push("Shift");
        }

        let key_str = match key {
            iced::keyboard::Key::Character(c) => {
                let upper = c.to_uppercase().to_string();
                if upper.len() == 1 && upper.chars().next().unwrap().is_alphanumeric() {
                    Some(upper)
                } else {
                    None
                }
            }
            iced::keyboard::Key::Named(named) => {
                use iced::keyboard::key::Named;
                match named {
                    Named::F1 => Some("F1".to_string()),
                    Named::F2 => Some("F2".to_string()),
                    Named::F3 => Some("F3".to_string()),
                    Named::F4 => Some("F4".to_string()),
                    Named::F5 => Some("F5".to_string()),
                    Named::F6 => Some("F6".to_string()),
                    Named::F7 => Some("F7".to_string()),
                    Named::F8 => Some("F8".to_string()),
                    Named::F9 => Some("F9".to_string()),
                    Named::F10 => Some("F10".to_string()),
                    Named::F11 => Some("F11".to_string()),
                    Named::F12 => Some("F12".to_string()),
                    Named::Space => Some("Space".to_string()),
                    Named::Enter => Some("Enter".to_string()),
                    Named::Tab => Some("Tab".to_string()),
                    Named::Insert => Some("Insert".to_string()),
                    Named::Delete => Some("Delete".to_string()),
                    Named::Home => Some("Home".to_string()),
                    Named::End => Some("End".to_string()),
                    Named::PageUp => Some("PageUp".to_string()),
                    Named::PageDown => Some("PageDown".to_string()),
                    Named::ArrowUp => Some("Up".to_string()),
                    Named::ArrowDown => Some("Down".to_string()),
                    Named::ArrowLeft => Some("Left".to_string()),
                    Named::ArrowRight => Some("Right".to_string()),
                    Named::PrintScreen => Some("PrintScreen".to_string()),
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(k) = key_str {
            if parts.is_empty() {
                None
            } else {
                parts.push(&k);
                Some(parts.join("+"))
            }
        } else {
            None
        }
    }

    fn tick_interval(&self) -> Duration {
        let base = self.config.performance.tick_interval_ms;
        let interval_ms = if self.view == View::Hidden
            && self.recording_state == RecordingState::Idle
            && self.recording_hotkey.is_none()
        {
            (base.saturating_mul(3)).div_ceil(2)
        } else {
            base
        };
        Duration::from_millis(interval_ms as u64)
    }

    pub fn view(&self) -> Element<'_, Message> {
        match self.view {
            View::Hidden => views::HiddenView::view(&self.theme, self.recording_state),
            View::PostCapture => views::PostCaptureView::view(&self.theme),
            View::Editor => {
                if let (Some(ref editor), Some(ref image)) =
                    (&self.editor_state, &self.pending_image)
                {
                    views::EditorView::view(&self.theme, editor, image)
                } else {
                    views::PostCaptureView::view(&self.theme)
                }
            }
            View::Settings => views::SettingsView::view(
                &self.theme,
                &self.settings_state,
                &self.config,
                self.recording_hotkey,
            ),
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        let tick = iced::time::every(self.tick_interval()).map(|_| Message::Tick);

        if self.recording_hotkey.is_some() {
            let keyboard = iced::keyboard::on_key_press(|key, modifiers| {
                Some(Message::KeyPressed(key, modifiers))
            });
            iced::Subscription::batch([tick, keyboard])
        } else {
            tick
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new().0
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(ref mut hm) = self.hotkey_manager {
            hm.unregister_all();
        }
        if let Some(ref mut recorder) = self.gif_recorder {
            recorder.reset();
        }
    }
}
