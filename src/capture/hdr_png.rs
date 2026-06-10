// HDR-preserving PNG writer. Emits a 16-bit-per-channel RGBA PNG with a
// `cICP` chunk that declares the file as BT.2020 + PQ + full-range, so HDR-
// capable viewers (Photos, Edge, irfanView w/ libpng>=1.6.39) display it as
// real HDR.
//
// PNG spec for cICP (added in PNG 3rd edition, 2024): a 4-byte payload
// (colour-primaries, transfer-characteristics, matrix-coefficients,
// video-full-range-flag). Values mirror H.273 codepoints. cICP must appear
// BEFORE IDAT, so we add it via `Writer::write_chunk` between header and
// image data.
//
// scope right now: HDR10 source (R16G16B16A16 native PQ from D3D11 swapchain
// scanout). ScRgb (float linear) needs a per-pixel matrix + PQ-encode pass
// before quantising to u16 — that path is in TODO state at the bottom.

use anyhow::{anyhow, Result};
use png::chunk::ChunkType;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use super::hdr::HdrFormat;

#[derive(Debug, Clone)]
pub struct HdrBitmap {
    pub width: u32,
    pub height: u32,
    pub format: HdrFormat,
    /// raw bytes straight from the DXGI swapchain. Layout depends on `format`:
    /// - `Hdr10`: R16G16B16A16, 8 bytes per pixel, little-endian.
    /// - `ScRgb`: R32G32B32A32 float, 16 bytes per pixel, little-endian.
    /// - `Hlg`:   4 bytes per pixel (HLG-encoded BT.2020 8-bit per channel).
    /// - `Sdr`:   should never reach this struct — caller drops it.
    pub data: Vec<u8>,
    pub max_luminance_nits: f32,
}

impl HdrBitmap {
    pub fn pixel_count(&self) -> u64 {
        (self.width as u64).saturating_mul(self.height as u64)
    }
}

/// HDR PNG output transfer characteristic. PQ is the source-native HDR10
/// format; HLG is transcoded by decoding PQ to linear nits then applying
/// the HLG OETF per BT.2100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTransfer {
    Pq,
    Hlg,
}

/// write `bitmap` to `path` as a 16-bit RGBA PNG with a `cICP` chunk.
/// currently only `HdrFormat::Hdr10` is fully supported as a source format;
/// the output transfer can be PQ (passthrough) or HLG (transcoded). other
/// source formats still return explanatory errors so the caller can fall
/// back to the SDR-only path without surprises.
pub fn encode_hdr_png(path: &Path, bitmap: &HdrBitmap, transfer: HdrTransfer) -> Result<()> {
    match bitmap.format {
        HdrFormat::Hdr10 => match transfer {
            HdrTransfer::Pq => encode_hdr10_png(path, bitmap),
            HdrTransfer::Hlg => encode_hdr10_as_hlg_png(path, bitmap),
        },
        HdrFormat::ScRgb => Err(anyhow!(
            "scRGB HDR encoding not yet implemented (needs matrix + PQ pass) — Phase 2"
        )),
        HdrFormat::Hlg => Err(anyhow!(
            "HLG HDR encoding not yet implemented (needs upsample to 16-bit) — Phase 2"
        )),
        HdrFormat::Sdr => Err(anyhow!(
            "encode_hdr_png called with SDR bitmap — programmer error"
        )),
    }
}

#[allow(clippy::uninit_vec)]
fn encode_hdr10_png(path: &Path, bitmap: &HdrBitmap) -> Result<()> {
    let pixel_count = bitmap.pixel_count();
    let expected_bytes = pixel_count
        .checked_mul(8)
        .ok_or_else(|| anyhow!("hdr10 dimensions overflow byte count"))?;
    if (bitmap.data.len() as u64) < expected_bytes {
        return Err(anyhow!(
            "hdr10 source buffer too small: have {}, need {}",
            bitmap.data.len(),
            expected_bytes
        ));
    }

    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let mut enc = png::Encoder::new(&mut w, bitmap.width, bitmap.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Sixteen);
    // PNG stores 16-bit channels in big-endian. The PNG crate handles the
    // byte-swap if we hand it a u8 slice via `write_image_data` — but only if
    // the slice is already big-endian. The DXGI data is little-endian, so we
    // need to swap once.
    let mut writer = enc.write_header()?;

    // cICP chunk: colour-primaries = 9 (BT.2020), transfer = 16 (PQ),
    // matrix = 0 (identity for RGB), full-range = 1.
    // PNG-3 §11.3.3.4 / H.273 codepoints.
    let cicp: [u8; 4] = [9, 16, 0, 1];
    writer.write_chunk(ChunkType(*b"cICP"), &cicp)?;

    let mut be_data = Vec::with_capacity(bitmap.data.len());
    unsafe {
        be_data.set_len(bitmap.data.len());
    }
    let src_ptr: *const u8 = bitmap.data.as_ptr();
    let dest_ptr: *mut u8 = be_data.as_mut_ptr();
    let num_u16s = bitmap.data.len() / 2;
    for i in 0..num_u16s {
        unsafe {
            let val = std::ptr::read_unaligned(src_ptr.add(i * 2) as *const u16);
            std::ptr::write_unaligned(dest_ptr.add(i * 2) as *mut u16, val.swap_bytes());
        }
    }
    writer.write_image_data(&be_data)?;
    writer.finish()?;
    Ok(())
}

