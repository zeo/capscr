use iced::widget::{
    button, column, container, horizontal_space, pick_list, row, scrollable, text, text_input,
    toggler,
};
use iced::{Alignment, Element, Length};

use crate::capture::{list_monitors, MonitorInfo};
use crate::config::{Config, PostCaptureAction, Theme, UploadDestination};
use crate::hotkeys::{format_hotkey_string, HotkeyAction};
use crate::ui::style::{
    container_style, surface_container_style, tile_button_style, tile_container_style,
    MonochromeTheme,
};
use crate::ui::Message;

pub struct SettingsView;

impl SettingsView {
    pub fn view(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let back_style = tile_button_style(theme);
        let container_bg = container_style(theme);

        let title = text("Settings").size(24);
        let back_btn = button(text("Back").size(12))
            .padding([6, 12])
            .style(move |_t, _s| back_style)
            .on_press(Message::HideSettings);

        let header = row![title, horizontal_space(), back_btn]
            .align_y(Alignment::Center)
            .spacing(16);

        let output_section = Self::output_section(theme, config);
        let capture_section = Self::capture_section(theme, config);
        let monitor_section = Self::monitor_section(theme);
        let post_capture_section = Self::post_capture_section(theme, config);
        let upload_section = Self::upload_section(theme, config);
        let hotkey_section = Self::hotkey_section(theme, config);
        let ui_section = Self::ui_section(theme, config);

        let sections = column![
            output_section,
            capture_section,
            monitor_section,
            post_capture_section,
            upload_section,
            hotkey_section,
            ui_section,
        ]
        .spacing(20);

        let scrollable_content = scrollable(sections).height(Length::Fill);
        let main_content = column![header, scrollable_content].spacing(20);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .style(move |_| container_bg)
            .into()
    }

    fn section_container(
        theme: &MonochromeTheme,
        title_str: &str,
        content: Element<'static, Message>,
    ) -> Element<'static, Message> {
        let style = surface_container_style(theme);
        let header = text(title_str.to_string()).size(16);
        let full = column![header, content].spacing(12);

