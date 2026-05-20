// HDR -> SDR tonemap pipeline.
//
// The `*_skiv` functions and the supporting ICtCp / PQ / tonemap-rolloff
// helpers are a Rust port of the SKIV (Special K Image Viewer) tonemap by
// Andon "Kaldaien" Coleman, MIT-licensed:
//   https://github.com/SpecialKO/SKIV
// Specifically: PostProcessingColor.hlsl, tone_mapping.hlsli, and
// colorspaces.hlsli, together with the per-frame ImageInfo / MaxCLL
// computation adapted from GotoFinal's open-source HDR tonemap
// reference (MIT-licensed).
//
// The Reinhard path below is kept only as a reference / fallback and is
// unused by the runtime pipeline.

use image::{Rgba, RgbaImage};

const MAX_TONEMAP_DIMENSION: u32 = 16384;
const MAX_TONEMAP_PIXELS: usize = 256 * 1024 * 1024;

const PQ_N: f32 = 2610.0 / 4096.0 / 4.0;
const PQ_M: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

// scRGB units where 1.0 = 80 nits, so 125 scRGB units = 10000 nits = PQ ceiling
const PQ_MAX_SCRGB: f32 = 125.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TonemapAlgorithm {
    Skiv,
    Reinhard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkivMode {
    NormalizeToCll,
    MapCllToDisplay,
}

#[derive(Debug, Clone, Copy)]
pub struct SkivParams {
    pub mode: SkivMode,
    pub sdr_brightness_nits: f32,
    pub user_brightness_scale: f32,
    pub use_p99_max_cll: bool,
}

impl Default for SkivParams {
    fn default() -> Self {
        Self {
            mode: SkivMode::MapCllToDisplay,
            sdr_brightness_nits: 80.0,
            user_brightness_scale: 1.0,
            use_p99_max_cll: true,
        }
    }
}

fn reinhard(v: f32) -> f32 {
    v / (1.0 + v)
}

pub fn pq_to_linear(pq: f32) -> f32 {
    let pq_clamped = pq.clamp(0.0, 1.0);
    let pq_pow = pq_clamped.powf(1.0 / PQ_M);
    let numerator = (pq_pow - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * pq_pow;
    if denominator <= 0.0 {
        0.0
    } else {
        10000.0 * (numerator / denominator).powf(1.0 / PQ_N)
    }
}

pub fn hlg_to_linear(hlg: f32) -> f32 {
    let b: f32 = 0.28466892;
    let c: f32 = 0.5599107;
    if hlg <= 0.5 {
        (hlg * hlg) / 3.0
    } else {
        ((hlg - c).exp() + b) / 12.0
    }
}

fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_to_pq_scaled(x: f32, max_value: f32) -> f32 {
    let s = if x >= 0.0 { 1.0 } else { -1.0 };
    let abs_x = x.abs();
    let xn = (abs_x / max_value).powf(PQ_N);
    let nd = (PQ_C1 + PQ_C2 * xn) / (1.0 + PQ_C3 * xn);
    s * nd.powf(PQ_M)
}

fn pq_to_linear_scaled(x: f32, max_value: f32) -> f32 {
    let s = if x >= 0.0 { 1.0 } else { -1.0 };
    let abs_x = x.abs();
    let xm = abs_x.powf(1.0 / PQ_M);
    let denom = PQ_C2 - PQ_C3 * xm;
    if denom <= 0.0 {
        return 0.0;
    }
    let nd = (xm - PQ_C1).max(0.0) / denom;
    s * nd.powf(1.0 / PQ_N) * max_value
}

fn rec709_to_xyz(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let x = 0.412_390_8 * r + 0.357_584_3 * g + 0.180_480_8 * b;
    let y = 0.212_639 * r + 0.715_168_7 * g + 0.072_192_3 * b;
    let z = 0.019_330_8 * r + 0.119_194_8 * g + 0.950_532_1 * b;
    (x, y, z)
}

fn xyz_to_rec709(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let r = 3.240_97 * x - 1.537_383_2 * y - 0.498_610_8 * z;
    let g = -0.969_243_6 * x + 1.875_967_5 * y + 0.041_555_1 * z;
    let b = 0.055_630_1 * x - 0.203_977 * y + 1.056_971_5 * z;
    (r, g, b)
}

fn xyz_to_lms(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let l = 0.3592 * x + 0.6976 * y - 0.0358 * z;
    let m = -0.1922 * x + 1.1004 * y + 0.0755 * z;
    let s = 0.0070 * x + 0.0749 * y + 0.8434 * z;
    (l, m, s)
}

fn lms_to_xyz(l: f32, m: f32, s: f32) -> (f32, f32, f32) {
    let x = 2.070_180_1 * l - 1.326_456_9 * m + 0.206_616 * s;
    let y = 0.364_988_2 * l + 0.680_467_4 * m - 0.045_421_8 * s;
    let z = -0.049_595_5 * l - 0.049_421_2 * m + 1.187_995_9 * s;
    (x, y, z)
}

fn rec709_to_ictcp(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (x, y, z) = rec709_to_xyz(r.max(0.0), g.max(0.0), b.max(0.0));
    let (l, m, s) = xyz_to_lms(x, y, z);
    let lp = linear_to_pq_scaled(l.max(0.0), PQ_MAX_SCRGB);
    let mp = linear_to_pq_scaled(m.max(0.0), PQ_MAX_SCRGB);
    let sp = linear_to_pq_scaled(s.max(0.0), PQ_MAX_SCRGB);
    let i = 0.5 * lp + 0.5 * mp;
    let ct = 1.6137 * lp - 3.3234 * mp + 1.7097 * sp;
    let cp = 4.3780 * lp - 4.2455 * mp - 0.1325 * sp;
    (i, ct, cp)
}

fn ictcp_to_rec709(i: f32, ct: f32, cp: f32) -> (f32, f32, f32) {
    let lp = i + 0.008_605_146 * ct + 0.111_035_6 * cp;
    let mp = i - 0.008_605_146 * ct - 0.111_035_6 * cp;
    let sp = i + 0.560_048_9 * ct - 0.320_637_47 * cp;
    let l = pq_to_linear_scaled(lp, PQ_MAX_SCRGB);
    let m = pq_to_linear_scaled(mp, PQ_MAX_SCRGB);
    let s = pq_to_linear_scaled(sp, PQ_MAX_SCRGB);
    let (x, y, z) = lms_to_xyz(l, m, s);
    xyz_to_rec709(x, y, z)
}

fn tonemap_sdr_skiv(l: f32, lc: f32) -> f32 {
    if lc <= f32::EPSILON {
        return l;
    }
    (l + l * l / (lc * lc)) / (1.0 + l)
}

fn tonemap_hdr_skiv(l: f32, lc: f32, ld: f32) -> f32 {
    if lc <= f32::EPSILON || ld <= f32::EPSILON {
        return l;
    }
    let a = ld / (lc * lc);
    let b = 1.0 / ld;
    l * (1.0 + a * l) / (1.0 + b * l)
}

fn pq_y(scrgb_value: f32) -> f32 {
    linear_to_pq_scaled(scrgb_value.max(0.0), PQ_MAX_SCRGB)
}

pub fn compute_max_cll_scrgb(scrgb_rgba: &[f32], use_p99: bool) -> f32 {
    let mut samples: Vec<f32> = Vec::with_capacity(scrgb_rgba.len() / 4);
    for chunk in scrgb_rgba.chunks_exact(4) {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        if !r.is_finite() || !g.is_finite() || !b.is_finite() {
            continue;
        }
        let m = r.max(g).max(b).max(0.0);
        samples.push(m);
    }
    if samples.is_empty() {
        return 1.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = if use_p99 {
        ((samples.len() as f32 - 1.0) * 0.99).round() as usize
    } else {
        samples.len() - 1
    };
    samples[idx.min(samples.len() - 1)].max(1.0)
}

fn tonemap_pixel_skiv(
    r_in: f32,
    g_in: f32,
    b_in: f32,
    cml: f32,
    dml: f32,
    mode: SkivMode,
) -> (f32, f32, f32) {
    let (i_pq, ct, cp) = rec709_to_ictcp(r_in, g_in, b_in);
    let i_in = i_pq.max(0.0);
    let i_out = match mode {
        SkivMode::NormalizeToCll => tonemap_sdr_skiv(i_in, cml),
        SkivMode::MapCllToDisplay => tonemap_hdr_skiv(i_in, cml, dml),
    };
    let (i_final, ct_final, cp_final) = if i_in > 0.0 && i_out > 0.0 {
        let i_scale = (i_in / i_out).min(i_out / i_in);
        (i_out, ct * i_scale, cp * i_scale)
    } else {
        (0.0, 0.0, 0.0)
    };
    ictcp_to_rec709(i_final, ct_final, cp_final)
}

pub fn scrgb_to_sdr_skiv(
    scrgb_rgba: &[f32],
    width: u32,
    height: u32,
    params: SkivParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION
    {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if scrgb_rgba.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }
    let scrgb = &scrgb_rgba[..pixel_count * 4];

    let display_max_scrgb = (params.sdr_brightness_nits / 80.0).max(1.0);
    let dml = pq_y(display_max_scrgb);

    let max_cll_raw = compute_max_cll_scrgb(scrgb, params.use_p99_max_cll).min(PQ_MAX_SCRGB);
    let max_cll_floored = max_cll_raw.max(display_max_scrgb);
    let cml = pq_y(max_cll_floored);

    let brightness = if params.user_brightness_scale > 0.0 {
        params.user_brightness_scale
    } else {
        1.0
    };

    let mut out = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let r_raw = scrgb[idx];
            let g_raw = scrgb[idx + 1];
            let b_raw = scrgb[idx + 2];
            let a_raw = scrgb[idx + 3];

            let r = if r_raw.is_finite() { r_raw * brightness } else { 0.0 };
            let g = if g_raw.is_finite() { g_raw * brightness } else { 0.0 };
            let b = if b_raw.is_finite() { b_raw * brightness } else { 0.0 };
            let a = if a_raw.is_finite() { a_raw.clamp(0.0, 1.0) } else { 1.0 };

            let (r_out, g_out, b_out) = tonemap_pixel_skiv(r, g, b, cml, dml, params.mode);

            let r_u = (linear_to_srgb(r_out.clamp(0.0, 1.0)) * 255.0).clamp(0.0, 255.0) as u8;
            let g_u = (linear_to_srgb(g_out.clamp(0.0, 1.0)) * 255.0).clamp(0.0, 255.0) as u8;
            let b_u = (linear_to_srgb(b_out.clamp(0.0, 1.0)) * 255.0).clamp(0.0, 255.0) as u8;
            let a_u = (a * 255.0).clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([r_u, g_u, b_u, a_u]));
        }
    }
    out
}

