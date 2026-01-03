use iced::{
    widget::{button, column, container, horizontal_space, pick_list, row, slider, text, toggler},
    Alignment, Element, Length,
};
use std::path::PathBuf;

use crate::config::{Config, ImageFormat, PostCaptureAction};
use crate::ui::style::MonochromeTheme;
use crate::ui::{Message, SettingChange};

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
        }
    }
}

pub struct SettingsView;

impl SettingsView {
    pub fn view<'a>(
        theme: &'a MonochromeTheme,
        state: &'a SettingsState,
        _config: &'a Config,
    ) -> Element<'a, Message> {
        let title = text("Settings")
            .size(24);

        let output_section = column![
            text("Output").size(18),
            row![
                text("Directory:").width(Length::Fixed(120.0)),
                text(state.output_directory.to_string_lossy().to_string())
                    .width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Format:").width(Length::Fixed(120.0)),
                pick_list(
                    ImageFormat::all(),
                    Some(state.format),
                    |f| Message::SettingChanged(SettingChange::Format(f))
                ),
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

        let hotkey_section = column![
            text("Hotkeys").size(18),
            row![
                text("Screenshot:").width(Length::Fixed(120.0)),
                text(&state.screenshot_hotkey),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            row![
                text("Record GIF:").width(Length::Fixed(120.0)),
                text(&state.gif_hotkey),
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

        let close_button = button(text("Close"))
            .padding([8, 16])
            .on_press(Message::CloseSettings);

        let content = column![
            title,
            output_section,
            hotkey_section,
            gif_section,
            behavior_section,
            row![horizontal_space(), close_button],
        ]
        .spacing(20)
        .padding(20)
        .width(Length::Fixed(450.0));

        let bg = theme.background();
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(move |_t| {
                container::Style {
                    background: Some(iced::Background::Color(bg)),
                    ..Default::default()
                }
            })
            .into()
    }
}
