use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_QUALITY: u8 = 100;
const MIN_GIF_FPS: u32 = 1;
const MAX_GIF_FPS: u32 = 60;
const MAX_GIF_DURATION_SECS: u32 = 300;
const MAX_DELAY_MS: u32 = 30000;
const MAX_FILENAME_TEMPLATE_LEN: usize = 128;
const MAX_HOTKEY_LEN: usize = 64;
const MIN_HDR_EXPOSURE: f32 = 0.1;
const MAX_HDR_EXPOSURE: f32 = 10.0;
const MAX_CUSTOM_URL_LEN: usize = 512;
const MAX_FORM_NAME_LEN: usize = 64;
const MAX_RESPONSE_PATH_LEN: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub output: OutputConfig,
    pub capture: CaptureConfig,
    pub hotkeys: HotkeyConfig,
    pub ui: UiConfig,
    #[serde(default)]
    pub post_capture: PostCaptureConfig,
    #[serde(default)]
    pub upload: UploadConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub directory: PathBuf,
    pub format: ImageFormat,
    pub quality: u8,
    pub filename_template: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl ImageFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Gif => "gif",
            ImageFormat::Webp => "webp",
            ImageFormat::Bmp => "bmp",
        }
    }

    pub fn all() -> &'static [ImageFormat] {
        &[
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::Webp,
            ImageFormat::Bmp,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::Webp => "WebP",
            ImageFormat::Bmp => "BMP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    pub show_cursor: bool,
    pub delay_ms: u32,
    pub gif_fps: u32,
    pub gif_max_duration_secs: u32,
    #[serde(default)]
    pub hdr_enabled: bool,
    #[serde(default)]
    pub hdr_tonemap: ToneMapMode,
    #[serde(default = "default_hdr_exposure")]
    pub hdr_exposure: f32,
}

fn default_hdr_exposure() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ToneMapMode {
    #[default]
    AcesFilmic,
    Reinhard,
    ReinhardExtended,
    Hable,
    Exposure,
}

