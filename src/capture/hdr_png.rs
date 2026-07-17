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
// scope: HDR10 source (R16G16B16A16 native PQ from D3D11 swapchain scanout).
// ScRgb (float linear) would need a per-pixel matrix + PQ-encode pass before
// quantising to u16; until then those sources take the tonemapped SDR path.

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
    /// raw bytes straight from the capture source. Layout depends on `format`:
    /// - `Hdr10`: packed R10G10B10A2 little-endian u32 words (r bits 0-9,
    ///   g 10-19, b 20-29, a 30-31), 4 bytes per pixel — DXGI
    ///   R10G10B10A2_UNORM and pipewire xBGR/ABGR_210LE share this layout.
    /// - `ScRgb`: R16G16B16A16 half-float, 8 bytes per pixel, little-endian.
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
            "scRGB HDR encoding not supported (needs matrix + PQ pass); falling back to SDR"
        )),
        HdrFormat::Hlg => Err(anyhow!(
            "HLG HDR encoding not supported (needs upsample to 16-bit); falling back to SDR"
        )),
        HdrFormat::Sdr => Err(anyhow!(
            "encode_hdr_png called with SDR bitmap — programmer error"
        )),
    }
}

#[allow(clippy::uninit_vec)]
// expand one packed R10G10B10A2 word into full-range 16-bit channels.
// (v << 6) | (v >> 4) replicates the top bits so 0 maps to 0 and 1023 to
// 65535 exactly; the 2-bit alpha spreads the same way
pub(crate) fn unpack_rgb10a2(word: u32) -> [u16; 4] {
    let widen = |v: u32| ((v << 6) | (v >> 4)) as u16;
    let alpha = ((word >> 30) & 0x3) as u16;
    [
        widen(word & 0x3FF),
        widen((word >> 10) & 0x3FF),
        widen((word >> 20) & 0x3FF),
        alpha * 21845,
    ]
}

fn packed_hdr10_words(bitmap: &HdrBitmap) -> Result<impl Iterator<Item = u32> + '_> {
    let pixel_count = bitmap.pixel_count();
    let expected_bytes = pixel_count
        .checked_mul(4)
        .ok_or_else(|| anyhow!("hdr10 dimensions overflow byte count"))?;
    if (bitmap.data.len() as u64) < expected_bytes {
        return Err(anyhow!(
            "hdr10 source buffer too small: have {}, need {}",
            bitmap.data.len(),
            expected_bytes
        ));
    }
    Ok(bitmap.data[..expected_bytes as usize]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
}

fn encode_hdr10_png(path: &Path, bitmap: &HdrBitmap) -> Result<()> {
    let words = packed_hdr10_words(bitmap)?;

    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let mut enc = png::Encoder::new(&mut w, bitmap.width, bitmap.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Sixteen);
    let mut writer = enc.write_header()?;

    // cICP chunk: colour-primaries = 9 (BT.2020), transfer = 16 (PQ),
    // matrix = 0 (identity for RGB), full-range = 1.
    // PNG-3 §11.3.3.4 / H.273 codepoints.
    let cicp: [u8; 4] = [9, 16, 0, 1];
    writer.write_chunk(ChunkType(*b"cICP"), &cicp)?;

    // PNG wants 16-bit channels big-endian; widen each packed word in place
    let mut be_data = Vec::with_capacity(bitmap.pixel_count() as usize * 8);
    for word in words {
        for channel in unpack_rgb10a2(word) {
            be_data.extend_from_slice(&channel.to_be_bytes());
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
fn encode_hdr10_as_hlg_png(path: &Path, bitmap: &HdrBitmap) -> Result<()> {
    let words = packed_hdr10_words(bitmap)?;

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

    let mut out = Vec::with_capacity(bitmap.pixel_count() as usize * 8);
    for word in words {
        let [r, g, b, a] = unpack_rgb10a2(word);
        for channel in [pq_to_hlg[r as usize], pq_to_hlg[g as usize], pq_to_hlg[b as usize], a] {
            out.extend_from_slice(&channel.to_be_bytes());
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
    fn unpack_widens_full_range() {
        assert_eq!(unpack_rgb10a2(0), [0, 0, 0, 0]);
        // r=1023, g=512, b=1, a=3
        let word = 1023 | (512 << 10) | (1 << 20) | (3u32 << 30);
        let [r, g, b, a] = unpack_rgb10a2(word);
        assert_eq!(r, 65535);
        assert_eq!(g, (512 << 6) | (512 >> 4));
        assert_eq!(b, 64);
        assert_eq!(a, 65535);
    }

    #[test]
    fn cicp_chunk_roundtrips() {
        let tmp = std::env::temp_dir().join("capscr-hdr-test.png");
        let bitmap = HdrBitmap {
            width: 2,
            height: 2,
            format: HdrFormat::Hdr10,
            data: vec![0u8; 16], // 4 pixels × 4 packed bytes
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
            data: vec![0u8; 16],
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

    // linux has no HDR pixel source yet (see the note in hdr.rs), so this
    // synthetic gradient stands in for a DXGI frame and proves the pipeline
    // from raw HDR10 bytes through tonemapping and the cICP-preserving
    // encoder on every platform
    #[test]
    fn synthetic_hdr10_pipeline_tonemaps_and_roundtrips_cicp() {
        use crate::capture::tonemapping::{hdr10_to_sdr_bt2390, TonemapParams};

        // 4x1 PQ gradient in packed 10-bit: black, ~mid gray, bright
        // highlight, PQ peak, opaque alpha
        let pq_levels = [0u32, 517, 778, 1023];
        let mut data = Vec::new();
        for level in pq_levels {
            let word = level | (level << 10) | (level << 20) | (3 << 30);
            data.extend_from_slice(&word.to_le_bytes());
        }
        let bitmap = HdrBitmap {
            width: 4,
            height: 1,
            format: HdrFormat::Hdr10,
            data: data.clone(),
            max_luminance_nits: 1000.0,
        };

        let pq_u16: Vec<u16> = data
            .chunks_exact(4)
            .flat_map(|c| unpack_rgb10a2(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect();
        let sdr = hdr10_to_sdr_bt2390(&pq_u16, 4, 1, 240.0, TonemapParams::default());
        assert_eq!((sdr.width(), sdr.height()), (4, 1));
        assert_eq!(sdr.get_pixel(0, 0)[0], 0);
        assert!(sdr.get_pixel(1, 0)[0] < sdr.get_pixel(2, 0)[0]);
        assert!(sdr.get_pixel(2, 0)[0] <= sdr.get_pixel(3, 0)[0]);

        let tmp = std::env::temp_dir().join("capscr-hdr10-pipeline-test.png");
        encode_hdr_png(&tmp, &bitmap, HdrTransfer::Pq).unwrap();
        let info = read_cicp(&tmp).expect("cICP chunk should be present");
        assert_eq!(
            (
                info.colour_primaries,
                info.transfer,
                info.matrix,
                info.full_range
            ),
            (9, 16, 0, 1)
        );
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
