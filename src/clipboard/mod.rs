#![allow(dead_code)]

use anyhow::{anyhow, Result};
use arboard::Clipboard;
use image::RgbaImage;
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const MAX_IMAGE_DIMENSION: u32 = 16384;
const MAX_NOTIFICATION_LEN: usize = 256;
const CLIPBOARD_MAX_RETRIES: u32 = 20;
const CLIPBOARD_RETRY_DELAY_MS: u64 = 100;

static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

pub struct ClipboardManager {
    clipboard: Clipboard,
}

impl ClipboardManager {
    pub fn new() -> Result<Self> {
        let clipboard = Clipboard::new()?;
        Ok(Self { clipboard })
    }

    fn retry_with_backoff<F, T>(&mut self, mut operation: F) -> Result<T>
    where
        F: FnMut(&mut Clipboard) -> std::result::Result<T, arboard::Error>,
    {
        let _lock = CLIPBOARD_LOCK
            .lock()
            .map_err(|_| anyhow!("Clipboard lock poisoned"))?;

        let mut last_error = None;

        for attempt in 0..CLIPBOARD_MAX_RETRIES {
            match operation(&mut self.clipboard) {
                Ok(result) => return Ok(result),
                Err(arboard::Error::ClipboardOccupied) => {
                    last_error = Some(arboard::Error::ClipboardOccupied);
                    if attempt < CLIPBOARD_MAX_RETRIES - 1 {
                        thread::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS));
                    }
                }
                Err(e) => return Err(anyhow!("Clipboard error: {}", e)),
            }
        }

        Err(anyhow!(
            "Clipboard occupied after {} retries: {:?}",
            CLIPBOARD_MAX_RETRIES,
            last_error
        ))
    }

    pub fn copy_image(&mut self, image: &RgbaImage) -> Result<()> {
        let width = image.width();
        let height = image.height();

        if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
            return Err(anyhow!("Image too large for clipboard"));
        }
        if width == 0 || height == 0 {
            return Err(anyhow!("Image has zero dimension"));
        }

        // borrow the pixels directly — arboard copies them into the clipboard
        // itself, so the previous to_vec was a redundant full-frame allocation
        // (tens of MB at 4K) on every clipboard capture, the default action.
        let raw: &[u8] = image.as_raw();
        let w = width as usize;
        let h = height as usize;

        self.retry_with_backoff(|clipboard| {
            let img_data = arboard::ImageData {
                width: w,
                height: h,
                bytes: std::borrow::Cow::Borrowed(raw),
            };
            clipboard.set_image(img_data)
        })
    }

    pub fn copy_file_path<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        if path_str.len() > 4096 {
            return Err(anyhow!("Path too long for clipboard"));
        }

        self.retry_with_backoff(|clipboard| clipboard.set_text(path_str.clone()))
    }

    pub fn copy_text(&mut self, text: &str) -> Result<()> {
        if text.len() > 4096 {
            return Err(anyhow!("Text too long for clipboard"));
        }
        let owned = text.to_string();
        self.retry_with_backoff(|clipboard| clipboard.set_text(owned.clone()))
    }
}

/// put the file itself on the clipboard as a CF_HDROP file list, the format
/// explorer/discord/slack expect when pasting a file. arboard has no file-list
/// support, so this goes through the win32 clipboard directly
#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;

    if !path.is_absolute() {
        return Err(anyhow!("Clipboard file copy requires an absolute path"));
    }
    if !path.exists() {
        return Err(anyhow!("File does not exist: {}", path.display()));
    }

    // canonicalize produces \\?\-prefixed paths that some paste targets
    // don't understand — hand them the plain drive-letter form
    let plain: std::path::PathBuf = match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(stripped) if !stripped.starts_with("UNC\\") => stripped.into(),
        _ => path.to_path_buf(),
    };

    let wide: Vec<u16> = plain
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0)) // path terminator
        .chain(std::iter::once(0)) // list terminator
        .collect();

    let header_size = std::mem::size_of::<DROPFILES>();
    let total_size = header_size + wide.len() * std::mem::size_of::<u16>();

    let _lock = CLIPBOARD_LOCK
        .lock()
        .map_err(|_| anyhow!("Clipboard lock poisoned"))?;

    unsafe {
        let hglobal: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, total_size)
            .map_err(|e| anyhow!("GlobalAlloc failed: {e}"))?;

        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(hglobal);
            return Err(anyhow!("GlobalLock failed"));
        }

        let dropfiles = DROPFILES {
            pFiles: header_size as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: false.into(),
            fWide: true.into(),
        };
        std::ptr::write_unaligned(ptr as *mut DROPFILES, dropfiles);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            ptr.byte_add(header_size) as *mut u16,
            wide.len(),
        );
        // GlobalUnlock signals "fully unlocked" through an error-shaped return;
        // the memory is fine, so the result is intentionally ignored
        let _ = GlobalUnlock(hglobal);

        let mut opened = false;
        for attempt in 0..CLIPBOARD_MAX_RETRIES {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            if attempt < CLIPBOARD_MAX_RETRIES - 1 {
                thread::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS));
            }
        }
        if !opened {
            let _ = GlobalFree(hglobal);
            return Err(anyhow!(
                "Clipboard occupied after {CLIPBOARD_MAX_RETRIES} retries"
            ));
        }

        let result = EmptyClipboard()
            .map_err(|e| anyhow!("EmptyClipboard failed: {e}"))
            .and_then(|_| {
                SetClipboardData(CF_HDROP.0 as u32, HANDLE(hglobal.0))
                    .map_err(|e| anyhow!("SetClipboardData failed: {e}"))
            });
        let _ = CloseClipboard();

        match result {
            // on success the clipboard owns the allocation
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(hglobal);
                Err(e)
            }
        }
    }
}