        container(full)
            .width(Length::Fill)
            .padding(16)
            .style(move |_| style)
            .into()
    }

    fn monitor_section(theme: &MonochromeTheme) -> Element<'static, Message> {
        let monitors: Vec<MonitorInfo> = list_monitors().unwrap_or_default();
        let tile_style = tile_container_style(theme);

        let monitor_rows: Vec<Element<'static, Message>> = monitors
            .iter()
            .map(|m| {
                let info = format!(
                    "[{}] {}: {}x{} at ({}, {}){}",
                    m.id,
                    m.name,
                    m.width,
                    m.height,
                    m.x,
                    m.y,
                    if m.is_primary { " [Primary]" } else { "" }
                );
                container(text(info).size(12))
                    .width(Length::Fill)
                    .padding(8)
                    .style(move |_| tile_style)
                    .into()
            })
            .collect();

        let monitor_list = if monitor_rows.is_empty() {
            column![text("No monitors detected").size(12)]
        } else {
            monitor_rows.into_iter().fold(column![].spacing(4), |col, elem| col.push(elem))
        };

        Self::section_container(theme, "Monitors", monitor_list.into())
    }

    fn output_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let btn_style = tile_button_style(theme);
        let dir_str = config.output.directory.to_string_lossy().to_string();
        let quality_str = format!("{}%", config.output.quality);

        let dir_row = row![
            text("Output Directory:").size(13),
            horizontal_space(),
            text(dir_str).size(12),
            button(text("Browse").size(11))
                .padding([4, 8])
                .style(move |_t, _s| btn_style)
                .on_press(Message::BrowseOutputDir),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let quality_row = row![
            text("Quality:").size(13),
            horizontal_space(),
            text(quality_str).size(12),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let content = column![dir_row, quality_row].spacing(12);
        Self::section_container(theme, "Output", content.into())
    }

    fn capture_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let delay_str = config.capture.delay_ms.to_string();
        let fps_str = config.capture.gif_fps.to_string();
        let show_cursor = config.capture.show_cursor;

        let cursor_row = row![
            text("Show Cursor:").size(13),
            horizontal_space(),
            toggler(show_cursor).on_toggle(Message::ToggleShowCursor),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let delay_row = row![
            text("Capture Delay (ms):").size(13),
            horizontal_space(),
            text_input("0", &delay_str)
                .width(60)
                .on_input(|s| Message::SetCaptureDelay(s.parse().unwrap_or(0))),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let fps_row = row![
            text("GIF FPS:").size(13),
            horizontal_space(),
            text_input("15", &fps_str)
                .width(60)
                .on_input(|s| Message::SetGifFps(s.parse().unwrap_or(15))),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let content = column![cursor_row, delay_row, fps_row].spacing(12);
        Self::section_container(theme, "Capture", content.into())
    }

    fn hotkey_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let hotkey_values = [
            (HotkeyAction::CaptureScreen, "screen", &config.hotkeys.capture_screen),
            (HotkeyAction::CaptureWindow, "window", &config.hotkeys.capture_window),
            (HotkeyAction::CaptureRegion, "region", &config.hotkeys.capture_region),
            (HotkeyAction::RecordGif, "gif", &config.hotkeys.record_gif),
        ];

        let mut rows = column![].spacing(12);

        for action in HotkeyAction::all() {
            let (_, key, value) = hotkey_values
                .iter()
                .find(|(a, _, _)| a == action)
                .unwrap();
            let key_str = (*key).to_string();
            let val_str = (*value).clone();
            let formatted = format_hotkey_string(&val_str);
            let label = action.display_name();

            let hotkey_row = row![
                text(format!("{}:", label)).size(13),
                text(format!("[{}]", formatted)).size(11),
                horizontal_space(),
                text_input("Ctrl+Shift+...", &val_str)
                    .width(150)
                    .on_input(move |s| Message::SetHotkey(key_str.clone(), s)),
            ]
            .spacing(8)
            .align_y(Alignment::Center);

            rows = rows.push(hotkey_row);
        }

        Self::section_container(theme, "Hotkeys", rows.into())
    }

    fn ui_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let theme_options = vec!["Dark", "Light"];
        let current_theme = match config.ui.theme {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        };
        let show_notif = config.ui.show_notifications;
        let copy_clip = config.ui.copy_to_clipboard;
        let min_tray = config.ui.minimize_to_tray;

        let theme_row = row![
            text("Theme:").size(13),
            horizontal_space(),
            pick_list(theme_options, Some(current_theme), |s| {
                Message::SetTheme(if s == "Dark" { Theme::Dark } else { Theme::Light })
            })
            .width(100),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let notify_row = row![
            text("Show Notifications:").size(13),
            horizontal_space(),
            toggler(show_notif).on_toggle(Message::ToggleNotifications),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let clipboard_row = row![
            text("Copy to Clipboard:").size(13),
            horizontal_space(),
            toggler(copy_clip).on_toggle(Message::ToggleClipboard),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let tray_row = row![
            text("Minimize to Tray:").size(13),
            horizontal_space(),
            toggler(min_tray).on_toggle(Message::ToggleMinimizeToTray),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let content = column![theme_row, notify_row, clipboard_row, tray_row].spacing(12);
        Self::section_container(theme, "Interface", content.into())
    }

    fn post_capture_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let current_action = config.post_capture.action;

        let action_options: Vec<&'static str> = PostCaptureAction::all()
            .iter()
            .map(|a| a.display_name())
            .collect();
        let current_action_str = current_action.display_name();

        let action_row = row![
            text("After Capture:").size(13),
            horizontal_space(),
            pick_list(action_options, Some(current_action_str), |s| {
                let action = PostCaptureAction::all()
                    .iter()
                    .find(|a| a.display_name() == s)
                    .copied()
                    .unwrap_or_default();
                Message::SetPostCaptureAction(action)
            })
            .width(150),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let content = column![action_row].spacing(12);
        Self::section_container(theme, "Post-Capture", content.into())
    }

    fn upload_section(theme: &MonochromeTheme, config: &Config) -> Element<'static, Message> {
        let current_dest = config.upload.destination;
        let custom_url = config.upload.custom_url.clone();
        let form_name = config.upload.custom_form_name.clone();
        let response_path = config.upload.custom_response_path.clone();

        let dest_options: Vec<&'static str> = UploadDestination::all()
            .iter()
            .map(|d| d.display_name())
            .collect();
        let current_dest_str = current_dest.display_name();

        let dest_row = row![
            text("Upload To:").size(13),
            horizontal_space(),
            pick_list(dest_options, Some(current_dest_str), |s| {
                let dest = UploadDestination::all()
                    .iter()
                    .find(|d| d.display_name() == s)
                    .copied()
                    .unwrap_or_default();
                Message::SetUploadDestination(dest)
            })
            .width(150),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let url_row = row![
            text("Custom URL:").size(13),
            horizontal_space(),
            text_input("https://...", &custom_url)
                .width(200)
                .on_input(Message::SetCustomUploadUrl),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let form_row = row![
            text("Form Field:").size(13),
            horizontal_space(),
            text_input("file", &form_name)
                .width(100)
                .on_input(Message::SetCustomFormName),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let path_row = row![
            text("Response Path:").size(13),
            horizontal_space(),
            text_input("url", &response_path)
                .width(100)
                .on_input(Message::SetCustomResponsePath),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let content = column![dest_row, url_row, form_row, path_row].spacing(12);
        Self::section_container(theme, "Upload", content.into())
    }
}
