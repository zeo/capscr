use anyhow::{anyhow, Result};
use image::RgbaImage;

use super::tonemapping::{hdr10_to_sdr_bt2390, hlg_to_sdr_bt2390, scrgb_to_sdr_bt2390, TonemapParams};
use super::current_tonemap_params;

const MAX_HDR_DIMENSION: u32 = 16384;
const MAX_HDR_PIXELS: usize = 256 * 1024 * 1024;

// IEEE 754 binary16 -> binary32 conversion. used to decode scRGB pixels from
// the DXGI desktop duplication texture (R16G16B16A16_FLOAT). manual unpack
// to avoid pulling in the `half` crate for one function.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 0x1) as u32;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let mant = (bits & 0x3FF) as u32;
    let out_bits: u32 = if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            // subnormal -> normalize
            let mut m = mant;
            let mut e: i32 = 1;
            while (m & 0x400) == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3FF;
            (sign << 31) | (((e + 127 - 15) as u32) << 23) | (m << 13)
        }
    } else if exp == 0x1F {
        // inf / nan
        (sign << 31) | (0xFF << 23) | (mant << 13)
    } else {
        // normal
        (sign << 31) | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(out_bits)
}

#[cfg(test)]
mod f16_tests {
    use super::f16_to_f32;
    #[test]
    fn zero() { assert_eq!(f16_to_f32(0x0000), 0.0); }
    #[test]
    fn one() { assert!((f16_to_f32(0x3C00) - 1.0).abs() < 1e-6); }
    #[test]
    fn two() { assert!((f16_to_f32(0x4000) - 2.0).abs() < 1e-6); }
    #[test]
    fn negative_one() { assert!((f16_to_f32(0xBC00) - -1.0).abs() < 1e-6); }
    #[test]
    fn half() { assert!((f16_to_f32(0x3800) - 0.5).abs() < 1e-6); }
    #[test]
    fn srgb_white_on_hdr_display_via_scrgb() {
        // 3.125 in half-float (scRGB value for SDR-white at 250 nits)
        // 3.125 = 1.5625 * 2^1 -> half-float bits: sign=0 exp=16 (1+15) mant=0x240
        let bits = 0x4240;
        let v = f16_to_f32(bits);
        assert!((v - 3.125).abs() < 1e-3, "{v}");
    }
}

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

    pub fn capture(&self) -> Result<RgbaImage> {
        let (img, _hdr) = self.capture_with_hdr_at(None)?;
        Ok(img)
    }

    pub fn capture_with_hdr(&self) -> Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
        self.capture_with_hdr_at(None)
    }

    pub fn capture_with_hdr_at(
        &self,
        target: Option<(i32, i32)>,
    ) -> Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
        #[cfg(target_os = "windows")]
        {
            let hdr_info = Self::get_display_hdr_info()?;

            if !hdr_info.is_hdr_enabled {
                tracing::debug!("capture_with_hdr_at: hdr not enabled, falling back to SDR");
                return Ok((self.capture_sdr_fallback_at(target)?, None));
            }

            let (raw_data, width, height, format) = self.capture_raw(target).map_err(|e| {
                tracing::warn!("capture_with_hdr_at: capture_raw failed — falling back: {e:#}");
                e
            })?;

            if width == 0 || height == 0 {
                return Err(anyhow!("Invalid capture dimensions"));
            }
            if width > MAX_HDR_DIMENSION || height > MAX_HDR_DIMENSION {
                return Err(anyhow!("Capture dimensions exceed maximum"));
            }

            let sdr_white = hdr_info.sdr_white_level.max(80.0);
            tracing::info!(
                "capture_with_hdr_at: passing sdr_white={:.0}nits to tonemap (raw_data {}B, {}x{}, {:?})",
                sdr_white,
                raw_data.len(),
                width,
                height,
                format,
            );
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

        let params: TonemapParams = current_tonemap_params();

        match format {
            HdrFormat::ScRgb => {
                let expected_bytes = pixel_count.saturating_mul(8);
                if raw_data.len() < expected_bytes {
                    tracing::warn!(
                        "scrgb capture: raw_data {} bytes < expected {}",
                        raw_data.len(),
                        expected_bytes
                    );
                    return RgbaImage::new(width, height);
                }
                // diagnostic: count non-zero bytes in raw_data so we can
                // distinguish "GPU handed us a zeroed texture" from "my
                // half-float decode is wrong". elevated to info for 0.3.57
                // and demoted once HDR captures are visually confirmed.
                let scan_len = expected_bytes.min(1 << 20);
                let nonzero = raw_data[..scan_len].iter().filter(|b| **b != 0).count();
                let first_nonzero_offset = raw_data[..expected_bytes]
                    .iter()
                    .position(|b| *b != 0)
                    .map(|o| o as i64)
                    .unwrap_or(-1);
                let bytes_dump: String = raw_data[..32.min(expected_bytes)]
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                tracing::info!(
                    "scrgb raw: total={}B, nonzero_in_first_{}MB={} first_nonzero_byte_offset={} first32B=[{}]",
                    expected_bytes,
                    scan_len / (1 << 20),
                    nonzero,
                    first_nonzero_offset,
                    bytes_dump,
                );

                let float_data: Vec<f32> = raw_data[..expected_bytes]
                    .chunks_exact(2)
                    .map(|chunk| {
                        let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
                        let v = f16_to_f32(bits);
                        if v.is_finite() { v } else { 0.0 }
                    })
                    .collect();
                scrgb_to_sdr_bt2390(&float_data, width, height, sdr_white, params)
            }
            HdrFormat::Hdr10 => {
                // DXGI desktop duplication delivers HDR10 as R10G10B10A2_UNORM,
                // 4 bytes per pixel, packed: r=bits 0-9, g=10-19, b=20-29, a=30-31.
                // unpack into PQ-normalized u16 quads for hdr10_to_sdr_bt2390.
                let expected_bytes = pixel_count.saturating_mul(4);
                if raw_data.len() < expected_bytes {
                    tracing::warn!(
                        "hdr10 capture: raw_data {} bytes < expected {}",
                        raw_data.len(),
                        expected_bytes
                    );
                    return RgbaImage::new(width, height);
                }
                let mut u16_data: Vec<u16> = Vec::with_capacity(pixel_count * 4);
                for chunk in raw_data[..expected_bytes].chunks_exact(4) {
                    let packed = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    let r = (packed & 0x3FF) as u16; // 10 bits
                    let g = ((packed >> 10) & 0x3FF) as u16;
                    let b = ((packed >> 20) & 0x3FF) as u16;
                    let a = ((packed >> 30) & 0x3) as u16; // 2 bits
                    // hdr10_to_sdr_bt2390 normalises by 65535.0, so map our
                    // 10-bit and 2-bit channels into the 16-bit space.
                    u16_data.push(r << 6 | r >> 4); // 10-bit -> 16-bit via bit replication
                    u16_data.push(g << 6 | g >> 4);
                    u16_data.push(b << 6 | b >> 4);
                    u16_data.push(a * 0x5555); // 2-bit -> 16-bit via repeat
                }
                hdr10_to_sdr_bt2390(&u16_data, width, height, sdr_white, params)
            }
            HdrFormat::Hlg => {
                let expected_bytes = pixel_count.saturating_mul(4);
                if raw_data.len() < expected_bytes {
                    return RgbaImage::new(width, height);
                }
                hlg_to_sdr_bt2390(raw_data, width, height, sdr_white, params)
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
                                // prefer the OS-reported SDR white level
                                // (DISPLAYCONFIG_SDR_WHITE_LEVEL — the actual
                                // value driven by the SDR-content brightness
                                // slider in Windows Settings). DXGI's
                                // MaxFullFrameLuminance is a panel capability
                                // figure that's typically much higher than
                                // what the user actually wants SDR to render
                                // at, so it was systematically blowing out
                                // SDR-on-HDR captures.
                                let sdr_white_level = query_displayconfig_sdr_white(&desc1.DeviceName)
                                    .unwrap_or(200.0)
                                    .max(80.0);

                                tracing::debug!(
                                    "hdr display info: colorspace={} sdr_white={:.0}nits max_lum={:.0}nits dxgi_max_full_frame={:.0}nits",
                                    color_space.0,
                                    sdr_white_level,
                                    desc1.MaxLuminance,
                                    desc1.MaxFullFrameLuminance,
                                );
                                return Ok(HdrDisplayInfo {
                                    is_hdr_enabled: true,
                                    format: match color_space.0 {
                                        12 => HdrFormat::Hdr10,
                                        13 => HdrFormat::Hlg,
                                        _ => HdrFormat::ScRgb,
                                    },
                                    max_luminance: desc1.MaxLuminance,
                                    min_luminance: desc1.MinLuminance,
                                    sdr_white_level,
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

    // query the user-configured SDR white level for the monitor whose
    // GDI device name matches `device_name`. returns nits, or None when the
    // OS doesn't expose the value (older Win10, non-HDR-aware path, etc).
    //
    // the API: GetDisplayConfigBufferSizes -> QueryDisplayConfig ->
    // DisplayConfigGetDeviceInfo with type
    // DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL. the returned
    // SDRWhiteLevel field encodes nits as (value * 80 / 1000), so a value
    // of 1000 means 80 nits (the SDR reference) and 3125 means 250 nits.
    fn query_displayconfig_sdr_white(device_name: &[u16; 32]) -> Option<f32> {
        use windows::Win32::Devices::Display::{
            DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
            QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO,
            DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
            DISPLAYCONFIG_SDR_WHITE_LEVEL, QDC_ONLY_ACTIVE_PATHS,
        };
        use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};

        // DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL = 11 isn't a named
        // constant in older windows-rs versions; use the literal type tag.
        // documented at:
        //   https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ne-wingdi-displayconfig_device_info_type
        const DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL: i32 = 11;

        unsafe {
            let mut path_count: u32 = 0;
            let mut mode_count: u32 = 0;
            let rc = GetDisplayConfigBufferSizes(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                &mut mode_count,
            );
            if rc != ERROR_SUCCESS {
                return None;
            }
            let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> =
                vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> =
                vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let rc = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if rc != ERROR_SUCCESS {
                return None;
            }
            paths.truncate(path_count as usize);

            for path in &paths {
                // resolve the path's source GDI device name and match it
                // against the DXGI output's DeviceName so we read SDR white
                // for the correct monitor (not just the first one).
                let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                    header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                        r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                        size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                        adapterId: path.sourceInfo.adapterId,
                        id: path.sourceInfo.id,
                    },
                    ..Default::default()
                };
                let rc = WIN32_ERROR(
                    DisplayConfigGetDeviceInfo(&mut source_name.header as *mut _) as u32,
                );
                if rc != ERROR_SUCCESS {
                    continue;
                }
                if !device_names_match(&source_name.viewGdiDeviceName, device_name) {
                    continue;
                }

                let mut sdr_white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                    header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                        r#type: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_TYPE(
                            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                        ),
                        size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                        adapterId: path.targetInfo.adapterId,
                        id: path.targetInfo.id,
                    },
                    ..Default::default()
                };
                let rc = WIN32_ERROR(
                    DisplayConfigGetDeviceInfo(&mut sdr_white.header as *mut _) as u32,
                );
                if rc != ERROR_SUCCESS {
                    continue;
                }
                if sdr_white.SDRWhiteLevel == 0 {
                    continue;
                }
                // 1000 == 80 nits (the Windows SDR reference)
                let nits = (sdr_white.SDRWhiteLevel as f32) * 80.0 / 1000.0;
                if (80.0..=10000.0).contains(&nits) {
                    return Some(nits);
                }
            }
            None
        }
    }

    fn device_names_match(a: &[u16], b: &[u16; 32]) -> bool {
        let len = a.len().min(b.len());
        for i in 0..len {
            if a[i] != b[i] {
                return false;
            }
            if a[i] == 0 {
                return true;
            }
        }
        true
    }

    pub fn capture_hdr_screen(
        target: Option<(i32, i32)>,
    ) -> Result<(Vec<u8>, u32, u32, HdrFormat)> {
        tracing::debug!("capture_hdr_screen: entering with target={target:?}");
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        };
        use windows::Win32::Graphics::Dxgi::{
            IDXGIOutput1, IDXGIOutput5, IDXGIResource, IDXGIOutputDuplication,
            DXGI_OUTDUPL_FRAME_INFO, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
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

            // when pAdapter is non-null, DriverType MUST be UNKNOWN — passing
            // HARDWARE returns E_INVALIDARG, which we silently swallowed all
            // through 0.3.50–0.3.53 and fell back to xcap GDI BitBlt, which
            // clips HDR luminance to 8-bit sRGB. that's the root cause of
            // every "still overblown" HDR capture the user reported.
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
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

            // prefer IDXGIOutput5::DuplicateOutput1 with an HDR-first format
            // list so DXGI hands us R16G16B16A16_FLOAT (scRGB) on HDR
            // displays instead of B8G8R8A8_UNORM (already-tonemapped SDR).
            // when the user reported "still overblown" through 0.3.55-0.3.64,
            // the cause was that IDXGIOutput1::DuplicateOutput was giving
            // us SDR pixels even on HDR displays — my tonemap was running
            // on already-clipped data.
            let supported_formats: [DXGI_FORMAT; 3] = [
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                DXGI_FORMAT_R10G10B10A2_UNORM,
                DXGI_FORMAT_B8G8R8A8_UNORM,
            ];

            fn duplicate_with_formats(
                output1: &IDXGIOutput1,
                device: &ID3D11Device,
                supported_formats: &[DXGI_FORMAT],
            ) -> Result<IDXGIOutputDuplication> {
                // try Output5 first (HDR-aware), fall back to Output1.
                if let Ok(output5) = output1.cast::<IDXGIOutput5>() {
                    match unsafe { output5.DuplicateOutput1(device, 0, supported_formats) } {
                        Ok(d) => return Ok(d),
                        Err(e) => tracing::warn!(
                            "DuplicateOutput1 failed — falling back to DuplicateOutput: {e}"
                        ),
                    }
                }
                unsafe {
                    output1
                        .DuplicateOutput(device)
                        .map_err(|e| anyhow!("DuplicateOutput failed: {e}"))
                }
            }

            let mut duplication = match duplicate_with_formats(&output1, &device, &supported_formats) {
                Ok(d) => d,
                Err(e) if e.downcast_ref::<windows::core::Error>().map(|w| w.code()) == Some(DXGI_ERROR_ACCESS_LOST) => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    duplicate_with_formats(&output1, &device, &supported_formats).map_err(|e2| {
                        anyhow!("Display capture is locked by another app: {e2}")
                    })?
                }
                Err(e) => return Err(e),
            };

            // Sleep briefly to let the DWM compositor populate the initial duplication texture
            std::thread::sleep(std::time::Duration::from_millis(10));

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;

            // DXGI Desktop Duplication's first AcquireNextFrame after a
            // freshly-created IDXGIOutputDuplication often returns a stale
            // or empty frame — LastPresentTime=0 and AccumulatedFrames=0.
            // accepting it gives back an all-zero raw_data buffer (that's
            // what produced the all-black HDR screenshots in 0.3.55-0.3.57).
            // loop until we see a frame with either a real present time or
            // an accumulated update, releasing each stale one back to the
            // duplication so the next call returns the next pipeline slot.
            let mut acquired = false;
            for attempt in 0..30 {
                let res = duplication.AcquireNextFrame(
                    10,
                    &mut frame_info,
                    &mut desktop_resource,
                );
                match res {
                    Ok(()) => {
                        let real_frame = frame_info.LastPresentTime != 0
                            || frame_info.AccumulatedFrames > 0
                            || desktop_resource.is_some();
                        if real_frame {
                            acquired = true;
                            break;
                        }
                        // stale prime frame — release and retry. ReleaseFrame
                        // is required after every successful AcquireNextFrame.
                        let _ = duplication.ReleaseFrame();
                        desktop_resource = None;
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST && attempt < 29 => {
                        if let Ok(fresh) = output1.DuplicateOutput(&device) {
                            duplication = fresh;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }

            if !acquired {
                return Err(anyhow!(
                    "Failed to acquire HDR frame after 30 attempts — display may be sleeping or no recent updates"
                ));
            }

            tracing::info!(
                "capture_hdr_screen: acquired frame with last_present_time={} accumulated_frames={}",
                frame_info.LastPresentTime,
                frame_info.AccumulatedFrames,
            );

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

            // kept at info temporarily for 0.3.55 — re-demote to debug once
            // the user has confirmed the half-float decode actually delivers
            // the expected HDR pixels on their display.
            tracing::info!(
                "capture_hdr_screen: {}x{} dxgi_format={:?} -> {:?}",
                width,
                height,
                tex_desc.Format,
                hdr_format,
            );

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

            struct MapGuard<'a> {
                context: &'a ID3D11DeviceContext,
                texture: &'a ID3D11Texture2D,
            }
            impl<'a> Drop for MapGuard<'a> {
                fn drop(&mut self) {
                    unsafe { self.context.Unmap(self.texture, 0) };
                }
            }
            let _map_guard = MapGuard { context: &context, texture: &staging_texture };

            let row_pitch = mapped.RowPitch as usize;
            if row_pitch < row_bytes {
                return Err(anyhow!("Invalid row pitch from GPU"));
            }

            let src_ptr = mapped.pData as *const u8;
            if src_ptr.is_null() {
                return Err(anyhow!("Null pointer from GPU mapping"));
            }

            let mut data = Vec::with_capacity(total_bytes);

            for y in 0..height {
                let row_offset = (y as usize)
                    .checked_mul(row_pitch)
                    .ok_or_else(|| anyhow!("Row offset overflow"))?;

                let row_start = src_ptr.add(row_offset);
                let row_slice = std::slice::from_raw_parts(row_start, row_bytes);
                data.extend_from_slice(row_slice);
            }

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
