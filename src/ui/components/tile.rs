use iced::widget::{button, column, container, text};
use iced::{Alignment, Element, Length};

use crate::ui::style::{tile_button_hovered_style, tile_button_style, MonochromeTheme};

pub struct Tile {
    pub icon: String,
    pub label: String,
    pub sublabel: Option<String>,
}

impl Tile {
    pub fn new(icon: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            icon: icon.into(),
            label: label.into(),
            sublabel: None,
        }
    }

    pub fn with_sublabel(mut self, sublabel: impl Into<String>) -> Self {
        self.sublabel = Some(sublabel.into());
        self
    }

    pub fn view<Message: Clone + 'static>(
        &self,
        theme: &MonochromeTheme,
        on_press: Message,
    ) -> Element<'static, Message> {
        let icon_text = text(self.icon.clone()).size(32);
        let label_text = text(self.label.clone()).size(14);

        let mut content = column![icon_text, label_text]
            .spacing(8)
            .align_x(Alignment::Center);

        if let Some(ref sub) = self.sublabel {
            content = content.push(text(sub.clone()).size(11));
        }

        let style = tile_button_style(theme);
        let hovered = tile_button_hovered_style(theme);

        button(
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(16),
        )
        .width(120)
        .height(120)
        .style(move |_theme, status| match status {
            button::Status::Hovered | button::Status::Pressed => hovered,
            _ => style,
        })
        .on_press(on_press)
        .into()
    }
}
