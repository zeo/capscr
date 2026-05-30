use anyhow::{anyhow, Result};
use image::{GenericImage, RgbaImage};
use xcap::Monitor;

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

        // per-monitor capture (HDR -> D2D HdrToneMap, SDR -> GDI, env
        // opt-ins for CPU/WGC) is handled by super::capture_one_monitor so
        // the freeze-frame, single-monitor and active-monitor paths all
        // share one tonemap-correct, black-frame-guarded pipeline.

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

            #[cfg(windows)]
            let img = match super::capture_one_monitor(monitor) {
                Ok(img) => img,
                Err(e) => {
                    tracing::warn!(
                        "RegionCapture: capture_one_monitor failed for {}x{}+{}+{}: {e:#}",
                        monitor.width, monitor.height, monitor.x, monitor.y,
                    );
                    continue;
                }
            };
            #[cfg(not(windows))]
            let img = {
                let screens = Monitor::all()?;
                let screen = match screens.into_iter().find(|s| s.id() == monitor.id) {
                    Some(s) => s,
                    None => continue,
                };
                match screen.capture_image() {
                    Ok(i) => super::orient_captured_image(
                        i, monitor.width, monitor.height, monitor.x, monitor.y,
                    ),
                    Err(_) => continue,
                }
            };

            let offset_x = (monitor.x - min_x) as u32;
            let offset_y = (monitor.y - min_y) as u32;
            if let Err(e) = combined.copy_from(&img, offset_x, offset_y) {
                tracing::warn!("Failed to copy monitor image into combined region buffer: {e}");
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
