use anyhow::{anyhow, Result};
use arboard::Clipboard;
use image::RgbaImage;
use std::path::Path;
use std::thread;
use std::time::Duration;

const MAX_IMAGE_DIMENSION: u32 = 16384;
const MAX_NOTIFICATION_LEN: usize = 256;
const CLIPBOARD_MAX_RETRIES: u32 = 5;
const CLIPBOARD_INITIAL_DELAY_MS: u64 = 10;

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
        let mut last_error = None;
        let mut delay = CLIPBOARD_INITIAL_DELAY_MS;

        for attempt in 0..CLIPBOARD_MAX_RETRIES {
            match operation(&mut self.clipboard) {
                Ok(result) => return Ok(result),
                Err(arboard::Error::ClipboardOccupied) => {
                    last_error = Some(arboard::Error::ClipboardOccupied);
                    if attempt < CLIPBOARD_MAX_RETRIES - 1 {
                        thread::sleep(Duration::from_millis(delay));
                        delay = (delay * 2).min(200);
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

        let rgba_data = image.as_raw().to_vec();
        let w = width as usize;
        let h = height as usize;

        self.retry_with_backoff(|clipboard| {
            let img_data = arboard::ImageData {
                width: w,
                height: h,
                bytes: std::borrow::Cow::Borrowed(&rgba_data),
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
}

const WINDOWS_INVALID_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

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

    let filename = path.file_name()
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
            let quality = quality.min(100);
            let rgb_image = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
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

pub fn show_notification(title: &str, body: &str) -> Result<()> {
    let safe_title = sanitize_notification_text(title);
    let safe_body = sanitize_notification_text(body);

    #[cfg(not(target_os = "macos"))]
    {
        notify_rust::Notification::new()
            .summary(&safe_title)
            .body(&safe_body)
            .timeout(notify_rust::Timeout::Milliseconds(3000))
            .show()?;
    }

    #[cfg(target_os = "macos")]
    {
        notify_rust::Notification::new()
            .summary(&safe_title)
            .body(&safe_body)
            .show()?;
    }

    Ok(())
}
