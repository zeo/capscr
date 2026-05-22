#![allow(dead_code)]

use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_QUALITY: u8 = 100;
const MIN_GIF_FPS: u32 = 1;
const MAX_GIF_FPS: u32 = 60;
const MAX_GIF_DURATION_SECS: u32 = 300;
const MAX_DELAY_MS: u32 = 30000;
const MAX_FILENAME_TEMPLATE_LEN: usize = 128;
const MAX_HOTKEY_LEN: usize = 64;
const MAX_CUSTOM_URL_LEN: usize = 512;
const MAX_FORM_NAME_LEN: usize = 64;
const MAX_RESPONSE_PATH_LEN: usize = 128;
const MIN_TICK_INTERVAL_MS: u32 = 16;
const MAX_TICK_INTERVAL_MS: u32 = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub output: OutputConfig,
    pub capture: CaptureConfig,
    #[serde(default)]
    pub hotkeys: HotkeyConfig,
    pub ui: UiConfig,
    #[serde(default)]
    pub post_capture: PostCaptureConfig,
    #[serde(default)]
    pub upload: UploadConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub marketplace: MarketplaceConfig,
    #[serde(default = "default_capture_tasks")]
    pub capture_tasks: Vec<CaptureTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureTask {
    pub id: String,
    pub name: String,
    pub hotkey: String,
    pub capture_mode: TaskCaptureMode,
    pub post_action: TaskPostAction,
    #[serde(default)]
    pub target_destination: Option<TaskUploadTarget>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCaptureMode {
    Region,
    Window,
    Fullscreen,
    ActiveMonitor,
    RegionGif,
}

impl TaskCaptureMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskCaptureMode::Region => "Region",
            TaskCaptureMode::Window => "Window",
            TaskCaptureMode::Fullscreen => "Fullscreen (selector)",
            TaskCaptureMode::ActiveMonitor => "Active monitor",
            TaskCaptureMode::RegionGif => "Region GIF",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskPostAction {
    Clipboard,
    SaveFile,
    Upload,
    SaveAndClipboard,
    OpenEditor,
    Prompt,
}

impl TaskPostAction {
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskPostAction::Clipboard => "Copy to clipboard",
            TaskPostAction::SaveFile => "Save to file",
            TaskPostAction::Upload => "Upload",
            TaskPostAction::SaveAndClipboard => "Save and copy",
            TaskPostAction::OpenEditor => "Open editor",
            TaskPostAction::Prompt => "Ask each time",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskUploadTarget {
    Imgur,
    Custom,
    Ftp,
    Sftp,
}

fn default_capture_tasks() -> Vec<CaptureTask> {
    vec![
        CaptureTask {
            id: "screenshot-save-clipboard".to_string(),
            name: "Screenshot — save + clipboard".to_string(),
            hotkey: "PrintScreen".to_string(),
            capture_mode: TaskCaptureMode::Region,
            post_action: TaskPostAction::SaveAndClipboard,
            target_destination: None,
        },
        CaptureTask {
            id: "gif-save".to_string(),
            name: "Region GIF to file".to_string(),
            hotkey: "Ctrl+Shift+G".to_string(),
            capture_mode: TaskCaptureMode::RegionGif,
            post_action: TaskPostAction::SaveFile,
            target_destination: None,
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub directory: PathBuf,
    pub format: ImageFormat,
    pub quality: u8,
    pub filename_template: String,
    /// when true and the source is HDR (HDR10 currently — scRGB / HLG arrive
    /// in Phase 2), capscr writes a `<basename>.hdr.png` sidecar alongside
    /// the normal SDR file. The sidecar is a 16-bit BT.2020 + PQ PNG with a
    /// `cICP` chunk so HDR-aware viewers display it as real HDR.
    #[serde(default)]
    pub preserve_hdr: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
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
    pub hdr: HdrConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HdrCompressionMode {
    MapCllToDisplay,
    NormalizeToCll,
}

impl HdrCompressionMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            HdrCompressionMode::MapCllToDisplay => "Map peak to display (SDR-friendly)",
            HdrCompressionMode::NormalizeToCll => "Normalize to peak (HDR-friendly)",
        }
    }

    pub fn all() -> &'static [HdrCompressionMode] {
        &[
            HdrCompressionMode::MapCllToDisplay,
            HdrCompressionMode::NormalizeToCll,
        ]
    }
}

impl std::fmt::Display for HdrCompressionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HdrConfig {
    pub mode: HdrCompressionMode,
    pub brightness_nits: f32,
    pub user_brightness_scale: f32,
    pub use_p99_max_cll: bool,
}

impl Default for HdrConfig {
    fn default() -> Self {
        Self {
            mode: HdrCompressionMode::MapCllToDisplay,
            // 0.0 is the sentinel for "auto — use the display's SDR white
            // level reported by DXGI". The previous default of 80 was a
            // hardcoded value that ignored the display, which made HDR
            // tonemap output too bright on displays with high SDR white
            // sliders (300+ nits) because effective_params() only auto-fills
            // when the param is <= 0.0.
            brightness_nits: 0.0,
            user_brightness_scale: 1.0,
            use_p99_max_cll: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HotkeyConfig {
    #[serde(default)]
    pub screenshot: String,
    #[serde(default)]
    pub record_gif: String,
    // user-controlled global kill switch toggled from the tray or Settings.
    // when true, the LL keyboard hook is installed but its match table is
    // empty, so no chord can fire a task. preserved across restarts.
    #[serde(default)]
    pub disabled_globally: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    pub theme: Theme,
    pub show_notifications: bool,
    pub copy_to_clipboard: bool,
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub check_updates_on_launch: bool,
}

fn default_true() -> bool {
    true
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

impl std::fmt::Display for PostCaptureAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
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
            play_sound: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UploadDestination {
    #[default]
    Imgur,
    Custom,
    Ftp,
    Sftp,
}

impl UploadDestination {
    pub fn all() -> &'static [UploadDestination] {
        &[
            UploadDestination::Imgur,
            UploadDestination::Custom,
            UploadDestination::Ftp,
            UploadDestination::Sftp,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            UploadDestination::Imgur => "Imgur",
            UploadDestination::Custom => "Custom HTTP",
            UploadDestination::Ftp => "FTP",
            UploadDestination::Sftp => "SFTP",
        }
    }
}

impl std::fmt::Display for UploadDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// note: deny_unknown_fields is intentionally *not* set on UploadConfig so that
// stale `[upload.sftp]` blocks from 0.3.0 configs deserialize without error
// and get pruned the next time the user saves their settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    pub destination: UploadDestination,
    pub copy_url_to_clipboard: bool,
    pub custom_url: String,
    pub custom_form_name: String,
    pub custom_response_path: String,
    #[serde(default = "default_imgur_client_id")]
    pub imgur_client_id: String,
    #[serde(default)]
    pub ftp: FtpUploadConfig,
    #[serde(default)]
    pub sftp: SftpUploadConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceConfig {
    /// JSON endpoint the marketplace browser queries. Defaults to capscr's
    /// canonical registry on rot.lt. Power users can point at their own
    /// mirror — must be HTTPS (validated client-side).
    #[serde(default = "default_marketplace_registry_url")]
    pub registry_url: String,
}

impl Default for MarketplaceConfig {
    fn default() -> Self {
        Self {
            registry_url: default_marketplace_registry_url(),
        }
    }
}

fn default_marketplace_registry_url() -> String {
    "https://rot.lt/capscr/registry.json".to_string()
}

fn default_imgur_client_id() -> String {
    // built-in bot Client-ID. Power users who hit Imgur's per-app rate limit
    // (or want their own analytics) can paste a personal Client-ID over this.
    "546c25a59c58ad7".to_string()
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            destination: UploadDestination::Imgur,
            copy_url_to_clipboard: true,
            custom_url: String::new(),
            custom_form_name: String::from("file"),
            custom_response_path: String::from("url"),
            imgur_client_id: default_imgur_client_id(),
            ftp: FtpUploadConfig::default(),
            sftp: SftpUploadConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FtpUploadConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ftp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// plaintext password (legacy field). Kept for backward compatibility
    /// only — Config::load migrates any non-empty value into the encrypted
    /// vault on next save. New configs leave this empty.
    #[serde(default)]
    pub password: String,
    /// DPAPI-wrapped password (hex-encoded). Bound to the current user
    /// account — copying config.toml to another machine or user account
    /// makes this unrecoverable.
    #[serde(default)]
    pub password_encrypted: String,
    #[serde(default)]
    pub remote_dir: String,
    #[serde(default)]
    pub use_tls: bool,
    #[serde(default)]
    pub public_url_template: String,
}

impl FtpUploadConfig {
    /// returns the decrypted password (or empty string if neither field set).
    /// Tries the encrypted vault first; falls back to plaintext for
    /// already-saved-but-not-yet-migrated configs.
    pub fn password_plaintext(&self) -> String {
        if !self.password_encrypted.is_empty() {
            match crate::secret::decrypt(&self.password_encrypted) {
                Ok(p) => return p,
                Err(e) => {
                    tracing::warn!(
                        "FTP password decrypt failed (corrupt blob or wrong user?): {e}"
                    );
                }
            }
        }
        self.password.clone()
    }
}

fn default_ftp_port() -> u16 {
    21
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SftpUploadConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_sftp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// plaintext password (legacy slot). present for parity with FtpUploadConfig
    /// so users restoring a hand-edited config don't have to retype credentials;
    /// migrate_secrets moves any value here into password_encrypted on next save
    #[serde(default)]
    pub password: String,
    /// DPAPI-wrapped password (hex-encoded). bound to the current user account
    /// — copying config.toml to another machine makes this unrecoverable
    #[serde(default)]
    pub password_encrypted: String,
    #[serde(default)]
    pub remote_dir: String,
    #[serde(default)]
    pub public_url_template: String,
    /// absolute filesystem path to an OpenSSH-format private key. when set,
    /// the upload path tries public-key auth first; on failure it falls back
    /// to password if one is configured. empty = password-only
    #[serde(default)]
    pub private_key_path: String,
    /// passphrase for an encrypted private key. plaintext slot — same vault
    /// treatment as `password` (migrated on save, cleared from disk)
    #[serde(default)]
    pub private_key_passphrase: String,
    /// DPAPI-wrapped passphrase blob (hex-encoded)
    #[serde(default)]
    pub private_key_passphrase_encrypted: String,
}

impl SftpUploadConfig {
    pub fn password_plaintext(&self) -> String {
        if !self.password_encrypted.is_empty() {
            match crate::secret::decrypt(&self.password_encrypted) {
                Ok(p) => return p,
                Err(e) => {
                    tracing::warn!(
                        "SFTP password decrypt failed (corrupt blob or wrong user?): {e}"
                    );
                }
            }
        }
        self.password.clone()
    }

    pub fn private_key_passphrase_plaintext(&self) -> String {
        if !self.private_key_passphrase_encrypted.is_empty() {
            match crate::secret::decrypt(&self.private_key_passphrase_encrypted) {
                Ok(p) => return p,
                Err(e) => {
                    tracing::warn!(
                        "SFTP key passphrase decrypt failed (corrupt blob or wrong user?): {e}"
                    );
                }
            }
        }
        self.private_key_passphrase.clone()
    }
}

fn default_sftp_port() -> u16 {
    22
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn all() -> &'static [Theme] {
        &[Theme::Dark, Theme::Light]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }
}

impl std::fmt::Display for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RendererBackend {
    #[default]
    Wgpu,
    TinySkia,
}

impl RendererBackend {
    pub fn all() -> &'static [RendererBackend] {
        &[RendererBackend::Wgpu, RendererBackend::TinySkia]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            RendererBackend::Wgpu => "WGPU",
            RendererBackend::TinySkia => "Tiny Skia",
        }
    }

    pub fn iced_backend_value(&self) -> &'static str {
        match self {
            RendererBackend::Wgpu => "wgpu",
            RendererBackend::TinySkia => "tiny-skia",
        }
    }
}

