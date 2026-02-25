use iced::{
    widget::{
        button, column, container, horizontal_space, pick_list, row, scrollable, slider, text,
        text_input, toggler,
    },
    Alignment, Element, Length,
};
use std::path::PathBuf;

use crate::config::{
    Config, ImageFormat, PostCaptureAction, RendererBackend, Theme, UploadDestination,
};
use crate::ui::style::MonochromeTheme;
use crate::ui::{HotkeyTarget, Message, SettingChange};

#[derive(Debug, Clone)]
pub struct SettingsState {
    pub output_directory: PathBuf,
    pub format: ImageFormat,
    pub quality: u8,
    pub screenshot_hotkey: String,
    pub gif_hotkey: String,
    pub gif_fps: u32,
    pub gif_max_duration: u32,
    pub show_notifications: bool,
    pub post_capture_action: PostCaptureAction,
    pub theme: Theme,
    pub play_sound: bool,
    pub upload_destination: UploadDestination,
    pub custom_upload_url: String,
    pub copy_url_to_clipboard: bool,
    pub tick_interval_ms: u32,
    pub renderer: RendererBackend,
    pub lazy_init_upload: bool,
    pub lazy_init_plugins: bool,
}

impl SettingsState {
    pub fn from_config(config: &Config) -> Self {
        Self {
            output_directory: config.output.directory.clone(),
            format: config.output.format,
            quality: config.output.quality,
            screenshot_hotkey: config.hotkeys.screenshot.clone(),
            gif_hotkey: config.hotkeys.record_gif.clone(),
            gif_fps: config.capture.gif_fps,
            gif_max_duration: config.capture.gif_max_duration_secs,
            show_notifications: config.ui.show_notifications,
            post_capture_action: config.post_capture.action,
            theme: config.ui.theme,
            play_sound: config.post_capture.play_sound,
            upload_destination: config.upload.destination,
            custom_upload_url: config.upload.custom_url.clone(),
            copy_url_to_clipboard: config.upload.copy_url_to_clipboard,
            tick_interval_ms: config.performance.tick_interval_ms,
            renderer: config.performance.renderer,
            lazy_init_upload: config.performance.lazy_init_upload,
            lazy_init_plugins: config.performance.lazy_init_plugins,
        }
    }
}

pub struct SettingsView;

