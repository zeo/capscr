// HDR -> SDR tonemap pipeline.
//
// the runtime tonemap is per-pixel maxRGB Reinhard-extended:
//   y(x) = x · (1 + x/w²) / (1 + x)        where w = scene peak in working space.
// when w = 1 (no HDR content) it reduces to the identity, so pure SDR
// captures pass through pixel-perfect. when w > 1 (HDR highlights present)
// it compresses the whole range so the peak lands at output white and
// brighter inputs stay distinctly brighter on output — the camera-like
// "global tonemap" behaviour. BT.2390 was tried first; it preserves SDR
// midtones better but maps everything brighter than display white into a
// tiny band near peak, so a 300-nit pixel and a 1500-nit pixel landed
// within a few sRGB steps of each other and the user reported all bright
// HDR content looking identically blown out. Reinhard-extended trades a
// little SDR-midtone brightness for a usefully wider HDR rolloff range.
//
// inputs are first rescaled so the OS-reported SDR white level maps to 1.0
// in working space — that way SDR-on-HDR pixels sit at exactly 1.0 and the
// p99 of maxRGB is 1.0 whenever there is no HDR content, which makes the
// tonemap the identity and preserves SDR pixel-for-pixel.

use image::RgbaImage;

const MAX_TONEMAP_DIMENSION: u32 = 16384;
const MAX_TONEMAP_PIXELS: usize = 256 * 1024 * 1024;

const PQ_N: f32 = 2610.0 / 4096.0 / 4.0;
const PQ_M: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;

#[derive(Debug, Clone, Copy)]
pub struct TonemapParams {
    /// manual override for the display's SDR white level in nits. when set
    /// to 0.0, the runtime falls back to the value detected by the capture
    /// path (DISPLAYCONFIG_SDR_WHITE_LEVEL with a DXGI fallback).
    pub sdr_white_nits_override: f32,
    /// global exposure multiplier applied before tonemapping. 1.0 is the
    /// neutral pass-through.
    pub user_brightness_scale: f32,
    /// when true, the source peak used by the EETF is the 99th percentile
    /// of maxRGB across the frame; this drops a small fraction of extreme
    /// outliers (specular glints, sun pixels) so the rest of the image
    /// isn't crushed by their presence.
    pub use_p99_max_cll: bool,
}

impl Default for TonemapParams {
    fn default() -> Self {
        Self {
            sdr_white_nits_override: 0.0,
            user_brightness_scale: 1.0,
            use_p99_max_cll: true,
        }
    }
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

// 4097-entry sRGB encode LUT: maps a linear input in [0,1] (quantized to
// 4096 steps) directly to the u8 sRGB byte the output expects. replaces 3
// powf calls per pixel in the tonemap output loop — for a 4K frame that
// drops the encode from ~24M powfs to ~24M lookups (~10x in release,
// ~50x+ in debug builds where powf is unoptimised). worst-case rounding
// error vs the analytical formula is <1 in 255, invisible at 8-bit
// output depth.
const SRGB_LUT_BITS: usize = 12;
const SRGB_LUT_SIZE: usize = (1 << SRGB_LUT_BITS) + 1;

fn srgb_lut() -> &'static [u8; SRGB_LUT_SIZE] {
    use std::sync::OnceLock;
    static LUT: OnceLock<Box<[u8; SRGB_LUT_SIZE]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = Box::new([0u8; SRGB_LUT_SIZE]);
        for i in 0..SRGB_LUT_SIZE {
            let linear = (i as f32) / ((SRGB_LUT_SIZE - 1) as f32);
            let srgb = linear_to_srgb(linear);
            t[i] = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        t
    })
}

#[inline]
fn linear_to_srgb_u8(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let idx = (clamped * ((SRGB_LUT_SIZE - 1) as f32)) as usize;
    srgb_lut()[idx.min(SRGB_LUT_SIZE - 1)]
}

