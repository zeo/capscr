// HDR -> SDR tonemap pipeline.
//
// the runtime tonemap is a per-pixel maxRGB BT.2390 EETF operating in
// PQ-encoded space. inputs are normalized so the OS-reported SDR white
// level maps to 1.0 in working space; the source peak (p99 of maxRGB) is
// PQ-encoded and used as the EETF's Lc; the SDR-white target is the EETF's
// Lw. the cubic Hermite knee starts at KS = 1.5·Lw - 0.5·Lc (in absolute
// PQ), so SDR-range pixels pass through nearly untouched while HDR
// highlights are smoothly rolled off. monotonic by construction because PQ
// compresses bright values into a small fraction of the [0,1] range, which
// keeps the cubic well-behaved no matter how bright the source.

use image::{Rgba, RgbaImage};

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

// BT.2390 EETF: maps PQ-encoded source pixel to PQ-encoded display pixel.
//   e1: input PQ value (in [0, lc_pq])
//   lw_pq: display peak in PQ
//   lc_pq: source peak in PQ (>= lw_pq)
// returns: tonemapped PQ value (in [0, lw_pq])
fn bt2390_eetf_pq(e1: f32, lw_pq: f32, lc_pq: f32) -> f32 {
    if lc_pq <= 0.0 {
        return 0.0;
    }
    // normalize so source peak is 1.0 in working space
    let e1n = (e1 / lc_pq).clamp(0.0, 1.0);
    let lw_n = (lw_pq / lc_pq).clamp(0.0, 1.0);
    let ks = (1.5 * lw_n - 0.5).max(0.0);
    let e2n = if e1n < ks || (1.0 - ks) <= f32::EPSILON {
        e1n
    } else {
        let t = (e1n - ks) / (1.0 - ks);
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        h00 * ks + h10 * (1.0 - ks) + h01 * lw_n
    };
    (e2n * lc_pq).clamp(0.0, lw_pq)
}

// per-pixel hue-preserving tonemap: compress maxRGB through the EETF in PQ
// space, then scale all three channels by the same ratio. this is the
// maxRGB flavour recommended by ITU-R BT.2390 for content where chroma
// fidelity matters more than perfect luminance reproduction — the right
// trade for screen-capture content (UI, text, photos).
fn tonemap_pixel(
    r: f32,
    g: f32,
    b: f32,
    sdr_white_nits: f32,
    lw_pq: f32,
    lc_pq: f32,
) -> (f32, f32, f32) {
    let m = r.max(g).max(b);
    if m <= f32::EPSILON {
        return (0.0, 0.0, 0.0);
    }
    // working space (1.0 = SDR white) -> absolute PQ
    let m_nits = m * sdr_white_nits;
    let m_pq = linear_to_pq_norm(m_nits / 10000.0);
    let out_pq = bt2390_eetf_pq(m_pq, lw_pq, lc_pq);
    let out_nits = pq_to_linear_norm(out_pq) * 10000.0;
    // absolute nits -> working space
    let m_out = out_nits / sdr_white_nits;
    let scale = m_out / m;
    (r * scale, g * scale, b * scale)
}

