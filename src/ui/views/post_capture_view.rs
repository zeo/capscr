use iced::widget::{button, column, container, horizontal_space, row, text};
use iced::{Alignment, Element, Length};

use crate::config::PostCaptureAction;
use crate::ui::style::{
    container_style, tile_button_hovered_style, tile_button_style, MonochromeTheme,
};
use crate::ui::Message;

pub struct PostCaptureView;

impl PostCaptureView {
    pub fn view(theme: &MonochromeTheme) -> Element<'static, Message> {
        let container_bg = container_style(theme);

        let title = text("Capture Complete").size(24);
        let subtitle = text("What would you like to do?").size(14);

        let header = column![title, subtitle].spacing(8);

        let actions = column![
            Self::action_button(theme, "Save to file", PostCaptureAction::SaveToFile),
            Self::action_button(theme, "Copy to clipboard", PostCaptureAction::CopyToClipboard),
            Self::action_button(theme, "Save and copy", PostCaptureAction::SaveAndCopy),
            Self::action_button(theme, "Upload", PostCaptureAction::Upload),
            Self::save_as_button(theme),
            Self::quick_copy_button(theme),
            Self::edit_button(theme),
        ]
        .spacing(8);

        let cancel_style = tile_button_style(theme);
        let cancel_btn = button(text("Cancel").size(12))
            .padding([6, 12])
            .style(move |_t, _s| cancel_style)
            .on_press(Message::DismissPostCapture);

        let footer = row![horizontal_space(), cancel_btn].align_y(Alignment::Center);

        let main_content = column![header, actions, footer]
            .spacing(20)
            .width(Length::Fill);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .style(move |_| container_bg)
            .into()
    }

    fn action_button(
        theme: &MonochromeTheme,
        label: &str,
        action: PostCaptureAction,
    ) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);
        let label_owned = label.to_string();

        button(
            container(text(label_owned).size(14))
                .width(Length::Fill)
                .padding(12)
                .center_x(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_t, status| {
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                hover_style
            } else {
                normal_style
            }
        })
        .on_press(Message::PostCaptureAction(action))
        .into()
    }

    fn save_as_button(theme: &MonochromeTheme) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);

        button(
            container(text("Save as...").size(14))
                .width(Length::Fill)
                .padding(12)
                .center_x(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_t, status| {
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                hover_style
            } else {
                normal_style
            }
        })
        .on_press(Message::SaveAs)
        .into()
    }

    fn quick_copy_button(theme: &MonochromeTheme) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);

        button(
            container(text("Quick copy to clipboard").size(14))
                .width(Length::Fill)
                .padding(12)
                .center_x(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_t, status| {
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                hover_style
            } else {
                normal_style
            }
        })
        .on_press(Message::CopyToClipboard)
        .into()
    }

    fn edit_button(theme: &MonochromeTheme) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);

        button(
            container(text("Edit / Draw").size(14))
                .width(Length::Fill)
                .padding(12)
                .center_x(Length::Fill),
        )
        .width(Length::Fill)
        .style(move |_t, status| {
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                hover_style
            } else {
                normal_style
            }
        })
        .on_press(Message::OpenEditor)
        .into()
    }
}
