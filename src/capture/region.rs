use anyhow::{anyhow, Result};
use image::{GenericImage, RgbaImage};
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
        #[cfg(windows)]
        {
            if let Ok(monitors) = super::fast_list_monitors() {
                let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
                let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
                return (min_x, min_y);
            }
        }
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
        
        #[cfg(windows)]
        let monitors = super::fast_list_monitors()?;
        #[cfg(not(windows))]
        let monitors = {
            let screens = Monitor::all()?;
            screens.into_iter().map(|s| super::MonitorInfo {
                id: s.id(),
                name: s.name().to_string(),
                x: s.x(),
                y: s.y(),
                width: s.width(),
                height: s.height(),
                is_primary: s.is_primary(),
            }).collect::<Vec<_>>()
        };

        if monitors.is_empty() {
            return Err(anyhow!("No monitors found"));
        }

        let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
        let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
        let max_x = monitors.iter().map(|m| m.x + m.width as i32).max().unwrap_or(0);
        let max_y = monitors.iter().map(|m| m.y + m.height as i32).max().unwrap_or(0);

        let total_width = (max_x - min_x) as u32;
        let total_height = (max_y - min_y) as u32;

        // capture path priority for HDR-display captures:
        //   1. CAPSCR_HDR_AWARE=1 -> custom CPU Reinhard tonemap (legacy
        //      opt-in, slow, tunable)
        //   2. CAPSCR_USE_WGC=1 -> Windows.Graphics.Capture (OS-side
        //      tonemap, instant, but can shift SDR brightness)
        //   3. default + HDR display -> D2D GPU pipeline (correct +
        //      instant, uses Direct2D HdrToneMap + WhiteLevelAdjustment)
        //   4. default + SDR display -> GDI BitBlt (instant, pixel-exact)
        let env_on = hdr_aware_enabled();
        let wgc_on = super::wgc_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();
        let use_cpu_hdr = env_on && hdr_avail;
        let use_wgc = wgc_on && !env_on;
        let use_d2d = hdr_avail && !env_on && !wgc_on;
        tracing::info!(
            "RegionCapture: cpu_hdr_env={} wgc_env={} hdr_avail={} -> cpu_hdr={} wgc={} d2d={}",
            env_on, wgc_on, hdr_avail, use_cpu_hdr, use_wgc, use_d2d,
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
            let mx0 = monitor.x;
            let my0 = monitor.y;
            let mx1 = mx0 + monitor.width as i32;
            let my1 = my0 + monitor.height as i32;
            let overlaps = mx0 < region_x1 && mx1 > region_x0
                && my0 < region_y1 && my1 > region_y0;
            if !overlaps {
                tracing::info!(
                    "RegionCapture: skipping non-overlapping monitor {}x{}+{}+{}",
                    monitor.width, monitor.height, mx0, my0,
                );
                continue;
            }

            let center = (
                monitor.x + (monitor.width as i32) / 2,
                monitor.y + (monitor.height as i32) / 2,
            );
            let gdi_capture = || {
                match super::fast_gdi_capture(monitor.x, monitor.y, monitor.width, monitor.height) {
                    Ok(img) => Ok(img),
                    Err(e) => {
                        tracing::warn!("fast GDI capture failed — falling back to xcap: {e:#}");
                        let screens = Monitor::all()?;
                        let screen = screens.into_iter().find(|s| s.id() == monitor.id)
                            .ok_or_else(|| anyhow!("xcap monitor not found"))?;
                        screen.capture_image().map_err(|e| anyhow!("{e}"))
                    }
                }
            };

            let img_result: Result<RgbaImage> = if use_cpu_hdr {
                HdrCapture::new()
                    .capture_with_hdr_at(Some(center))
                    .map(|(img, _)| img)
                    .or_else(|e| {
                        tracing::warn!(
                            "CPU HDR capture failed for monitor at {},{} — GDI fallback: {e:#}",
                            monitor.x,
                            monitor.y,
                        );
                        gdi_capture()
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
                r.or_else(|_| gdi_capture())
            } else if use_d2d {
                let t0 = std::time::Instant::now();
                let r = super::d2d_capture_at_point(Some(center));
                let dt = t0.elapsed().as_millis();
                match &r {
                    Ok(img) => tracing::info!(
                        "D2D capture {}x{} at ({},{}) in {dt}ms",
                        img.width(), img.height(), center.0, center.1
                    ),
                    Err(e) => tracing::warn!(
                        "D2D capture failed at ({},{}) in {dt}ms — GDI fallback: {e:#}",
                        center.0, center.1
                    ),
                }
                r.or_else(|_| gdi_capture())
            } else {
                gdi_capture()
            };

            if let Ok(img) = img_result {
                let img = orient_captured_image(
                    img,
                    monitor.width,
                    monitor.height,
                    monitor.x,
                    monitor.y,
                );
                let offset_x = (monitor.x - min_x) as u32;
                let offset_y = (monitor.y - min_y) as u32;

                if let Err(e) = combined.copy_from(&img, offset_x, offset_y) {
                    tracing::warn!("Failed to copy monitor image into combined region buffer: {e}");
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
