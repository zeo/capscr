use anyhow::{anyhow, Result};
use image::{GenericImage, RgbaImage};
use xcap::Monitor;

use super::{Capture, MonitorInfo};
use super::hdr::HdrCapture;

pub struct ScreenCapture {
    monitor_id: Option<u32>,
}

impl ScreenCapture {
    pub fn new() -> Self {
        Self { monitor_id: None }
    }

    pub fn with_monitor(monitor_id: u32) -> Self {
        Self {
            monitor_id: Some(monitor_id),
        }
    }

    pub fn primary() -> Result<Self> {
        #[cfg(windows)]
        {
            if let Ok(monitors) = super::fast_list_monitors() {
                if let Some(primary) = monitors.into_iter().find(|m| m.is_primary) {
                    return Ok(Self {
                        monitor_id: Some(primary.id),
                    });
                }
            }
        }
        let monitors = Monitor::all()?;
        let primary = monitors
            .into_iter()
            .find(|m| m.is_primary())
            .ok_or_else(|| anyhow!("No primary monitor found"))?;
        Ok(Self {
            monitor_id: Some(primary.id()),
        })
    }

    pub fn at_point(x: i32, y: i32) -> Result<Self> {
        #[cfg(windows)]
        {
            if let Ok(monitors) = super::fast_list_monitors() {
                if let Some(m) = monitors.into_iter().find(|m| {
                    x >= m.x && x < m.x + m.width as i32 &&
                    y >= m.y && y < m.y + m.height as i32
                }) {
                    return Ok(Self {
                        monitor_id: Some(m.id),
                    });
                }
            }
        }
        let monitor = Monitor::from_point(x, y)?;
        Ok(Self {
            monitor_id: Some(monitor.id()),
        })
    }

    pub fn all_monitors() -> Result<RgbaImage> {
        const MAX_TOTAL_DIMENSION: i32 = 32768;

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
        let max_x = monitors
            .iter()
            .map(|m| m.x.saturating_add(m.width as i32))
            .max()
            .unwrap_or(0);
        let max_y = monitors
            .iter()
            .map(|m| m.y.saturating_add(m.height as i32))
            .max()
            .unwrap_or(0);

        let width_i32 = max_x.saturating_sub(min_x);
        let height_i32 = max_y.saturating_sub(min_y);

        if width_i32 <= 0 || height_i32 <= 0 {
            return Err(anyhow!("Invalid monitor dimensions"));
        }
        if width_i32 > MAX_TOTAL_DIMENSION || height_i32 > MAX_TOTAL_DIMENSION {
            return Err(anyhow!("Combined monitor area too large"));
        }

        let env_on = super::hdr_aware_enabled();
        let wgc_on = super::wgc_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();
        let use_cpu_hdr = env_on && hdr_avail;
        let use_wgc = wgc_on && !env_on;
        let use_d2d = hdr_avail && !env_on && !wgc_on;

        let total_width = width_i32 as u32;
        let total_height = height_i32 as u32;

        let mut combined = RgbaImage::new(total_width, total_height);

        for monitor in monitors {
            let center = (
                monitor.x + (monitor.width as i32) / 2,
                monitor.y + (monitor.height as i32) / 2,
            );
            let gdi_capture = || {
                match super::fast_gdi_capture(monitor.x, monitor.y, monitor.width, monitor.height) {
                    Ok(img) => Ok(img),
                    Err(_) => {
                        let screens = Monitor::all()?;
                        let screen = screens.into_iter().find(|s| s.id() == monitor.id)
                            .ok_or_else(|| anyhow!("Monitor not found"))?;
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
                            "CPU HDR capture failed for monitor at {},{} — fallback to GDI: {e:#}",
                            monitor.x,
                            monitor.y
                        );
                        gdi_capture()
                    })
            } else if use_wgc {
                let r = super::wgc_capture_at_point(center.0, center.1);
                r.or_else(|_| gdi_capture())
            } else if use_d2d {
                let r = super::d2d_capture_at_point(Some(center));
                r.or_else(|_| gdi_capture())
            } else {
                gdi_capture()
            };

            let img = img_result?;
            let img = super::orient_captured_image(
                img,
                monitor.width,
                monitor.height,
                monitor.x,
                monitor.y,
            );
            let offset_x_i32 = monitor.x.saturating_sub(min_x);
            let offset_y_i32 = monitor.y.saturating_sub(min_y);

            if offset_x_i32 < 0 || offset_y_i32 < 0 {
                continue;
            }

            let offset_x = offset_x_i32 as u32;
            let offset_y = offset_y_i32 as u32;

            if let Err(e) = combined.copy_from(&img, offset_x, offset_y) {
                tracing::warn!("Failed to copy monitor image into combined buffer: {e}");
            }
        }