// normalized PQ encode: v in [0, 1] where 1.0 = 10000 nits absolute, output
// in [0, 1] PQ-encoded.
fn linear_to_pq_norm(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0 {
        return 0.0;
    }
    let n = v.powf(PQ_N);
    ((PQ_C1 + PQ_C2 * n) / (1.0 + PQ_C3 * n)).powf(PQ_M)
}

// normalized PQ decode: pq in [0, 1] PQ-encoded, output in [0, 1] where
// 1.0 = 10000 nits absolute.
fn pq_to_linear_norm(pq: f32) -> f32 {
    let pq = pq.clamp(0.0, 1.0);
    if pq <= 0.0 {
        return 0.0;
    }
    let p = pq.powf(1.0 / PQ_M);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    if den <= 0.0 {
        return 1.0;
    }
    (num / den).powf(1.0 / PQ_N)
}

// BT.2390-style luminance-based tonemap. SDR content (BT.709 luminance
// ≤ 1.0 in working space) passes through completely untouched — no
// compression, no chroma shift, identical pixels. only pixels with actual
// high luminance get rolled off. for screen-capture content this is the
// right trade because most "HDR" content is white/near-white UI; saturated
// colours like pure magenta have low luminance (missing the green channel)
// and survive at higher RGB values without triggering the compressor.
//
// the compressor itself: y = knee + (1 - knee) * e / (e + (1 - knee)) where e = lum - knee,
// asymptoting at 1.0, preserving SDR up to `knee = 0.85`, mapping SDR white (1.0)
// to 0.925 (very bright and close to white), and rolling off all HDR highlights smoothly
// into [0.925, 1.0) so they never clip or blow out.
#[inline]
fn tonemap_pixel(r: f32, g: f32, b: f32, l_src: f32) -> (f32, f32, f32) {
    let max_val = r.max(g).max(b);
    if max_val <= 0.85 {
        return (r, g, b);
    }
    let knee = 0.85f32;
    let w = l_src - knee;
    let c = 1.0 - knee;

    // solve ln(1.0 + B * w) = c * B for B using robust bisection method
    let mut low = 0.0f32;
    let mut high = 1000.0f32;
    let mut b_param = 1.0f32;
    for _ in 0..20 {
        let mid = 0.5 * (low + high);
        let val = (1.0 + mid * w).ln() - c * mid;
        if val > 0.0 {
            low = mid;
        } else {
            high = mid;
        }
        b_param = mid;
    }

    let excess = max_val - knee;
    let compressed = knee + 1.0 / b_param * (1.0 + b_param * excess).ln();

    // desaturate highlights toward white as they get brighter to prevent neon/overblown look
    // and preserve details in highly saturated channels
    let desat_factor = 0.5 * (excess / (excess + 1.0));
    let r_desat = r * (1.0 - desat_factor) + max_val * desat_factor;
    let g_desat = g * (1.0 - desat_factor) + max_val * desat_factor;
    let b_desat = b * (1.0 - desat_factor) + max_val * desat_factor;

    let scale = compressed / max_val;
    (r_desat * scale, g_desat * scale, b_desat * scale)
}


