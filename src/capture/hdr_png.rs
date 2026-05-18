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
// Scope right now: HDR10 source (R16G16B16A16 native PQ from D3D11 swapchain
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
    /// Raw bytes straight from the DXGI swapchain. Layout depends on `format`:
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

/// Write `bitmap` to `path` as a 16-bit RGBA PNG with a `cICP` chunk.
/// Currently only `HdrFormat::Hdr10` is fully supported; other formats
/// return an explanatory error so the caller can fall back to the SDR-only
/// path without surprises.
pub fn encode_hdr_png(path: &Path, bitmap: &HdrBitmap) -> Result<()> {
    match bitmap.format {
        HdrFormat::Hdr10 => encode_hdr10_png(path, bitmap),
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

    // Convert little-endian u16s to big-endian for PNG. Allocates once.
    let mut be_data = Vec::with_capacity(bitmap.data.len());
    for chunk in bitmap.data.chunks_exact(2) {
        // chunk[0] is low byte (LE); flip to high-first.
        be_data.push(chunk[1]);
        be_data.push(chunk[0]);
    }
    writer.write_image_data(&be_data)?;
    writer.finish()?;
    Ok(())
}

/// Probe a PNG for a `cICP` chunk that signals HDR. Used by the editor to
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
        encode_hdr_png(&tmp, &bitmap).unwrap();
        let info = read_cicp(&tmp).expect("cICP chunk should be present");
        assert_eq!(info.colour_primaries, 9);
        assert_eq!(info.transfer, 16);
        assert_eq!(info.matrix, 0);
        assert_eq!(info.full_range, 1);
        assert!(info.is_hdr());
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn cicp_absent_returns_none() {
        let tmp = std::env::temp_dir().join("capscr-sdr-test.png");
        // Write a plain SDR PNG via the image crate.
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 64, 200, 255]));
        img.save(&tmp).unwrap();
        assert!(read_cicp(&tmp).is_none());
        let _ = std::fs::remove_file(tmp);
    }
}
