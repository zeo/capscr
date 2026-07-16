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
            .find(|w| w.title().map(|t| t.contains(title)).unwrap_or(false))
            .ok_or_else(|| anyhow!("Window with title '{}' not found", title))?;
        Ok(Self {
            window_id: window.id()?,
        })
    }

    #[cfg(test)]
    pub fn focused() -> Result<Self> {
        let windows = Window::all()?;
        let window = windows
            .into_iter()
            .find(|w| {
                !w.is_minimized().unwrap_or(true)
                    && !w.title().map(|t| t.is_empty()).unwrap_or(true)
            })
            .ok_or_else(|| anyhow!("No focused window found"))?;
        Ok(Self {
            window_id: window.id()?,
        })
    }

    fn find_window(&self) -> Result<Window> {
        let windows = Window::all()?;
        windows
            .into_iter()
            .find(|w| w.id().map(|i| i == self.window_id).unwrap_or(false))
            .ok_or_else(|| anyhow!("Window {} not found", self.window_id))
    }

    #[cfg(test)]
    pub fn list_application_windows() -> Result<Vec<WindowInfo>> {
        let windows = Window::all()?;
        let mut app_windows: Vec<WindowInfo> = windows
            .into_iter()
            .filter_map(|w| {
                let title = w.title().ok()?;
                let (width, height) = (w.width().ok()?, w.height().ok()?);
                if title.is_empty() || width <= 50 || height <= 50 || w.is_minimized().ok()? {
                    return None;
                }
                Some(WindowInfo {
                    id: w.id().ok()?,
                    title,
                    app_name: w.app_name().ok()?,
                    x: w.x().ok()?,
                    y: w.y().ok()?,
                    width,
                    height,
                })
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
            use windows::Win32::Graphics::Dwm::{
                DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
            };
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
                )
                .is_ok();
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
                                    self.window_id,
                                    img.width(),
                                    img.height(),
                                    t0.elapsed().as_millis()
                                );
                                return Ok(img);
                            }
                            Err(e) => {
                                tracing::warn!("WGC window capture failed — fallthrough: {e:#}")
                            }
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
                #[cfg(target_os = "linux")]
                {
                    self_capture_screen_region(self.window_id).map_err(|fallback| {
                        _e.context(format!("x11 region fallback also failed: {fallback:#}"))
                    })
                }
                #[cfg(not(any(windows, target_os = "linux")))]
                Err(_e)
            }
        }
    }
}

// xcap can drop a window from its enumeration (stale ids, override-redirect
// surfaces) while the X server still knows its geometry; ask the server
// directly and grab that screen region, mirroring the DWM frame-bounds
// fallback on windows. only reached from the x11 selector flow — wayland
// window captures route through the wayland chain
#[cfg(target_os = "linux")]
fn self_capture_screen_region(window_id: u32) -> Result<RgbaImage> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;

    let (conn, _) = x11rb::connect(None)?;
    let geometry = conn.get_geometry(window_id)?.reply()?;
    let root = conn
        .setup()
        .roots
        .first()
        .ok_or_else(|| anyhow!("no x11 screen"))?
        .root;
    let origin = conn.translate_coordinates(window_id, root, 0, 0)?.reply()?;
    let region = super::Rectangle {
        x: origin.dst_x as i32,
        y: origin.dst_y as i32,
        width: geometry.width.max(1) as u32,
        height: geometry.height.max(1) as u32,
    };
    super::region::RegionCapture::new(region).capture()
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