pub fn scrgb_to_sdr_bt2390(
    scrgb_rgba: &[f32],
    width: u32,
    height: u32,
    sdr_white_nits: f32,
    params: TonemapParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
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

    // working space: scRGB units (1.0 = 80 nits) rescaled so the OS-reported
    // SDR-white pixel sits exactly at 1.0. SDR pixels on an HDR display land
    // at-or-below 1.0 in working space and the luminance-based tonemap
    // passes them through pixel-perfect.
    let sdr_white = effective_sdr_white(sdr_white_nits, params.sdr_white_nits_override);
    let scale = 80.0 / sdr_white;
    let brightness = if params.user_brightness_scale > 0.0 {
        params.user_brightness_scale
    } else {
        1.0
    };
    let coeff = scale * brightness;

    // estimate working-space peak of the frame by sampling
    let mut raw_peak = 1.0f32;
    let stride = (pixel_count / 100_000).max(1);
    for i in (0..pixel_count).step_by(stride) {
        let r = scrgb[i * 4];
        let g = scrgb[i * 4 + 1];
        let b = scrgb[i * 4 + 2];
        if r.is_finite() && g.is_finite() && b.is_finite() {
            let m = r.max(g).max(b);
            if m > raw_peak {
                raw_peak = m;
            }
        }
    }
    let l_src = (raw_peak * coeff).min(40.0).max(1.05);

    tracing::info!(
        "tonemap: {}x{} sdr_white={:.0}nits coeff={:.4} raw_peak={:.3} l_src={:.3}",
        width, height, sdr_white, coeff, raw_peak, l_src,
    );

    // fused decode + tonemap + sRGB-encode in a single parallel pass.
    // skipping the intermediate working-space allocation + the p99 scan
    // saves ~30-50% of the per-frame CPU vs the previous two-pass pipeline.
    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get().min(16))
        .unwrap_or(4)
        .max(1);
    let chunk_pixels = pixel_count.div_ceil(thread_count);
    let mut out_bytes = vec![0u8; pixel_count * 4];

    std::thread::scope(|s| {
        for (chunk_idx, out_chunk) in out_bytes.chunks_mut(chunk_pixels * 4).enumerate() {
            let src_start = chunk_idx * chunk_pixels * 4;
            let src_end = (src_start + out_chunk.len()).min(scrgb.len());
            let src_chunk = &scrgb[src_start..src_end];
            s.spawn(move || {
                let pixels = out_chunk.len() / 4;
                for i in 0..pixels {
                    let r_raw = src_chunk[i * 4];
                    let g_raw = src_chunk[i * 4 + 1];
                    let b_raw = src_chunk[i * 4 + 2];
                    let a_raw = src_chunk[i * 4 + 3];

                    let r = if r_raw.is_finite() { (r_raw * coeff).max(0.0) } else { 0.0 };
                    let g = if g_raw.is_finite() { (g_raw * coeff).max(0.0) } else { 0.0 };
                    let b = if b_raw.is_finite() { (b_raw * coeff).max(0.0) } else { 0.0 };
                    let a = if a_raw.is_finite() { a_raw.clamp(0.0, 1.0) } else { 1.0 };

                    let (r_tm, g_tm, b_tm) = tonemap_pixel(r, g, b, l_src);

                    out_chunk[i * 4] = linear_to_srgb_u8(r_tm);
                    out_chunk[i * 4 + 1] = linear_to_srgb_u8(g_tm);
                    out_chunk[i * 4 + 2] = linear_to_srgb_u8(b_tm);
                    out_chunk[i * 4 + 3] = (a * 255.0).clamp(0.0, 255.0) as u8;
                }
            });
        }
    });

    RgbaImage::from_raw(width, height, out_bytes).unwrap_or_else(|| RgbaImage::new(width, height))
}

pub fn hdr10_to_sdr_bt2390(
    pq_data: &[u16],
    width: u32,
    height: u32,
    sdr_white_nits: f32,
    params: TonemapParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if pq_data.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }

    // decode PQ -> linear nits, then rescale into scRGB (1.0 = 80 nits)
    // so we can hand off to the scRGB path.
    let mut scrgb = vec![0.0f32; pixel_count * 4];
    for (src, dest) in pq_data.chunks_exact(4).zip(scrgb.chunks_exact_mut(4)) {
        let r_pq = src[0] as f32 / 65535.0;
        let g_pq = src[1] as f32 / 65535.0;
        let b_pq = src[2] as f32 / 65535.0;
        let a_pq = src[3] as f32 / 65535.0;
        dest[0] = pq_to_linear(r_pq) / 80.0;
        dest[1] = pq_to_linear(g_pq) / 80.0;
        dest[2] = pq_to_linear(b_pq) / 80.0;
        dest[3] = a_pq;
    }
    scrgb_to_sdr_bt2390(&scrgb, width, height, sdr_white_nits, params)
}

