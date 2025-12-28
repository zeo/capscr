#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod config;
mod hotkeys;
mod recording;
mod ui;
mod upload;

use iced::{window, Size};
use tracing_subscriber::EnvFilter;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::load().unwrap_or_default();
    let _ = config.ensure_output_dir();

    iced::application(ui::App::title, ui::App::update, ui::App::view)
        .subscription(ui::App::subscription)
        .theme(ui::App::theme)
        .window(window::Settings {
            size: Size::new(600.0, 500.0),
            min_size: Some(Size::new(400.0, 350.0)),
            resizable: true,
            decorations: true,
            ..Default::default()
        })
        .run_with(ui::App::new)
}
