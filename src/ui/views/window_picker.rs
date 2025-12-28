use iced::widget::{button, column, container, horizontal_space, row, scrollable, text};
use iced::{Alignment, Element, Length};

use crate::capture::WindowInfo;
use crate::ui::style::{
    container_style, tile_button_style, tile_button_hovered_style, MonochromeTheme,
};
use crate::ui::Message;

pub struct WindowPicker;

impl WindowPicker {
    pub fn view(theme: &MonochromeTheme, windows: &[WindowInfo]) -> Element<'static, Message> {
        let back_style = tile_button_style(theme);
        let container_bg = container_style(theme);

        let title = text("Select Window").size(24);
        let back_btn = button(text("Cancel").size(12))
            .padding([6, 12])
            .style(move |_t, _s| back_style)
            .on_press(Message::HideWindowPicker);

        let header = row![title, horizontal_space(), back_btn]
            .align_y(Alignment::Center)
            .spacing(16);

        let window_list: Element<'static, Message> = if windows.is_empty() {
            column![text("No windows found").size(14)].into()
        } else {
            let items = windows
                .iter()
                .fold(column![].spacing(8), |col, window| {
                    let item = Self::window_item(theme, window);
                    col.push(item)
                });
            scrollable(items).height(Length::Fill).into()
        };

        let main_content = column![header, window_list].spacing(20);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .style(move |_| container_bg)
            .into()
    }

    fn window_item(
        theme: &MonochromeTheme,
        window: &WindowInfo,
    ) -> Element<'static, Message> {
        let window_id = window.id;
        let title_str = window.title.clone();
        let app_str = window.app_name.clone();
        let size_str = format!("{}x{} at ({}, {})", window.width, window.height, window.x, window.y);

        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);

        let title_text = text(title_str).size(14);
        let app_text = text(app_str).size(11);
        let size_text = text(size_str).size(11);

        let info = column![title_text, app_text].spacing(4);
        let content = row![info, horizontal_space(), size_text]
            .align_y(Alignment::Center)
            .spacing(16);

        button(
            container(content)
                .width(Length::Fill)
                .padding(12)
        )
        .width(Length::Fill)
        .style(move |_t, status| {
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                hover_style
            } else {
                normal_style
            }
        })
        .on_press(Message::SelectWindow(window_id))
        .into()
    }
}