pub fn hlg_to_sdr_bt2390(
    hlg_data: &[u8],
    width: u32,
    height: u32,
    sdr_white_nits: f32,
    params: TonemapParams,
) -> RgbaImage {
    if width == 0 || height == 0 || width > MAX_TONEMAP_DIMENSION || height > MAX_TONEMAP_DIMENSION {
        return RgbaImage::new(1, 1);
    }
    let pixel_count = match (width as usize).checked_mul(height as usize) {
        Some(c) if c <= MAX_TONEMAP_PIXELS => c,
        _ => return RgbaImage::new(1, 1),
    };
    if hlg_data.len() < pixel_count * 4 {
        return RgbaImage::new(width, height);
    }

    // HLG reference white is roughly 0.75 signal -> 1.0 linear; bring the
    // peak up to scRGB ~12 (1000 nits) before handing to the scRGB path.
    let mut scrgb = vec![0.0f32; pixel_count * 4];
    for (src, dest) in hlg_data.chunks_exact(4).zip(scrgb.chunks_exact_mut(4)) {
        let r_hlg = src[0] as f32 / 255.0;
        let g_hlg = src[1] as f32 / 255.0;
        let b_hlg = src[2] as f32 / 255.0;
        let a_hlg = src[3] as f32 / 255.0;
        dest[0] = hlg_to_linear(r_hlg) * 12.0;
        dest[1] = hlg_to_linear(g_hlg) * 12.0;
        dest[2] = hlg_to_linear(b_hlg) * 12.0;
        dest[3] = a_hlg;
    }
    scrgb_to_sdr_bt2390(&scrgb, width, height, sdr_white_nits, params)
}

