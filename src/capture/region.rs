use anyhow::{anyhow, Result};
use image::RgbaImage;
use xcap::Monitor;

use super::hdr::HdrCapture;
use super::{Capture, Rectangle};

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

        // when any monitor in the desktop has HDR enabled, GDI BitBlt
        // (xcap::Monitor::capture_image's default path on Windows) hands
        // back the SDR-clipped composition of HDR pixels — magenta/cyan
        // HDR highlights crush to pure 255-channel sRGB and the result
        // looks blown out. take the slower HDR-aware path on a per-monitor
        // basis: HdrCapture uses DXGI Desktop Duplication, then runs the
        // BT.2390 tonemap so the captured pixels match what the user
        // perceives on screen.
        let any_hdr = HdrCapture::is_hdr_available();

        let mut combined = RgbaImage::new(total_width, total_height);

        for monitor in &monitors {
            let img_result = if any_hdr {
                let center = (
                    monitor.x() + (monitor.width() as i32) / 2,
                    monitor.y() + (monitor.height() as i32) / 2,
                );
                HdrCapture::new()
                    .capture_with_hdr_at(Some(center))
                    .map(|(img, _)| img)
                    .or_else(|_| monitor.capture_image().map_err(|e| anyhow!("{e}")))
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
