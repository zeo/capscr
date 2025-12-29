use iced::mouse;
use iced::widget::{button, canvas, column, container, row, text, Canvas};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};
use image::RgbaImage;
use std::sync::Arc;

use crate::ui::style::{
    container_style, tile_button_hovered_style, tile_button_style, MonochromeTheme,
};
use crate::ui::Message;

#[derive(Debug, Clone)]
pub struct Stroke {
    pub points: Vec<Point>,
    pub color: Color,
    pub width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawTool {
    Pen,
    Eraser,
}

#[derive(Debug)]
pub struct EditorState {
    pub strokes: Vec<Stroke>,
    pub current_stroke: Option<Stroke>,
    pub tool: DrawTool,
    pub color: Color,
    pub stroke_width: f32,
    cache: canvas::Cache,
}

impl EditorState {
    pub fn new(_image_width: u32, _image_height: u32) -> Self {
        Self {
            strokes: Vec::new(),
            current_stroke: None,
            tool: DrawTool::Pen,
            color: Color::from_rgb(1.0, 0.0, 0.0),
            stroke_width: 3.0,
            cache: canvas::Cache::new(),
        }
    }

    pub fn start_stroke(&mut self, position: Point) {
        let color = match self.tool {
            DrawTool::Pen => self.color,
            DrawTool::Eraser => Color::WHITE,
        };
        let width = match self.tool {
            DrawTool::Pen => self.stroke_width,
            DrawTool::Eraser => self.stroke_width * 3.0,
        };
        self.current_stroke = Some(Stroke {
            points: vec![position],
            color,
            width,
        });
    }

    pub fn add_point(&mut self, position: Point) {
        if let Some(ref mut stroke) = self.current_stroke {
            stroke.points.push(position);
            self.cache.clear();
        }
    }

    pub fn end_stroke(&mut self) {
        if let Some(stroke) = self.current_stroke.take() {
            if stroke.points.len() > 1 {
                self.strokes.push(stroke);
            }
        }
        self.cache.clear();
    }

    pub fn set_tool(&mut self, tool: DrawTool) {
        self.tool = tool;
    }

    pub fn set_color(&mut self, color: Color) {
        self.color = color;
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current_stroke = None;
        self.cache.clear();
    }

    pub fn apply_to_image(&self, image: &RgbaImage) -> RgbaImage {
        let mut result = image.clone();
        let (width, height) = (result.width() as f32, result.height() as f32);

        for stroke in &self.strokes {
            draw_stroke_on_image(&mut result, stroke, width, height);
        }

        result
    }
}

fn draw_stroke_on_image(image: &mut RgbaImage, stroke: &Stroke, _width: f32, _height: f32) {
    let color = [
        (stroke.color.r * 255.0) as u8,
        (stroke.color.g * 255.0) as u8,
        (stroke.color.b * 255.0) as u8,
        255u8,
    ];

    for window in stroke.points.windows(2) {
        let p1 = window[0];
        let p2 = window[1];
        draw_line(image, p1, p2, color, stroke.width);
    }
}

fn draw_line(image: &mut RgbaImage, p1: Point, p2: Point, color: [u8; 4], width: f32) {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let dist = (dx * dx + dy * dy).sqrt();
    let steps = (dist * 2.0).max(1.0) as i32;

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = p1.x + dx * t;
        let y = p1.y + dy * t;
        draw_circle(image, x, y, width / 2.0, color);
    }
}

fn draw_circle(image: &mut RgbaImage, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let (img_width, img_height) = (image.width() as i32, image.height() as i32);
    let r = radius.ceil() as i32;
    let cx_i = cx as i32;
    let cy_i = cy as i32;

    for dy in -r..=r {
        for dx in -r..=r {
            let dist_sq = (dx * dx + dy * dy) as f32;
            if dist_sq <= radius * radius {
                let px = cx_i + dx;
                let py = cy_i + dy;
                if px >= 0 && px < img_width && py >= 0 && py < img_height {
                    image.put_pixel(px as u32, py as u32, image::Rgba(color));
                }
            }
        }
    }
}

impl canvas::Program<Message> for EditorState {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry<Renderer>> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            for stroke in &self.strokes {
                draw_stroke(frame, stroke);
            }

            if let Some(ref stroke) = self.current_stroke {
                draw_stroke(frame, stroke);
            }
        });

        vec![geometry]
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        let Some(position) = cursor.position_in(bounds) else {
            return (canvas::event::Status::Ignored, None);
        };

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                (
                    canvas::event::Status::Captured,
                    Some(Message::EditorStartStroke(position)),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                (
                    canvas::event::Status::Captured,
                    Some(Message::EditorAddPoint(position)),
                )
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                (
                    canvas::event::Status::Captured,
                    Some(Message::EditorEndStroke),
                )
            }
            _ => (canvas::event::Status::Ignored, None),
        }
    }
}

