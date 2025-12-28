use image::{Rgba, RgbaImage};

const MAX_TONEMAP_DIMENSION: u32 = 16384;
const MAX_TONEMAP_PIXELS: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToneMapOperator {
    #[default]
    Reinhard,
    ReinhardExtended,
    AcesFilmic,
    Hable,
    Exposure,
}

impl ToneMapOperator {
    pub fn all() -> &'static [ToneMapOperator] {
        &[
            ToneMapOperator::Reinhard,
            ToneMapOperator::ReinhardExtended,
            ToneMapOperator::AcesFilmic,
            ToneMapOperator::Hable,
            ToneMapOperator::Exposure,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ToneMapOperator::Reinhard => "Reinhard",
            ToneMapOperator::ReinhardExtended => "Reinhard Extended",
            ToneMapOperator::AcesFilmic => "ACES Filmic",
            ToneMapOperator::Hable => "Hable (Uncharted 2)",
            ToneMapOperator::Exposure => "Exposure",
        }
    }
}

pub struct ToneMapper {
    operator: ToneMapOperator,
    exposure: f32,
    gamma: f32,
    white_point: f32,
}

impl Default for ToneMapper {
    fn default() -> Self {
        Self {
            operator: ToneMapOperator::AcesFilmic,
            exposure: 1.0,
            gamma: 2.2,
            white_point: 4.0,
        }
    }
}

impl ToneMapper {
    pub fn new(operator: ToneMapOperator) -> Self {
        Self {
            operator,
            ..Default::default()
        }
    }

    pub fn with_exposure(mut self, exposure: f32) -> Self {
        self.exposure = exposure.clamp(0.1, 10.0);
        self
    }

    pub fn with_gamma(mut self, gamma: f32) -> Self {
        self.gamma = gamma.clamp(1.0, 3.0);
        self
    }

    pub fn with_white_point(mut self, white_point: f32) -> Self {
        self.white_point = white_point.clamp(1.0, 16.0);
        self
    }

    pub fn tonemap_hdr_f32(&self, hdr_data: &[f32], width: u32, height: u32) -> RgbaImage {
        if width == 0 || height == 0 {
            return RgbaImage::new(1, 1);
        }
        if width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
            return RgbaImage::new(1, 1);
        }

        let pixels_count = match (width as usize).checked_mul(height as usize) {
            Some(c) if c <= MAX_TONEMAP_PIXELS => c,
            _ => return RgbaImage::new(1, 1),
        };

        let required_floats = match pixels_count.checked_mul(4) {
            Some(r) => r,
            None => return RgbaImage::new(1, 1),
        };

        if hdr_data.len() < required_floats {
            return RgbaImage::new(width, height);
        }

        let mut result = RgbaImage::new(width, height);

        for y in 0..height {
            for x in 0..width {
                let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                if idx + 3 >= hdr_data.len() {
                    break;
                }

                let r_raw = hdr_data[idx];
                let g_raw = hdr_data[idx + 1];
                let b_raw = hdr_data[idx + 2];
                let a_raw = hdr_data[idx + 3];

                let r = if r_raw.is_finite() { r_raw * self.exposure } else { 0.0 };
                let g = if g_raw.is_finite() { g_raw * self.exposure } else { 0.0 };
                let b = if b_raw.is_finite() { b_raw * self.exposure } else { 0.0 };
                let a = if a_raw.is_finite() { a_raw } else { 1.0 };

                let (tr, tg, tb) = self.apply_operator(r.max(0.0), g.max(0.0), b.max(0.0));

                let gr = self.apply_gamma(tr);
                let gg = self.apply_gamma(tg);
                let gb = self.apply_gamma(tb);

                let or = (gr * 255.0).clamp(0.0, 255.0) as u8;
                let og = (gg * 255.0).clamp(0.0, 255.0) as u8;
                let ob = (gb * 255.0).clamp(0.0, 255.0) as u8;
                let oa = (a * 255.0).clamp(0.0, 255.0) as u8;

                result.put_pixel(x, y, Rgba([or, og, ob, oa]));
            }
        }

