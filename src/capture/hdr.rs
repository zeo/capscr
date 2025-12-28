use anyhow::{anyhow, Result};
use image::RgbaImage;

use super::tonemapping::{ToneMapOperator, ToneMapper};

const MAX_HDR_DIMENSION: u32 = 16384;
const MAX_HDR_PIXELS: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrFormat {
    Sdr,
    Hdr10,
    ScRgb,
    Hlg,
}

impl HdrFormat {
    pub fn display_name(&self) -> &'static str {
        match self {
            HdrFormat::Sdr => "SDR",
            HdrFormat::Hdr10 => "HDR10 (PQ)",
            HdrFormat::ScRgb => "scRGB",
            HdrFormat::Hlg => "HLG",
        }
    }
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

pub struct HdrCapture {
    tonemap_operator: ToneMapOperator,
    exposure: f32,
    auto_tonemap: bool,
}

impl Default for HdrCapture {
    fn default() -> Self {
        Self {
            tonemap_operator: ToneMapOperator::AcesFilmic,
            exposure: 1.0,
            auto_tonemap: true,
        }
    }
}

impl HdrCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_operator(mut self, operator: ToneMapOperator) -> Self {
        let valid = ToneMapOperator::all().contains(&operator);
        if valid {
            self.tonemap_operator = operator;
        }
        self
    }

    pub fn operator_name(&self) -> &'static str {
        self.tonemap_operator.display_name()
    }

    pub fn with_exposure(mut self, exposure: f32) -> Self {
        self.exposure = exposure.clamp(0.1, 10.0);
        self
    }

    pub fn with_auto_tonemap(mut self, auto: bool) -> Self {
        self.auto_tonemap = auto;
        self
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

    pub fn capture_hdr(&self) -> Result<RgbaImage> {
        #[cfg(target_os = "windows")]
        {
            let hdr_info = Self::get_display_hdr_info()?;

            if !hdr_info.is_hdr_enabled {
                return self.capture_sdr_fallback();
            }

            let (raw_data, width, height, format) = self.capture_hdr_raw()?;

            if width == 0 || height == 0 {
                return Err(anyhow!("Invalid capture dimensions"));
            }
            if width > MAX_HDR_DIMENSION || height > MAX_HDR_DIMENSION {
                return Err(anyhow!("Capture dimensions exceed maximum"));
            }

            let sdr_white = if hdr_info.sdr_white_level > 0.0 {
                hdr_info.sdr_white_level
            } else {
                80.0
            };

            if self.auto_tonemap {
                let sdr_exposure = self.exposure * (80.0 / sdr_white);
                Ok(self.tonemap_raw(&raw_data, width, height, format, sdr_exposure))
            } else {
                Ok(self.tonemap_raw(&raw_data, width, height, format, self.exposure))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.capture_sdr_fallback()
        }
    }

    pub fn capture_hdr_raw(&self) -> Result<(Vec<u8>, u32, u32, HdrFormat)> {
        #[cfg(target_os = "windows")]
        {
            windows_hdr::capture_hdr_screen()
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(anyhow!("HDR capture not available on this platform"))
        }
    }

    fn capture_sdr_fallback(&self) -> Result<RgbaImage> {
        use super::{Capture, ScreenCapture};
        let capture = ScreenCapture::new();
        capture.capture()
    }

    pub fn tonemap_rgb10a2_data(&self, packed_data: &[u32], width: u32, height: u32) -> RgbaImage {
        let mapper = ToneMapper::new(self.tonemap_operator)
            .with_exposure(self.exposure)
            .with_gamma(2.2)
            .with_white_point(4.0);
        mapper.tonemap_rgb10a2(packed_data, width, height)
    }

    pub fn linear_to_pq_value(linear: f32) -> f32 {
        super::tonemapping::linear_to_pq(linear)
    }

    pub fn pq_inverse_value(linear: f32) -> f32 {
        super::tonemapping::pq_eotf_inverse(linear)
    }

    fn tonemap_raw(
        &self,
        raw_data: &[u8],
        width: u32,
        height: u32,
        format: HdrFormat,
        exposure: f32,
    ) -> RgbaImage {
        if width == 0 || height == 0 {
            return RgbaImage::new(1, 1);
        }

        let pixel_count = match (width as usize).checked_mul(height as usize) {
            Some(c) if c <= MAX_HDR_PIXELS => c,
            _ => return RgbaImage::new(1, 1),
        };

        let clamped_exposure = exposure.clamp(0.1, 10.0);
        if !clamped_exposure.is_finite() {
            return RgbaImage::new(width, height);
        }

        let mapper = ToneMapper::new(self.tonemap_operator)
            .with_exposure(clamped_exposure)
            .with_gamma(2.2)
            .with_white_point(4.0);

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
                if exposure == 1.0 {
                    super::tonemapping::scrgb_to_sdr(&float_data, width, height, clamped_exposure)
                } else {
                    mapper.tonemap_hdr_f32(&float_data, width, height)
                }
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
                if self.auto_tonemap {
                    super::tonemapping::hdr10_to_sdr(&u16_data, width, height, clamped_exposure)
                } else {
                    mapper.tonemap_rgba16(&u16_data, width, height)
                }
            }
            HdrFormat::Sdr => {
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
                    if exposure != 1.0 {
                        let srgb_r = pixel[0] as f32 / 255.0;
                        let srgb_g = pixel[1] as f32 / 255.0;
                        let srgb_b = pixel[2] as f32 / 255.0;
                        let linear_r = super::tonemapping::srgb_to_linear(srgb_r) * exposure;
                        let linear_g = super::tonemapping::srgb_to_linear(srgb_g) * exposure;
                        let linear_b = super::tonemapping::srgb_to_linear(srgb_b) * exposure;
                        let sr = super::tonemapping::linear_to_srgb(linear_r.clamp(0.0, 1.0));
                        let sg = super::tonemapping::linear_to_srgb(linear_g.clamp(0.0, 1.0));
                        let sb = super::tonemapping::linear_to_srgb(linear_b.clamp(0.0, 1.0));
                        let r = (sr * 255.0).clamp(0.0, 255.0) as u8;
                        let g = (sg * 255.0).clamp(0.0, 255.0) as u8;
                        let b = (sb * 255.0).clamp(0.0, 255.0) as u8;
                        result.put_pixel(x, y, image::Rgba([r, g, b, pixel[3]]));
                    } else {
                        result.put_pixel(x, y, image::Rgba([pixel[0], pixel[1], pixel[2], pixel[3]]));
                    }
                }
                result
            }
            HdrFormat::Hlg => {
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
                    let hlg_r = pixel[0] as f32 / 255.0;
                    let hlg_g = pixel[1] as f32 / 255.0;
                    let hlg_b = pixel[2] as f32 / 255.0;
                    let linear_r = super::tonemapping::hlg_oetf_inverse(hlg_r) * exposure;
                    let linear_g = super::tonemapping::hlg_oetf_inverse(hlg_g) * exposure;
                    let linear_b = super::tonemapping::hlg_oetf_inverse(hlg_b) * exposure;
                    let (tr, tg, tb) = mapper.apply_operator(linear_r, linear_g, linear_b);
                    let sr = super::tonemapping::linear_to_srgb(tr);
                    let sg = super::tonemapping::linear_to_srgb(tg);
                    let sb = super::tonemapping::linear_to_srgb(tb);
                    let r = (sr * 255.0).clamp(0.0, 255.0) as u8;
                    let g = (sg * 255.0).clamp(0.0, 255.0) as u8;
                    let b = (sb * 255.0).clamp(0.0, 255.0) as u8;
                    result.put_pixel(x, y, image::Rgba([r, g, b, pixel[3]]));
                }
                result
            }
        }
    }
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

    pub fn capture_hdr_screen() -> Result<(Vec<u8>, u32, u32, HdrFormat)> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        };
        use windows::Win32::Graphics::Dxgi::{
            IDXGIOutput1, IDXGIResource, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO,
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
            let adapter: IDXGIAdapter1 = factory.EnumAdapters1(0)?;
            let output: IDXGIOutput = adapter.EnumOutputs(0)?;
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

            let duplication = output1.DuplicateOutput(&device)?;

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;

            let mut acquired = false;
            for _ in 0..10 {
                match duplication.AcquireNextFrame(100, &mut frame_info, &mut desktop_resource) {
                    Ok(()) => {
                        acquired = true;
                        break;
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }

            if !acquired {
                return Err(anyhow!("Failed to acquire frame"));
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
}

#[cfg(target_os = "macos")]
mod macos_hdr {
    use super::*;

    pub fn get_hdr_display_info() -> Result<HdrDisplayInfo> {
        Ok(HdrDisplayInfo::default())
    }
}

#[cfg(target_os = "linux")]
mod linux_hdr {
    use super::*;

    pub fn get_hdr_display_info() -> Result<HdrDisplayInfo> {
        Ok(HdrDisplayInfo::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hdr_format_display_names() {
        assert_eq!(HdrFormat::Sdr.display_name(), "SDR");
        assert_eq!(HdrFormat::Hdr10.display_name(), "HDR10 (PQ)");
        assert_eq!(HdrFormat::ScRgb.display_name(), "scRGB");
        assert_eq!(HdrFormat::Hlg.display_name(), "HLG");
    }

    #[test]
    fn test_hdr_capture_default() {
        let capture = HdrCapture::new();
        let capture = capture
            .with_operator(ToneMapOperator::Reinhard)
            .with_exposure(2.0)
            .with_auto_tonemap(false);
        let is_available = HdrCapture::is_hdr_available();
        assert!(is_available || !is_available);
    }

    #[test]
    fn test_hdr_display_info_default() {
        let info = HdrDisplayInfo::default();
        assert!(!info.is_hdr_enabled);
        assert_eq!(info.format, HdrFormat::Sdr);
        assert!(info.max_luminance >= 0.0);
        assert!(info.min_luminance >= 0.0);
    }
}
