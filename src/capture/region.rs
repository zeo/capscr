use anyhow::{anyhow, Result};
use image::RgbaImage;
use xcap::Monitor;

use super::hdr::HdrCapture;
use super::{hdr_aware_enabled, orient_captured_image, Capture, Rectangle};

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
        tracing::info!(
            "RegionCapture::capture entry: region={}x{}+{}+{}",
            self.region.width, self.region.height, self.region.x, self.region.y
        );
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

        // capture path priority:
        //   1. CAPSCR_HDR_AWARE=1 + HDR display -> custom CPU tonemap
        //      (slow, gives us control over the look)
        //   2. HDR display detected -> WGC (Windows.Graphics.Capture)
        //      (instant, OS-side tonemap, matches Snipping Tool quality)
        //   3. default -> GDI BitBlt via xcap (instant, overblown on HDR)
        let env_on = hdr_aware_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();
        let use_cpu_hdr = env_on && hdr_avail;
        let use_wgc = !env_on && hdr_avail;
        tracing::info!(
            "RegionCapture: env_on={} hdr_avail={} -> cpu_hdr={} wgc={}",
            env_on,
            hdr_avail,
            use_cpu_hdr,
            use_wgc,
        );

        // selection bounds in virtual-screen coords. used to skip monitors
        // that don't overlap the region — for a 100x331 region on monitor A,
        // there's no point spending 500ms-2s capturing monitor B (especially
        // when monitor B is 4K and would dominate the screenshot delay the
        // user reported as "screenshot taking a while").
        let region_x0 = self.region.x;
        let region_y0 = self.region.y;
        let region_x1 = self.region.x + self.region.width as i32;
        let region_y1 = self.region.y + self.region.height as i32;
        let mut combined = RgbaImage::new(total_width, total_height);

        for monitor in &monitors {
            // skip monitors entirely outside the selection region.
            let mx0 = monitor.x();
            let my0 = monitor.y();
            let mx1 = mx0 + monitor.width() as i32;
            let my1 = my0 + monitor.height() as i32;
            let overlaps = mx0 < region_x1 && mx1 > region_x0
                && my0 < region_y1 && my1 > region_y0;
            if !overlaps {
                tracing::info!(
                    "RegionCapture: skipping non-overlapping monitor {}x{}+{}+{}",
                    monitor.width(), monitor.height(), mx0, my0,
                );
                continue;
            }

            let center = (
                monitor.x() + (monitor.width() as i32) / 2,
                monitor.y() + (monitor.height() as i32) / 2,
            );
            let img_result: Result<RgbaImage> = if use_cpu_hdr {
                HdrCapture::new()
                    .capture_with_hdr_at(Some(center))
                    .map(|(img, _)| img)
                    .or_else(|e| {
                        tracing::warn!(
                            "CPU HDR capture failed for monitor at {},{} — GDI fallback: {e:#}",
                            monitor.x(),
                            monitor.y(),
                        );
                        monitor.capture_image().map_err(|e| anyhow!("{e}"))
                    })
            } else if use_wgc {
                let t0 = std::time::Instant::now();
                let r = super::wgc_capture_at_point(center.0, center.1);
                let dt = t0.elapsed().as_millis();
                match &r {
                    Ok(img) => tracing::info!(
                        "WGC capture {}x{} at ({},{}) in {dt}ms",
                        img.width(), img.height(), center.0, center.1
                    ),
                    Err(e) => tracing::warn!(
                        "WGC capture failed at ({},{}) in {dt}ms — GDI fallback: {e:#}",
                        center.0, center.1
                    ),
                }
                r.or_else(|_| monitor.capture_image().map_err(|e| anyhow!("{e}")))
            } else {
                monitor.capture_image().map_err(|e| anyhow!("{e}"))
            };

            if let Ok(img) = img_result {
                let img = orient_captured_image(
                    img,
                    monitor.width(),
                    monitor.height(),
                    monitor.x(),
                    monitor.y(),
                );
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
