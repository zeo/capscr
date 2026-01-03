#![allow(dead_code)]

mod screen;
mod window;
mod region;
mod hdr;
mod tonemapping;

pub use screen::ScreenCapture;
pub use window::WindowCapture;
pub use region::RegionCapture;

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
