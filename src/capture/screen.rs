use anyhow::{anyhow, Result};
use image::{GenericImage, RgbaImage};
use xcap::Monitor;

use super::{Capture, MonitorInfo};

pub struct ScreenCapture {
    monitor_id: Option<u32>,
}

impl ScreenCapture {
    pub fn new() -> Self {
        Self { monitor_id: None }
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
            .find(|m| m.is_primary().unwrap_or(false))
            .ok_or_else(|| anyhow!("No primary monitor found"))?;
        Ok(Self {
            monitor_id: Some(primary.id()?),
        })
    }

    pub fn at_point(x: i32, y: i32) -> Result<Self> {
        #[cfg(windows)]
        {
            if let Ok(monitors) = super::fast_list_monitors() {
                if let Some(m) = monitors.into_iter().find(|m| {
                    x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + m.height as i32
                }) {
                    return Ok(Self {
                        monitor_id: Some(m.id),
                    });
                }
            }
        }
        let monitor = Monitor::from_point(x, y)?;
        Ok(Self {
            monitor_id: Some(monitor.id()?),
        })
    }

    pub fn all_monitors() -> Result<RgbaImage> {
        const MAX_TOTAL_DIMENSION: i32 = 32768;

        #[cfg(windows)]
        let monitors = super::fast_list_monitors()?;
        #[cfg(not(windows))]
        let monitors = super::list_monitors()?;

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

        let total_width = width_i32 as u32;
        let total_height = height_i32 as u32;

        // when the portal is the only wayland source, its screenshot already
        // covers the whole desktop: one call instead of n crops of the same
        // shot, and one permission prompt instead of n on first run
        #[cfg(target_os = "linux")]
        if super::is_wayland_session() && super::wayland_chain::portal_only() {
            return super::portal_whole_desktop();
        }

        let mut combined = RgbaImage::new(total_width, total_height);

        // capture monitors concurrently when there's more than one: the
        // per-monitor readbacks overlap and the work spreads across cores.
        // windows HDR captures serialise on HDR_CAPTURE_LOCK inside
        // capture_one_monitor, wayland's ext-copy session serialises on its
        // own mutex, and each worker forces par_convert serial
        // (capture_serial) so the inner pixel conversions don't
        // oversubscribe. a single monitor stays on the calling thread.
        let captured: Vec<Option<RgbaImage>> = if monitors.len() > 1 {
            std::thread::scope(|s| {
                let handles: Vec<_> = monitors
                    .iter()
                    .map(|m| {
                        s.spawn(move || {
                            super::capture_serial(|| super::capture_one_monitor(m).ok())
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or(None))
                    .collect()
            })
        } else {
            vec![super::capture_one_monitor(&monitors[0]).ok()]
        };

        for (monitor, img) in monitors.iter().zip(captured) {
            let Some(img) = img else {
                tracing::warn!(
                    "capture_one_monitor failed for {}x{}+{}+{}",
                    monitor.width,
                    monitor.height,
                    monitor.x,
                    monitor.y,
                );
                continue;
            };
            let offset_x_i32 = monitor.x.saturating_sub(min_x);
            let offset_y_i32 = monitor.y.saturating_sub(min_y);
            if offset_x_i32 < 0 || offset_y_i32 < 0 {
                continue;
            }
            if let Err(e) = combined.copy_from(&img, offset_x_i32 as u32, offset_y_i32 as u32) {
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
                .find(|m| m.id().map(|i| i == id).unwrap_or(false))
                .ok_or_else(|| anyhow!("Monitor {} not found", id)),
            None => monitors
                .into_iter()
                .find(|m| m.is_primary().unwrap_or(false))
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
                    None => monitors
                        .into_iter()
                        .find(|m| m.is_primary)
                        .or_else(|| super::fast_list_monitors().ok()?.into_iter().next()),
                };
                if let Some(m) = info {
                    return Ok(m);
                }
            }
        }
        let monitor = self.find_monitor()?;
        super::monitor_info(&monitor)
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
        let monitor_info = self.get_monitor_info()?;

        let img = super::capture_one_monitor(&monitor_info)?;

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