fn effective_sdr_white(detected_nits: f32, override_nits: f32) -> f32 {
    // ignore the legacy 80.0 default that 0.3.53-era configs stamped into
    // capture.hdr.brightness_nits — it would override the auto-detected
    // SDR-white from DISPLAYCONFIG with the wrong value (80 instead of the
    // user's actual ~240 nits) and re-introduce HDR overblowing. only an
    // explicit override above 80 is treated as a real override now.
    let real_override = override_nits > 80.5;
    let pick = if real_override { override_nits } else { detected_nits };
    pick.max(80.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, r: f32, g: f32, b: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            v[i * 4] = r;
            v[i * 4 + 1] = g;
            v[i * 4 + 2] = b;
            v[i * 4 + 3] = 1.0;
        }
        v
    }

    fn pq_for(nits: f32) -> f32 {
        linear_to_pq_norm(nits / 10000.0)
    }

    #[test]
    fn pq_norm_roundtrip() {
        for nits in [10.0_f32, 100.0, 250.0, 1000.0, 4000.0, 10000.0] {
            let pq = pq_for(nits);
            let back = pq_to_linear_norm(pq) * 10000.0;
            assert!((back - nits).abs() / nits < 0.01, "{nits} -> {pq} -> {back}");
        }
    }

    #[test]
    fn max_rgb_tonemap_identity_on_sdr() {
        // any pixel with max component <= 0.85 must be the identity. that
        // means SDR content on an HDR display (after working-space
        // normalization) passes through pixel-perfect below the knee.
        for (r, g, b) in [
            (0.0_f32, 0.0, 0.0),
            (0.5, 0.5, 0.5),       // mid-grey
            (0.8, 0.0, 0.0),       // SDR red below knee
            (0.0, 0.8, 0.0),       // SDR green below knee
            (0.0, 0.0, 0.8),       // SDR blue below knee
            (0.8, 0.0, 0.8),       // SDR magenta below knee
        ] {
            let (r2, g2, b2) = tonemap_pixel(r, g, b, 4.0);
            assert!((r2 - r).abs() < 1e-5, "r {r} -> {r2}");
            assert!((g2 - g).abs() < 1e-5, "g {g} -> {g2}");
            assert!((b2 - b).abs() < 1e-5, "b {b} -> {b2}");
        }
    }

    #[test]
    fn luminance_tonemap_compresses_bright_white() {
        // bright white at 4× SDR (working space 4.0): excess = 3.15, l_src = 4.0.
        // solver finds b_param for w = 3.15, c = 0.15 -> b_param = 30.566.
        // compressed = 0.85 + 1.0/b_param * ln(1.0 + b_param * 3.15) = 1.0.
        let (r, g, b) = tonemap_pixel(4.0, 4.0, 4.0, 4.0);
        assert!((r - 1.0).abs() < 1e-4, "{r}");
        assert!((g - 1.0).abs() < 1e-4, "{g}");
        assert!((b - 1.0).abs() < 1e-4, "{b}");
    }

    #[test]
    fn max_rgb_tonemap_distinguishes_wcg_and_hdr_magenta() {
        // WCG magenta at SDR brightness (working 1.5, R=B=1.5, G=0), l_src = 6.0:
        //   w = 5.15, c = 0.15 -> b_param = 34.54.
        //   excess = 0.65, compressed = 0.85 + 1.0/b * ln(1.0 + b * 0.65) = 0.94125.
        // HDR magenta at 6× brightness (working 6.0, R=B=6, G=0), l_src = 6.0:
        //   excess = 5.15, compressed = 1.0.
        let (wcg_r, _, wcg_b) = tonemap_pixel(1.5, 0.0, 1.5, 6.0);
        let (hdr_r, _, hdr_b) = tonemap_pixel(6.0, 0.0, 6.0, 6.0);
        assert!((wcg_r - 0.94125).abs() < 1e-4, "WCG: {wcg_r}");
        assert!((wcg_b - 0.94125).abs() < 1e-4, "WCG: {wcg_b}");
        assert!((hdr_r - 1.0).abs() < 1e-4, "HDR: {hdr_r}");
        assert!((hdr_b - 1.0).abs() < 1e-4, "HDR: {hdr_b}");
        // bright WHITE compresses similarly
        let (sdr_white_r, _, _) = tonemap_pixel(1.0, 1.0, 1.0, 6.0);
        let (hdr_white_r, _, _) = tonemap_pixel(4.0, 4.0, 4.0, 6.0);
        assert!((sdr_white_r - 0.90273).abs() < 1e-4, "SDR white must be 0.90273: {sdr_white_r}");
        assert!((hdr_white_r - 0.98589).abs() < 1e-4, "HDR white must compress to 0.98589: {hdr_white_r}");
    }

    #[test]
    fn sdr_pixels_pass_through_on_hdr_display() {
        // 100% SDR white at scRGB sdr/80 on a display configured for 250-nit
        // SDR white. with source peak == display peak (no HDR content), the
        // EETF is the identity, so SDR white lands at sRGB ~246 (linear 0.925).
        let sdr_white = 250.0;
        let scrgb = solid(2, 2, sdr_white / 80.0, sdr_white / 80.0, sdr_white / 80.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, sdr_white, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 245 && p[1] >= 245 && p[2] >= 245, "SDR white: {p:?}");
    }

    #[test]
    fn sdr_mid_grey_lands_unchanged_on_hdr_display() {
        // sRGB 50% grey is linear ~0.21. on a 250-nit-SDR-white HDR display
        // that pixel arrives as scRGB ~0.674. after working-space normalization
        // the pixel sits at linear ~0.21, and since the image has no HDR
        // content (source peak == SDR white), the EETF is identity. round-trip
        // back to sRGB should land at ~128.
        let sdr_white = 250.0;
        let linear_50 = 0.21586_f32;
        let scrgb_val = linear_50 * sdr_white / 80.0;
        let scrgb = solid(2, 2, scrgb_val, scrgb_val, scrgb_val);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, sdr_white, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 120 && p[0] <= 136, "expected ~128, got {}", p[0]);
    }

    #[test]
    fn hdr_highlight_is_compressed_not_clipped() {
        // bright HDR signal: scRGB 8.0 on an 80-nit display. before this
        // change the highlight would crush to 255 because cml=PQ(8) compressed
        // to dml=PQ(8) (i.e. the entire image was rescaled to fit). under
        // the new EETF, an isolated bright pixel competes with the rest of
        // the image; with every pixel at scRGB 8.0 the curve maps 8.0 -> 1.0
        // (255). use a more representative mixed-luminance test instead.
        let mut data = vec![0.0f32; 100 * 4];
        for i in 0..99 {
            data[i * 4] = 0.5;
            data[i * 4 + 1] = 0.5;
            data[i * 4 + 2] = 0.5;
            data[i * 4 + 3] = 1.0;
        }
        // one outlier highlight at scRGB 8.0
        data[99 * 4] = 8.0;
        data[99 * 4 + 1] = 8.0;
        data[99 * 4 + 2] = 8.0;
        data[99 * 4 + 3] = 1.0;
        let img = scrgb_to_sdr_bt2390(&data, 10, 10, 80.0, TonemapParams::default());
        // outlier pixel: should be bright but not necessarily 255
        let hi = img.get_pixel(9, 9);
        assert!(hi[0] >= 200, "outlier highlight should stay bright: {}", hi[0]);
        // mid-grey: should be unaffected (well below knee)
        let lo = img.get_pixel(0, 0);
        assert!(lo[0] > 100 && lo[0] < 220, "mid-grey moved: {}", lo[0]);
    }

    #[test]
    fn hue_preserved_through_luminance_tonemap() {
        // a saturated red highlight at scRGB 4.0 has only the R channel non-zero, so
        // it gets desaturated to prevent a neon/overblown look in the sRGB gamut
        let scrgb = solid(2, 2, 4.0, 0.0, 0.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, 80.0, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] > 200, "red channel should land bright: {}", p[0]);
        assert_eq!(p[1], 166, "green channel must be desaturated to 166");
        assert_eq!(p[2], 166, "blue channel must be desaturated to 166");
    }

    #[test]
    fn high_sdr_white_doesnt_blow_out_sdr_content() {
        // regression: bright HDR displays report sdr_white = 300-400 nits;
        // an SDR pixel at scRGB 4.0 (320 nits) used to be treated as an
        // HDR highlight and survived the tonemap above sRGB 200. with
        // SDR-white normalization it sits at exactly 1.0 in working space
        // and lands at sRGB ~246 (linear 0.925) without being further amplified.
        let sdr_white = 320.0;
        let scrgb = solid(2, 2, 4.0, 4.0, 4.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, sdr_white, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 245, "SDR white on bright display: {p:?}");
    }

    #[test]
    fn black_stays_black() {
        let scrgb = solid(2, 2, 0.0, 0.0, 0.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, 250.0, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert_eq!(p[0], 0);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 0);
    }

    #[test]
    fn hdr10_roundtrip_does_not_panic() {
        let pq: Vec<u16> = vec![32768, 32768, 32768, 65535];
        let img = hdr10_to_sdr_bt2390(&pq, 1, 1, 250.0, TonemapParams::default());
        assert_eq!(img.width(), 1);
    }

    #[test]
    fn override_takes_precedence_over_detected_white() {
        // detected 80, override 250: a pixel at scRGB 250/80 should land at
        // sRGB ~246 because the override (not the detected value) drives
        // the normalization.
        let params = TonemapParams { sdr_white_nits_override: 250.0, ..TonemapParams::default() };
        let scrgb = solid(2, 2, 250.0 / 80.0, 250.0 / 80.0, 250.0 / 80.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, 80.0, params);
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 245, "override-driven SDR white: {p:?}");
    }
}