impl SettingsView {
    pub fn view<'a>(
        theme: &'a MonochromeTheme,
        state: &'a SettingsState,
        _config: &'a Config,
        recording_hotkey: Option<HotkeyTarget>,
    ) -> Element<'a, Message> {
        let title = text("Settings").size(24);

        let appearance_section = column![
            text("Appearance").size(18),
            row![
                text("Theme:").width(Length::Fixed(150.0)),
                pick_list(
                    Theme::all(),
                    Some(state.theme),
                    |t| Message::SettingChanged(SettingChange::Theme(t))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let output_section = column![
            text("Output").size(18),
            row![
                text("Directory:").width(Length::Fixed(120.0)),
                text(state.output_directory.to_string_lossy().to_string()).width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Format:").width(Length::Fixed(120.0)),
                pick_list(ImageFormat::all(), Some(state.format), |f| {
                    Message::SettingChanged(SettingChange::Format(f))
                }),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text(format!("Quality: {}", state.quality)).width(Length::Fixed(120.0)),
                slider(1..=100, state.quality, |q| {
                    Message::SettingChanged(SettingChange::Quality(q))
                })
                .width(Length::Fixed(200.0)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let screenshot_hotkey_display = if recording_hotkey == Some(HotkeyTarget::Screenshot) {
            "Press keys... (Esc to cancel)".to_string()
        } else {
            state.screenshot_hotkey.clone()
        };

        let gif_hotkey_display = if recording_hotkey == Some(HotkeyTarget::RecordGif) {
            "Press keys... (Esc to cancel)".to_string()
        } else {
            state.gif_hotkey.clone()
        };

        let hotkey_section = column![
            text("Hotkeys").size(18),
            text("Click to change, then press your desired key combination").size(12),
            row![
                text("Screenshot:").width(Length::Fixed(120.0)),
                button(text(screenshot_hotkey_display))
                    .padding([6, 12])
                    .on_press(Message::StartHotkeyRecording(HotkeyTarget::Screenshot)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Record GIF:").width(Length::Fixed(120.0)),
                button(text(gif_hotkey_display))
                    .padding([6, 12])
                    .on_press(Message::StartHotkeyRecording(HotkeyTarget::RecordGif)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let gif_section = column![
            text("GIF Recording").size(18),
            row![
                text(format!("FPS: {}", state.gif_fps)).width(Length::Fixed(120.0)),
                slider(1..=60, state.gif_fps as u8, |fps| {
                    Message::SettingChanged(SettingChange::GifFps(fps as u32))
                })
                .width(Length::Fixed(200.0)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text(format!("Max Duration: {}s", state.gif_max_duration))
                    .width(Length::Fixed(120.0)),
                slider(5..=300, state.gif_max_duration as u16, |d| {
                    Message::SettingChanged(SettingChange::GifMaxDuration(d as u32))
                })
                .width(Length::Fixed(200.0)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let mut upload_section = column![
            text("Upload").size(18),
            row![
                text("Destination:").width(Length::Fixed(150.0)),
                pick_list(
                    UploadDestination::all(),
                    Some(state.upload_destination),
                    |d| Message::SettingChanged(SettingChange::UploadDestination(d))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        if state.upload_destination == UploadDestination::Custom {
            upload_section = upload_section.push(
                row![
                    text("Custom URL:").width(Length::Fixed(150.0)),
                    text_input("https://example.com/upload", &state.custom_upload_url)
                        .on_input(
                            |url| Message::SettingChanged(SettingChange::CustomUploadUrl(url))
                        )
                        .width(Length::Fixed(250.0)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }

        upload_section = upload_section.push(
            row![
                text("Copy URL to clipboard:").width(Length::Fixed(150.0)),
                toggler(state.copy_url_to_clipboard)
                    .on_toggle(|v| Message::SettingChanged(SettingChange::CopyUrlToClipboard(v))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );

        let behavior_section = column![
            text("Behavior").size(18),
            row![
                text("Show Notifications:").width(Length::Fixed(150.0)),
                toggler(state.show_notifications)
                    .on_toggle(|v| Message::SettingChanged(SettingChange::ShowNotifications(v))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Play Sound:").width(Length::Fixed(150.0)),
                toggler(state.play_sound)
                    .on_toggle(|v| Message::SettingChanged(SettingChange::PlaySound(v))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("After Capture:").width(Length::Fixed(150.0)),
                pick_list(
                    PostCaptureAction::all(),
                    Some(state.post_capture_action),
                    |a| Message::SettingChanged(SettingChange::PostCaptureAction(a))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let performance_section = column![
            text("Performance").size(18),
            row![
                text(format!("Tick Interval: {}ms", state.tick_interval_ms))
                    .width(Length::Fixed(180.0)),
                slider(16..=500, state.tick_interval_ms as u16, |ms| {
                    Message::SettingChanged(SettingChange::TickIntervalMs(ms as u32))
                })
                .width(Length::Fixed(200.0)),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Renderer (restart required):").width(Length::Fixed(180.0)),
                pick_list(RendererBackend::all(), Some(state.renderer), |renderer| {
                    Message::SettingChanged(SettingChange::RendererBackend(renderer))
                }),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Lazy init upload:").width(Length::Fixed(180.0)),
                toggler(state.lazy_init_upload)
                    .on_toggle(|v| Message::SettingChanged(SettingChange::LazyInitUpload(v))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Lazy init plugins:").width(Length::Fixed(180.0)),
                toggler(state.lazy_init_plugins)
                    .on_toggle(|v| Message::SettingChanged(SettingChange::LazyInitPlugins(v))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(10);

        let close_button = button(text("Close"))
            .padding([8, 16])
            .on_press(Message::CloseSettings);

        let content = column![
            title,
            appearance_section,
            output_section,
            hotkey_section,
            gif_section,
            upload_section,
            behavior_section,
            performance_section,
            row![horizontal_space(), close_button],
        ]
        .spacing(20)
        .padding(20)
        .width(Length::Fixed(480.0));

        let bg = theme.background();
        container(scrollable(content))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(move |_t| container::Style {
                background: Some(iced::Background::Color(bg)),
                ..Default::default()
            })
            .into()
    }
}
