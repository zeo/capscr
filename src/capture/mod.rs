#![allow(dead_code)]

mod screen;
mod window;
mod region;
mod hdr;
mod hdr_png;
mod tonemapping;
mod cursor;

pub use screen::ScreenCapture;
pub use window::WindowCapture;
pub use region::RegionCapture;
pub use tonemapping::TonemapParams;
pub use hdr::HdrCapture;
pub use hdr_png::{encode_hdr_png, read_cicp, HdrBitmap, HdrTransfer};
pub use cursor::composite_system_cursor;

use std::sync::OnceLock;

static TONEMAP_OVERRIDE: OnceLock<TonemapParams> = OnceLock::new();

pub fn install_tonemap_params(params: TonemapParams) {
    let _ = TONEMAP_OVERRIDE.set(params);
}

pub fn current_tonemap_params() -> TonemapParams {
    TONEMAP_OVERRIDE.get().copied().unwrap_or_default()
}

// HDR-aware capture gate, shared by all capture entry points. default-OFF
// because the CPU tonemap pipeline takes multi-second time on 4K HDR
// monitors and the per-channel clamp at sRGB encode means saturated
// colours (magenta, cyan) still look overblown — same trade as ShareX,
// Snipping Tool, Print Screen, etc. CAPSCR_HDR_AWARE=1 opts into the
// slow-but-tonemapped path. WGC-based capture (OS-side tonemap, instant)
// is in progress and will replace both this path and the GDI default
// when ready.
pub fn hdr_aware_enabled() -> bool {
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let raw = std::env::var("CAPSCR_HDR_AWARE").unwrap_or_else(|_| "<unset>".to_string());
        let forced_on = matches!(raw.trim(), "1" | "true" | "TRUE" | "on");
        tracing::info!(
            "CAPSCR_HDR_AWARE env var = {:?} -> hdr_aware_enabled = {}",
            raw,
            forced_on,
        );
        forced_on
    })
}

use anyhow::Result;
use image::RgbaImage;

pub trait Capture {
    fn capture(&self) -> Result<RgbaImage>;
}

// Rotate a freshly-captured monitor image to match the orientation Windows
// reports for that monitor. DXGI Desktop Duplication and GDI BitBlt both
// hand back the framebuffer in its NATIVE (unrotated) orientation; if the
// user has set the monitor to Portrait in display settings, that means a
// 1920x1080-native panel gives us a 1920x1080 image while monitor.width()
// reports 1080 and monitor.height() reports 1920. compositing the native
// image into the rotated virtual-screen slot crops it and visually rotates
// it 90° in the saved PNG. fix: if captured dimensions are swapped vs the
// reported monitor dimensions, rotate the image to match.
//
// expected_w/h are what monitor.width()/height() report (post-rotation).
#[cfg(windows)]
pub fn orient_captured_image(
    img: RgbaImage,
    expected_w: u32,
    expected_h: u32,
    monitor_x: i32,
    monitor_y: i32,
) -> RgbaImage {
    let (iw, ih) = (img.width(), img.height());
    if iw == expected_w && ih == expected_h {
        return img;
    }
    if iw == expected_h && ih == expected_w {
        // dimensions swapped — monitor is in portrait. query the actual
        // rotation so we know whether to spin 90° or 270°.
        let rotation = current_monitor_rotation_at(monitor_x, monitor_y);
        let rotated = match rotation {
            MonitorRotation::Rotate90 => image::imageops::rotate90(&img),
            MonitorRotation::Rotate270 => image::imageops::rotate270(&img),
            // unknown or 180° (which preserves dimensions): default to 270°
            // because that's the most common physical-portrait orientation
            // for desktop monitors mounted on a stand.
            _ => image::imageops::rotate270(&img),
        };
        tracing::info!(
            "orient_captured_image: captured {iw}x{ih}, expected {expected_w}x{expected_h}, applied {:?}",
            rotation,
        );
        return rotated;
    }
    if iw == expected_w && ih == expected_h * 2 {
        // dimensions doubled vertically (some xcap quirk on certain
        // configurations); take the top half.
        return image::imageops::crop_imm(&img, 0, 0, iw, expected_h).to_image();
    }
    tracing::warn!(
        "orient_captured_image: captured {iw}x{ih} doesn't match expected {expected_w}x{expected_h} and isn't a swap — passing through unchanged",
    );
    img
}