pub fn hdr10_to_sdr_skiv(
    pq_data: &[u16],
    width: u32,
    height: u32,
    params: SkivParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION
    {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if pq_data.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }

    let mut scrgb = vec![0.0f32; pixel_count * 4];
    for i in 0..pixel_count {
        let r_pq = pq_data[i * 4] as f32 / 65535.0;
        let g_pq = pq_data[i * 4 + 1] as f32 / 65535.0;
        let b_pq = pq_data[i * 4 + 2] as f32 / 65535.0;
        let a_pq = pq_data[i * 4 + 3] as f32 / 65535.0;
        scrgb[i * 4] = pq_to_linear(r_pq) / 80.0;
        scrgb[i * 4 + 1] = pq_to_linear(g_pq) / 80.0;
        scrgb[i * 4 + 2] = pq_to_linear(b_pq) / 80.0;
        scrgb[i * 4 + 3] = a_pq;
    }

    scrgb_to_sdr_skiv(&scrgb, width, height, params)
}

pub fn hlg_to_sdr_skiv(
    hlg_data: &[u8],
    width: u32,
    height: u32,
    params: SkivParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION
    {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if hlg_data.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }

    // HLG reference white is 75% signal -> 100 nits-ish. scale to scRGB (80 nits/unit).
    let mut scrgb = vec![0.0f32; pixel_count * 4];
    for i in 0..pixel_count {
        let r_hlg = hlg_data[i * 4] as f32 / 255.0;
        let g_hlg = hlg_data[i * 4 + 1] as f32 / 255.0;
        let b_hlg = hlg_data[i * 4 + 2] as f32 / 255.0;
        let a_hlg = hlg_data[i * 4 + 3] as f32 / 255.0;
        let r_lin = hlg_to_linear(r_hlg) * 12.0;
        let g_lin = hlg_to_linear(g_hlg) * 12.0;
        let b_lin = hlg_to_linear(b_hlg) * 12.0;
        scrgb[i * 4] = r_lin;
        scrgb[i * 4 + 1] = g_lin;
        scrgb[i * 4 + 2] = b_lin;
        scrgb[i * 4 + 3] = a_hlg;
    }

    scrgb_to_sdr_skiv(&scrgb, width, height, params)
}