// transcode HDR10 (PQ-encoded BT.2020) to HLG-encoded BT.2020 16-bit PNG.
// pipeline per pixel channel:
//   1. PQ EOTF: decode the source u16 (normalised 0..1) back to linear nits
//      in [0, 10000]
//   2. normalise: divide by HLG nominal peak (1000 nits) so 1.0 maps to the
//      reference HDR white
//   3. HLG OETF: encode linear E -> non-linear E' per BT.2100 / ARIB STD-B67
//   4. quantise back to u16, write 16-bit RGBA PNG, attach cICP 9/18/0/1
#[allow(clippy::uninit_vec)]
fn encode_hdr10_as_hlg_png(path: &Path, bitmap: &HdrBitmap) -> Result<()> {
    let pixel_count = bitmap.pixel_count();
    let expected_bytes = pixel_count
        .checked_mul(8)
        .ok_or_else(|| anyhow!("hdr10 dimensions overflow byte count"))?;
    if (bitmap.data.len() as u64) < expected_bytes {
        return Err(anyhow!(
            "hdr10 source buffer too small: have {}, need {}",
            bitmap.data.len(),
            expected_bytes
        ));
    }

    // build a 65536-entry LUT mapping PQ-encoded u16 -> HLG-encoded u16.
    // the alpha channel passes through unchanged.
    let mut pq_to_hlg = vec![0u16; 65536];
    for i in 0..65536u32 {
        let pq_norm = i as f32 / 65535.0;
        let nits = pq_eotf_to_nits(pq_norm);
        // normalise to HLG nominal peak. BT.2100 uses 1000 nits as the
        // reference white for HLG; values above are theoretically allowed
        // but capped here at 1.0 since HLG OETF is only defined on [0, 1]
        let linear = (nits / 1000.0).clamp(0.0, 1.0);
        let hlg_norm = hlg_oetf(linear);
        pq_to_hlg[i as usize] = (hlg_norm * 65535.0).round().clamp(0.0, 65535.0) as u16;
    }

    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let mut enc = png::Encoder::new(&mut w, bitmap.width, bitmap.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Sixteen);
    let mut writer = enc.write_header()?;

    // cICP: BT.2020 (9), HLG / ARIB STD-B67 (18), identity matrix (0), full range (1)
    let cicp: [u8; 4] = [9, 18, 0, 1];
    writer.write_chunk(ChunkType(*b"cICP"), &cicp)?;

    let mut out = Vec::with_capacity(bitmap.data.len());
    unsafe {
        out.set_len(bitmap.data.len());
    }
    let src_ptr: *const u8 = bitmap.data.as_ptr();
    let dest_ptr: *mut u8 = out.as_mut_ptr();
    let num_pixels = bitmap.data.len() / 8;
    for i in 0..num_pixels {
        unsafe {
            let offset = i * 8;
            let r = std::ptr::read_unaligned(src_ptr.add(offset) as *const u16);
            let g = std::ptr::read_unaligned(src_ptr.add(offset + 2) as *const u16);
            let b = std::ptr::read_unaligned(src_ptr.add(offset + 4) as *const u16);
            let a = std::ptr::read_unaligned(src_ptr.add(offset + 6) as *const u16);

            let r2 = pq_to_hlg[u16::from_le(r) as usize].to_be();
            let g2 = pq_to_hlg[u16::from_le(g) as usize].to_be();
            let b2 = pq_to_hlg[u16::from_le(b) as usize].to_be();
            let a2 = u16::from_le(a).to_be();

            std::ptr::write_unaligned(dest_ptr.add(offset) as *mut u16, r2);
            std::ptr::write_unaligned(dest_ptr.add(offset + 2) as *mut u16, g2);
            std::ptr::write_unaligned(dest_ptr.add(offset + 4) as *mut u16, b2);
            std::ptr::write_unaligned(dest_ptr.add(offset + 6) as *mut u16, a2);
        }
    }
    writer.write_image_data(&out)?;
    writer.finish()?;
    Ok(())
}

// SMPTE ST 2084 / BT.2100 PQ EOTF (decode). takes a normalised 0..1 input
// and returns absolute luminance in nits, range [0, 10000].
fn pq_eotf_to_nits(pq_norm: f32) -> f32 {
    let m1 = 2610.0 / 16384.0;
    let m2 = (2523.0 / 4096.0) * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = (2413.0 / 4096.0) * 32.0;
    let c3 = (2392.0 / 4096.0) * 32.0;
    if pq_norm <= 0.0 {
        return 0.0;
    }
    let e_p = pq_norm.powf(1.0 / m2);
    let num = (e_p - c1).max(0.0);
    let den = c2 - c3 * e_p;
    if den <= 0.0 {
        return 10000.0;
    }
    let y = (num / den).powf(1.0 / m1);
    10000.0 * y.clamp(0.0, 1.0)
}

