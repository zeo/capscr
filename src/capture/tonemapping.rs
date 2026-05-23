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

// Reinhard-extended global tonemap. monotonic, smooth, identity when
// l_src == 1.0 (no HDR content), maps l_src -> 1.0 exactly.
fn reinhard_extended(x: f32, l_src: f32) -> f32 {
    let w2 = l_src * l_src;
    if w2 <= f32::EPSILON {
        return 0.0;
    }
    let y = x * (1.0 + x / w2) / (1.0 + x);
    y.clamp(0.0, 1.0)
}

// per-pixel hue-preserving tonemap: compress maxRGB, then scale all three
// channels by the same ratio. the maxRGB approach preserves channel ratios
// (hence hue and chroma) at the cost of slightly desaturating very bright
// highlights — the right trade for screen-capture content.
fn tonemap_pixel(r: f32, g: f32, b: f32, l_src: f32) -> (f32, f32, f32) {
    let m = r.max(g).max(b);
    if m <= f32::EPSILON {
        return (0.0, 0.0, 0.0);
    }
    let m_out = reinhard_extended(m, l_src);
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
    // quickselect-based percentile picker — O(n) instead of O(n log n).
    // sorting 8M+ samples for every 4K HDR capture was multi-second on the
    // user's machine and the dominant slice of "huge delay when pressing
    // my screenshot key" (0.3.55 report).
    let target_idx = if use_p99 {
        ((samples.len() as f32 - 1.0) * 0.99).round() as usize
    } else {
        samples.len() - 1
    };
    let idx = target_idx.min(samples.len() - 1);
    let (_, nth, _) = samples
        .select_nth_unstable_by(idx, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    nth.max(1.0)
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

    // diagnostic: sample 8 pixels spread across the frame so we can verify
    // the decode pipeline actually delivers HDR values (and isn't silently
    // producing all-zero output). re-demote to debug once HDR captures are
    // visually confirmed end-to-end.
    let mut sample_str = String::new();
    let stride = (pixel_count / 8).max(1);
    for k in 0..8 {
        let i = (k * stride).min(pixel_count - 1);
        let r = scrgb[i * 4];
        let g = scrgb[i * 4 + 1];
        let b = scrgb[i * 4 + 2];
        sample_str.push_str(&format!(
            " [{}: scRGB={:.2},{:.2},{:.2}]",
            i, r, g, b
        ));
    }
    tracing::info!(
        "tonemap: {}x{} sdr_white={:.0}nits l_src={:.3} (peak {:.0}nits) compress={} samples:{}",
        width,
        height,
        sdr_white,
        l_src,
        l_src * sdr_white,
        needs_compression,
        sample_str,
    );

    let mut out = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let r = working[idx];
            let g = working[idx + 1];
            let b = working[idx + 2];
            let a = working[idx + 3];

            let (r_tm, g_tm, b_tm) = if needs_compression {
                tonemap_pixel(r, g, b, l_src)
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
    fn reinhard_identity_when_no_hdr() {
        // when l_src == 1.0 (no HDR content), Reinhard-extended reduces
        // to y(x) = x · (1 + x) / (1 + x) = x exactly. SDR captures must
        // pass through unchanged.
        for x in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let y = reinhard_extended(x, 1.0);
            assert!((y - x).abs() < 1e-5, "{x} -> {y} (should be identity)");
        }
    }

    #[test]
    fn reinhard_maps_peak_to_one() {
        // by construction, reinhard_extended(l_src, l_src) == 1.0.
        for w in [1.0_f32, 1.5, 2.0, 4.0, 8.0, 18.75] {
            let y = reinhard_extended(w, w);
            assert!((y - 1.0).abs() < 1e-4, "peak {w} -> {y}");
        }
    }

    #[test]
    fn reinhard_monotonic_for_all_inputs() {
        let mut prev = -1.0_f32;
        for i in 0..=2000 {
            let x = i as f32 * 0.01;
            let y = reinhard_extended(x, 18.75);
            assert!(y >= prev - 1e-6, "non-monotonic at {x}: {y} < {prev}");
            prev = y;
        }
    }

    #[test]
    fn reinhard_differentiates_wcg_and_hdr() {
        // headline regression: a 300-nit "WCG" pixel and a 1000-nit "HDR"
        // pixel must encode to distinctly different sRGB values, not both
        // clamp to 255. previously BT.2390 mapped both into a ~5-step band
        // near display white.
        let wcg = reinhard_extended(1.2, 4.0); // 300 nits / 250 sdr-white
        let hdr = reinhard_extended(4.0, 4.0); // 1000 nits / 250 sdr-white
        assert!(
            hdr - wcg > 0.1,
            "WCG {wcg:.3} vs HDR {hdr:.3} — must differ by >0.1 in linear (~25 sRGB steps)"
        );
        assert!((hdr - 1.0).abs() < 1e-4, "HDR peak must hit 1.0 exactly: {hdr}");
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