        Ok(combined)
    }

    fn find_monitor(&self) -> Result<Monitor> {
        let monitors = Monitor::all()?;

        match self.monitor_id {
            Some(id) => monitors
                .into_iter()
                .find(|m| m.id() == id)
                .ok_or_else(|| anyhow!("Monitor {} not found", id)),
            None => monitors
                .into_iter()
                .find(|m| m.is_primary())
                .or_else(|| Monitor::all().ok()?.into_iter().next())
                .ok_or_else(|| anyhow!("No monitors found")),
        }
    }

    pub fn get_monitor_info(&self) -> Result<MonitorInfo> {
        #[cfg(windows)]
        {
            if let Ok(monitors) = super::fast_list_monitors() {
                let info = match self.monitor_id {
                    Some(id) => monitors.into_iter().find(|m| m.id == id),
                    None => monitors.into_iter().find(|m| m.is_primary).or_else(|| super::fast_list_monitors().ok()?.into_iter().next()),
                };
                if let Some(m) = info {
                    return Ok(m);
                }
            }
        }
        let monitor = self.find_monitor()?;
        Ok(MonitorInfo {
            id: monitor.id(),
            name: monitor.name().to_string(),
            x: monitor.x(),
            y: monitor.y(),
            width: monitor.width(),
            height: monitor.height(),
            is_primary: monitor.is_primary(),
        })
    }
}

impl Default for ScreenCapture {
    fn default() -> Self {
        Self::new()
    }
}

const MAX_CAPTURE_DIMENSION: u32 = 16384;
const MAX_CAPTURE_PIXELS: u64 = 256 * 1024 * 1024;

impl Capture for ScreenCapture {
    fn capture(&self) -> Result<RgbaImage> {
        tracing::info!("ScreenCapture::capture entry");
        // path priority matches RegionCapture — see src/capture/region.rs
        // for the rationale on each env-var gate.
        let env_on = super::hdr_aware_enabled();
        let wgc_on = super::wgc_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();

        if env_on && hdr_avail {
            let hdr = HdrCapture::new();
            match hdr.capture() {
                Ok(img) => return Ok(img),
                Err(e) => tracing::warn!("ScreenCapture CPU HDR failed — fallthrough: {e:#}"),
            }
        }
        
        let monitor_info = self.get_monitor_info()?;
        let center = (
            monitor_info.x + (monitor_info.width as i32) / 2,
            monitor_info.y + (monitor_info.height as i32) / 2,
        );

        #[cfg(windows)]
        if hdr_avail && !env_on {
            if wgc_on {
                let t0 = std::time::Instant::now();
                match super::wgc_capture_at_point(center.0, center.1) {
                    Ok(img) => {
                        tracing::info!(
                            "ScreenCapture WGC {}x{} in {}ms",
                            img.width(), img.height(), t0.elapsed().as_millis()
                        );
                        return Ok(img);
                    }
                    Err(e) => tracing::warn!("ScreenCapture WGC failed — fallthrough: {e:#}"),
                }
            } else {
                let t0 = std::time::Instant::now();
                match super::d2d_capture_at_point(Some(center)) {
                    Ok(img) => {
                        tracing::info!(
                            "ScreenCapture D2D {}x{} in {}ms",
                            img.width(), img.height(), t0.elapsed().as_millis()
                        );
                        return Ok(img);
                    }
                    Err(e) => tracing::warn!("ScreenCapture D2D failed — GDI fallback: {e:#}"),
                }
            }
        }
        let img = match super::fast_gdi_capture(monitor_info.x, monitor_info.y, monitor_info.width, monitor_info.height) {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!("fast GDI capture failed — falling back to xcap: {e:#}");
                let m = self.find_monitor()?;
                m.capture_image()?
            }
        };
        let img = super::orient_captured_image(
            img,
            monitor_info.width,
            monitor_info.height,
            monitor_info.x,
            monitor_info.y,
        );

        if img.width() > MAX_CAPTURE_DIMENSION || img.height() > MAX_CAPTURE_DIMENSION {
            return Err(anyhow!("Captured image dimensions exceed safety limit"));
        }
        let pixel_count = (img.width() as u64).saturating_mul(img.height() as u64);
        if pixel_count > MAX_CAPTURE_PIXELS {
            return Err(anyhow!("Captured image exceeds maximum pixel count"));
        }

        Ok(img)
    }
}
