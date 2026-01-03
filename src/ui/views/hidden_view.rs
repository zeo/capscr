use iced::{widget::text, Element};

use crate::recording::RecordingState;
use crate::ui::style::MonochromeTheme;
use crate::ui::Message;

pub struct HiddenView;

impl HiddenView {
    pub fn view(_theme: &MonochromeTheme, recording_state: RecordingState) -> Element<'static, Message> {
        let status = match recording_state {
            RecordingState::Idle => "Ready",
            RecordingState::Recording => "Recording...",
            RecordingState::Processing => "Processing...",
        };
        text(status).into()
    }
}
