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

        // HDR-aware path: when an HDR display is in play and the gate is
        // on, capture the screen via DXGI Desktop Duplication and crop to
        // the window's DWM frame bounds. matches what RegionCapture +
        // ScreenCapture do, so the per-window output isn't mismatched
        // against region/full-screen captures of the same HDR content.
        #[cfg(windows)]
        if super::hdr_aware_enabled() && super::HdrCapture::is_hdr_available() {
            match self_capture_screen_region(self.window_id) {
                Ok(img) => return Ok(img),
                Err(e) => tracing::warn!(
                    "WindowCapture HDR path failed — GDI fallback: {e:#}"
                ),
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