// BT.2100 HLG OETF (ARIB STD-B67). input E in [0, 1], output E' in [0, 1].
// constants per BT.2100 Table 5: a = 0.17883277, b = 0.28466892, c = 0.55991073.
// the trailing digit is rounded by f32 (~7 significant decimal digits) so the
// grouped-by-three underscored form below is bit-exact equivalent.
fn hlg_oetf(e: f32) -> f32 {
    let a = 0.178_832_77_f32;
    let b = 0.284_668_92_f32;
    let c = 0.559_910_7_f32;
    if e <= 1.0 / 12.0 {
        (3.0 * e).max(0.0).sqrt()
    } else {
        a * (12.0 * e - b).ln() + c
    }
}

/// probe a PNG for a `cICP` chunk that signals HDR. Used by the editor to
/// detect HDR captures it shouldn't quietly downgrade to SDR on save.
pub fn read_cicp(path: &Path) -> Option<CicpInfo> {
    let bytes = std::fs::read(path).ok()?;
    parse_cicp(&bytes)
}

#[derive(Debug, Clone, Copy)]
pub struct CicpInfo {
    pub colour_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: u8,
}

impl CicpInfo {
    pub fn is_hdr(&self) -> bool {
        // PQ (16) and HLG (18) are HDR transfers. Most other values are SDR.
        matches!(self.transfer, 16 | 18)
    }
}

fn parse_cicp(bytes: &[u8]) -> Option<CicpInfo> {
    // PNG signature is 8 bytes. After that: length(4) | type(4) | data(N) | crc(4).
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8;
    while i + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let typ = &bytes[i + 4..i + 8];
        let data_start = i + 8;
        let data_end = data_start.checked_add(len)?;
        if data_end + 4 > bytes.len() {
            return None;
        }
        if typ == b"cICP" {
            if len < 4 {
                return None;
            }
            return Some(CicpInfo {
                colour_primaries: bytes[data_start],
                transfer: bytes[data_start + 1],
                matrix: bytes[data_start + 2],
                full_range: bytes[data_start + 3],
            });
        }
        if typ == b"IDAT" || typ == b"IEND" {
            // cICP must appear before IDAT; if we hit IDAT we're done.
            return None;
        }
        i = data_end + 4; // skip CRC
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cicp_chunk_roundtrips() {
        let tmp = std::env::temp_dir().join("capscr-hdr-test.png");
        let bitmap = HdrBitmap {
            width: 2,
            height: 2,
            format: HdrFormat::Hdr10,
            data: vec![0u8; 32], // 4 pixels × 8 bytes
            max_luminance_nits: 1000.0,
        };
        encode_hdr_png(&tmp, &bitmap, HdrTransfer::Pq).unwrap();
        let info = read_cicp(&tmp).expect("cICP chunk should be present");
        assert_eq!(info.colour_primaries, 9);
        assert_eq!(info.transfer, 16);
        assert_eq!(info.matrix, 0);
        assert_eq!(info.full_range, 1);
        assert!(info.is_hdr());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn hlg_output_writes_cicp_9_18() {
        let tmp = std::env::temp_dir().join("capscr-hlg-test.png");
        let bitmap = HdrBitmap {
            width: 2,
            height: 2,
            format: HdrFormat::Hdr10,
            data: vec![0u8; 32],
            max_luminance_nits: 1000.0,
        };
        encode_hdr_png(&tmp, &bitmap, HdrTransfer::Hlg).unwrap();
        let info = read_cicp(&tmp).expect("cICP chunk should be present");
        assert_eq!(info.colour_primaries, 9);
        assert_eq!(info.transfer, 18);
        assert_eq!(info.matrix, 0);
        assert_eq!(info.full_range, 1);
        assert!(info.is_hdr());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn pq_eotf_known_points() {
        // 0 -> 0 nits
        assert!((pq_eotf_to_nits(0.0)).abs() < 0.01);
        // 1.0 -> 10000 nits (PQ peak)
        assert!((pq_eotf_to_nits(1.0) - 10000.0).abs() < 1.0);
        // ~0.5081 PQ encoding maps to ~100 nits (PQ midpoint reference)
        let mid = pq_eotf_to_nits(0.5081);
        assert!(
            (mid - 100.0).abs() < 5.0,
            "expected ~100 nits at PQ 0.5081, got {mid}"
        );
    }

    #[test]
    fn hlg_oetf_known_points() {
        // 0 -> 0
        assert!(hlg_oetf(0.0).abs() < 1e-6);
        // 1/12 -> sqrt(3 * 1/12) = 0.5 (the OETF stitch point)
        assert!((hlg_oetf(1.0 / 12.0) - 0.5).abs() < 1e-4);
        // 1.0 -> a*ln(12-b)+c ≈ 1.0
        assert!((hlg_oetf(1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn cicp_absent_returns_none() {
        let tmp = std::env::temp_dir().join("capscr-sdr-test.png");
        // write a plain SDR PNG via the image crate.
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 64, 200, 255]));
        img.save(&tmp).unwrap();
        assert!(read_cicp(&tmp).is_none());
        let _ = std::fs::remove_file(tmp);
    }
}
