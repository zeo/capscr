#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod config;
mod hotkeys;
mod overlay;
mod plugin;
mod recording;
mod sound;
mod ui;
mod upload;

use iced::{window, Size, Point};
use tracing_subscriber::EnvFilter;

const ICON_DATA: &[u8] = include_bytes!("../icon.ico");

fn load_icon() -> Option<window::Icon> {
    let img = image::load_from_memory(ICON_DATA).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    window::icon::from_rgba(rgba.into_raw(), width, height).ok()
}

fn get_bottom_center_position() -> Point {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        unsafe {
            let screen_width = GetSystemMetrics(SM_CXSCREEN) as f32;
            let screen_height = GetSystemMetrics(SM_CYSCREEN) as f32;
            let toolbar_width = 340.0;
            let toolbar_height = 50.0;
            let margin = 40.0;
            Point::new(
                (screen_width - toolbar_width) / 2.0,
                screen_height - toolbar_height - margin,
            )
        }
    }
    #[cfg(not(windows))]
    {
        Point::new(500.0, 800.0)
    }
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::load().unwrap_or_default();
    let _ = config.ensure_output_dir();

    let icon = load_icon();
    let position = get_bottom_center_position();

    iced::application(ui::App::title, ui::App::update, ui::App::view)
        .subscription(ui::App::subscription)
        .theme(ui::App::theme)
        .window(window::Settings {
            size: Size::new(340.0, 50.0),
            min_size: Some(Size::new(300.0, 50.0)),
            max_size: Some(Size::new(500.0, 80.0)),
            position: window::Position::Specific(position),
            resizable: false,
            decorations: false,
            transparent: true,
            level: window::Level::AlwaysOnTop,
            icon,
            ..Default::default()
        })
        .run_with(ui::App::new)
}
