use anyhow::Result;
use image::RgbaImage;

use super::{Capture, Rectangle, ScreenCapture};

pub struct RegionCapture {
    region: Rectangle,
}

impl RegionCapture {
    pub fn new(region: Rectangle) -> Self {
        Self { region }
    }

    pub fn from_coords(start_x: i32, start_y: i32, end_x: i32, end_y: i32) -> Self {
        Self {
            region: Rectangle::normalize(start_x, start_y, end_x, end_y),
        }
    }

    pub fn region(&self) -> &Rectangle {
        &self.region
    }
}

impl Capture for RegionCapture {
    fn capture(&self) -> Result<RgbaImage> {
        let full_screen = ScreenCapture::all_monitors()?;

        let src_x = self.region.x.max(0) as u32;
        let src_y = self.region.y.max(0) as u32;
        let width = self.region.width.min(full_screen.width().saturating_sub(src_x));
        let height = self.region.height.min(full_screen.height().saturating_sub(src_y));

        if width == 0 || height == 0 {
            return Ok(RgbaImage::new(1, 1));
        }

        let cropped = image::imageops::crop_imm(&full_screen, src_x, src_y, width, height).to_image();
        Ok(cropped)
    }
}
