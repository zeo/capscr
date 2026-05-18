use anyhow::{anyhow, Result};
use image::RgbaImage;

use super::tonemapping::{hdr10_to_sdr_skiv, hlg_to_sdr_skiv, scrgb_to_sdr_skiv, SkivParams};
use super::current_skiv_params;

const MAX_HDR_DIMENSION: u32 = 16384;
const MAX_HDR_PIXELS: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrFormat {
    Sdr,
    Hdr10,
    ScRgb,
    Hlg,
}

#[derive(Debug, Clone)]
pub struct HdrDisplayInfo {
    pub is_hdr_enabled: bool,
    pub format: HdrFormat,
    pub max_luminance: f32,
    pub min_luminance: f32,
    pub sdr_white_level: f32,
}

impl Default for HdrDisplayInfo {
    fn default() -> Self {
        Self {
            is_hdr_enabled: false,
            format: HdrFormat::Sdr,
            max_luminance: 80.0,
            min_luminance: 0.0,
            sdr_white_level: 80.0,
        }
    }
}

/// HDR capture with automatic Reinhard tonemapping to SDR.
pub struct HdrCapture;

impl HdrCapture {
    pub fn new() -> Self {
        Self
    }

    pub fn get_display_hdr_info() -> Result<HdrDisplayInfo> {
        #[cfg(target_os = "windows")]
        {
            windows_hdr::get_hdr_display_info()
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(HdrDisplayInfo::default())
        }
    }

    pub fn is_hdr_available() -> bool {
        Self::get_display_hdr_info()
            .map(|info| info.is_hdr_enabled)
            .unwrap_or(false)
    }

    /// Capture HDR content and automatically tonemap to SDR.
    pub fn capture(&self) -> Result<RgbaImage> {
        let (img, _hdr) = self.capture_with_hdr_at(None)?;
        Ok(img)
    }

    /// Same capture as `capture()`, but also returns the raw HDR bitmap when
    /// the source was HDR. Used by the save path to write a sidecar HDR PNG
    /// alongside the tonemapped SDR PNG.
    pub fn capture_with_hdr(&self) -> Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
        self.capture_with_hdr_at(None)
    }

    /// Variant that targets the monitor containing the given desktop point.
    /// `None` falls back to the first DXGI output (legacy behaviour).
    pub fn capture_with_hdr_at(
        &self,
        target: Option<(i32, i32)>,
    ) -> Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
        #[cfg(target_os = "windows")]
        {
            let hdr_info = Self::get_display_hdr_info()?;

            if !hdr_info.is_hdr_enabled {
                return Ok((self.capture_sdr_fallback_at(target)?, None));
            }

            let (raw_data, width, height, format) = self.capture_raw(target)?;

            if width == 0 || height == 0 {
                return Err(anyhow!("Invalid capture dimensions"));
            }
            if width > MAX_HDR_DIMENSION || height > MAX_HDR_DIMENSION {
                return Err(anyhow!("Capture dimensions exceed maximum"));
            }

            let sdr_white = hdr_info.sdr_white_level.max(80.0);
            let sdr_img = self.tonemap(&raw_data, width, height, format, sdr_white);
            let hdr_bitmap = if matches!(format, HdrFormat::Sdr) {
                None
            } else {
                Some(crate::capture::HdrBitmap {
                    width,
                    height,
                    format,
                    data: raw_data,
                    max_luminance_nits: hdr_info.max_luminance,
                })
            };
            Ok((sdr_img, hdr_bitmap))
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok((self.capture_sdr_fallback_at(target)?, None))
        }
    }

    fn capture_raw(&self, target: Option<(i32, i32)>) -> Result<(Vec<u8>, u32, u32, HdrFormat)> {
        #[cfg(target_os = "windows")]
        {
            windows_hdr::capture_hdr_screen(target)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = target;
            Err(anyhow!("HDR capture not available on this platform"))
        }
    }

    fn capture_sdr_fallback_at(&self, target: Option<(i32, i32)>) -> Result<RgbaImage> {
        use super::{Capture, ScreenCapture};
        let capture = match target {
            Some((x, y)) => ScreenCapture::at_point(x, y)
                .unwrap_or_else(|_| ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())),
            None => ScreenCapture::new(),
        };
        capture.capture()
    }

    fn tonemap(&self, raw_data: &[u8], width: u32, height: u32, format: HdrFormat, sdr_white: f32) -> RgbaImage {
        if width == 0 || height == 0 {
            return RgbaImage::new(1, 1);
        }

        let pixel_count = match (width as usize).checked_mul(height as usize) {
            Some(c) if c <= MAX_HDR_PIXELS => c,
            _ => return RgbaImage::new(1, 1),
        };

        let params = effective_params(current_skiv_params(), sdr_white);

        match format {
            HdrFormat::ScRgb => {
                let expected_bytes = pixel_count.saturating_mul(16);
                if raw_data.len() < expected_bytes {
                    return RgbaImage::new(width, height);
                }
                let float_data: Vec<f32> = raw_data
                    .chunks_exact(4)
                    .map(|chunk| {
                        let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
                        let val = f32::from_le_bytes(bytes);
                        if val.is_finite() { val } else { 0.0 }
                    })
                    .collect();
                scrgb_to_sdr_skiv(&float_data, width, height, params)
            }
            HdrFormat::Hdr10 => {
                let expected_bytes = pixel_count.saturating_mul(8);
                if raw_data.len() < expected_bytes {
                    return RgbaImage::new(width, height);
                }
                let u16_data: Vec<u16> = raw_data
                    .chunks_exact(2)
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
                hdr10_to_sdr_skiv(&u16_data, width, height, params)
            }
            HdrFormat::Hlg => {
                let expected_bytes = pixel_count.saturating_mul(4);
                if raw_data.len() < expected_bytes {
                    return RgbaImage::new(width, height);
                }
                hlg_to_sdr_skiv(raw_data, width, height, params)
            }
            HdrFormat::Sdr => {
                // Already SDR, just copy
                let expected_bytes = pixel_count.saturating_mul(4);
                if raw_data.len() < expected_bytes {
                    return RgbaImage::new(width, height);
                }
                let mut result = RgbaImage::new(width, height);
                for (i, pixel) in raw_data.chunks_exact(4).enumerate() {
                    if i >= pixel_count {
                        break;
                    }
                    let x = (i as u32) % width;
                    let y = (i as u32) / width;
                    result.put_pixel(x, y, image::Rgba([pixel[0], pixel[1], pixel[2], pixel[3]]));
                }
                result
            }
        }
    }
}

