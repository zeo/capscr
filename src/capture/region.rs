use anyhow::{anyhow, Result};
use image::RgbaImage;
use xcap::Monitor;

use super::hdr::HdrCapture;
use super::{Capture, Rectangle};

// HDR-aware capture is now default-on when an HDR display is detected,
// because tauri-cli was sanitising the CAPSCR_HDR_AWARE env var out of
// the child process env so the opt-in gate never tripped. CAPSCR_HDR_AWARE=0
// forces the fast GDI BitBlt fallback (overblown but instant) if the new
// DXGI Desktop Duplication pipeline regresses on someone's hardware.
fn hdr_aware_enabled() -> bool {
    use std::sync::OnceLock;
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let raw = std::env::var("CAPSCR_HDR_AWARE").unwrap_or_else(|_| "<unset>".to_string());
        let forced_off = matches!(raw.trim(), "0" | "false" | "FALSE" | "off");
        let enabled = !forced_off;
        tracing::info!(
            "CAPSCR_HDR_AWARE env var = {:?} -> hdr_aware_enabled = {}",
            raw,
            enabled,
        );
        enabled
    })
}

pub struct RegionCapture {
    region: Rectangle,
}

impl RegionCapture {
    pub fn new(region: Rectangle) -> Self {
        Self { region }
    }

    #[cfg(test)]
    pub fn from_coords(start_x: i32, start_y: i32, end_x: i32, end_y: i32) -> Self {
        Self {
            region: Rectangle::normalize(start_x, start_y, end_x, end_y),
        }
    }

    #[cfg(test)]
    pub fn region(&self) -> &Rectangle {
        &self.region
    }

    fn get_virtual_screen_origin() -> (i32, i32) {
        if let Ok(monitors) = Monitor::all() {
            let min_x = monitors.iter().map(|m| m.x()).min().unwrap_or(0);
            let min_y = monitors.iter().map(|m| m.y()).min().unwrap_or(0);
            (min_x, min_y)
        } else {
            (0, 0)
        }
    }
}

impl Capture for RegionCapture {
    fn capture(&self) -> Result<RgbaImage> {
        // intentionally GDI BitBlt via xcap::Monitor::capture_image. the
        // DXGI Desktop Duplication path (HdrCapture) was tried in 0.3.50-0.3.57
        // for HDR-correct rendering of bright pixels; on real user setups it
        // produced zeroed textures (overlay/compositor interaction) and added
        // multi-second CPU tonemap latency. reverted here so captures are
        // instant again — HDR content reads as overblown-but-visible (sRGB
        // 255 across bright channels) just like Snipping Tool and other
        // GDI-based capture tools. HDR-aware capture is future work; it
        // needs a GPU shader tonemap and a way to coexist with capscr's
        // own dim overlay.
        let monitors = Monitor::all()?;
        if monitors.is_empty() {
            return Err(anyhow!("No monitors found"));
        }

        let min_x = monitors.iter().map(|m| m.x()).min().unwrap_or(0);
        let min_y = monitors.iter().map(|m| m.y()).min().unwrap_or(0);
        let max_x = monitors.iter().map(|m| m.x() + m.width() as i32).max().unwrap_or(0);
        let max_y = monitors.iter().map(|m| m.y() + m.height() as i32).max().unwrap_or(0);

        let total_width = (max_x - min_x) as u32;
        let total_height = (max_y - min_y) as u32;

        let env_on = hdr_aware_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();
        let use_hdr = env_on && hdr_avail;
        tracing::info!(
            "RegionCapture: env_on={} hdr_avail={} -> use_hdr={}",
            env_on,
            hdr_avail,
            use_hdr,
        );
        let mut combined = RgbaImage::new(total_width, total_height);

        for monitor in &monitors {
            let img_result: Result<RgbaImage> = if use_hdr {
                let center = (
                    monitor.x() + (monitor.width() as i32) / 2,
                    monitor.y() + (monitor.height() as i32) / 2,
                );
                HdrCapture::new()
                    .capture_with_hdr_at(Some(center))
                    .map(|(img, _)| img)
                    .or_else(|e| {
                        tracing::warn!(
                            "HDR-aware capture failed for monitor at {},{} — GDI fallback: {e:#}",
                            monitor.x(),
                            monitor.y(),
                        );
                        monitor.capture_image().map_err(|e| anyhow!("{e}"))
                    })
            } else {
                monitor.capture_image().map_err(|e| anyhow!("{e}"))
            };

            if let Ok(img) = img_result {
                let offset_x = (monitor.x() - min_x) as u32;
                let offset_y = (monitor.y() - min_y) as u32;

                for (x, y, pixel) in img.enumerate_pixels() {
                    let dest_x = offset_x + x;
                    let dest_y = offset_y + y;
                    if dest_x < total_width && dest_y < total_height {
                        combined.put_pixel(dest_x, dest_y, *pixel);
                    }
                }
            }
        }

        let img_x = (self.region.x - min_x).max(0) as u32;
        let img_y = (self.region.y - min_y).max(0) as u32;

        let crop_width = self.region.width.min(total_width.saturating_sub(img_x));
        let crop_height = self.region.height.min(total_height.saturating_sub(img_y));

        if crop_width == 0 || crop_height == 0 {
            return Err(anyhow!("Invalid region dimensions"));
        }

        let cropped = image::imageops::crop_imm(&combined, img_x, img_y, crop_width, crop_height).to_image();
        Ok(cropped)
    }
}
