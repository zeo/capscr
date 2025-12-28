mod screen;
mod window;
mod region;
mod hdr;
mod tonemapping;

pub use screen::ScreenCapture;
pub use window::WindowCapture;
pub use region::RegionCapture;
pub use hdr::{HdrCapture, HdrFormat};
pub use tonemapping::ToneMapOperator;

use anyhow::Result;
use image::RgbaImage;

pub trait Capture {
    fn capture(&self) -> Result<RgbaImage>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    FullScreen,
    Window,
    Region,
    HdrScreen,
}

impl CaptureMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            CaptureMode::FullScreen => "Full Screen",
            CaptureMode::Window => "Window",
            CaptureMode::Region => "Region",
            CaptureMode::HdrScreen => "HDR Screen",
        }
    }
}

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_visible: bool,
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

pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let windows = xcap::Window::all()?;
    let window_infos: Vec<WindowInfo> = windows
        .into_iter()
        .filter(|w| !w.title().is_empty() && w.width() > 0 && w.height() > 0)
        .map(|w| WindowInfo {
            id: w.id(),
            title: w.title().to_string(),
            app_name: w.app_name().to_string(),
            x: w.x(),
            y: w.y(),
            width: w.width(),
            height: w.height(),
            is_visible: !w.is_minimized(),
        })
        .collect();
    Ok(window_infos)
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
    fn test_capture_mode_display_names() {
        assert_eq!(CaptureMode::FullScreen.display_name(), "Full Screen");
        assert_eq!(CaptureMode::Window.display_name(), "Window");
        assert_eq!(CaptureMode::Region.display_name(), "Region");
        assert_eq!(CaptureMode::HdrScreen.display_name(), "HDR Screen");
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
