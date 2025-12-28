use iced::widget::{button, column, container, horizontal_space, row, text};
use iced::{Alignment, Element, Length};

use crate::capture::CaptureMode;
use crate::config::ImageFormat;
use crate::recording::RecordingState;
use crate::ui::components::Tile;
use crate::ui::style::{
    container_style, primary_button_style, surface_container_style,
    MonochromeTheme,
};
use crate::ui::Message;

pub struct MainView;

impl MainView {
    pub fn view(
        theme: &MonochromeTheme,
        recording_state: RecordingState,
        current_format: ImageFormat,
        frame_count: usize,
    ) -> Element<'static, Message> {
        let screen_tile = Tile::new("[ ]", "Screen").with_sublabel(CaptureMode::FullScreen.display_name());
        let window_tile = Tile::new("[=]", "Window").with_sublabel(CaptureMode::Window.display_name());
        let region_tile = Tile::new("[/]", "Region").with_sublabel(CaptureMode::Region.display_name());
        let hdr_tile = Tile::new("[H]", "HDR").with_sublabel(CaptureMode::HdrScreen.display_name());
        let gif_sublabel = match recording_state {
            RecordingState::Idle => "Record clip".to_string(),
            RecordingState::Recording => format!("{} frames", frame_count),
            RecordingState::Processing => "Processing...".to_string(),
        };
        let gif_tile = Tile::new("(o)", "GIF").with_sublabel(&gif_sublabel);

        let style_surface = surface_container_style(theme);
        let style_container = container_style(theme);

        let tiles_row = row![
            screen_tile.view(theme, Message::Capture(CaptureMode::FullScreen)),
            window_tile.view(theme, Message::ShowWindowPicker),
            region_tile.view(theme, Message::Capture(CaptureMode::Region)),
            hdr_tile.view(theme, Message::Capture(CaptureMode::HdrScreen)),
            gif_tile.view(theme, Message::ToggleGifRecording),
        ]
        .spacing(16)
        .align_y(Alignment::Center);

        let tiles_container = container(tiles_row)
            .width(Length::Shrink)
            .padding(20)
            .style(move |_| style_surface);

        let style_surface2 = surface_container_style(theme);
        let format_buttons = ImageFormat::all()
            .iter()
            .fold(row![].spacing(8), |r, &fmt| {
                let is_selected = fmt == current_format;
                let label = fmt.display_name();

                let style = if is_selected {
                    primary_button_style(theme)
                } else {
                    crate::ui::style::tile_button_style(theme)
                };

                r.push(
                    button(text(label).size(12))
                        .padding([6, 12])
                        .style(move |_t, _s| style)
                        .on_press(Message::SetFormat(fmt)),
                )
            });

        let settings_style = crate::ui::style::tile_button_style(theme);
        let settings_btn = button(text("Settings").size(12))
            .padding([6, 12])
            .style(move |_t, _s| settings_style)
            .on_press(Message::ShowSettings);

        let bottom_bar = row![format_buttons, horizontal_space(), settings_btn]
            .spacing(16)
            .align_y(Alignment::Center);

        let bottom_container = container(bottom_bar)
            .width(Length::Fill)
            .padding(16)
            .style(move |_| style_surface2);

        let main_content = column![
            container(column![].height(Length::Fill)).height(Length::FillPortion(1)),
            tiles_container,
            container(column![].height(Length::Fill)).height(Length::FillPortion(1)),
            bottom_container,
        ]
        .align_x(Alignment::Center)
        .spacing(16);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .style(move |_| style_container)
            .into()
    }
}