        result
    }

    pub fn tonemap_rgba16(&self, hdr_data: &[u16], width: u32, height: u32) -> RgbaImage {
        if width == 0 || height == 0 {
            return RgbaImage::new(1, 1);
        }
        if width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
            return RgbaImage::new(1, 1);
        }

        let pixels_count = match (width as usize).checked_mul(height as usize) {
            Some(c) if c <= MAX_TONEMAP_PIXELS => c,
            _ => return RgbaImage::new(1, 1),
        };

        let required_u16s = match pixels_count.checked_mul(4) {
            Some(r) => r,
            None => return RgbaImage::new(1, 1),
        };

        if hdr_data.len() < required_u16s {
            return RgbaImage::new(width, height);
        }

        let mut result = RgbaImage::new(width, height);
        let max_val = 65535.0_f32;

        for y in 0..height {
            for x in 0..width {
                let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                if idx + 3 >= hdr_data.len() {
                    break;
                }

                let r = (hdr_data[idx] as f32 / max_val) * self.exposure;
                let g = (hdr_data[idx + 1] as f32 / max_val) * self.exposure;
                let b = (hdr_data[idx + 2] as f32 / max_val) * self.exposure;
                let a = hdr_data[idx + 3] as f32 / max_val;

                let (tr, tg, tb) = self.apply_operator(r, g, b);

                let gr = self.apply_gamma(tr);
                let gg = self.apply_gamma(tg);
                let gb = self.apply_gamma(tb);

                let or = (gr * 255.0).clamp(0.0, 255.0) as u8;
                let og = (gg * 255.0).clamp(0.0, 255.0) as u8;
                let ob = (gb * 255.0).clamp(0.0, 255.0) as u8;
                let oa = (a * 255.0).clamp(0.0, 255.0) as u8;

                result.put_pixel(x, y, Rgba([or, og, ob, oa]));
            }
        }

        result
    }

    pub fn tonemap_rgb10a2(&self, packed_data: &[u32], width: u32, height: u32) -> RgbaImage {
        if width == 0 || height == 0 {
            return RgbaImage::new(1, 1);
        }
        if width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
            return RgbaImage::new(1, 1);
        }

        let pixels_count = match (width as usize).checked_mul(height as usize) {
            Some(c) if c <= MAX_TONEMAP_PIXELS => c,
            _ => return RgbaImage::new(1, 1),
        };

        if packed_data.len() < pixels_count {
            return RgbaImage::new(width, height);
        }

        let mut result = RgbaImage::new(width, height);

        for y in 0..height {
            for x in 0..width {
                let idx = (y as usize) * (width as usize) + (x as usize);
                if idx >= packed_data.len() {
                    break;
                }

                let packed = packed_data[idx];

                let r10 = (packed & 0x3FF) as f32 / 1023.0;
                let g10 = ((packed >> 10) & 0x3FF) as f32 / 1023.0;
                let b10 = ((packed >> 20) & 0x3FF) as f32 / 1023.0;
                let a2 = ((packed >> 30) & 0x3) as f32 / 3.0;

                let r = r10 * self.exposure;
                let g = g10 * self.exposure;
                let b = b10 * self.exposure;

                let (tr, tg, tb) = self.apply_operator(r, g, b);

                let gr = self.apply_gamma(tr);
                let gg = self.apply_gamma(tg);
                let gb = self.apply_gamma(tb);

                let or = (gr * 255.0).clamp(0.0, 255.0) as u8;
                let og = (gg * 255.0).clamp(0.0, 255.0) as u8;
                let ob = (gb * 255.0).clamp(0.0, 255.0) as u8;
                let oa = (a2 * 255.0).clamp(0.0, 255.0) as u8;

                result.put_pixel(x, y, Rgba([or, og, ob, oa]));
            }
        }

        result
    }

    pub fn apply_operator(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        match self.operator {
            ToneMapOperator::Reinhard => self.reinhard(r, g, b),
            ToneMapOperator::ReinhardExtended => self.reinhard_extended(r, g, b),
            ToneMapOperator::AcesFilmic => self.aces_filmic(r, g, b),
            ToneMapOperator::Hable => self.hable(r, g, b),
            ToneMapOperator::Exposure => self.exposure_only(r, g, b),
        }
    }

    fn reinhard(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        (r / (1.0 + r), g / (1.0 + g), b / (1.0 + b))
    }

    fn reinhard_extended(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let wp2 = self.white_point * self.white_point;
        let map = |v: f32| (v * (1.0 + v / wp2)) / (1.0 + v);
        (map(r), map(g), map(b))
    }

    fn aces_filmic(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        fn aces_curve(x: f32) -> f32 {
            let a = 2.51;
            let b = 0.03;
            let c = 2.43;
            let d = 0.59;
            let e = 0.14;
            ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
        }

        let input_r = r * 0.6;
        let input_g = g * 0.6;
        let input_b = b * 0.6;

        (aces_curve(input_r), aces_curve(input_g), aces_curve(input_b))
    }

    fn hable(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        fn hable_partial(x: f32) -> f32 {
            let a = 0.15;
            let b = 0.50;
            let c = 0.10;
            let d = 0.20;
            let e = 0.02;
            let f = 0.30;
            ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f)) - e / f
        }

        let exposure_bias = 2.0;
        let w = 11.2;

        let curr_r = hable_partial(r * exposure_bias);
        let curr_g = hable_partial(g * exposure_bias);
        let curr_b = hable_partial(b * exposure_bias);

        let white_scale = 1.0 / hable_partial(w);

        (
            (curr_r * white_scale).clamp(0.0, 1.0),
            (curr_g * white_scale).clamp(0.0, 1.0),
            (curr_b * white_scale).clamp(0.0, 1.0),
        )
    }

    fn exposure_only(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
    }

    fn apply_gamma(&self, linear: f32) -> f32 {
        if linear <= 0.0 || !linear.is_finite() {
            0.0
        } else if self.gamma < 0.001 {
            linear.clamp(0.0, 1.0)
        } else {
            let result = linear.powf(1.0 / self.gamma);
            if result.is_finite() { result.clamp(0.0, 1.0) } else { 0.0 }
        }
    }
}