impl ToneMapMode {
    pub fn all() -> &'static [ToneMapMode] {
        &[
            ToneMapMode::AcesFilmic,
            ToneMapMode::Reinhard,
            ToneMapMode::ReinhardExtended,
            ToneMapMode::Hable,
            ToneMapMode::Exposure,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ToneMapMode::AcesFilmic => "ACES Filmic",
            ToneMapMode::Reinhard => "Reinhard",
            ToneMapMode::ReinhardExtended => "Reinhard Extended",
            ToneMapMode::Hable => "Hable (Uncharted 2)",
            ToneMapMode::Exposure => "Exposure Only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyConfig {
    pub capture_screen: String,
    pub capture_window: String,
    pub capture_region: String,
    pub record_gif: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    pub theme: Theme,
    pub show_notifications: bool,
    pub copy_to_clipboard: bool,
    pub minimize_to_tray: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PostCaptureAction {
    #[default]
    SaveToFile,
    CopyToClipboard,
    SaveAndCopy,
    Upload,
    PromptUser,
}

impl PostCaptureAction {
    pub fn all() -> &'static [PostCaptureAction] {
        &[
            PostCaptureAction::SaveToFile,
            PostCaptureAction::CopyToClipboard,
            PostCaptureAction::SaveAndCopy,
            PostCaptureAction::Upload,
            PostCaptureAction::PromptUser,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            PostCaptureAction::SaveToFile => "Save to file",
            PostCaptureAction::CopyToClipboard => "Copy to clipboard",
            PostCaptureAction::SaveAndCopy => "Save and copy",
            PostCaptureAction::Upload => "Upload to web",
            PostCaptureAction::PromptUser => "Ask me each time",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCaptureConfig {
    pub action: PostCaptureAction,
    pub open_file_after_save: bool,
    pub play_sound: bool,
}

impl Default for PostCaptureConfig {
    fn default() -> Self {
        Self {
            action: PostCaptureAction::SaveAndCopy,
            open_file_after_save: false,
            play_sound: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UploadDestination {
    #[default]
    Imgur,
    Custom,
}

impl UploadDestination {
    pub fn all() -> &'static [UploadDestination] {
        &[UploadDestination::Imgur, UploadDestination::Custom]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            UploadDestination::Imgur => "Imgur",
            UploadDestination::Custom => "Custom server",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    pub destination: UploadDestination,
    pub copy_url_to_clipboard: bool,
    pub custom_url: String,
    pub custom_form_name: String,
    pub custom_response_path: String,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            destination: UploadDestination::Imgur,
            copy_url_to_clipboard: true,
            custom_url: String::new(),
            custom_form_name: String::from("file"),
            custom_response_path: String::from("url"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.output.quality > MAX_QUALITY {
            return Err(anyhow!("quality must be <= {}", MAX_QUALITY));
        }
        if self.capture.gif_fps < MIN_GIF_FPS || self.capture.gif_fps > MAX_GIF_FPS {
            return Err(anyhow!("gif_fps must be between {} and {}", MIN_GIF_FPS, MAX_GIF_FPS));
        }
        if self.capture.gif_max_duration_secs > MAX_GIF_DURATION_SECS {
            return Err(anyhow!("gif_max_duration_secs must be <= {}", MAX_GIF_DURATION_SECS));
        }
        if self.capture.delay_ms > MAX_DELAY_MS {
            return Err(anyhow!("delay_ms must be <= {}", MAX_DELAY_MS));
        }
        if !self.capture.hdr_exposure.is_finite()
            || self.capture.hdr_exposure < MIN_HDR_EXPOSURE
            || self.capture.hdr_exposure > MAX_HDR_EXPOSURE
        {
            return Err(anyhow!("hdr_exposure must be between {} and {}", MIN_HDR_EXPOSURE, MAX_HDR_EXPOSURE));
        }
        if self.output.filename_template.len() > MAX_FILENAME_TEMPLATE_LEN {
            return Err(anyhow!("filename_template too long"));
        }
        if self.output.filename_template.contains('/')
            || self.output.filename_template.contains('\\')
            || self.output.filename_template.contains("..")
        {
            return Err(anyhow!("filename_template contains invalid path characters"));
        }
        for hotkey in [
            &self.hotkeys.capture_screen,
            &self.hotkeys.capture_window,
            &self.hotkeys.capture_region,
            &self.hotkeys.record_gif,
        ] {
            if hotkey.len() > MAX_HOTKEY_LEN {
                return Err(anyhow!("hotkey string too long"));
            }
            if !hotkey.chars().all(|c| c.is_alphanumeric() || c == '+' || c == ' ') {
                return Err(anyhow!("hotkey contains invalid characters"));
            }
        }
        if self.upload.custom_url.len() > MAX_CUSTOM_URL_LEN {
            return Err(anyhow!("custom upload URL too long"));
        }
        if !self.upload.custom_url.is_empty() && !self.upload.custom_url.starts_with("https://") {
            return Err(anyhow!("custom upload URL must use HTTPS"));
        }
        if self.upload.custom_form_name.len() > MAX_FORM_NAME_LEN {
            return Err(anyhow!("custom form name too long"));
        }
        if !self.upload.custom_form_name.is_empty()
            && !self
                .upload
                .custom_form_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(anyhow!("custom form name contains invalid characters"));
        }
        if self.upload.custom_response_path.len() > MAX_RESPONSE_PATH_LEN {
            return Err(anyhow!("custom response path too long"));
        }
        if !self.upload.custom_response_path.is_empty() {
            if !self
                .upload
                .custom_response_path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
            {
                return Err(anyhow!("custom response path contains invalid characters"));
            }
            if self.upload.custom_response_path.starts_with('.')
                || self.upload.custom_response_path.ends_with('.')
                || self.upload.custom_response_path.contains("..")
            {
                return Err(anyhow!("custom response path has invalid format"));
            }
        }
        Ok(())
    }

    fn sanitize(&mut self) {
        self.output.quality = self.output.quality.min(MAX_QUALITY);
        self.capture.gif_fps = self.capture.gif_fps.clamp(MIN_GIF_FPS, MAX_GIF_FPS);
        self.capture.gif_max_duration_secs = self.capture.gif_max_duration_secs.min(MAX_GIF_DURATION_SECS);
        self.capture.delay_ms = self.capture.delay_ms.min(MAX_DELAY_MS);
        self.capture.hdr_exposure = if self.capture.hdr_exposure.is_finite() {
            self.capture.hdr_exposure.clamp(MIN_HDR_EXPOSURE, MAX_HDR_EXPOSURE)
        } else {
            1.0
        };

        if self.output.filename_template.len() > MAX_FILENAME_TEMPLATE_LEN
            || self.output.filename_template.contains('/')
            || self.output.filename_template.contains('\\')
            || self.output.filename_template.contains("..")
        {
            self.output.filename_template = "capture_%Y%m%d_%H%M%S".to_string();
        }

        if self.upload.custom_form_name.len() > MAX_FORM_NAME_LEN
            || !self
                .upload
                .custom_form_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.upload.custom_form_name = "file".to_string();
        }

        if self.upload.custom_response_path.len() > MAX_RESPONSE_PATH_LEN
            || !self
                .upload
                .custom_response_path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
            || self.upload.custom_response_path.starts_with('.')
            || self.upload.custom_response_path.ends_with('.')
            || self.upload.custom_response_path.contains("..")
        {
            self.upload.custom_response_path = "url".to_string();
        }

        if self.upload.custom_url.len() > MAX_CUSTOM_URL_LEN
            || (!self.upload.custom_url.is_empty()
                && !self.upload.custom_url.starts_with("https://"))
        {
            self.upload.custom_url = String::new();
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let pictures_dir = directories::UserDirs::new()
            .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| {
                directories::BaseDirs::new()
                    .map(|b| b.home_dir().to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            });

        let output_dir = pictures_dir.join("capscr");

        Self {
            output: OutputConfig {
                directory: output_dir,
                format: ImageFormat::Png,
                quality: 90,
                filename_template: "capture_%Y%m%d_%H%M%S".to_string(),
            },
            capture: CaptureConfig {
                show_cursor: true,
                delay_ms: 0,
                gif_fps: 15,
                gif_max_duration_secs: 30,
                hdr_enabled: true,
                hdr_tonemap: ToneMapMode::AcesFilmic,
                hdr_exposure: 1.0,
            },
            hotkeys: HotkeyConfig {
                capture_screen: "Ctrl+Shift+S".to_string(),
                capture_window: "Ctrl+Shift+W".to_string(),
                capture_region: "Ctrl+Shift+R".to_string(),
                record_gif: "Ctrl+Shift+G".to_string(),
            },
            ui: UiConfig {
                theme: Theme::Dark,
                show_notifications: true,
                copy_to_clipboard: true,
                minimize_to_tray: true,
            },
            post_capture: PostCaptureConfig::default(),
            upload: UploadConfig::default(),
        }
    }
}

impl Config {
    pub fn config_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "capscr", "capscr").map(|p| p.config_dir().to_path_buf())
    }

    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|p| p.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        if let Some(path) = Self::config_path() {
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                let mut config: Config = toml::from_str(&content)?;
                config.sanitize();
                config.validate()?;
                return Ok(config);
            }
        }
        Ok(Config::default())
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        if let Some(dir) = Self::config_dir() {
            fs::create_dir_all(&dir)?;
            if let Some(path) = Self::config_path() {
                let content = toml::to_string_pretty(self)?;
                fs::write(&path, content)?;
            }
        }
        Ok(())
    }

    pub fn ensure_output_dir(&self) -> Result<()> {
        let dir = &self.output.directory;
        if dir.as_os_str().is_empty() {
            return Err(anyhow!("Output directory path is empty"));
        }

        let dir_str = dir.to_string_lossy();
        if dir_str.contains("..") {
            return Err(anyhow!("Output directory contains path traversal"));
        }

        #[cfg(windows)]
        {
            if dir_str.starts_with("\\\\") {
                return Err(anyhow!("Network paths are not allowed"));
            }
        }

        fs::create_dir_all(dir)?;
        let canonical = fs::canonicalize(dir)?;

        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        let pictures = directories::UserDirs::new()
            .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()));

        let mut allowed = false;

        if let Some(home_dir) = &home {
            if let Ok(canonical_home) = fs::canonicalize(home_dir) {
                if canonical.starts_with(&canonical_home) {
                    allowed = true;
                }
            }
        }

        if let Some(pictures_dir) = &pictures {
            if let Ok(canonical_pictures) = fs::canonicalize(pictures_dir) {
                if canonical.starts_with(&canonical_pictures) {
                    allowed = true;
                }
            }
        }

        #[cfg(unix)]
        if canonical.starts_with("/tmp") || canonical.starts_with("/var/tmp") {
            allowed = true;
        }

        #[cfg(windows)]
        {
            let canonical_str = canonical.to_string_lossy().to_lowercase();
            if canonical_str.contains("\\temp\\") || canonical_str.contains("\\tmp\\") {
                allowed = true;
            }
            if canonical_str.starts_with("c:\\users\\") {
                allowed = true;
            }
        }

        if !allowed {
            return Err(anyhow!(
                "Output directory must be within user home, pictures, or temp directory"
            ));
        }

        Ok(())
    }

    pub fn generate_filename(&self) -> String {
        let now = chrono::Local::now();
        let formatted = now.format(&self.output.filename_template).to_string();
        let sanitized: String = formatted
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .take(200)
            .collect();
        let safe_name = if sanitized.is_empty() {
            format!("capture_{}", now.timestamp())
        } else {
            sanitized
        };
        format!("{}.{}", safe_name, self.output.format.extension())
    }

    pub fn output_path(&self) -> PathBuf {
        self.output.directory.join(self.generate_filename())
    }
}
