#![windows_subsystem = "windows"]

mod capture;
mod clipboard;
mod config;
mod hotkeys;
mod overlay;
mod plugin;
mod recording;
mod sound;
mod tray;
mod ui;
mod upload;

use iced::{window, Size};
use tracing_subscriber::EnvFilter;

const ICON_DATA: &[u8] = include_bytes!("../icon.ico");

fn load_icon() -> Option<window::Icon> {
    let img = image::load_from_memory(ICON_DATA).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    window::icon::from_rgba(rgba.into_raw(), width, height).ok()
}

#[cfg(windows)]
fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
fn set_dpi_awareness() {}

fn install_hdr_runtime_from_config(config: &config::Config) {
    use capture::{SkivMode, SkivParams};
    use config::HdrCompressionMode;

    let mode = match config.capture.hdr.mode {
        HdrCompressionMode::MapCllToDisplay => SkivMode::MapCllToDisplay,
        HdrCompressionMode::NormalizeToCll => SkivMode::NormalizeToCll,
    };
    let params = SkivParams {
        mode,
        sdr_brightness_nits: config.capture.hdr.brightness_nits,
        user_brightness_scale: config.capture.hdr.user_brightness_scale,
        use_p99_max_cll: config.capture.hdr.use_p99_max_cll,
    };
    capture::install_skiv_params(params);
}

fn main() -> iced::Result {
    set_dpi_awareness();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = config::Config::load().unwrap_or_default();
    let _ = config.ensure_output_dir();
    install_hdr_runtime_from_config(&config);
    std::env::set_var(
        "ICED_BACKEND",
        config.performance.renderer.iced_backend_value(),
    );

    let icon = load_icon();

    iced::application(ui::App::title, ui::App::update, ui::App::view)
        .subscription(ui::App::subscription)
        .theme(ui::App::theme)
        .window(window::Settings {
            size: Size::new(1.0, 1.0),
            min_size: Some(Size::new(1.0, 1.0)),
            max_size: None,
            position: window::Position::Default,
            visible: false,
            resizable: true,
            decorations: true,
            transparent: false,
            level: window::Level::Normal,
            icon,
            ..Default::default()
        })
        .run_with(ui::App::new)
}