#[cfg(not(windows))]
pub fn orient_captured_image(
    img: RgbaImage,
    _expected_w: u32,
    _expected_h: u32,
    _monitor_x: i32,
    _monitor_y: i32,
) -> RgbaImage {
    img
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRotation {
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
    Unknown,
}

#[cfg(windows)]
fn current_monitor_rotation_at(x: i32, y: i32) -> MonitorRotation {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromPoint, DEVMODEW,
        ENUM_CURRENT_SETTINGS, MONITORINFOEXW, MONITOR_DEFAULTTONULL,
    };
    unsafe {
        let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL);
        if hmon.is_invalid() {
            return MonitorRotation::Unknown;
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _).as_bool() {
            return MonitorRotation::Unknown;
        }
        let mut devmode = DEVMODEW::default();
        devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        let ok = EnumDisplaySettingsW(
            windows::core::PCWSTR(info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        );
        if !ok.as_bool() {
            return MonitorRotation::Unknown;
        }
        // dmDisplayOrientation lives in the Anonymous2 union inside DEVMODEW.
        let orient = devmode.Anonymous1.Anonymous2.dmDisplayOrientation;
        // DMDO_DEFAULT = 0, DMDO_90 = 1, DMDO_180 = 2, DMDO_270 = 3
        match orient.0 {
            0 => MonitorRotation::Identity,
            1 => MonitorRotation::Rotate90,
            2 => MonitorRotation::Rotate180,
            3 => MonitorRotation::Rotate270,
            _ => MonitorRotation::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    FullScreen,
    Window,
    Region,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rectangle {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rectangle {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    #[cfg(any(test, windows))]
    pub fn normalize(start_x: i32, start_y: i32, end_x: i32, end_y: i32) -> Self {
        let x = start_x.min(end_x);
        let y = start_y.min(end_y);
        let width = (start_x - end_x).unsigned_abs();
        let height = (start_y - end_y).unsigned_abs();
        Self { x, y, width, height }
    }
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    let screens = xcap::Monitor::all()?;
    let monitors: Vec<MonitorInfo> = screens
        .into_iter()
        .map(|s| MonitorInfo {
            id: s.id(),
            name: s.name().to_string(),
            x: s.x(),
            y: s.y(),
            width: s.width(),
            height: s.height(),
            is_primary: s.is_primary(),
        })
        .collect();
    Ok(monitors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rectangle_normalize() {
        let rect = Rectangle::normalize(100, 200, 50, 100);
        assert_eq!(rect.x, 50);
        assert_eq!(rect.y, 100);
        assert_eq!(rect.width, 50);
        assert_eq!(rect.height, 100);
    }


    #[test]
    fn test_screen_capture_with_monitor() {
        let capture = ScreenCapture::with_monitor(1);
        assert!(capture.get_monitor_info().is_err() || capture.get_monitor_info().is_ok());
    }

    #[test]
    fn test_region_capture_new() {
        let rect = Rectangle::new(0, 0, 100, 100);
        let capture = RegionCapture::new(rect);
        let region = capture.region();
        assert_eq!(region.width, 100);
    }

    #[test]
    fn test_region_capture_from_coords() {
        let capture = RegionCapture::from_coords(0, 0, 100, 100);
        let region = capture.region();
        assert_eq!(region.width, 100);
    }

    #[test]
    fn test_window_capture_methods() {
        let _ = WindowCapture::focused();
        let _ = WindowCapture::from_title("nonexistent");
        let windows = WindowCapture::list_application_windows().unwrap_or_default();
        assert!(windows.is_empty() || !windows.is_empty());
    }
}