const WINDOWS_INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn get_unique_filepath(path: &Path) -> std::path::PathBuf {
    // use create_new to atomically claim the file slot, eliminating the TOCTOU
    // race where two simultaneous captures both see "not exists" and write the
    // same filename, with the second clobbering the first.
    let try_claim = |p: &Path| -> bool {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
        {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            // parent dir not yet created — treat path as available; save_image
            // creates the dir before writing
            Err(_) => !p.exists(),
        }
    };

    if try_claim(path) {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    for i in 1..1000 {
        let new_name = if ext.is_empty() {
            format!("{}_{}", stem, i)
        } else {
            format!("{}_{}.{}", stem, i, ext)
        };
        let new_path = parent.join(&new_name);
        if try_claim(&new_path) {
            return new_path;
        }
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let fallback_name = if ext.is_empty() {
        format!("{}_{}", stem, timestamp)
    } else {
        format!("{}_{}.{}", stem, timestamp, ext)
    };
    parent.join(&fallback_name)
}

fn validate_filename(filename: &str) -> Result<()> {
    if filename.is_empty() {
        return Err(anyhow!("Empty filename"));
    }

    if filename.len() > 255 {
        return Err(anyhow!("Filename too long (max 255 characters)"));
    }

    if filename.contains("..") {
        return Err(anyhow!("Invalid filename: contains '..'"));
    }

    for c in WINDOWS_INVALID_CHARS {
        if filename.contains(*c) {
            return Err(anyhow!("Invalid character '{}' in filename", c));
        }
    }

    for c in filename.chars() {
        if c.is_control() {
            return Err(anyhow!("Control characters not allowed in filename"));
        }
    }

    if filename.ends_with('.') || filename.ends_with(' ') {
        return Err(anyhow!("Filename cannot end with '.' or space"));
    }

    let base_name = filename.split('.').next().unwrap_or("");
    let upper = base_name.to_uppercase();
    if WINDOWS_RESERVED_NAMES.contains(&upper.as_str()) {
        return Err(anyhow!("'{}' is a reserved filename on Windows", base_name));
    }

    Ok(())
}

pub fn save_image<P: AsRef<Path>>(
    image: &RgbaImage,
    path: P,
    format: crate::config::ImageFormat,
    quality: u8,
) -> Result<()> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageEncoder;
    use std::fs::OpenOptions;
    use std::io::BufWriter;

    let path = path.as_ref();

    let filename = path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid filename"))?
        .to_string_lossy();

    validate_filename(&filename)?;

    if image.width() > MAX_IMAGE_DIMENSION || image.height() > MAX_IMAGE_DIMENSION {
        return Err(anyhow!("Image too large to save"));
    }
    if image.width() == 0 || image.height() == 0 {
        return Err(anyhow!("Image has zero dimension"));
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    match format {
        crate::config::ImageFormat::Png => {
            image.save(path)?;
        }
        crate::config::ImageFormat::Jpeg => {
            use image::buffer::ConvertBuffer;
            let quality = quality.min(100);
            // convert RGBA->RGB directly (JPEG has no alpha) instead of cloning
            // into a DynamicImage first — one allocation instead of two.
            let rgb_image: image::RgbImage = image.convert();
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            let writer = BufWriter::new(file);
            let encoder = JpegEncoder::new_with_quality(writer, quality);
            encoder.write_image(
                &rgb_image,
                rgb_image.width(),
                rgb_image.height(),
                image::ExtendedColorType::Rgb8,
            )?;
        }
        crate::config::ImageFormat::Gif => {
            image.save(path)?;
        }
        crate::config::ImageFormat::Webp => {
            image.save(path)?;
        }
        crate::config::ImageFormat::Bmp => {
            image.save(path)?;
        }
    }

    Ok(())
}

fn sanitize_notification_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_NOTIFICATION_LEN)
        .collect()
}

/// minimum gap between two identical (title, body) notifications.
/// Without this guard a tight retry loop in upload/capture can spam Action
/// Center with five copies of the same "Capture saved" toast.
const NOTIFICATION_DEDUPE_MS: u128 = 1500;

pub fn show_notification(title: &str, body: &str) -> Result<()> {
    let safe_title = sanitize_notification_text(title);
    let safe_body = sanitize_notification_text(body);

    // dedupe: drop the call if an identical notification fired within the
    // last NOTIFICATION_DEDUPE_MS. Cheap mutex on a 2-tuple is fine here
    // because this path is called at human speed.
    if !should_emit(&safe_title, &safe_body) {
        return Ok(());
    }

    let mut n = notify_rust::Notification::new();
    n.summary(&safe_title)
        .body(&safe_body)
        .timeout(notify_rust::Timeout::Milliseconds(3000));

    // anchor the toast to our explicit AUMID so Windows Action Center groups
    // notifications under "capscr" with our icon, not under the PowerShell
    // fallback (the generic blue icon).
    #[cfg(windows)]
    n.app_id("io.rot.capscr");

    n.show()?;

    Ok(())
}

fn should_emit(title: &str, body: &str) -> bool {
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: std::sync::OnceLock<Mutex<Option<(String, String, Instant)>>> =
        std::sync::OnceLock::new();
    let cell = LAST.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = cell.lock() else {
        return true; // poisoned mutex → fail open, never silence accidentally
    };
    let now = Instant::now();
    if let Some((t, b, when)) = guard.as_ref() {
        if t == title && b == body && now.duration_since(*when).as_millis() < NOTIFICATION_DEDUPE_MS
        {
            return false;
        }
    }
    *guard = Some((title.to_string(), body.to_string(), now));
    true
}