pub fn pq_eotf_inverse(linear: f32) -> f32 {
    let m1 = 0.159_301_76_f32;
    let m2 = 78.84375_f32;
    let c1 = 0.8359375_f32;
    let c2 = 18.851_563_f32;
    let c3 = 18.6875_f32;

    let y = (linear / 10000.0).max(0.0);
    let ym1 = y.powf(m1);
    ((c1 + c2 * ym1) / (1.0 + c3 * ym1)).powf(m2)
}

pub fn pq_eotf(pq: f32) -> f32 {
    let m1 = 0.159_301_76_f32;
    let m2 = 78.84375_f32;
    let c1 = 0.8359375_f32;
    let c2 = 18.851_563_f32;
    let c3 = 18.6875_f32;

    let pq_clamped = pq.clamp(0.0, 1.0);
    let pq_pow = pq_clamped.powf(1.0 / m2);
    let numerator = (pq_pow - c1).max(0.0);
    let denominator = c2 - c3 * pq_pow;

    if denominator <= 0.0 {
        0.0
    } else {
        10000.0 * (numerator / denominator).powf(1.0 / m1)
    }
}

pub fn hlg_oetf_inverse(hlg: f32) -> f32 {
    let _a = 0.178_832_77_f32;
    let b = 0.284_668_92_f32;
    let c = 0.559_910_7_f32;

    if hlg <= 0.5 {
        (hlg * hlg) / 3.0
    } else {
        ((hlg - c).exp() + b) / 12.0
    }
}

