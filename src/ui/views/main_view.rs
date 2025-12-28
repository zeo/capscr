use iced::widget::{button, container, horizontal_space, row, text};
use iced::{Alignment, Border, Color, Element, Length};

use crate::config::ImageFormat;
use crate::recording::RecordingState;
use crate::ui::style::{primary_button_style, MonochromeTheme};
use crate::ui::Message;

pub struct MainView;

impl MainView {
    pub fn view(
        theme: &MonochromeTheme,
        recording_state: RecordingState,
        current_format: ImageFormat,
        _frame_count: usize,
    ) -> Element<'static, Message> {
        let format_buttons = ImageFormat::all()
            .iter()
            .fold(row![].spacing(4), |r, &fmt| {
                let is_selected = fmt == current_format;
                let label = fmt.display_name();

                let style = if is_selected {
                    primary_button_style(theme)
                } else {
                    crate::ui::style::tile_button_style(theme)
                };

                r.push(
                    button(text(label).size(11))
                        .padding([4, 8])
                        .style(move |_t, _s| style)
                        .on_press(Message::SetFormat(fmt)),
                )
            });

        let recording_indicator = match recording_state {
            RecordingState::Idle => text("").size(11),
            RecordingState::Recording => text("[REC]").size(11),
            RecordingState::Processing => text("[...]").size(11),
        };

        let settings_style = crate::ui::style::tile_button_style(theme);
        let settings_btn = button(text("[=]").size(11))
            .padding([4, 8])
            .style(move |_t, _s| settings_style)
            .on_press(Message::ShowSettings);

        let toolbar = row![
            format_buttons,
            recording_indicator,
            horizontal_space(),
            settings_btn,
        ]
        .spacing(8)
        .padding(8)
        .align_y(Alignment::Center);

        let surface = theme.surface();
        let border_color = if theme.is_dark {
            Color::from_rgb(0.3, 0.3, 0.3)
        } else {
            Color::from_rgb(0.7, 0.7, 0.7)
        };

        container(toolbar)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(Color::from_rgba(
                    surface.r,
                    surface.g,
                    surface.b,
                    0.95,
                ))),
                border: Border {
                    color: border_color,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            })
            .into()
    }
}
