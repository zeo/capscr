use anyhow::{anyhow, Result};
use image::RgbaImage;
use xcap::Window;

use super::{Capture, WindowInfo};

pub struct WindowCapture {
    window_id: u32,
}

impl WindowCapture {
    pub fn new(window_id: u32) -> Self {
        Self { window_id }
    }

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

    pub fn get_window_info(&self) -> Result<WindowInfo> {
        let window = self.find_window()?;
        Ok(WindowInfo {
            id: window.id(),
            title: window.title().to_string(),
            app_name: window.app_name().to_string(),
            x: window.x(),
            y: window.y(),
            width: window.width(),
            height: window.height(),
            is_visible: !window.is_minimized(),
        })
    }

    pub fn list_application_windows() -> Result<Vec<WindowInfo>> {
        let windows = Window::all()?;
        let mut app_windows: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| {
                !w.title().is_empty()
                    && w.width() > 50
                    && w.height() > 50
                    && !w.is_minimized()
                    && !is_system_window(w)
            })
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

        app_windows.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(app_windows)
    }
}

fn is_system_window(window: &Window) -> bool {
    let title = window.title().to_lowercase();
    let app = window.app_name().to_lowercase();

    let system_keywords = [
        "desktop",
        "taskbar",
        "start menu",
        "notification",
        "system tray",
        "program manager",
        "shell_traywnd",
        "applicationframehost",
    ];

    system_keywords
        .iter()
        .any(|k| title.contains(k) || app.contains(k))
}

impl Capture for WindowCapture {
    fn capture(&self) -> Result<RgbaImage> {
        let window = self.find_window()?;
        let img = window.capture_image()?;
        Ok(img)
    }
}
