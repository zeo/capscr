use iced::widget::{button, container};
use iced::{Border, Color};

pub const BACKGROUND_DARK: Color = Color::from_rgb(0.1, 0.1, 0.1);
pub const BACKGROUND_LIGHT: Color = Color::from_rgb(0.95, 0.95, 0.95);
pub const SURFACE_DARK: Color = Color::from_rgb(0.15, 0.15, 0.15);
pub const SURFACE_LIGHT: Color = Color::from_rgb(0.9, 0.9, 0.9);
pub const TILE_DARK: Color = Color::from_rgb(0.2, 0.2, 0.2);
pub const TILE_LIGHT: Color = Color::from_rgb(0.85, 0.85, 0.85);
pub const ACCENT_DARK: Color = Color::from_rgb(0.4, 0.4, 0.4);
pub const ACCENT_LIGHT: Color = Color::from_rgb(0.3, 0.3, 0.3);
pub const TEXT_DARK: Color = Color::from_rgb(0.9, 0.9, 0.9);
pub const TEXT_LIGHT: Color = Color::from_rgb(0.1, 0.1, 0.1);
pub const HOVER_DARK: Color = Color::from_rgb(0.25, 0.25, 0.25);
pub const HOVER_LIGHT: Color = Color::from_rgb(0.75, 0.75, 0.75);
pub const BORDER_RADIUS: f32 = 12.0;
pub const SMALL_RADIUS: f32 = 8.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct MonochromeTheme {
    pub is_dark: bool,
}

impl MonochromeTheme {
    pub fn dark() -> Self {
        Self { is_dark: true }
    }

    pub fn light() -> Self {
        Self { is_dark: false }
    }

    pub fn background(&self) -> Color {
        if self.is_dark {
            BACKGROUND_DARK
        } else {
            BACKGROUND_LIGHT
        }
    }

    pub fn surface(&self) -> Color {
        if self.is_dark {
            SURFACE_DARK
        } else {
            SURFACE_LIGHT
        }
    }

    pub fn tile(&self) -> Color {
        if self.is_dark {
            TILE_DARK
        } else {
            TILE_LIGHT
        }
    }

    pub fn accent(&self) -> Color {
        if self.is_dark {
            ACCENT_DARK
        } else {
            ACCENT_LIGHT
        }
    }

    pub fn text(&self) -> Color {
        if self.is_dark {
            TEXT_DARK
        } else {
            TEXT_LIGHT
        }
    }

    pub fn hover(&self) -> Color {
        if self.is_dark {
            HOVER_DARK
        } else {
            HOVER_LIGHT
        }
    }
}

pub fn tile_button_style(theme: &MonochromeTheme) -> button::Style {
    let bg = theme.tile();
    let text = theme.text();

    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: text,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: BORDER_RADIUS.into(),
        },
        shadow: iced::Shadow::default(),
    }
}

pub fn tile_button_hovered_style(theme: &MonochromeTheme) -> button::Style {
    let bg = theme.hover();
    let text = theme.text();

    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: text,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: BORDER_RADIUS.into(),
        },
        shadow: iced::Shadow::default(),
    }
}

pub fn primary_button_style(theme: &MonochromeTheme) -> button::Style {
    let bg = theme.accent();
    let text = if theme.is_dark {
        TEXT_DARK
    } else {
        Color::WHITE
    };

    button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: text,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: SMALL_RADIUS.into(),
        },
        shadow: iced::Shadow::default(),
    }
}

pub fn container_style(theme: &MonochromeTheme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.background())),
        text_color: Some(theme.text()),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        shadow: iced::Shadow::default(),
    }
}

pub fn surface_container_style(theme: &MonochromeTheme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.surface())),
        text_color: Some(theme.text()),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: BORDER_RADIUS.into(),
        },
        shadow: iced::Shadow::default(),
    }
}

pub fn tile_container_style(theme: &MonochromeTheme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.tile())),
        text_color: Some(theme.text()),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: BORDER_RADIUS.into(),
        },
        shadow: iced::Shadow::default(),
    }
}
