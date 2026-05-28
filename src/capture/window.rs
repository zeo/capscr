use anyhow::{anyhow, Result};
use image::RgbaImage;
use xcap::Window;

use super::Capture;
#[cfg(test)]
use super::WindowInfo;

pub struct WindowCapture {
    window_id: u32,
}

impl WindowCapture {
    pub fn new(window_id: u32) -> Self {
        Self { window_id }
    }

    #[cfg(test)]
    pub fn from_title(title: &str) -> Result<Self> {
        let windows = Window::all()?;
        let window = windows
            .into_iter()
            .find(|w| w.title().contains(title))
            .ok_or_else(|| anyhow!("Window with title '{}' not found", title))?;
        Ok(Self {
            window_id: window.id(),
        })
    }

    #[cfg(test)]
    pub fn focused() -> Result<Self> {
        let windows = Window::all()?;
        let window = windows
            .into_iter()
            .find(|w| !w.is_minimized() && !w.title().is_empty())
            .ok_or_else(|| anyhow!("No focused window found"))?;
        Ok(Self {
            window_id: window.id(),
        })
    }

    fn find_window(&self) -> Result<Window> {
        let windows = Window::all()?;
        windows
            .into_iter()
            .find(|w| w.id() == self.window_id)
            .ok_or_else(|| anyhow!("Window {} not found", self.window_id))
    }

    #[cfg(test)]
    pub fn list_application_windows() -> Result<Vec<WindowInfo>> {
        let windows = Window::all()?;
        let mut app_windows: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| {
                !w.title().is_empty()
                    && w.width() > 50
                    && w.height() > 50
                    && !w.is_minimized()
            })
            .map(|w| WindowInfo {
                id: w.id(),
                title: w.title().to_string(),
                app_name: w.app_name().to_string(),
                x: w.x(),
                y: w.y(),
                width: w.width(),
                height: w.height(),
            })
            .collect();

        app_windows.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(app_windows)
    }
}

impl Capture for WindowCapture {
    fn capture(&self) -> Result<RgbaImage> {
        tracing::info!("WindowCapture::capture entry: window_id={}", self.window_id);

        // capture path priority for window captures:
        //   1. CAPSCR_HDR_AWARE=1 + HDR display -> custom CPU tonemap
        //      via screen-region capture (slow, tunable look)
        //   2. HDR display -> WGC for the window's HWND
        //      (instant, OS-side tonemap)
        //   3. default -> xcap's GDI BitBlt (instant, overblown on HDR)
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, RECT};
            use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
            use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

            let wgc_on = super::wgc_enabled();
            let hwnd = HWND(self.window_id as usize as *mut _);
            
            let center_res = unsafe {
                let mut r = RECT::default();
                let ok = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    &mut r as *mut RECT as *mut _,
                    std::mem::size_of::<RECT>() as u32,
                ).is_ok();
                if ok || GetWindowRect(hwnd, &mut r).is_ok() {
                    Ok(((r.left + r.right) / 2, (r.top + r.bottom) / 2))
                } else {
                    Err(anyhow!("Failed to get window rect"))
                }
            };

            if let Ok(center) = center_res {
                let is_hdr = super::HdrCapture::is_hdr_at_point(center.0, center.1);
                if is_hdr {
                    if wgc_on {
                        let t0 = std::time::Instant::now();
                        match super::wgc::capture_window(hwnd) {
                            Ok(img) => {
                                tracing::info!(
                                    "WGC capture (window {}) {}x{} in {}ms",
                                    self.window_id, img.width(), img.height(),
                                    t0.elapsed().as_millis()
                                );
                                return Ok(img);
                            }
                            Err(e) => tracing::warn!(
                                "WGC window capture failed — fallthrough: {e:#}"
                            ),
                        }
                    } else {
                        match self_capture_screen_region(self.window_id) {
                            Ok(img) => return Ok(img),
                            Err(e) => tracing::warn!(
                                "WindowCapture CPU HDR path failed — fallthrough: {e:#}"
                            ),
                        }
                    }
                }
            }
        }

        match self.find_window() {
            Ok(window) => {
                let img = window.capture_image()?;
                Ok(img)
            }
            Err(_e) => {
                // xcap excludes windows owned by the current process to avoid
                // a GetWindowText deadlock (see xcap impl_window.rs::is_valid_window).
                // Use a screen-region capture via the window's DWM-extended
                // frame bounds: this works for any window class including
                // WebView2/Chromium/Tauri whose DirectComposition surfaces
                // are invisible to PrintWindow.
                #[cfg(windows)]
                {
                    self_capture_screen_region(self.window_id)
                }
                #[cfg(not(windows))]
                Err(_e)
            }
        }
    }
}

#[cfg(windows)]
fn d2d_window_capture(hwnd_u32: u32) -> Result<RgbaImage> {
    // D2D capture is per-monitor; for a window we need to figure out which
    // monitor the window lives on (via its center), capture that monitor
    // through the D2D pipeline, then crop to the window's DWM frame bounds
    // expressed in the captured monitor's local coordinates.
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let hwnd = HWND(hwnd_u32 as usize as *mut _);
    let rect = unsafe {
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok();
        if !ok {
            GetWindowRect(hwnd, &mut r).map_err(|e| anyhow!("GetWindowRect failed: {e}"))?;
        }
        r
    };

    // resolve which monitor + the monitor's origin
    let (mon_origin_x, mon_origin_y) = unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
        if hmon.is_invalid() {
            return Err(anyhow!("MonitorFromWindow returned null"));
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi as *mut _).as_bool() {
            return Err(anyhow!("GetMonitorInfoW failed"));
        }
        (mi.rcMonitor.left, mi.rcMonitor.top)
    };

    let center = (
        (rect.left + rect.right) / 2,
        (rect.top + rect.bottom) / 2,
    );
    let full = super::d2d_capture_at_point(Some(center))?;

    // crop window-bounds out of the full monitor capture
    let local_x = (rect.left - mon_origin_x).max(0) as u32;
    let local_y = (rect.top - mon_origin_y).max(0) as u32;
    let crop_w = ((rect.right - rect.left) as u32).min(full.width().saturating_sub(local_x));
    let crop_h = ((rect.bottom - rect.top) as u32).min(full.height().saturating_sub(local_y));
    if crop_w == 0 || crop_h == 0 {
        return Err(anyhow!("D2D window crop has zero size"));
    }
    let cropped = image::imageops::crop_imm(&full, local_x, local_y, crop_w, crop_h).to_image();
    let _ = POINT { x: 0, y: 0 }; // suppress unused
    Ok(cropped)
}

#[cfg(windows)]
fn self_capture_screen_region(hwnd_u32: u32) -> Result<RgbaImage> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let hwnd = HWND(hwnd_u32 as usize as *mut _);

    let rect = unsafe {
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok();
        if !ok {
            GetWindowRect(hwnd, &mut r).map_err(|e| anyhow!("GetWindowRect failed: {e}"))?;
        }
        r
    };

    let width = (rect.right - rect.left).max(1);
    let height = (rect.bottom - rect.top).max(1);

    // hand off to RegionCapture which uses xcap::Monitor (DXGI Desktop
    // Duplication on Windows). DXGI captures the actual composed desktop,
    // so it sees every surface including DComp-backed WebView2 content.
    let region = super::Rectangle {
        x: rect.left,
        y: rect.top,
        width: width as u32,
        height: height as u32,
    };
    super::region::RegionCapture::new(region).capture()
}