fn p99_max_rgb(working: &[f32], use_p99: bool) -> f32 {
    let mut samples: Vec<f32> = Vec::with_capacity(working.len() / 4);
    for chunk in working.chunks_exact(4) {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        if !r.is_finite() || !g.is_finite() || !b.is_finite() {
            continue;
        }
        samples.push(r.max(g).max(b).max(0.0));
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

    // build working space: scRGB units (1.0 = 80 nits) rescaled so the
    // display's SDR-white pixel sits exactly at 1.0. an SDR-only image on
    // an HDR display has pixels at scRGB sdr_white_nits/80; after this
    // rescale every one of those pixels is at-or-below 1.0 in working space.
    let sdr_white = effective_sdr_white(sdr_white_nits, params.sdr_white_nits_override);
    let scale = 80.0 / sdr_white;
    let brightness = if params.user_brightness_scale > 0.0 {
        params.user_brightness_scale
    } else {
        1.0
    };
    let coeff = scale * brightness;

    let mut working = vec![0.0f32; pixel_count * 4];
    for i in 0..pixel_count {
        let r = scrgb[i * 4];
        let g = scrgb[i * 4 + 1];
        let b = scrgb[i * 4 + 2];
        let a = scrgb[i * 4 + 3];
        working[i * 4] = if r.is_finite() { (r * coeff).max(0.0) } else { 0.0 };
        working[i * 4 + 1] = if g.is_finite() { (g * coeff).max(0.0) } else { 0.0 };
        working[i * 4 + 2] = if b.is_finite() { (b * coeff).max(0.0) } else { 0.0 };
        working[i * 4 + 3] = if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 };
    }

    let l_src = p99_max_rgb(&working, params.use_p99_max_cll);
    let needs_compression = l_src > 1.0 + 1e-4;

    // pre-compute PQ peaks. lc_pq is the source content peak (p99 maxRGB);
    // lw_pq is the display SDR-white target. both expressed against the PQ
    // reference of 10000 nits.
    let lw_pq = linear_to_pq_norm(sdr_white / 10000.0);
    let lc_pq = if needs_compression {
        linear_to_pq_norm((l_src * sdr_white).min(10000.0) / 10000.0)
    } else {
        lw_pq
    };

    let mut out = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let r = working[idx];
            let g = working[idx + 1];
            let b = working[idx + 2];
            let a = working[idx + 3];

            let (r_tm, g_tm, b_tm) = if needs_compression {
                tonemap_pixel(r, g, b, sdr_white, lw_pq, lc_pq)
            } else {
                (r, g, b)
            };

            let r_u = (linear_to_srgb(r_tm.clamp(0.0, 1.0)) * 255.0).round().clamp(0.0, 255.0) as u8;
            let g_u = (linear_to_srgb(g_tm.clamp(0.0, 1.0)) * 255.0).round().clamp(0.0, 255.0) as u8;
            let b_u = (linear_to_srgb(b_tm.clamp(0.0, 1.0)) * 255.0).round().clamp(0.0, 255.0) as u8;
            let a_u = (a * 255.0).clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([r_u, g_u, b_u, a_u]));
        }
    }
    out
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
    for i in 0..pixel_count {
        let r_hlg = hlg_data[i * 4] as f32 / 255.0;
        let g_hlg = hlg_data[i * 4 + 1] as f32 / 255.0;
        let b_hlg = hlg_data[i * 4 + 2] as f32 / 255.0;
        let a_hlg = hlg_data[i * 4 + 3] as f32 / 255.0;
        scrgb[i * 4] = hlg_to_linear(r_hlg) * 12.0;
        scrgb[i * 4 + 1] = hlg_to_linear(g_hlg) * 12.0;
        scrgb[i * 4 + 2] = hlg_to_linear(b_hlg) * 12.0;
        scrgb[i * 4 + 3] = a_hlg;
    }
    scrgb_to_sdr_bt2390(&scrgb, width, height, sdr_white_nits, params)
}