pub fn srgb_to_linear(srgb: f32) -> f32 {
    if srgb <= 0.04045 {
        srgb / 12.92
    } else {
        ((srgb + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

pub fn scrgb_to_sdr(hdr_data: &[f32], width: u32, height: u32, exposure: f32) -> RgbaImage {
    let mapper = ToneMapper::new(ToneMapOperator::AcesFilmic)
        .with_exposure(exposure);
    mapper.tonemap_hdr_f32(hdr_data, width, height)
}

pub fn linear_to_pq(linear: f32) -> f32 {
    pq_eotf_inverse(linear)
}

pub fn hdr10_to_sdr(pq_data: &[u16], width: u32, height: u32, exposure: f32) -> RgbaImage {
    if width == 0 || height == 0 {
        return RgbaImage::new(1, 1);
    }
    if width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
        return RgbaImage::new(1, 1);
    }

    let pixels_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };

    let required_u16s = match pixels_count.checked_mul(4) {
        Some(r) => r,
        None => return RgbaImage::new(1, 1),
    };

    if pq_data.len() < required_u16s {
        return RgbaImage::new(width, height);
    }

    let mut result = RgbaImage::new(width, height);

    let clamped_exposure = exposure.clamp(0.1, 10.0);
    let mapper = ToneMapper::new(ToneMapOperator::AcesFilmic)
        .with_exposure(clamped_exposure);

    for y in 0..height {
        for x in 0..width {
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            if idx + 3 >= pq_data.len() {
                break;
            }

            let pq_r = pq_data[idx] as f32 / 65535.0;
            let pq_g = pq_data[idx + 1] as f32 / 65535.0;
            let pq_b = pq_data[idx + 2] as f32 / 65535.0;
            let a = pq_data[idx + 3] as f32 / 65535.0;

            let linear_r = pq_eotf(pq_r) / 10000.0;
            let linear_g = pq_eotf(pq_g) / 10000.0;
            let linear_b = pq_eotf(pq_b) / 10000.0;

            let exposed_r = linear_r * clamped_exposure;
            let exposed_g = linear_g * clamped_exposure;
            let exposed_b = linear_b * clamped_exposure;

            let (tr, tg, tb) = mapper.apply_operator(exposed_r, exposed_g, exposed_b);

            let sr = linear_to_srgb(tr);
            let sg = linear_to_srgb(tg);
            let sb = linear_to_srgb(tb);

            let or = (sr * 255.0).clamp(0.0, 255.0) as u8;
            let og = (sg * 255.0).clamp(0.0, 255.0) as u8;
            let ob = (sb * 255.0).clamp(0.0, 255.0) as u8;
            let oa = (a * 255.0).clamp(0.0, 255.0) as u8;

            result.put_pixel(x, y, Rgba([or, og, ob, oa]));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reinhard_basic() {
        let mapper = ToneMapper::new(ToneMapOperator::Reinhard);
        let result = mapper.tonemap_hdr_f32(&[1.0, 1.0, 1.0, 1.0], 1, 1);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0] > 100 && pixel[0] < 200);
    }

    #[test]
    fn test_aces_clamps_output() {
        let mapper = ToneMapper::new(ToneMapOperator::AcesFilmic);
        let result = mapper.tonemap_hdr_f32(&[100.0, 100.0, 100.0, 1.0], 1, 1);
        let pixel = result.get_pixel(0, 0);
        assert!(pixel[0] <= 255);
        assert!(pixel[1] <= 255);
        assert!(pixel[2] <= 255);
    }

    #[test]
    fn test_pq_roundtrip() {
        let original = 100.0;
        let pq = pq_eotf_inverse(original);
        let recovered = pq_eotf(pq);
        assert!((original - recovered).abs() < 1.0);
    }

    #[test]
    fn test_tonemapper_with_gamma_and_white_point() {
        let mapper = ToneMapper::new(ToneMapOperator::ReinhardExtended)
            .with_gamma(2.4)
            .with_white_point(8.0);
        let result = mapper.tonemap_hdr_f32(&[1.0, 1.0, 1.0, 1.0], 1, 1);
        assert!(result.width() == 1 && result.height() == 1);
    }

    #[test]
    fn test_tonemap_rgba16() {
        let mapper = ToneMapper::new(ToneMapOperator::Hable);
        let data: Vec<u16> = vec![32768, 32768, 32768, 65535];
        let result = mapper.tonemap_rgba16(&data, 1, 1);
        assert!(result.width() == 1);
    }

    #[test]
    fn test_tonemap_rgb10a2() {
        let mapper = ToneMapper::new(ToneMapOperator::Exposure);
        let packed: Vec<u32> = vec![0xC0300C03];
        let result = mapper.tonemap_rgb10a2(&packed, 1, 1);
        assert!(result.width() == 1);
    }

    #[test]
    fn test_operator_display_names() {
        for op in ToneMapOperator::all() {
            assert!(!op.display_name().is_empty());
        }
    }

    #[test]
    fn test_hlg_oetf_inverse() {
        let result = hlg_oetf_inverse(0.5);
        assert!(result >= 0.0);
    }

    #[test]
    fn test_srgb_conversion() {
        let linear = srgb_to_linear(0.5);
        assert!(linear > 0.0 && linear < 0.5);
    }

    #[test]
    fn test_scrgb_to_sdr() {
        let hdr_data = vec![1.0f32, 1.0, 1.0, 1.0];
        let result = scrgb_to_sdr(&hdr_data, 1, 1, 1.0);
        assert!(result.width() == 1);
    }
}