impl Default for HdrCapture {
    fn default() -> Self {
        Self::new()
    }
}

fn effective_params(mut params: SkivParams, display_sdr_white_nits: f32) -> SkivParams {
    if params.sdr_brightness_nits <= 0.0 {
        params.sdr_brightness_nits = display_sdr_white_nits.max(80.0);
    }
    params
}

#[cfg(target_os = "windows")]
mod windows_hdr {
    use super::*;

    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput6,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

    pub fn get_hdr_display_info() -> Result<HdrDisplayInfo> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;

            let mut adapter_idx = 0u32;
            while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                let adapter: IDXGIAdapter1 = adapter;
                let mut output_idx = 0u32;

                while let Ok(output) = adapter.EnumOutputs(output_idx) {
                    let output: IDXGIOutput = output;

                    if let Ok(output6) = output.cast::<IDXGIOutput6>() {
                        if let Ok(desc1) = output6.GetDesc1() {
                            let color_space = desc1.ColorSpace;
                            let is_hdr = color_space.0 == 12 || color_space.0 == 13 || color_space.0 == 14;

                            if is_hdr {
                                return Ok(HdrDisplayInfo {
                                    is_hdr_enabled: true,
                                    format: match color_space.0 {
                                        12 => HdrFormat::Hdr10,
                                        13 => HdrFormat::Hlg,
                                        _ => HdrFormat::ScRgb,
                                    },
                                    max_luminance: desc1.MaxLuminance,
                                    min_luminance: desc1.MinLuminance,
                                    sdr_white_level: if desc1.MaxFullFrameLuminance > 0.0 {
                                        desc1.MaxFullFrameLuminance.min(400.0)
                                    } else {
                                        80.0
                                    },
                                });
                            }
                        }
                    }

                    output_idx += 1;
                }
                adapter_idx += 1;
            }

            Ok(HdrDisplayInfo::default())
        }
    }

    pub fn capture_hdr_screen(
        target: Option<(i32, i32)>,
    ) -> Result<(Vec<u8>, u32, u32, HdrFormat)> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        };
        use windows::Win32::Graphics::Dxgi::{
            IDXGIOutput1, IDXGIResource, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO,
            DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
        };

        struct FrameGuard<'a> {
            duplication: &'a IDXGIOutputDuplication,
            acquired: bool,
        }

        impl<'a> Drop for FrameGuard<'a> {
            fn drop(&mut self) {
                if self.acquired {
                    unsafe { let _ = self.duplication.ReleaseFrame(); }
                }
            }
        }

        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let (adapter, output) = pick_adapter_output(&factory, target)?;
            let output1: IDXGIOutput1 = output.cast()?;

            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;

            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;

            let device = device.ok_or_else(|| anyhow!("Failed to create D3D11 device"))?;
            let context = context.ok_or_else(|| anyhow!("Failed to get device context"))?;

            // DuplicateOutput can fail with ACCESS_LOST when another
            // exclusive-mode app (full-screen game, RDP) is holding the
            // output. Recreate once after a brief wait before giving up.
            let mut duplication = match output1.DuplicateOutput(&device) {
                Ok(d) => d,
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    output1.DuplicateOutput(&device).map_err(|e2| {
                        anyhow!("Display capture is locked by another app: {e2}")
                    })?
                }
                Err(e) => return Err(anyhow!("DuplicateOutput failed: {e}")),
            };

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;

            let mut acquired = false;
            for attempt in 0..10 {
                match duplication.AcquireNextFrame(100, &mut frame_info, &mut desktop_resource) {
                    Ok(()) => {
                        acquired = true;
                        break;
                    }
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST && attempt < 9 => {
                        // Display config changed (monitor unplug, sleep/wake,
                        // resolution switch). The duplication object is dead;
                        // re-acquire by recreating it from the same output.
                        if let Ok(fresh) = output1.DuplicateOutput(&device) {
                            duplication = fresh;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                        // Compositor produced no new frame within 100ms —
                        // common on idle desktops. Just keep waiting.
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }

            if !acquired {
                return Err(anyhow!(
                    "Failed to acquire HDR frame after 10 attempts — display may be sleeping"
                ));
            }

            let _frame_guard = FrameGuard { duplication: &duplication, acquired: true };

            let desktop_resource =
                desktop_resource.ok_or_else(|| anyhow!("No desktop resource"))?;
            let desktop_texture: ID3D11Texture2D = desktop_resource.cast()?;

            let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
            desktop_texture.GetDesc(&mut tex_desc);

            let width = tex_desc.Width;
            let height = tex_desc.Height;

            if width == 0 || height == 0 {
                return Err(anyhow!("Invalid texture dimensions"));
            }
            if width > MAX_HDR_DIMENSION || height > MAX_HDR_DIMENSION {
                return Err(anyhow!("Texture dimensions exceed maximum"));
            }

            let hdr_format = match tex_desc.Format {
                DXGI_FORMAT_R16G16B16A16_FLOAT => HdrFormat::ScRgb,
                DXGI_FORMAT_R10G10B10A2_UNORM => HdrFormat::Hdr10,
                _ => HdrFormat::Sdr,
            };

            let bytes_per_pixel = get_bytes_per_pixel(tex_desc.Format);

            let row_bytes = (width as usize).checked_mul(bytes_per_pixel)
                .ok_or_else(|| anyhow!("Row size overflow"))?;
            let total_bytes = row_bytes.checked_mul(height as usize)
                .ok_or_else(|| anyhow!("Total size overflow"))?;

            if total_bytes > MAX_HDR_PIXELS * 16 {
                return Err(anyhow!("Capture data too large"));
            }

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: tex_desc.Format,
                SampleDesc: tex_desc.SampleDesc,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: Default::default(),
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: Default::default(),
            };

            let mut staging_texture: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))?;
            let staging_texture = staging_texture.ok_or_else(|| anyhow!("Failed to create staging texture"))?;

            context.CopyResource(&staging_texture, &desktop_texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

            let row_pitch = mapped.RowPitch as usize;
            if row_pitch < row_bytes {
                context.Unmap(&staging_texture, 0);
                return Err(anyhow!("Invalid row pitch from GPU"));
            }

            let src_ptr = mapped.pData as *const u8;
            if src_ptr.is_null() {
                context.Unmap(&staging_texture, 0);
                return Err(anyhow!("Null pointer from GPU mapping"));
            }

            let mut data = Vec::with_capacity(total_bytes);

            for y in 0..height {
                let row_offset = (y as usize).checked_mul(row_pitch)
                    .ok_or_else(|| {
                        context.Unmap(&staging_texture, 0);
                        anyhow!("Row offset overflow")
                    })?;

                let row_start = src_ptr.add(row_offset);
                let row_slice = std::slice::from_raw_parts(row_start, row_bytes);
                data.extend_from_slice(row_slice);
            }

            context.Unmap(&staging_texture, 0);

            Ok((data, width, height, hdr_format))
        }
    }

    fn get_bytes_per_pixel(format: DXGI_FORMAT) -> usize {
        use windows::Win32::Graphics::Dxgi::Common::*;
        match format {
            DXGI_FORMAT_R16G16B16A16_FLOAT => 8,
            DXGI_FORMAT_R10G10B10A2_UNORM => 4,
            DXGI_FORMAT_B8G8R8A8_UNORM => 4,
            DXGI_FORMAT_R8G8B8A8_UNORM => 4,
            _ => 4,
        }
    }

    fn pick_adapter_output(
        factory: &IDXGIFactory1,
        target: Option<(i32, i32)>,
    ) -> Result<(IDXGIAdapter1, IDXGIOutput)> {
        unsafe {
            if let Some((tx, ty)) = target {
                let mut adapter_idx = 0u32;
                while let Ok(adapter) = factory.EnumAdapters1(adapter_idx) {
                    let mut output_idx = 0u32;
                    while let Ok(output) = adapter.EnumOutputs(output_idx) {
                        if let Ok(desc) = output.GetDesc() {
                            let r = desc.DesktopCoordinates;
                            if tx >= r.left && tx < r.right && ty >= r.top && ty < r.bottom {
                                return Ok((adapter, output));
                            }
                        }
                        output_idx += 1;
                    }
                    adapter_idx += 1;
                }
            }
            let adapter = factory.EnumAdapters1(0)?;
            let output = adapter.EnumOutputs(0)?;
            Ok((adapter, output))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hdr_display_info_default() {
        let info = HdrDisplayInfo::default();
        assert!(!info.is_hdr_enabled);
        assert_eq!(info.format, HdrFormat::Sdr);
    }

    #[test]
    fn test_hdr_capture_creation() {
        let _capture = HdrCapture::new();
        let _is_available = HdrCapture::is_hdr_available();
    }
}