pub fn scrgb_to_sdr(scrgb_rgba: &[f32], width: u32, height: u32, sdr_white_level: f32) -> RgbaImage {
    let params = SkivParams {
        sdr_brightness_nits: sdr_white_level.max(80.0),
        ..SkivParams::default()
    };
    scrgb_to_sdr_skiv(scrgb_rgba, width, height, params)
}

pub fn hdr10_to_sdr(pq_data: &[u16], width: u32, height: u32, sdr_white_level: f32) -> RgbaImage {
    let params = SkivParams {
        sdr_brightness_nits: sdr_white_level.max(80.0),
        ..SkivParams::default()
    };
    hdr10_to_sdr_skiv(pq_data, width, height, params)
}

pub fn hlg_to_sdr(hlg_data: &[u8], width: u32, height: u32, sdr_white_level: f32) -> RgbaImage {
    let params = SkivParams {
        sdr_brightness_nits: sdr_white_level.max(80.0),
        ..SkivParams::default()
    };
    hlg_to_sdr_skiv(hlg_data, width, height, params)
}

pub fn scrgb_to_sdr_reinhard(
    hdr_data: &[f32],
    width: u32,
    height: u32,
    sdr_white_level: f32,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION
    {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if hdr_data.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }

    let mut result = RgbaImage::new(width, height);
    let white_scale = 80.0 / sdr_white_level.max(80.0);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let r = hdr_data[idx];
            let g = hdr_data[idx + 1];
            let b = hdr_data[idx + 2];
            let a = hdr_data[idx + 3];

            let r_scaled = if r.is_finite() { r * white_scale } else { 0.0 };
            let g_scaled = if g.is_finite() { g * white_scale } else { 0.0 };
            let b_scaled = if b.is_finite() { b * white_scale } else { 0.0 };

            let r_tm = reinhard(r_scaled.max(0.0));
            let g_tm = reinhard(g_scaled.max(0.0));
            let b_tm = reinhard(b_scaled.max(0.0));

            let r_out = (linear_to_srgb(r_tm) * 255.0).clamp(0.0, 255.0) as u8;
            let g_out = (linear_to_srgb(g_tm) * 255.0).clamp(0.0, 255.0) as u8;
            let b_out = (linear_to_srgb(b_tm) * 255.0).clamp(0.0, 255.0) as u8;
            let a_out = if a.is_finite() {
                (a * 255.0).clamp(0.0, 255.0) as u8
            } else {
                255
            };
            result.put_pixel(x, y, Rgba([r_out, g_out, b_out, a_out]));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reinhard_basic() {
        assert!((reinhard(1.0) - 0.5).abs() < 0.001);
        assert!((reinhard(0.0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn pq_roundtrip_is_stable() {
        for nits_scrgb in [0.5_f32, 1.0, 2.5, 10.0, 50.0, 100.0, 124.0] {
            let pq = linear_to_pq_scaled(nits_scrgb, PQ_MAX_SCRGB);
            let back = pq_to_linear_scaled(pq, PQ_MAX_SCRGB);
            assert!(
                (back - nits_scrgb).abs() < 0.01,
                "roundtrip {nits_scrgb} -> {pq} -> {back}"
            );
        }
    }

    #[test]
    fn pq_max_value_maps_to_one() {
        let pq = linear_to_pq_scaled(PQ_MAX_SCRGB, PQ_MAX_SCRGB);
        assert!((pq - 1.0).abs() < 1e-4);
    }

    #[test]
    fn ictcp_roundtrip_preserves_grey() {
        let (r, g, b) = (0.5, 0.5, 0.5);
        let (i, ct, cp) = rec709_to_ictcp(r, g, b);
        let (r2, g2, b2) = ictcp_to_rec709(i, ct, cp);
        assert!((r2 - r).abs() < 0.01, "r: {r} -> {r2}");
        assert!((g2 - g).abs() < 0.01, "g: {g} -> {g2}");
        assert!((b2 - b).abs() < 0.01, "b: {b} -> {b2}");
    }

    #[test]
    fn ictcp_roundtrip_preserves_red() {
        let (r, g, b) = (1.5, 0.1, 0.1);
        let (i, ct, cp) = rec709_to_ictcp(r, g, b);
        let (r2, g2, b2) = ictcp_to_rec709(i, ct, cp);
        assert!((r2 - r).abs() < 0.05);
        assert!((g2 - g).abs() < 0.05);
        assert!((b2 - b).abs() < 0.05);
    }

    #[test]
    fn skiv_tonemap_does_not_panic_on_black() {
        let data = vec![0.0_f32; 16];
        let img = scrgb_to_sdr_skiv(&data, 2, 2, SkivParams::default());
        assert_eq!(img.width(), 2);
        let pixel = img.get_pixel(0, 0);
        assert_eq!(pixel[0], 0);
        assert_eq!(pixel[1], 0);
        assert_eq!(pixel[2], 0);
    }

    #[test]
    fn skiv_tonemap_compresses_hdr_highlights() {
        let mut data = vec![0.0_f32; 16];
        for px in 0..4 {
            data[px * 4] = 10.0;
            data[px * 4 + 1] = 10.0;
            data[px * 4 + 2] = 10.0;
            data[px * 4 + 3] = 1.0;
        }
        let img = scrgb_to_sdr_skiv(&data, 2, 2, SkivParams::default());
        let pixel = img.get_pixel(0, 0);
        assert!(pixel[0] > 200, "highlight should stay bright: {}", pixel[0]);
        assert!(pixel[0] < 255, "highlight should not clip to 255 immediately: {}", pixel[0]);
    }

    #[test]
    fn skiv_preserves_low_luminance_signal() {
        let mut data = vec![0.0_f32; 16];
        for px in 0..4 {
            data[px * 4] = 0.5;
            data[px * 4 + 1] = 0.5;
            data[px * 4 + 2] = 0.5;
            data[px * 4 + 3] = 1.0;
        }
        let img = scrgb_to_sdr_skiv(&data, 2, 2, SkivParams::default());
        let pixel = img.get_pixel(0, 0);
        assert!(pixel[0] > 100 && pixel[0] < 220, "mid-grey landed at {}", pixel[0]);
    }

    #[test]
    fn skiv_p99_ignores_extreme_outlier() {
        let mut data = vec![0.0_f32; 400 * 4];
        for px in 0..399 {
            data[px * 4] = 1.0;
            data[px * 4 + 1] = 1.0;
            data[px * 4 + 2] = 1.0;
            data[px * 4 + 3] = 1.0;
        }
        data[399 * 4] = 100.0;
        data[399 * 4 + 1] = 100.0;
        data[399 * 4 + 2] = 100.0;
        data[399 * 4 + 3] = 1.0;
        let p99 = compute_max_cll_scrgb(&data, true);
        let p100 = compute_max_cll_scrgb(&data, false);
        assert!(p99 < 5.0, "p99 should drop the outlier: {p99}");
        assert!(p100 > 99.0, "p100 should include the outlier: {p100}");
    }

    #[test]
    fn test_hdr10_to_sdr() {
        let pq_data: Vec<u16> = vec![32768, 32768, 32768, 65535];
        let result = hdr10_to_sdr(&pq_data, 1, 1, 203.0);
        assert_eq!(result.width(), 1);
    }
}