fn draw_stroke(frame: &mut canvas::Frame, stroke: &Stroke) {
    if stroke.points.len() < 2 {
        return;
    }

    let path = canvas::Path::new(|builder| {
        if let Some(first) = stroke.points.first() {
            builder.move_to(*first);
            for point in stroke.points.iter().skip(1) {
                builder.line_to(*point);
            }
        }
    });

    frame.stroke(
        &path,
        canvas::Stroke::default()
            .with_color(stroke.color)
            .with_width(stroke.width)
            .with_line_cap(canvas::LineCap::Round)
            .with_line_join(canvas::LineJoin::Round),
    );
}

pub struct EditorView;

impl EditorView {
    pub fn view<'a>(
        theme: &MonochromeTheme,
        editor: &'a EditorState,
        _image: &Arc<RgbaImage>,
    ) -> Element<'a, Message> {
        let container_bg = container_style(theme);

        let title = text("Edit Screenshot").size(18);

        let tool_buttons = row![
            Self::tool_button(theme, "Pen", DrawTool::Pen, editor.tool == DrawTool::Pen),
            Self::tool_button(
                theme,
                "Eraser",
                DrawTool::Eraser,
                editor.tool == DrawTool::Eraser
            ),
        ]
        .spacing(8);

        let color_buttons = row![
            Self::color_button(theme, Color::from_rgb(1.0, 0.0, 0.0), "Red"),
            Self::color_button(theme, Color::from_rgb(0.0, 1.0, 0.0), "Green"),
            Self::color_button(theme, Color::from_rgb(0.0, 0.0, 1.0), "Blue"),
            Self::color_button(theme, Color::from_rgb(1.0, 1.0, 0.0), "Yellow"),
            Self::color_button(theme, Color::BLACK, "Black"),
            Self::color_button(theme, Color::WHITE, "White"),
        ]
        .spacing(4);

        let tools = column![tool_buttons, color_buttons].spacing(8);

        let canvas_element: Canvas<&EditorState, Message, Theme, Renderer> =
            canvas(editor).width(Length::Fill).height(Length::Fill);

        let canvas_container = container(canvas_element)
            .width(Length::Fill)
            .height(Length::FillPortion(4))
            .padding(2)
            .style(move |_| iced::widget::container::Style {
                background: Some(iced::Background::Color(Color::from_rgb(0.2, 0.2, 0.2))),
                border: iced::Border {
                    color: Color::from_rgb(0.4, 0.4, 0.4),
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            });

        let action_buttons = row![
            Self::action_button(theme, "Clear", Message::EditorClear),
            Self::action_button(theme, "Done", Message::EditorDone),
            Self::action_button(theme, "Cancel", Message::EditorCancel),
        ]
        .spacing(8);

        let content = column![title, tools, canvas_container, action_buttons]
            .spacing(12)
            .width(Length::Fill)
            .height(Length::Fill);

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .style(move |_| container_bg)
            .into()
    }

    fn tool_button(
        theme: &MonochromeTheme,
        label: &str,
        tool: DrawTool,
        selected: bool,
    ) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);
        let label_owned = label.to_string();

        let style = if selected { hover_style } else { normal_style };

        button(text(label_owned).size(12))
            .padding([6, 12])
            .style(move |_t, status| {
                if selected
                    || matches!(status, button::Status::Hovered | button::Status::Pressed)
                {
                    hover_style
                } else {
                    style
                }
            })
            .on_press(Message::EditorSetTool(tool))
            .into()
    }

    fn color_button(
        theme: &MonochromeTheme,
        color: Color,
        _name: &str,
    ) -> Element<'static, Message> {
        let _ = theme;
        let size = 24.0;

        button(container(text("")).width(size).height(size))
            .padding(2)
            .style(move |_t, _s| button::Style {
                background: Some(iced::Background::Color(color)),
                border: iced::Border {
                    color: Color::from_rgb(0.5, 0.5, 0.5),
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            })
            .on_press(Message::EditorSetColor(color))
            .into()
    }

    fn action_button(
        theme: &MonochromeTheme,
        label: &str,
        message: Message,
    ) -> Element<'static, Message> {
        let normal_style = tile_button_style(theme);
        let hover_style = tile_button_hovered_style(theme);
        let label_owned = label.to_string();

        button(text(label_owned).size(12))
            .padding([8, 16])
            .style(move |_t, status| {
                if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                    hover_style
                } else {
                    normal_style
                }
            })
            .on_press(message)
            .into()
    }
}