impl std::fmt::Display for RendererBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceConfig {
    pub tick_interval_ms: u32,
    pub renderer: RendererBackend,
    pub lazy_init_upload: bool,
    pub lazy_init_plugins: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            tick_interval_ms: 100,
            renderer: RendererBackend::TinySkia,
            lazy_init_upload: true,
            lazy_init_plugins: true,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.output.quality > MAX_QUALITY {
            return Err(anyhow!("quality must be <= {}", MAX_QUALITY));
        }
        if self.capture.gif_fps < MIN_GIF_FPS || self.capture.gif_fps > MAX_GIF_FPS {
            return Err(anyhow!(
                "gif_fps must be between {} and {}",
                MIN_GIF_FPS,
                MAX_GIF_FPS
            ));
        }
        if self.capture.gif_max_duration_secs > MAX_GIF_DURATION_SECS {
            return Err(anyhow!(
                "gif_max_duration_secs must be <= {}",
                MAX_GIF_DURATION_SECS
            ));
        }
        if self.capture.delay_ms > MAX_DELAY_MS {
            return Err(anyhow!("delay_ms must be <= {}", MAX_DELAY_MS));
        }
        if self.output.filename_template.len() > MAX_FILENAME_TEMPLATE_LEN {
            return Err(anyhow!("filename_template too long"));
        }
        if self.output.filename_template.contains('/')
            || self.output.filename_template.contains('\\')
            || self.output.filename_template.contains("..")
        {
            return Err(anyhow!(
                "filename_template contains invalid path characters"
            ));
        }
        for hotkey in [&self.hotkeys.screenshot, &self.hotkeys.record_gif] {
            if hotkey.len() > MAX_HOTKEY_LEN {
                return Err(anyhow!("hotkey string too long"));
            }
            if !hotkey
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == ' ')
            {
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
        if self.performance.tick_interval_ms < MIN_TICK_INTERVAL_MS
            || self.performance.tick_interval_ms > MAX_TICK_INTERVAL_MS
        {
            return Err(anyhow!(
                "tick_interval_ms must be between {} and {}",
                MIN_TICK_INTERVAL_MS,
                MAX_TICK_INTERVAL_MS
            ));
        }
        if !self.capture.hdr.brightness_nits.is_finite()
            || self.capture.hdr.brightness_nits < 0.0
            || self.capture.hdr.brightness_nits > 10000.0
        {
            return Err(anyhow!(
                "hdr.brightness_nits must be 0 (auto-detect) or between 1 and 10000"
            ));
        }
        if !self.capture.hdr.user_brightness_scale.is_finite()
            || self.capture.hdr.user_brightness_scale <= 0.0
            || self.capture.hdr.user_brightness_scale > 100.0
        {
            return Err(anyhow!(
                "hdr.user_brightness_scale must be between 0 (exclusive) and 100"
            ));
        }

        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_hotkeys = std::collections::HashSet::new();
        for task in &self.capture_tasks {
            if task.id.is_empty()
                || !task.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(anyhow!(
                    "capture_task id '{}' must be lowercase alphanumeric with hyphens",
                    task.id
                ));
            }
            if task.id.len() > 64 {
                return Err(anyhow!("capture_task id too long: {}", task.id));
            }
            if !seen_ids.insert(task.id.clone()) {
                return Err(anyhow!("duplicate capture_task id: {}", task.id));
            }
            if !task.hotkey.is_empty() && !seen_hotkeys.insert(task.hotkey.clone()) {
                return Err(anyhow!(
                    "duplicate hotkey '{}' on task '{}'",
                    task.hotkey, task.id
                ));
            }
            if task.name.is_empty() || task.name.len() > 128 {
                return Err(anyhow!("capture_task name length invalid for {}", task.id));
            }
            if task.hotkey.len() > MAX_HOTKEY_LEN {
                return Err(anyhow!("capture_task '{}' hotkey too long", task.id));
            }
            if !task
                .hotkey
                .chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == ' ')
            {
                return Err(anyhow!(
                    "capture_task '{}' hotkey contains invalid characters",
                    task.id
                ));
            }
        }
        Ok(())
    }

    fn sanitize(&mut self) {
        self.output.quality = self.output.quality.min(MAX_QUALITY);
        self.capture.gif_fps = self.capture.gif_fps.clamp(MIN_GIF_FPS, MAX_GIF_FPS);
        self.capture.gif_max_duration_secs = self
            .capture
            .gif_max_duration_secs
            .min(MAX_GIF_DURATION_SECS);
        self.capture.delay_ms = self.capture.delay_ms.min(MAX_DELAY_MS);

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

        self.performance.tick_interval_ms = self
            .performance
            .tick_interval_ms
            .clamp(MIN_TICK_INTERVAL_MS, MAX_TICK_INTERVAL_MS);
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
                preserve_hdr: false,
            },
            capture: CaptureConfig {
                show_cursor: true,
                delay_ms: 0,
                gif_fps: 15,
                gif_max_duration_secs: 30,
                hdr: HdrConfig::default(),
            },
            hotkeys: HotkeyConfig {
                screenshot: "Ctrl+Shift+S".to_string(),
                record_gif: "Ctrl+Shift+G".to_string(),
                disabled_globally: false,
            },
            ui: UiConfig {
                theme: Theme::Dark,
                show_notifications: true,
                copy_to_clipboard: true,
                minimize_to_tray: true,
                auto_start: false,
                check_updates_on_launch: true,
            },
            post_capture: PostCaptureConfig::default(),
            upload: UploadConfig::default(),
            performance: PerformanceConfig::default(),
            marketplace: MarketplaceConfig::default(),
            capture_tasks: default_capture_tasks(),
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
                match toml::from_str::<Config>(&content) {
                    Ok(mut config) => {
                        config.sanitize();
                        if let Err(e) = config.validate() {
                            // keep the validated-default fallback but preserve the
                            // user's file by renaming it — they get a clean state
                            // without losing the data they hand-edited.
                            Self::backup_corrupt_config(&path, &content);
                            tracing::warn!(
                                "config at {path:?} failed validation: {e}. \
                                 falling back to defaults; original saved to .bak"
                            );
                            return Ok(Config::default());
                        }
                        // first-launch-after-upgrade migration: if the legacy
                        // plaintext FTP password is set but the encrypted slot
                        // is empty, wrap it now and persist. The user never
                        // sees their cleartext password on disk again.
                        let needs_secret_migration = (!config.upload.ftp.password.is_empty()
                            && config.upload.ftp.password_encrypted.is_empty())
                            || (!config.upload.sftp.password.is_empty()
                                && config.upload.sftp.password_encrypted.is_empty())
                            || (!config.upload.sftp.private_key_passphrase.is_empty()
                                && config.upload.sftp.private_key_passphrase_encrypted.is_empty());
                        if needs_secret_migration {
                            config.migrate_secrets();
                            if let Err(e) = config.save() {
                                tracing::warn!(
                                    "secret migration save failed: {e}; \
                                     plaintext password stays on disk for now"
                                );
                            }
                        }
                        return Ok(config);
                    }
                    Err(e) => {
                        Self::backup_corrupt_config(&path, &content);
                        tracing::warn!(
                            "config at {path:?} failed to parse: {e}. \
                             falling back to defaults; original saved to .bak"
                        );
                        return Ok(Config::default());
                    }
                }
            }
        }
        Ok(Config::default())
    }

    fn backup_corrupt_config(path: &Path, content: &str) {
        let mut backup = path.to_path_buf();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let new_name = format!(
            "config.bad.{stamp}.toml"
        );
        backup.set_file_name(new_name);
        // write-then-replace: never delete the user's broken config until
        // the backup has been written successfully.
        if let Err(e) = fs::write(&backup, content) {
            tracing::error!("could not back up corrupt config to {backup:?}: {e}");
        }
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        // migrate any plaintext FTP password into the DPAPI vault on save so
        // the on-disk config never carries credentials in the clear once the
        // user has done at least one save with 0.3.43+
        let mut to_persist = self.clone();
        to_persist.migrate_secrets();
        if let Some(dir) = Self::config_dir() {
            fs::create_dir_all(&dir)?;
            if let Some(path) = Self::config_path() {
                let content = toml::to_string_pretty(&to_persist)?;
                // atomic write: write to a temp file, then rename so a crash
                // mid-write can't leave config.toml truncated/corrupted
                let tmp = path.with_extension("toml.tmp");
                fs::write(&tmp, &content)?;
                fs::rename(&tmp, &path)?;
            }
        }
        Ok(())
    }

    /// in-place: encrypt any plaintext secrets that haven't been wrapped yet
    /// and clear the plaintext field. Idempotent — running twice is a no-op
    /// after the first.
    pub fn migrate_secrets(&mut self) {
        let ftp = &mut self.upload.ftp;
        if !ftp.password.is_empty() && ftp.password_encrypted.is_empty() {
            match crate::secret::encrypt(&ftp.password) {
                Ok(blob) => {
                    ftp.password_encrypted = blob;
                    ftp.password.clear();
                    tracing::info!("migrated FTP password into encrypted vault");
                }
                Err(e) => {
                    tracing::warn!(
                        "FTP password DPAPI encrypt failed; leaving plaintext: {e}"
                    );
                }
            }
        }
        let sftp = &mut self.upload.sftp;
        if !sftp.password.is_empty() && sftp.password_encrypted.is_empty() {
            match crate::secret::encrypt(&sftp.password) {
                Ok(blob) => {
                    sftp.password_encrypted = blob;
                    sftp.password.clear();
                    tracing::info!("migrated SFTP password into encrypted vault");
                }
                Err(e) => {
                    tracing::warn!(
                        "SFTP password DPAPI encrypt failed; leaving plaintext: {e}"
                    );
                }
            }
        }
        if !sftp.private_key_passphrase.is_empty()
            && sftp.private_key_passphrase_encrypted.is_empty()
        {
            match crate::secret::encrypt(&sftp.private_key_passphrase) {
                Ok(blob) => {
                    sftp.private_key_passphrase_encrypted = blob;
                    sftp.private_key_passphrase.clear();
                    tracing::info!("migrated SFTP key passphrase into encrypted vault");
                }
                Err(e) => {
                    tracing::warn!(
                        "SFTP key passphrase DPAPI encrypt failed; leaving plaintext: {e}"
                    );
                }
            }
        }
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
        let pictures =
            directories::UserDirs::new().and_then(|d| d.picture_dir().map(|p| p.to_path_buf()));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_performance_config() {
        let config = Config::default();
        assert_eq!(config.performance.tick_interval_ms, 100);
        assert_eq!(config.performance.renderer, RendererBackend::TinySkia);
        assert!(config.performance.lazy_init_upload);
        assert!(config.performance.lazy_init_plugins);
    }

    #[test]
    fn test_validate_rejects_invalid_tick_interval() {
        let mut config = Config::default();
        config.performance.tick_interval_ms = 10;
        assert!(config.validate().is_err());
        config.performance.tick_interval_ms = 700;
        assert!(config.validate().is_err());
    }
}
