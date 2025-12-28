use anyhow::{anyhow, Result};
use arboard::Clipboard;
use image::RgbaImage;
use std::path::Path;

const MAX_IMAGE_DIMENSION: u32 = 16384;
const MAX_NOTIFICATION_LEN: usize = 256;

pub struct ClipboardManager {
    clipboard: Clipboard,
}

impl ClipboardManager {
    pub fn new() -> Result<Self> {
        let clipboard = Clipboard::new()?;
        Ok(Self { clipboard })
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

        let rgba_data = image.as_raw();

        let img_data = arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Borrowed(rgba_data),
        };

        self.clipboard.set_image(img_data)?;
        Ok(())
    }

    pub fn copy_file_path<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        if path_str.len() > 4096 {
            return Err(anyhow!("Path too long for clipboard"));
        }
        self.clipboard.set_text(path_str)?;
        Ok(())
    }
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

    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(anyhow!("Invalid filename characters"));
    }

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
