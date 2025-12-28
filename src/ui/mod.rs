pub mod components;
pub mod style;
pub mod views;

use iced::{Element, Task, Theme};
use image::RgbaImage;

use crate::capture::{CaptureMode, HdrCapture, Rectangle, RegionCapture, ToneMapOperator, WindowCapture, WindowInfo, list_windows};
use crate::clipboard::{save_image, show_notification, ClipboardManager};
use crate::config::{Config, ImageFormat, PostCaptureAction, ToneMapMode, UploadDestination};
use crate::hotkeys::{HotkeyAction, HotkeyManager};
use crate::recording::{GifRecorder, RecordingSettings, RecordingState};
use crate::upload::{CustomUploader, ImageUploader, UploadService};

use self::style::MonochromeTheme;
use self::views::{MainView, SettingsView, WindowPicker};

#[derive(Debug, Clone)]
pub enum Message {
    Capture(CaptureMode),
    ShowWindowPicker,
    HideWindowPicker,
    SelectWindow(u32),
    ToggleGifRecording,
    SetFormat(ImageFormat),
    ShowSettings,
    HideSettings,
    BrowseOutputDir,
    SetOutputDir(String),
    ToggleShowCursor(bool),
    SetCaptureDelay(u32),
    SetGifFps(u32),
    SetHotkey(String, String),
    SetTheme(crate::config::Theme),
    ToggleNotifications(bool),
    ToggleClipboard(bool),
    ToggleMinimizeToTray(bool),
    HotkeyTriggered(HotkeyAction),
    CaptureComplete(Result<String, String>),
    GifSaved(Result<String, String>),
    Tick,
    WindowsListed(Vec<WindowInfo>),
    ImageCaptured(CapturedImage),
    PostCaptureAction(PostCaptureAction),
    SaveAs,
    SaveAsPath(Option<std::path::PathBuf>),
    UploadComplete(Result<(String, Option<String>), String>),
    CopyToClipboard,
    SetPostCaptureAction(PostCaptureAction),
    SetUploadDestination(UploadDestination),
    SetCustomUploadUrl(String),
    SetCustomFormName(String),
    SetCustomResponsePath(String),
    DismissPostCapture,
    ToggleHdrEnabled(bool),
    SetHdrTonemap(ToneMapMode),
    SetHdrExposure(String),
}

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub image: std::sync::Arc<RgbaImage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Main,
    Settings,
    WindowPicker,
    PostCapture,
}

