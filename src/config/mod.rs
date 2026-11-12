#[cfg(target_os = "linux")]
use anyhow::Context;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
// container-level default: any field absent from config.toml is filled from
// Config::default() rather than failing the whole parse. combined with dropping
// deny_unknown_fields, this keeps one stale, forward-versioned, or partial field
// from wiping every task and stored credential (the sanitize/validate repair
// pass only runs *after* a successful parse, so it never saw this).
#[serde(default)]
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
    pub marketplace: MarketplaceConfig,
    #[serde(default = "default_capture_tasks")]
    pub capture_tasks: Vec<CaptureTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CaptureTask {
    pub id: String,
    pub name: String,
    pub hotkey: String,
    pub capture_mode: TaskCaptureMode,
    pub post_action: TaskPostAction,
    #[serde(default)]
    pub target_destination: Option<TaskUploadTarget>,
    /// per-task pre-capture delay in ms; None falls back to the global
    /// capture.delay_ms, so a timed task can coexist with instant ones
    #[serde(default)]
    pub delay_ms: Option<u32>,
}

impl Default for CaptureTask {
    fn default() -> Self {
        // only reached when a task entry in config.toml is missing fields; a
        // blank id makes sanitize()/validate() drop it rather than the parser
        // discarding the whole task list
        Self {
            id: String::new(),
            name: String::new(),
            hotkey: String::new(),
            capture_mode: TaskCaptureMode::Region,
            post_action: TaskPostAction::SaveAndClipboard,
            target_destination: None,
            delay_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCaptureMode {
    Region,
    RegionLast,
    Window,
    Fullscreen,
    ActiveMonitor,
    RegionGif,
    RegionMp4,
}

impl TaskCaptureMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskCaptureMode::Region => "Region",
            TaskCaptureMode::RegionLast => "Region (last)",
            TaskCaptureMode::Window => "Window",
            TaskCaptureMode::Fullscreen => "Fullscreen (selector)",
            TaskCaptureMode::ActiveMonitor => "Active monitor",
            TaskCaptureMode::RegionGif => "Region GIF",
            TaskCaptureMode::RegionMp4 => "Region MP4",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskPostAction {
    // tasks, the global post_capture.action and take_screenshot all read
    // this enum; the aliases keep every spelling any of them once wrote
    #[serde(alias = "CopyToClipboard", alias = "copy-to-clipboard")]
    Clipboard,
    #[serde(alias = "SaveToFile", alias = "save-to-file")]
    SaveFile,
    #[serde(alias = "Upload")]
    Upload,
    #[serde(alias = "SaveAndCopy", alias = "save-and-copy")]
    SaveAndClipboard,
    OpenEditor,
    Annotate,
    #[serde(alias = "PromptUser", alias = "prompt-user")]
    Prompt,
    #[serde(alias = "DoNothing")]
    DoNothing,
    /// run OCR on the fresh capture and copy the detected text to the clipboard
    CopyText,
}

impl TaskPostAction {
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskPostAction::Clipboard => "Copy to clipboard",
            TaskPostAction::SaveFile => "Save to file",
            TaskPostAction::Upload => "Upload",
            TaskPostAction::SaveAndClipboard => "Save and copy",
            TaskPostAction::OpenEditor => "Open editor",
            TaskPostAction::Annotate => "Annotate",
            TaskPostAction::Prompt => "Ask each time",
            TaskPostAction::DoNothing => "Do nothing",
            TaskPostAction::CopyText => "Copy detected text (OCR)",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskUploadTarget {
    Imgur,
    Custom,
    Sftp,
    S3,
}

fn default_capture_tasks() -> Vec<CaptureTask> {
    vec![
        CaptureTask {
            id: "screenshot-save-clipboard".to_string(),
            name: "Screenshot — save + clipboard".to_string(),
            hotkey: "".to_string(),
            capture_mode: TaskCaptureMode::Region,
            post_action: TaskPostAction::SaveAndClipboard,
            target_destination: None,
            delay_ms: None,
        },
        CaptureTask {
            id: "gif-save".to_string(),
            name: "Region GIF to file".to_string(),
            hotkey: "Ctrl+Shift+G".to_string(),
            capture_mode: TaskCaptureMode::RegionGif,
            post_action: TaskPostAction::SaveFile,
            target_destination: None,
            delay_ms: None,
        },
        CaptureTask {
            id: "mp4-save".to_string(),
            name: "Region MP4 to file".to_string(),
            hotkey: "Ctrl+Shift+V".to_string(),
            capture_mode: TaskCaptureMode::RegionMp4,
            post_action: TaskPostAction::SaveFile,
            target_destination: None,
            delay_ms: None,
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

/// the default captures directory: <Pictures>/capscr, falling back to the home
/// dir then the current dir when neither is resolvable
fn default_output_dir() -> PathBuf {
    let pictures_dir = directories::UserDirs::new()
        .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        });
    pictures_dir.join("capscr")
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            directory: default_output_dir(),
            format: ImageFormat::Png,
            quality: 90,
            filename_template: "capture_%Y%m%d_%H%M%S".to_string(),
            preserve_hdr: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Avif,
    Jxl,
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
            ImageFormat::Avif => "avif",
            ImageFormat::Jxl => "jxl",
        }
    }

    pub fn all() -> &'static [ImageFormat] {
        &[
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::Webp,
            ImageFormat::Bmp,
            ImageFormat::Avif,
            ImageFormat::Jxl,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::Webp => "WebP",
            ImageFormat::Bmp => "BMP",
            ImageFormat::Avif => "AVIF",
            ImageFormat::Jxl => "JPEG XL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub show_cursor: bool,
    pub delay_ms: u32,
    pub gif_fps: u32,
    pub gif_max_duration_secs: u32,
    #[serde(default = "default_video_fps")]
    pub video_fps: u32,
    #[serde(default)]
    pub video_quality: VideoQuality,
    #[serde(default)]
    pub hdr: HdrConfig,
    #[serde(default = "default_record_audio")]
    pub record_audio: bool,
    #[serde(default)]
    pub record_mic: bool,
    #[serde(default)]
    pub gif: GifOptions,
    #[serde(default)]
    pub video_encoder: VideoEncoder,
}

/// h264 encoder for mp4 recordings; a hardware pick that fails its trial
/// encode falls back to libx264
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VideoEncoder {
    #[default]
    Auto,
    Software,
    Nvenc,
    Qsv,
    Amf,
}

fn default_record_audio() -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GifPalette {
    #[default]
    Global,
    PerFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GifEngine {
    #[default]
    Builtin,
    Ffmpeg,
}

/// how a gif recording is shrunk into 256-color frames
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GifOptions {
    pub scale_percent: u8,
    // 0 leaves the width alone
    pub max_width: u32,
    pub colors: u16,
    pub dither: bool,
    pub palette: GifPalette,
    // unchanged pixels become transparent so each frame only stores what moved
    pub diff_frames: bool,
    pub engine: GifEngine,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self {
            scale_percent: 100,
            max_width: 0,
            colors: 256,
            dither: true,
            palette: GifPalette::Global,
            diff_frames: true,
            engine: GifEngine::Builtin,
        }
    }
}

impl GifOptions {
    fn repair(&mut self) {
        self.scale_percent = self.scale_percent.clamp(10, 100);
        self.colors = self.colors.clamp(2, 256);
        if self.max_width != 0 {
            self.max_width = self.max_width.clamp(16, 4096);
        }
    }
}

fn default_video_fps() -> u32 {
    30
}

/// x264 crf tier for mp4 recordings; gifs are palette-bound and unaffected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VideoQuality {
    #[default]
    High,
    Balanced,
    Compact,
}

impl VideoQuality {
    pub fn crf(self) -> u8 {
        match self {
            VideoQuality::High => 18,
            VideoQuality::Balanced => 23,
            VideoQuality::Compact => 28,
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            show_cursor: true,
            delay_ms: 0,
            gif_fps: 15,
            gif_max_duration_secs: 30,
            video_fps: 30,
            video_quality: VideoQuality::default(),
            hdr: HdrConfig::default(),
            record_audio: false,
            record_mic: false,
            gif: GifOptions::default(),
            video_encoder: VideoEncoder::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HdrOutputFormat {
    /// 16-bit BT.2020 PNG with PQ transfer (cICP 9/16/0/1). default — the
    /// best-supported HDR PNG format for Windows Photos, Edge, IrfanView w/
    /// libpng>=1.6.39.
    #[default]
    Pq,
    /// 16-bit BT.2020 PNG with HLG transfer (cICP 9/18/0/1). useful when
    /// targeting TV/broadcast displays or HLG-aware viewers. transcoded from
    /// the HDR10 PQ source by decoding PQ to linear nits then applying HLG OETF.
    Hlg,
}

impl HdrOutputFormat {
    pub fn all() -> &'static [HdrOutputFormat] {
        &[HdrOutputFormat::Pq, HdrOutputFormat::Hlg]
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            HdrOutputFormat::Pq => "PQ (BT.2020, HDR10-style — default)",
            HdrOutputFormat::Hlg => "HLG (BT.2020, broadcast-style)",
        }
    }
}

impl std::fmt::Display for HdrOutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrConfig {
    /// manual override for the display's SDR white level in nits. 0.0 means
    /// auto-detect via DISPLAYCONFIG_SDR_WHITE_LEVEL (with a DXGI fallback).
    pub brightness_nits: f32,
    pub user_brightness_scale: f32,
    /// HDR-preserving PNG transfer characteristic when preserve_hdr is on.
    /// applies only to HDR sources; SDR captures bypass the HDR encoder
    /// entirely
    #[serde(default)]
    pub output_format: HdrOutputFormat,
}

impl Default for HdrConfig {
    fn default() -> Self {
        Self {
            brightness_nits: 0.0,
            user_brightness_scale: 1.0,
            output_format: HdrOutputFormat::Pq,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HotkeyConfig {
    #[serde(default)]
    pub screenshot: String,
    #[serde(default)]
    pub record_gif: String,
    // pauses and resumes the running recording; unbound by default
    #[serde(default)]
    pub pause_recording: String,
    // user-controlled global kill switch toggled from the tray or Settings.
    // when true, the LL keyboard hook is installed but its match table is
    // empty, so no chord can fire a task. preserved across restarts.
    #[serde(default)]
    pub disabled_globally: bool,
    // linux "advanced input": opt-in for the raw evdev backend (mouse-button
    // hotkeys, portal-less wayland keyboards). None means unset and resolves
    // at startup to true iff mouse-button bindings already exist, so
    // established setups migrate without a settings visit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_input: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CloseBehavior {
    #[default]
    MinimizeToTray,
    MinimizeToTaskbar,
    Exit,
}

impl CloseBehavior {
    pub fn all() -> &'static [CloseBehavior] {
        &[
            CloseBehavior::MinimizeToTray,
            CloseBehavior::MinimizeToTaskbar,
            CloseBehavior::Exit,
        ]
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            CloseBehavior::MinimizeToTray => "Minimize to system tray",
            CloseBehavior::MinimizeToTaskbar => "Minimize to taskbar",
            CloseBehavior::Exit => "Close/Exit application",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: Theme,
    pub show_notifications: bool,
    pub copy_to_clipboard: bool,
    #[serde(default)]
    pub close_behavior: CloseBehavior,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub check_updates_on_launch: bool,
    #[serde(default)]
    pub save_clipboard_to_history: bool,
    // set once the user dismisses the "no system tray" first-run banner, so
    // it never nags again on tray-less desktops (vanilla gnome)
    #[serde(default)]
    pub tray_hint_dismissed: bool,
    // destroy the hidden hub webview instead of keeping it warm; linux always does
    #[serde(default)]
    pub free_memory_when_hidden: bool,
    // labels of windows already grown to their first-open share of the screen
    #[serde(default)]
    pub grown_windows: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            show_notifications: true,
            copy_to_clipboard: true,
            close_behavior: CloseBehavior::MinimizeToTray,
            // opt-in: a fresh install must not add itself to Windows startup
            // before the user has chosen to, and with no persisted config the
            // launch-time reconcile would otherwise re-add it every boot
            auto_start: false,
            check_updates_on_launch: true,
            save_clipboard_to_history: false,
            tray_hint_dismissed: false,
            free_memory_when_hidden: false,
            grown_windows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PostCaptureConfig {
    pub action: TaskPostAction,
    pub open_file_after_save: bool,
    pub play_sound: bool,
}

impl Default for PostCaptureConfig {
    fn default() -> Self {
        Self {
            action: TaskPostAction::SaveAndClipboard,
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
    Sftp,
    S3,
    /// the plugin named by `UploadConfig::plugin`
    Plugin,
}

/// tray ids for plugin destinations; the plugin id follows the prefix
pub const PLUGIN_MENU_PREFIX: &str = "dest_plugin:";

impl UploadDestination {
    pub fn all() -> &'static [UploadDestination] {
        &[
            UploadDestination::Imgur,
            UploadDestination::Custom,
            UploadDestination::Sftp,
            UploadDestination::S3,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            UploadDestination::Imgur => "Imgur",
            UploadDestination::Custom => "Custom HTTP",
            UploadDestination::Sftp => "SFTP",
            UploadDestination::S3 => "S3 Compatible",
            UploadDestination::Plugin => "Plugin",
        }
    }

    pub fn menu_id(&self) -> String {
        format!("dest_{self:?}").to_lowercase()
    }

    pub fn from_menu_id(id: &str) -> Option<Self> {
        Self::all().iter().copied().find(|d| d.menu_id() == id)
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
#[serde(default)]
pub struct UploadConfig {
    pub destination: UploadDestination,
    pub copy_url_to_clipboard: bool,
    pub custom_url: String,
    pub custom_form_name: String,
    pub custom_response_path: String,
    #[serde(default = "default_imgur_client_id")]
    pub imgur_client_id: String,
    #[serde(default)]
    pub sftp: SftpUploadConfig,
    #[serde(default)]
    pub s3: S3UploadConfig,
    /// plugin id used when the destination is Plugin
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
    crate::upload::BUILTIN_IMGUR_CLIENT_ID.to_string()
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
            sftp: SftpUploadConfig::default(),
            s3: S3UploadConfig::default(),
            plugin: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SftpUploadConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_sftp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// plaintext password (legacy slot), kept so users restoring a hand-edited
    /// config don't have to retype credentials;
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
        if !self.password.is_empty() {
            return self.password.clone();
        }
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
        String::new()
    }

    pub fn private_key_passphrase_plaintext(&self) -> String {
        if !self.private_key_passphrase.is_empty() {
            return self.private_key_passphrase.clone();
        }
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
        String::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct S3UploadConfig {
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub access_key_id: String,
    /// plaintext secret key (legacy field)
    #[serde(default)]
    pub secret_access_key: String,
    /// DPAPI-wrapped secret key
    #[serde(default)]
    pub secret_access_key_encrypted: String,
    #[serde(default)]
    pub public_url_template: String,
}

impl S3UploadConfig {
    pub fn secret_access_key_plaintext(&self) -> String {
        if !self.secret_access_key.is_empty() {
            return self.secret_access_key.clone();
        }
        if !self.secret_access_key_encrypted.is_empty() {
            match crate::secret::decrypt(&self.secret_access_key_encrypted) {
                Ok(p) => return p,
                Err(e) => {
                    tracing::warn!("S3 secret access key decrypt failed: {e}");
                }
            }
        }
        String::new()
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
        if self.capture.video_fps < MIN_GIF_FPS || self.capture.video_fps > MAX_GIF_FPS {
            return Err(anyhow!(
                "video_fps must be between {} and {}",
                MIN_GIF_FPS,
                MAX_GIF_FPS
            ));
        }
        let mut gif = self.capture.gif.clone();
        gif.repair();
        if gif != self.capture.gif {
            return Err(anyhow!(
                "gif options out of range: scale 10-100%, colors 2-256, max width 0 or 16-4096"
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
        if !template_is_valid(&self.output.filename_template) {
            return Err(anyhow!("filename_template has an invalid % specifier"));
        }
        // reject a clearly-bad output directory at save time, before set_config
        // widens the asset-protocol scope to it. sanitize() repairs these in a
        // loaded config, so this only rejects a fresh bad value the UI tries to
        // save. UNC is deliberately NOT rejected here: an enterprise Pictures
        // folder redirected to a share makes the default dir itself UNC, and
        // rejecting it would fail load and wipe the whole config; ensure_output_dir
        // still surfaces an unreachable-folder notice at capture time.
        if self.output.directory.as_os_str().is_empty() {
            return Err(anyhow!("output directory path is empty"));
        }
        if self.output.directory.to_string_lossy().contains("..") {
            return Err(anyhow!("output directory contains path traversal"));
        }
        for hotkey in [
            &self.hotkeys.screenshot,
            &self.hotkeys.record_gif,
            &self.hotkeys.pause_recording,
        ] {
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
                || !task
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
                    task.hotkey,
                    task.id
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
        self.capture.video_fps = self.capture.video_fps.clamp(MIN_GIF_FPS, MAX_GIF_FPS);
        self.capture.gif.repair();
        self.capture.gif_max_duration_secs = self
            .capture
            .gif_max_duration_secs
            .min(MAX_GIF_DURATION_SECS);
        self.capture.delay_ms = self.capture.delay_ms.min(MAX_DELAY_MS);

        if self.output.filename_template.len() > MAX_FILENAME_TEMPLATE_LEN
            || self.output.filename_template.contains('/')
            || self.output.filename_template.contains('\\')
            || self.output.filename_template.contains("..")
            || !template_is_valid(&self.output.filename_template)
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

        // repair a bad output directory in place so validate() doesn't fail the
        // whole config: reset an empty or traversal path to the default captures
        // dir. UNC is left alone — it's a legitimate redirected-Pictures target
        // and default_output_dir() could itself be UNC, so "repairing" it would
        // just fail validate again and wipe the config.
        {
            let out_dir = self.output.directory.to_string_lossy();
            if self.output.directory.as_os_str().is_empty() || out_dir.contains("..") {
                drop(out_dir);
                self.output.directory = default_output_dir();
            }
        }

        // repair everything validate() would reject so a parseable config is
        // always salvageable in place: without this, one bad hdr value or one
        // duplicate task hotkey made load() throw away every setting (theme,
        // output dir, upload destination, in-memory secrets) for the session.
        if !self.capture.hdr.brightness_nits.is_finite() || self.capture.hdr.brightness_nits < 0.0 {
            self.capture.hdr.brightness_nits = 0.0;
        } else if self.capture.hdr.brightness_nits > 10000.0 {
            self.capture.hdr.brightness_nits = 10000.0;
        }
        if !self.capture.hdr.user_brightness_scale.is_finite()
            || self.capture.hdr.user_brightness_scale <= 0.0
        {
            self.capture.hdr.user_brightness_scale = 1.0;
        } else if self.capture.hdr.user_brightness_scale > 100.0 {
            self.capture.hdr.user_brightness_scale = 100.0;
        }

        let hotkey_chars_ok = |s: &str| {
            s.chars()
                .all(|c| c.is_alphanumeric() || c == '+' || c == ' ')
        };
        for hk in [
            &mut self.hotkeys.screenshot,
            &mut self.hotkeys.record_gif,
            &mut self.hotkeys.pause_recording,
        ] {
            if hk.len() > MAX_HOTKEY_LEN || !hotkey_chars_ok(hk) {
                hk.clear();
            }
        }

        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_hotkeys = std::collections::HashSet::new();
        self.capture_tasks.retain_mut(|task| {
            // an empty/malformed id or name can't be repaired meaningfully; drop
            // the task rather than nuke the whole config
            let id_ok = !task.id.is_empty()
                && task.id.len() <= 64
                && task
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            let name_ok = !task.name.is_empty() && task.name.len() <= 128;
            if !id_ok || !name_ok || !seen_ids.insert(task.id.clone()) {
                return false;
            }
            // a malformed or already-taken hotkey just unbinds the task, keeping
            // the rest of it intact
            if task.hotkey.len() > MAX_HOTKEY_LEN || !hotkey_chars_ok(&task.hotkey) {
                task.hotkey.clear();
            }
            if !task.hotkey.is_empty() && !seen_hotkeys.insert(task.hotkey.clone()) {
                task.hotkey.clear();
            }
            true
        });
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output: OutputConfig::default(),
            capture: CaptureConfig::default(),
            hotkeys: HotkeyConfig {
                screenshot: "Ctrl+Shift+S".to_string(),
                record_gif: "Ctrl+Shift+G".to_string(),
                pause_recording: String::new(),
                disabled_globally: false,
                advanced_input: None,
            },
            ui: UiConfig::default(),
            post_capture: PostCaptureConfig::default(),
            upload: UploadConfig::default(),
            marketplace: MarketplaceConfig::default(),
            capture_tasks: default_capture_tasks(),
        }
    }
}

pub fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "capscr", "capscr")
}

impl Config {
    pub fn config_dir() -> Option<PathBuf> {
        project_dirs().map(|p| p.config_dir().to_path_buf())
    }

    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|p| p.join("config.toml"))
    }

    pub fn load() -> Result<(Self, Vec<String>)> {
        match Self::config_path() {
            Some(path) => Self::load_from(&path),
            None => Ok((Config::default(), Vec::new())),
        }
    }

    /// the saved tasks, read without the migration saves load() can make
    pub fn peek_tasks() -> Result<Vec<CaptureTask>> {
        let Some(path) = Self::config_path().filter(|path| path.exists()) else {
            return Ok(default_capture_tasks());
        };
        Self::peek_tasks_from(&path)
    }

    fn peek_tasks_from(path: &Path) -> Result<Vec<CaptureTask>> {
        let mut value: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
        migrate_legacy_ftp(&mut value);
        let mut config: Config = value.try_into()?;
        config.sanitize();
        Ok(config.capture_tasks)
    }

    fn load_from(path: &Path) -> Result<(Self, Vec<String>)> {
        if !path.exists() {
            return Ok((Config::default(), Vec::new()));
        }
        let original = fs::read_to_string(path)?;
        let mut content = original.clone();
        let mut legacy_ftp = None;
        if let Ok(mut value) = toml::from_str::<toml::Value>(&original) {
            if let Some(found) = migrate_legacy_ftp(&mut value) {
                if let Ok(text) = toml::to_string(&value) {
                    content = text;
                    legacy_ftp = Some(found);
                }
            }
        }
        let loaded = toml::from_str::<Config>(&content)
            .map_err(anyhow::Error::from)
            .and_then(|mut config| {
                config.sanitize();
                config.validate().map(|()| config)
            });
        let mut config = match loaded {
            Ok(config) => config,
            Err(e) => {
                // defaults keep the app usable; the user's file is copied aside
                // first so the next settings save can't destroy what they wrote
                tracing::warn!(
                    "config at {path:?} couldn't be loaded: {e:#}; falling back to defaults"
                );
                let reason = e.to_string();
                let reason = reason.lines().next().unwrap_or_default().trim();
                let notice = match Self::backup_corrupt_config(path, &original) {
                    Some(backup) => format!(
                        "config.toml couldn't be loaded ({reason}); defaults are in use \
                         and your old file is saved as {}",
                        backup.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    None => format!(
                        "config.toml couldn't be loaded ({reason}) or backed up; defaults \
                         are in use, so copy config.toml somewhere safe before saving settings"
                    ),
                };
                return Ok((Config::default(), vec![notice]));
            }
        };
        // first-launch-after-upgrade migration: a legacy plaintext password with
        // an empty encrypted slot is wrapped now and persisted, so cleartext
        // never stays on disk
        let needs_save = legacy_ftp.is_some() || config.has_plaintext_secrets();
        #[cfg(target_os = "linux")]
        let needs_save = needs_save
            || [
                &config.upload.sftp.password_encrypted,
                &config.upload.sftp.private_key_passphrase_encrypted,
                &config.upload.s3.secret_access_key_encrypted,
            ]
            .iter()
            .any(|blob| !blob.is_empty() && !blob.starts_with("keyring:"));
        if needs_save {
            if let Err(e) = config.save_and_migrate_secrets() {
                tracing::warn!(
                    "config migration save failed: {e}; the old file stays on disk for now"
                );
            } else if let Some(legacy) = &legacy_ftp {
                let mut retired = crate::secret::SecretTransaction::default();
                retired.retire(&legacy.secret);
                retired.commit();
            }
        }
        let notices = legacy_ftp
            .and_then(|legacy| legacy.notice)
            .into_iter()
            .collect();
        Ok((config, notices))
    }

    fn backup_corrupt_config(path: &Path, content: &str) -> Option<PathBuf> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let backup = path.with_file_name(format!("config.bad.{stamp}.toml"));
        match fs::write(&backup, content) {
            Ok(()) => Some(backup),
            Err(e) => {
                tracing::error!("could not back up corrupt config to {backup:?}: {e}");
                None
            }
        }
    }

    pub(crate) fn save_and_migrate_secrets(&mut self) -> Result<()> {
        *self = self.persisted_copy()?;
        Ok(())
    }

    fn persisted_copy(&self) -> Result<Self> {
        self.validate()?;
        let mut to_persist = self.clone();
        let transaction = to_persist.migrate_secrets()?;
        let dir = Self::config_dir().ok_or_else(|| anyhow!("config directory is unavailable"))?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        let content = toml::to_string_pretty(&to_persist)?;
        // atomic write: write to a temp file, then rename so a crash
        // mid-write can't leave config.toml truncated/corrupted
        let tmp = path.with_extension(format!("toml.{}.tmp", uuid::Uuid::new_v4().as_simple()));
        if let Err(error) = fs::write(&tmp, &content) {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow!("config temp write failed: {error}"));
        }
        if let Err(error) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow!("config atomic rename failed: {error}"));
        }
        transaction.commit();
        Ok(to_persist)
    }

    fn has_plaintext_secrets(&self) -> bool {
        !self.upload.sftp.password.is_empty()
            || !self.upload.sftp.private_key_passphrase.is_empty()
            || !self.upload.s3.secret_access_key.is_empty()
    }

    /// in-place: encrypt any plaintext secrets that haven't been wrapped yet
    /// and clear the plaintext field. Idempotent — running twice is a no-op
    /// after the first.
    fn migrate_secrets(&mut self) -> Result<crate::secret::SecretTransaction> {
        let mut transaction = crate::secret::SecretTransaction::default();
        #[cfg(target_os = "linux")]
        for blob in [
            &mut self.upload.sftp.password_encrypted,
            &mut self.upload.sftp.private_key_passphrase_encrypted,
            &mut self.upload.s3.secret_access_key_encrypted,
        ] {
            if !blob.is_empty() && !blob.starts_with("keyring:") {
                let plaintext = crate::secret::decrypt(blob)
                    .context("couldn't read a legacy on-disk credential")?;
                *blob = transaction
                    .replace(&plaintext, blob)
                    .context("couldn't move a legacy credential into the system keyring")?;
            }
        }

        migrate_secret(
            &mut self.upload.sftp.password,
            &mut self.upload.sftp.password_encrypted,
            "SFTP password",
            &mut transaction,
        )?;
        migrate_secret(
            &mut self.upload.sftp.private_key_passphrase,
            &mut self.upload.sftp.private_key_passphrase_encrypted,
            "SFTP key passphrase",
            &mut transaction,
        )?;
        migrate_secret(
            &mut self.upload.s3.secret_access_key,
            &mut self.upload.s3.secret_access_key_encrypted,
            "S3 secret access key",
            &mut transaction,
        )?;
        Ok(transaction)
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

        // any local directory the user can actually write to is a valid
        // output dir — people keep captures on secondary drives. Probe
        // writability directly instead of whitelisting home/pictures/temp.
        let probe = dir.join(".capscr-write-probe");
        fs::write(&probe, b"").map_err(|e| anyhow!("Output directory is not writable: {e}"))?;
        let _ = fs::remove_file(&probe);

        Ok(())
    }

    pub fn generate_filename(&self) -> String {
        use std::fmt::Write;
        let now = chrono::Local::now();
        // chrono's Display panics on a bad specifier and release builds abort
        let mut formatted = String::new();
        if write!(formatted, "{}", now.format(&self.output.filename_template)).is_err() {
            formatted.clear();
        }
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

#[derive(Debug, PartialEq)]
struct LegacyFtp {
    notice: Option<String>,
    secret: String,
}

// plain ftp upload was removed in 0.6.0: anything still pointing at it moves to
// sftp, and the old [upload.ftp] table and its stored password are dropped
fn migrate_legacy_ftp(value: &mut toml::Value) -> Option<LegacyFtp> {
    let mut moved = false;
    let tasks = value
        .get_mut("capture_tasks")
        .and_then(toml::Value::as_array_mut);
    for task in tasks.into_iter().flatten() {
        if let Some(target) = task.get_mut("target_destination") {
            if target.as_str() == Some("ftp") {
                *target = "sftp".into();
                moved = true;
            }
        }
    }
    let mut ftp = None;
    if let Some(upload) = value.get_mut("upload").and_then(toml::Value::as_table_mut) {
        if let Some(destination) = upload.get_mut("destination") {
            if destination.as_str() == Some("Ftp") {
                *destination = "Sftp".into();
                moved = true;
            }
        }
        ftp = upload.remove("ftp");
        let host = ftp
            .as_ref()
            .and_then(|ftp| ftp.get("host"))
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if moved && !host.is_empty() {
            let sftp = upload
                .entry("sftp")
                .or_insert_with(|| toml::Table::new().into());
            if let Some(sftp) = sftp.as_table_mut() {
                if sftp
                    .get("host")
                    .and_then(toml::Value::as_str)
                    .unwrap_or_default()
                    .is_empty()
                {
                    sftp.insert("host".into(), host.into());
                }
            }
        }
    }
    if !moved && ftp.is_none() {
        return None;
    }
    let field = |key: &str| {
        ftp.as_ref()
            .and_then(|ftp| ftp.get(key))
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let host = field("host");
    let notice = moved.then(|| {
        let at = if host.is_empty() {
            String::new()
        } else {
            format!(" (host {host})")
        };
        format!(
            "ftp upload was removed, so uploads that used ftp now go to sftp{at}. \
             check the sftp settings in destinations before the next upload"
        )
    });
    Some(LegacyFtp {
        notice,
        secret: field("password_encrypted"),
    })
}

fn template_is_valid(template: &str) -> bool {
    chrono::format::StrftimeItems::new(template).parse().is_ok()
}

fn migrate_secret(
    plaintext: &mut String,
    encrypted: &mut String,
    label: &str,
    transaction: &mut crate::secret::SecretTransaction,
) -> Result<()> {
    if plaintext.is_empty() {
        return Ok(());
    }
    let blob = transaction
        .replace(plaintext, encrypted)
        .map_err(|error| anyhow!("couldn't store {label} in the credential vault: {error:#}"))?;
    plaintext.clear();
    *encrypted = blob;
    tracing::info!("migrated {label} into encrypted vault");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_and_reads_files_written_by_toml_08() {
        let mut expected = Config::default();
        let text = toml::to_string_pretty(&expected).unwrap();
        let reparsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(toml::to_string_pretty(&reparsed).unwrap(), text);

        // the fixture is Config::default() as toml 0.8 wrote it, with the
        // machine-specific folder and the shared client id swapped out
        expected.output.directory = PathBuf::from("/captures");
        expected.upload.imgur_client_id.clear();
        expected.capture_tasks[0].name = "Screenshot, save + clipboard".into();
        let old: Config = toml::from_str(include_str!("fixtures/default-toml08.toml")).unwrap();
        assert_eq!(
            toml::to_string_pretty(&old).unwrap(),
            toml::to_string_pretty(&expected).unwrap()
        );
    }

    #[test]
    fn validate_rejects_bad_template_specifiers() {
        for bad in ["cap_%", "cap_%Q", "%Y%"] {
            let mut config = Config::default();
            config.output.filename_template = bad.to_string();
            assert!(config.validate().is_err(), "{bad} should be rejected");
        }
        for good in ["capture_%Y%m%d_%H%M%S", "shot-%Y-%m-%d"] {
            let mut config = Config::default();
            config.output.filename_template = good.to_string();
            assert!(config.validate().is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn sanitize_resets_bad_template() {
        let mut config = Config::default();
        config.output.filename_template = "cap_%".to_string();
        config.sanitize();
        assert_eq!(config.output.filename_template, "capture_%Y%m%d_%H%M%S");
    }

    #[test]
    fn generate_filename_survives_bad_template() {
        let mut config = Config::default();
        config.output.filename_template = "cap_%Q".to_string();
        let name = config.generate_filename();
        assert!(name.starts_with("capture_"), "{name}");
        assert!(name.ends_with(config.output.format.extension()), "{name}");
    }

    #[test]
    fn video_quality_settings_default_and_repair() {
        let config = Config::default();
        assert_eq!(config.capture.video_fps, 30);
        assert_eq!(config.capture.video_quality.crf(), 18);
        assert_eq!(VideoQuality::Balanced.crf(), 23);
        assert_eq!(VideoQuality::Compact.crf(), 28);

        // configs written before the fields existed must deserialize to defaults
        let old: CaptureConfig = toml::from_str(
            "show_cursor = true\ndelay_ms = 0\ngif_fps = 15\ngif_max_duration_secs = 30\n",
        )
        .unwrap();
        assert_eq!(old.video_fps, 30);
        assert_eq!(old.video_quality, VideoQuality::High);

        let mut config = Config::default();
        config.capture.video_fps = 500;
        config.sanitize();
        assert_eq!(config.capture.video_fps, 60);
    }

    #[test]
    fn gif_options_default_validate_and_repair() {
        let old: CaptureConfig = toml::from_str("gif_fps = 15\n").unwrap();
        assert_eq!(old.gif, GifOptions::default());

        let mut config = Config::default();
        config.capture.gif.colors = 1;
        config.capture.gif.scale_percent = 0;
        config.capture.gif.max_width = 8;
        assert!(config.validate().unwrap_err().to_string().contains("gif"));
        config.sanitize();
        assert_eq!(config.capture.gif.colors, 2);
        assert_eq!(config.capture.gif.scale_percent, 10);
        assert_eq!(config.capture.gif.max_width, 16);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn sanitize_repairs_instead_of_discarding_config() {
        let mut config = Config::default();
        // an unrelated setting that must survive the repair
        config.output.quality = 55;
        // values validate() rejects that sanitize() must now clamp
        config.capture.hdr.brightness_nits = f32::NAN;
        config.capture.hdr.user_brightness_scale = -3.0;
        // make the screenshot task collide with gif-save's hotkey
        config.capture_tasks[0].hotkey = "Ctrl+Shift+G".to_string();
        // a structurally broken task that can't be repaired
        config.capture_tasks.push(CaptureTask {
            id: "Bad ID!".to_string(),
            name: "x".to_string(),
            hotkey: String::new(),
            capture_mode: TaskCaptureMode::Region,
            post_action: TaskPostAction::SaveFile,
            target_destination: None,
            delay_ms: None,
        });

        config.sanitize();

        assert!(
            config.validate().is_ok(),
            "a parseable config must always sanitize to a valid one"
        );
        assert_eq!(config.output.quality, 55, "unrelated settings must survive");
        assert!(config.capture.hdr.brightness_nits.is_finite());
        assert!(config.capture.hdr.user_brightness_scale > 0.0);
        assert!(
            config.capture_tasks.iter().all(|t| t.id != "Bad ID!"),
            "the malformed-id task should be dropped"
        );
        let bound = config
            .capture_tasks
            .iter()
            .filter(|t| t.hotkey == "Ctrl+Shift+G")
            .count();
        assert_eq!(
            bound, 1,
            "the duplicate hotkey should be unbound on one task"
        );
    }

    #[test]
    fn load_tolerates_missing_field_and_unknown_key() {
        // regression: a config that predates user_brightness_scale and also carries a
        // top-level key from a newer build must still parse — never wipe every
        // task and stored credential the way deny_unknown_fields + a required
        // field once did
        let full = toml::to_string(&Config::default()).unwrap();
        assert!(full.contains("user_brightness_scale"));
        let trimmed = full
            .lines()
            .filter(|l| !l.contains("user_brightness_scale"))
            .collect::<Vec<_>>()
            .join("\n");
        let doctored = format!("future_only_key = \"ignored\"\n{trimmed}");
        let parsed: Config =
            toml::from_str(&doctored).expect("partial + forward-versioned config must still parse");
        assert_eq!(
            parsed.capture.hdr.user_brightness_scale, 1.0,
            "a missing field falls back to its default, not a wiped config"
        );
        assert_eq!(
            parsed.capture_tasks.len(),
            Config::default().capture_tasks.len(),
            "the user's tasks must survive a missing/unknown field"
        );
    }

    #[test]
    fn pending_plaintext_replacements_are_authoritative() {
        let mut config = Config::default();
        config.upload.sftp.password = "new sftp".into();
        config.upload.sftp.password_encrypted = "stale".into();
        config.upload.sftp.private_key_passphrase = "new passphrase".into();
        config.upload.sftp.private_key_passphrase_encrypted = "stale".into();
        config.upload.s3.secret_access_key = "new s3".into();
        config.upload.s3.secret_access_key_encrypted = "stale".into();

        assert!(config.has_plaintext_secrets());
        assert_eq!(config.upload.sftp.password_plaintext(), "new sftp");
        assert_eq!(
            config.upload.sftp.private_key_passphrase_plaintext(),
            "new passphrase"
        );
        assert_eq!(config.upload.s3.secret_access_key_plaintext(), "new s3");
    }

    #[test]
    #[cfg(windows)]
    fn migration_replaces_every_stored_secret_and_clears_plaintext() {
        let old = crate::secret::encrypt("old").expect("encrypt old secret");
        let mut config = Config::default();
        config.upload.sftp.password = "new sftp".into();
        config.upload.sftp.password_encrypted = old.clone();
        config.upload.sftp.private_key_passphrase = "new passphrase".into();
        config.upload.sftp.private_key_passphrase_encrypted = old.clone();
        config.upload.s3.secret_access_key = "new s3".into();
        config.upload.s3.secret_access_key_encrypted = old;

        let transaction = config.migrate_secrets().expect("migrate replacements");
        transaction.commit();

        assert!(!config.has_plaintext_secrets());
        assert_eq!(config.upload.sftp.password_plaintext(), "new sftp");
        assert_eq!(
            config.upload.sftp.private_key_passphrase_plaintext(),
            "new passphrase"
        );
        assert_eq!(config.upload.s3.secret_access_key_plaintext(), "new s3");
    }

    #[test]
    fn rejected_save_does_not_replace_stored_credentials() {
        let mut config = Config::default();
        config.output.quality = MAX_QUALITY + 1;
        config.upload.sftp.password = "pending replacement".into();
        config.upload.sftp.password_encrypted = "keyring:stored".into();

        let error = config
            .save_and_migrate_secrets()
            .expect_err("invalid config must be rejected");

        assert!(error.to_string().contains("quality"));
        assert_eq!(config.upload.sftp.password, "pending replacement");
        assert_eq!(
            config.upload.sftp.password_encrypted, "keyring:stored",
            "validation must run before credential storage"
        );
    }

    fn migrated(text: &str) -> (Option<LegacyFtp>, toml::Value) {
        let mut value: toml::Value = toml::from_str(text).unwrap();
        (migrate_legacy_ftp(&mut value), value)
    }

    fn backups_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("config.bad."))
            })
            .collect()
    }

    fn assert_reset_with_backup(text: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, text).unwrap();

        let (config, notices) = Config::load_from(&path).unwrap();

        assert_eq!(config.output.quality, Config::default().output.quality);
        let backups = backups_in(dir.path());
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), text);
        assert_eq!(notices.len(), 1);
        let name = backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(notices[0].contains(&name), "{}", notices[0]);
    }

    #[test]
    fn valid_config_loads_without_notices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut saved = Config::default();
        saved.output.quality = 77;
        fs::write(&path, toml::to_string_pretty(&saved).unwrap()).unwrap();

        let (config, notices) = Config::load_from(&path).unwrap();

        assert_eq!(config.output.quality, 77);
        assert!(notices.is_empty());
        assert!(backups_in(dir.path()).is_empty());
    }

    #[test]
    fn garbled_config_falls_back_and_names_the_backup() {
        assert_reset_with_backup("this is = [not toml\n");
    }

    #[test]
    fn mistyped_config_falls_back_and_names_the_backup() {
        assert_reset_with_backup("[output]\nquality = \"high\"\n");
    }

    #[test]
    fn peeking_tasks_migrates_in_memory_and_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut saved = Config::default();
        saved.capture_tasks[0].target_destination = Some(TaskUploadTarget::Sftp);
        let text = toml::to_string_pretty(&saved).unwrap().replace(
            "target_destination = \"sftp\"",
            "target_destination = \"ftp\"",
        );
        assert!(text.contains("\"ftp\""));
        fs::write(&path, &text).unwrap();

        let tasks = Config::peek_tasks_from(&path).unwrap();

        assert_eq!(tasks[0].id, saved.capture_tasks[0].id);
        assert_eq!(tasks[0].target_destination, Some(TaskUploadTarget::Sftp));
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_config_loads_defaults_without_notices() {
        let dir = tempfile::tempdir().unwrap();
        let (config, notices) = Config::load_from(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config.output.quality, Config::default().output.quality);
        assert!(notices.is_empty());
    }

    #[test]
    fn legacy_ftp_destination_moves_to_sftp_with_its_host() {
        let (legacy, value) = migrated(
            "[upload]\ndestination = \"Ftp\"\n[upload.ftp]\nhost = \"files.example.com\"\n\
             password_encrypted = \"keyring:old\"\n",
        );
        let legacy = legacy.unwrap();
        assert!(legacy.notice.unwrap().contains("files.example.com"));
        assert_eq!(legacy.secret, "keyring:old");
        assert!(value["upload"].get("ftp").is_none());
        let config: Config = toml::from_str(&toml::to_string(&value).unwrap()).unwrap();
        assert_eq!(config.upload.destination, UploadDestination::Sftp);
        assert_eq!(config.upload.sftp.host, "files.example.com");
    }

    #[test]
    fn legacy_ftp_keeps_an_existing_sftp_host() {
        let (legacy, value) = migrated(
            "[upload]\ndestination = \"Ftp\"\n[upload.ftp]\nhost = \"old.example.com\"\n\
             [upload.sftp]\nhost = \"kept.example.com\"\n",
        );
        assert!(legacy.unwrap().notice.is_some());
        assert_eq!(
            value["upload"]["sftp"]["host"].as_str(),
            Some("kept.example.com")
        );
    }

    #[test]
    fn legacy_ftp_task_target_moves_to_sftp() {
        let (legacy, value) =
            migrated("[[capture_tasks]]\nid = \"t\"\ntarget_destination = \"ftp\"\n");
        assert!(legacy.unwrap().notice.is_some());
        assert_eq!(
            value["capture_tasks"][0]["target_destination"].as_str(),
            Some("sftp")
        );
    }

    #[test]
    fn unused_ftp_table_is_dropped_without_a_notice() {
        let (legacy, value) = migrated(
            "[upload]\ndestination = \"Imgur\"\n[upload.ftp]\npassword_encrypted = \"keyring:old\"\n",
        );
        assert_eq!(
            legacy,
            Some(LegacyFtp {
                notice: None,
                secret: "keyring:old".into()
            })
        );
        assert!(value["upload"].get("ftp").is_none());
    }

    #[test]
    fn config_without_ftp_is_left_alone() {
        assert_eq!(migrated("[upload]\ndestination = \"Sftp\"\n").0, None);
        assert_eq!(migrated("").0, None);
    }

    #[test]
    fn destination_menu_ids_round_trip() {
        for destination in UploadDestination::all() {
            assert_eq!(
                UploadDestination::from_menu_id(&destination.menu_id()),
                Some(*destination)
            );
        }
        assert_eq!(UploadDestination::from_menu_id("dest_ftp"), None);
        assert_eq!(UploadDestination::from_menu_id("dest_plugin"), None);
        assert_eq!(
            UploadDestination::from_menu_id(&format!("{PLUGIN_MENU_PREFIX}imgur")),
            None
        );
    }

    #[test]
    fn plugin_destination_round_trips_and_old_configs_still_load() {
        let mut config = Config::default();
        config.upload.destination = UploadDestination::Plugin;
        config.upload.plugin = "my-uploader".into();
        let back: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(back.upload.destination, UploadDestination::Plugin);
        assert_eq!(back.upload.plugin, "my-uploader");

        let old: Config = toml::from_str("[upload]\ndestination = \"S3\"\n").unwrap();
        assert_eq!(old.upload.destination, UploadDestination::S3);
        assert_eq!(old.upload.plugin, "");
    }

    #[test]
    fn sanitize_repairs_a_bad_output_directory() {
        let mut config = Config::default();
        config.output.directory = PathBuf::from("../../secrets");
        config.sanitize();
        assert!(
            config.validate().is_ok(),
            "a traversal output dir must be repaired, not fail the whole config"
        );
        assert!(!config.output.directory.to_string_lossy().contains(".."));

        let mut empty = Config::default();
        empty.output.directory = PathBuf::new();
        empty.sanitize();
        assert!(!empty.output.directory.as_os_str().is_empty());
    }

    #[test]
    fn post_action_reads_every_accepted_spelling() {
        // the wire names changed twice (PascalCase, then a different
        // kebab-case); configs and callers from any era must keep
        // loading instead of resetting the field to default
        use TaskPostAction::*;
        let cases = [
            ("clipboard", Clipboard),
            ("copy-to-clipboard", Clipboard),
            ("CopyToClipboard", Clipboard),
            ("save-file", SaveFile),
            ("save-to-file", SaveFile),
            ("SaveToFile", SaveFile),
            ("upload", Upload),
            ("Upload", Upload),
            ("save-and-clipboard", SaveAndClipboard),
            ("save-and-copy", SaveAndClipboard),
            ("SaveAndCopy", SaveAndClipboard),
            ("open-editor", OpenEditor),
            ("annotate", Annotate),
            ("prompt", Prompt),
            ("prompt-user", Prompt),
            ("PromptUser", Prompt),
            ("do-nothing", DoNothing),
            ("DoNothing", DoNothing),
            ("copy-text", CopyText),
        ];
        for (wire, expected) in cases {
            let action: TaskPostAction = serde_json::from_str(&format!("\"{wire}\""))
                .unwrap_or_else(|_| panic!("lost spelling {wire}"));
            assert_eq!(action, expected);
        }
    }

    #[test]
    fn post_action_writes_kebab_case() {
        // the frontend sends and compares these strings verbatim
        use TaskPostAction::*;
        for (action, wire) in [
            (Clipboard, "clipboard"),
            (SaveFile, "save-file"),
            (Upload, "upload"),
            (SaveAndClipboard, "save-and-clipboard"),
            (OpenEditor, "open-editor"),
            (Annotate, "annotate"),
            (Prompt, "prompt"),
            (DoNothing, "do-nothing"),
            (CopyText, "copy-text"),
        ] {
            assert_eq!(
                serde_json::to_string(&action).unwrap(),
                format!("\"{wire}\"")
            );
        }
    }
}