fn effective_sdr_white(detected_nits: f32, override_nits: f32) -> f32 {
    let pick = if override_nits > 0.0 { override_nits } else { detected_nits };
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
    fn eetf_identity_when_source_equals_display() {
        // when source peak == display peak, EETF should be the identity
        // function across the full range — there's nothing to compress.
        let lw = pq_for(250.0);
        for nits in [10.0_f32, 50.0, 125.0, 200.0, 250.0] {
            let e = pq_for(nits);
            let out = bt2390_eetf_pq(e, lw, lw);
            assert!((out - e).abs() < 1e-4, "{nits} nits: {e} -> {out}");
        }
    }

    #[test]
    fn eetf_caps_at_display_peak() {
        let lw = pq_for(250.0);
        for src_nits in [400.0_f32, 1000.0, 4000.0] {
            let lc = pq_for(src_nits);
            let out = bt2390_eetf_pq(lc, lw, lc);
            assert!((out - lw).abs() < 1e-4, "src {src_nits}: {lc} -> {out}, want {lw}");
        }
    }

    #[test]
    fn eetf_monotonic_for_typical_hdr() {
        let lw = pq_for(250.0);
        let lc = pq_for(1000.0);
        let mut prev = -1.0;
        for i in 0..=1000 {
            let nits = i as f32 * 1.0; // 0..1000 nits in 1-nit steps
            let e = pq_for(nits);
            let out = bt2390_eetf_pq(e, lw, lc);
            assert!(out >= prev - 1e-6, "non-monotonic at {nits} nits: {out} < {prev}");
            prev = out;
        }
    }

    #[test]
    fn eetf_monotonic_for_extreme_hdr() {
        let lw = pq_for(250.0);
        let lc = pq_for(10000.0);
        let mut prev = -1.0;
        for i in 0..=10000 {
            let nits = i as f32;
            let e = pq_for(nits);
            let out = bt2390_eetf_pq(e, lw, lc);
            assert!(out >= prev - 1e-6, "non-monotonic at {nits} nits: {out} < {prev}");
            prev = out;
        }
    }

    #[test]
    fn sdr_pixels_pass_through_on_hdr_display() {
        // 100% SDR white at scRGB sdr/80 on a display configured for 250-nit
        // SDR white. with source peak == display peak (no HDR content), the
        // EETF is the identity, so SDR white lands at sRGB 255.
        let sdr_white = 250.0;
        let scrgb = solid(2, 2, sdr_white / 80.0, sdr_white / 80.0, sdr_white / 80.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, sdr_white, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 250 && p[1] >= 250 && p[2] >= 250, "SDR white clipped: {p:?}");
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
    fn outlier_doesnt_crush_image_when_p99_enabled() {
        // 399 pixels at scRGB 1.0 + 1 pixel at scRGB 100.0. with p99 the
        // source peak drops the outlier and the rest of the image isn't
        // compressed by its presence.
        let mut data = vec![0.0f32; 400 * 4];
        for i in 0..399 {
            data[i * 4] = 1.0;
            data[i * 4 + 1] = 1.0;
            data[i * 4 + 2] = 1.0;
            data[i * 4 + 3] = 1.0;
        }
        data[399 * 4] = 100.0;
        data[399 * 4 + 1] = 100.0;
        data[399 * 4 + 2] = 100.0;
        data[399 * 4 + 3] = 1.0;
        let p99 = p99_max_rgb(&data, true);
        let p100 = p99_max_rgb(&data, false);
        assert!(p99 < 5.0, "p99 should drop outlier: {p99}");
        assert!(p100 > 99.0, "p100 should include outlier: {p100}");
    }

    #[test]
    fn hue_is_preserved_through_eetf() {
        // a pure-red highlight at scRGB 4.0 should stay red after tonemapping
        // — channel ratios must be invariant under the maxRGB EETF.
        let scrgb = solid(2, 2, 4.0, 0.0, 0.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, 80.0, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] > 200, "red channel should land bright: {}", p[0]);
        assert_eq!(p[1], 0, "green channel must stay 0: {}", p[1]);
        assert_eq!(p[2], 0, "blue channel must stay 0: {}", p[2]);
    }

    #[test]
    fn high_sdr_white_doesnt_blow_out_sdr_content() {
        // regression: bright HDR displays report sdr_white = 300-400 nits;
        // an SDR pixel at scRGB 4.0 (320 nits) used to be treated as an
        // HDR highlight and survived the tonemap above sRGB 200. with
        // SDR-white normalization it sits at exactly 1.0 in working space
        // and lands at output white without being further amplified.
        let sdr_white = 320.0;
        let scrgb = solid(2, 2, 4.0, 4.0, 4.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, sdr_white, TonemapParams::default());
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 250, "SDR white on bright display should land at 255: {p:?}");
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
        // output white because the override (not the detected value) drives
        // the normalization.
        let params = TonemapParams { sdr_white_nits_override: 250.0, ..TonemapParams::default() };
        let scrgb = solid(2, 2, 250.0 / 80.0, 250.0 / 80.0, 250.0 / 80.0);
        let img = scrgb_to_sdr_bt2390(&scrgb, 2, 2, 80.0, params);
        let p = img.get_pixel(0, 0);
        assert!(p[0] >= 250, "override-driven SDR white clipped: {p:?}");
    }
}