pub struct App {
    config: Config,
    theme: MonochromeTheme,
    view: View,
    recording_state: RecordingState,
    gif_recorder: Option<GifRecorder>,
    windows: Vec<WindowInfo>,
    clipboard: Option<ClipboardManager>,
    hotkey_manager: Option<HotkeyManager>,
    pending_image: Option<std::sync::Arc<RgbaImage>>,
    last_upload_url: Option<String>,
    last_delete_url: Option<String>,
    last_save_path: Option<std::path::PathBuf>,
}

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
            let _ = hm.register(HotkeyAction::CaptureScreen, &config.hotkeys.capture_screen);
            let _ = hm.register(HotkeyAction::CaptureWindow, &config.hotkeys.capture_window);
            let _ = hm.register(HotkeyAction::CaptureRegion, &config.hotkeys.capture_region);
            let _ = hm.register(HotkeyAction::RecordGif, &config.hotkeys.record_gif);
        }

        let app = Self {
            config,
            theme,
            view: View::Main,
            recording_state: RecordingState::Idle,
            gif_recorder: None,
            windows: Vec::new(),
            clipboard,
            hotkey_manager,
            pending_image: None,
            last_upload_url: None,
            last_delete_url: None,
            last_save_path: None,
        };

        (app, Task::none())
    }

    pub fn title(&self) -> String {
        String::from("capscr")
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
            Message::Capture(mode) => {
                return self.perform_capture(mode);
            }
            Message::ShowWindowPicker => {
                self.view = View::WindowPicker;
                return Task::perform(
                    async {
                        WindowCapture::list_application_windows().unwrap_or_else(|_| {
                            list_windows().unwrap_or_default().into_iter().filter(|w| {
                                w.is_visible && w.width > 50 && w.height > 50
                            }).collect()
                        })
                    },
                    Message::WindowsListed,
                );
            }
            Message::HideWindowPicker => {
                self.view = View::Main;
            }
            Message::SelectWindow(window_id) => {
                self.view = View::Main;
                return self.capture_window(window_id);
            }
            Message::ToggleGifRecording => {
                return self.toggle_gif_recording();
            }
            Message::SetFormat(format) => {
                self.config.output.format = format;
                let _ = self.config.save();
            }
            Message::ShowSettings => {
                self.view = View::Settings;
            }
            Message::HideSettings => {
                self.view = View::Main;
                let _ = self.config.save();
            }
            Message::BrowseOutputDir => {
                return Task::perform(
                    async {
                        if let Some(path) = rfd::AsyncFileDialog::new().pick_folder().await {
                            return path.path().to_string_lossy().to_string();
                        }
                        String::new()
                    },
                    Message::SetOutputDir,
                );
            }
            Message::SetOutputDir(dir) => {
                if !dir.is_empty() {
                    self.config.output.directory = dir.into();
                    let _ = self.config.save();
                }
            }
            Message::ToggleShowCursor(val) => {
                self.config.capture.show_cursor = val;
            }
            Message::SetCaptureDelay(val) => {
                self.config.capture.delay_ms = val;
            }
            Message::SetGifFps(val) => {
                self.config.capture.gif_fps = val.clamp(1, 60);
            }
            Message::SetHotkey(action, hotkey) => {
                if let Some(ref mut hm) = self.hotkey_manager {
                    let hotkey_action = match action.as_str() {
                        "screen" => Some(HotkeyAction::CaptureScreen),
                        "window" => Some(HotkeyAction::CaptureWindow),
                        "region" => Some(HotkeyAction::CaptureRegion),
                        "gif" => Some(HotkeyAction::RecordGif),
                        _ => None,
                    };
                    if let Some(hk_action) = hotkey_action {
                        let _ = hm.unregister(hk_action);
                        let _ = hm.register(hk_action, &hotkey);
                    }
                }
                match action.as_str() {
                    "screen" => self.config.hotkeys.capture_screen = hotkey,
                    "window" => self.config.hotkeys.capture_window = hotkey,
                    "region" => self.config.hotkeys.capture_region = hotkey,
                    "gif" => self.config.hotkeys.record_gif = hotkey,
                    _ => {}
                }
            }
            Message::SetTheme(t) => {
                self.config.ui.theme = t;
                self.theme = match t {
                    crate::config::Theme::Dark => MonochromeTheme::dark(),
                    crate::config::Theme::Light => MonochromeTheme::light(),
                };
            }
            Message::ToggleNotifications(val) => {
                self.config.ui.show_notifications = val;
            }
            Message::ToggleClipboard(val) => {
                self.config.ui.copy_to_clipboard = val;
            }
            Message::ToggleMinimizeToTray(val) => {
                self.config.ui.minimize_to_tray = val;
            }
            Message::HotkeyTriggered(action) => {
                match action {
                    HotkeyAction::CaptureScreen => {
                        return self.perform_capture(CaptureMode::FullScreen);
                    }
                    HotkeyAction::CaptureWindow => {
                        self.view = View::WindowPicker;
                    }
                    HotkeyAction::CaptureRegion => {
                        return self.perform_capture(CaptureMode::Region);
                    }
                    HotkeyAction::RecordGif => {
                        return self.toggle_gif_recording();
                    }
                }
            }
            Message::CaptureComplete(result) => {
                match result {
                    Ok(path) => {
                        self.last_save_path = Some(std::path::PathBuf::from(&path));
                        if let Some(ref mut cb) = self.clipboard {
                            let _ = cb.copy_file_path(&path);
                        }
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Capture Complete", &format!("Saved to {}", path));
                        }
                    }
                    Err(e) => {
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Capture Failed", &e);
                        }
                    }
                }
            }
            Message::GifSaved(result) => {
                self.recording_state = RecordingState::Idle;
                if let Some(ref mut recorder) = self.gif_recorder {
                    recorder.reset();
                }
                self.gif_recorder = None;
                match result {
                    Ok(path) => {
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
            }
            Message::WindowsListed(windows) => {
                self.windows = windows;
            }
            Message::ImageCaptured(captured) => {
                self.pending_image = Some(captured.image);
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
                self.view = View::Main;
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
                self.view = View::Main;
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
                self.view = View::Main;
                self.pending_image = None;
                match result {
                    Ok((url, delete_url)) => {
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
                        if self.config.ui.show_notifications {
                            let _ = show_notification("Upload Failed", &e);
                        }
                    }
                }
            }
            Message::CopyToClipboard => {
                return self.copy_pending_to_clipboard();
            }
            Message::SetPostCaptureAction(action) => {
                self.config.post_capture.action = action;
                let _ = self.config.save();
            }
            Message::SetUploadDestination(dest) => {
                self.config.upload.destination = dest;
                let _ = self.config.save();
            }
            Message::SetCustomUploadUrl(url) => {
                self.config.upload.custom_url = url;
                let _ = self.config.save();
            }
            Message::SetCustomFormName(name) => {
                self.config.upload.custom_form_name = name;
                let _ = self.config.save();
            }
            Message::SetCustomResponsePath(path) => {
                self.config.upload.custom_response_path = path;
                let _ = self.config.save();
            }
            Message::DismissPostCapture => {
                self.view = View::Main;
                self.pending_image = None;
            }
            Message::ToggleHdrEnabled(val) => {
                self.config.capture.hdr_enabled = val;
                let _ = self.config.save();
            }
            Message::SetHdrTonemap(mode) => {
                self.config.capture.hdr_tonemap = mode;
                let _ = self.config.save();
            }
            Message::SetHdrExposure(val) => {
                if let Ok(exp) = val.parse::<f32>() {
                    self.config.capture.hdr_exposure = exp.clamp(0.1, 10.0);
                    let _ = self.config.save();
                }
            }
        }
        Task::none()
    }

    fn perform_capture(&mut self, mode: CaptureMode) -> Task<Message> {
        match mode {
            CaptureMode::FullScreen => {
                Task::perform(
                    async move {
                        use crate::capture::{Capture, ScreenCapture, list_monitors};
                        let monitors = list_monitors().unwrap_or_default();
                        let capture = if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
                            ScreenCapture::with_monitor(primary.id)
                        } else {
                            ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())
                        };
                        let _monitor_info = capture.get_monitor_info();
                        capture.capture()
                    },
                    |result| match result {
                        Ok(image) => Message::ImageCaptured(CapturedImage {
                            image: std::sync::Arc::new(image),
                        }),
                        Err(e) => Message::CaptureComplete(Err(e.to_string())),
                    },
                )
            }
            CaptureMode::Window => Task::none(),
            CaptureMode::HdrScreen => {
                let hdr_enabled = self.config.capture.hdr_enabled;
                let tonemap_mode = self.config.capture.hdr_tonemap;
                let exposure = self.config.capture.hdr_exposure;

                Task::perform(
                    async move {
                        let tonemap_op = match tonemap_mode {
                            ToneMapMode::AcesFilmic => ToneMapOperator::AcesFilmic,
                            ToneMapMode::Reinhard => ToneMapOperator::Reinhard,
                            ToneMapMode::ReinhardExtended => ToneMapOperator::ReinhardExtended,
                            ToneMapMode::Hable => ToneMapOperator::Hable,
                            ToneMapMode::Exposure => ToneMapOperator::Exposure,
                        };

                        let hdr_capture = HdrCapture::new()
                            .with_operator(tonemap_op)
                            .with_exposure(exposure)
                            .with_auto_tonemap(hdr_enabled);

                        hdr_capture.capture_hdr()
                    },
                    |result| match result {
                        Ok(image) => Message::ImageCaptured(CapturedImage {
                            image: std::sync::Arc::new(image),
                        }),
                        Err(e) => Message::CaptureComplete(Err(e.to_string())),
                    },
                )
            }
            CaptureMode::Region => {
                Task::perform(
                    async move {
                        use crate::capture::{Capture, RegionCapture, ScreenCapture};
                        let full = ScreenCapture::all_monitors()?;
                        let w = full.width();
                        let h = full.height();
                        let capture = RegionCapture::from_coords(
                            (w / 4) as i32,
                            (h / 4) as i32,
                            (w * 3 / 4) as i32,
                            (h * 3 / 4) as i32,
                        );
                        let _region_info = capture.region();
                        capture.capture()
                    },
                    |result| match result {
                        Ok(image) => Message::ImageCaptured(CapturedImage {
                            image: std::sync::Arc::new(image),
                        }),
                        Err(e) => Message::CaptureComplete(Err(e.to_string())),
                    },
                )
            }
        }
    }

    fn capture_window(&mut self, window_id: u32) -> Task<Message> {
        Task::perform(
            async move {
                use crate::capture::Capture;
                let capture = if window_id == 0 {
                    WindowCapture::focused().unwrap_or_else(|_| {
                        WindowCapture::from_title("").unwrap_or_else(|_| WindowCapture::new(0))
                    })
                } else {
                    WindowCapture::new(window_id)
                };
                let _window_info = capture.get_window_info();
                capture.capture()
            },
            |result| match result {
                Ok(image) => Message::ImageCaptured(CapturedImage {
                    image: std::sync::Arc::new(image),
                }),
                Err(e) => Message::CaptureComplete(Err(e.to_string())),
            },
        )
    }

    fn toggle_gif_recording(&mut self) -> Task<Message> {
        match self.recording_state {
            RecordingState::Idle => {
                let settings = RecordingSettings {
                    fps: self.config.capture.gif_fps,
                    max_duration: std::time::Duration::from_secs(
                        self.config.capture.gif_max_duration_secs as u64,
                    ),
                    quality: self.config.output.quality,
                };
                let region = Rectangle::new(0, 0, 1920, 1080);
                let _region_capture = RegionCapture::new(region);
                let mut recorder = GifRecorder::new(settings).with_region(region);
                if recorder.start().is_ok() {
                    self.recording_state = RecordingState::Recording;
                    self.gif_recorder = Some(recorder);
                }
            }
            RecordingState::Recording => {
                if let Some(ref mut recorder) = self.gif_recorder {
                    recorder.stop();
                    self.recording_state = RecordingState::Processing;

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
            }
            RecordingState::Processing => {}
        }
        Task::none()
    }

    fn save_pending_image(&mut self) -> Task<Message> {
        if let Some(ref image) = self.pending_image {
            let image = image.clone();
            let format = self.config.output.format;
            let quality = self.config.output.quality;
            let output_path = self.config.output_path();
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
        self.view = View::Main;
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
            let image = image.clone();
            let destination = self.config.upload.destination;
            let custom_url = self.config.upload.custom_url.clone();
            let custom_form_name = self.config.upload.custom_form_name.clone();
            let custom_response_path = self.config.upload.custom_response_path.clone();
            self.pending_image = None;

            return Task::perform(
                async move {
                    let uploader = match ImageUploader::new() {
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

    pub fn view(&self) -> Element<'_, Message> {
        let frame_count = self.gif_recorder.as_ref().map(|r| r.frame_count()).unwrap_or(0);
        let _gif_state = self.gif_recorder.as_ref().map(|r| r.state());
        match self.view {
            View::Main => MainView::view(
                &self.theme,
                self.recording_state,
                self.config.output.format,
                frame_count,
            ),
            View::Settings => SettingsView::view(&self.theme, &self.config),
            View::WindowPicker => WindowPicker::view(&self.theme, &self.windows),
            View::PostCapture => views::PostCaptureView::view(&self.theme),
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        iced::time::every(std::time::Duration::from_millis(100)).map(|_| Message::Tick)
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
