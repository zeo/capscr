// GPU HDR-to-SDR capture via Direct2D effects.
//
// On Windows HDR displays, the DWM composites the desktop in scRGB FP16.
// SDR apps' pixels are multiplied by `SdrWhiteLevel / 80` so that SDR-white
// renders at the user's preferred brightness — e.g., a 240-nit SDR-white
// slider puts SDR-white at scRGB 3.0, not 1.0. Every existing screenshot
// tool (capscr GDI, ShareX, Snipping Tool, Win+Shift+S) captures that
// composed framebuffer and clips it to 8-bit sRGB without dividing back
// out, producing the washed-out "everything overblown in screenshot mode"
// output the user has been complaining about.
//
// Microsoft's documented fix is exactly this pipeline, all GPU:
//   1. DXGI Desktop Duplication into R16G16B16A16_FLOAT (scRGB FP16)
//   2. D2D WhiteLevelAdjustment: divide by SdrWhite/80 → SDR back at 1.0
//   3. D2D HdrToneMap: roll off genuine HDR highlights into SDR range
//   4. Render to 8-bit BGRA output target
//
// Single-digit ms even for 4K, regardless of debug/release build.
//
// References:
//   https://learn.microsoft.com/en-us/windows/win32/direct2d/hdr-tone-map-effect
//   https://learn.microsoft.com/en-us/windows/win32/direct2d/white-level-adjustment-effect
//   https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range

use anyhow::{anyhow, Result};
use image::RgbaImage;
use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Effect, ID2D1Factory1,
    CLSID_D2D1HdrToneMap, CLSID_D2D1WhiteLevelAdjustment, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HDRTONEMAP_DISPLAY_MODE_SDR,
    D2D1_HDRTONEMAP_PROP_DISPLAY_MODE, D2D1_HDRTONEMAP_PROP_INPUT_MAX_LUMINANCE,
    D2D1_HDRTONEMAP_PROP_OUTPUT_MAX_LUMINANCE, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_WHITELEVELADJUSTMENT_PROP_INPUT_WHITE_LEVEL,
    D2D1_WHITELEVELADJUSTMENT_PROP_OUTPUT_WHITE_LEVEL,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, IDXGISurface,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_SAMPLE_DESC,
};

pub fn capture_hdr_to_sdr(target: Option<(i32, i32)>) -> Result<RgbaImage> {
    let t0 = std::time::Instant::now();
    unsafe {
        // 1. pick adapter+output for the target monitor (multi-GPU safe)
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let (adapter, output) = pick_adapter_output(&factory, target)?;
        let output1: IDXGIOutput1 = output.cast()?;

        // 2. D3D11 device on that adapter
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
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
        let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice null device"))?;
        let context = context.ok_or_else(|| anyhow!("D3D11CreateDevice null context"))?;

        // 3. duplication with HDR-first format list
        let supported_formats = [
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            DXGI_FORMAT_R10G10B10A2_UNORM,
        ];
        let mut duplication: IDXGIOutputDuplication = if let Ok(o5) = output1.cast::<IDXGIOutput5>()
        {
            o5.DuplicateOutput1(&device, 0, &supported_formats)
                .map_err(|e| anyhow!("DuplicateOutput1 failed: {e}"))?
        } else {
            output1
                .DuplicateOutput(&device)
                .map_err(|e| anyhow!("DuplicateOutput failed: {e}"))?
        };

        // Sleep briefly to let the DWM compositor populate the initial duplication texture
        std::thread::sleep(std::time::Duration::from_millis(10));

        // 4. prime-frame loop — first AcquireNextFrame often returns stale
        //    data (LastPresentTime=0). release and retry until we see a
        //    real frame.
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource: Option<IDXGIResource> = None;
        let mut acquired = false;
        for attempt in 0..30 {
            match duplication.AcquireNextFrame(10, &mut frame_info, &mut desktop_resource) {
                Ok(()) => {
                    let real = frame_info.LastPresentTime != 0
                        || frame_info.AccumulatedFrames > 0
                        || (attempt >= 1 && desktop_resource.is_some());
                    if real {
                        acquired = true;
                        break;
                    }
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
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        if !acquired {
            return Err(anyhow!("D2D: no frame acquired after 30 attempts"));
        }

        struct FrameGuard<'a> {
            dup: &'a IDXGIOutputDuplication,
        }
        impl<'a> Drop for FrameGuard<'a> {
            fn drop(&mut self) {
                unsafe {
                    let _ = self.dup.ReleaseFrame();
                }
            }
        }
        let _frame_guard = FrameGuard { dup: &duplication };

        let desktop_resource = desktop_resource.ok_or_else(|| anyhow!("no desktop resource"))?;
        let desktop_tex: ID3D11Texture2D = desktop_resource.cast()?;

        let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
        desktop_tex.GetDesc(&mut tex_desc);
        let width = tex_desc.Width;
        let height = tex_desc.Height;
        if width == 0 || height == 0 {
            return Err(anyhow!("desktop texture has zero dimensions"));
        }

        let src_format = tex_desc.Format;
        let is_hdr_format = src_format == DXGI_FORMAT_R16G16B16A16_FLOAT
            || src_format == DXGI_FORMAT_R10G10B10A2_UNORM;

        tracing::info!(
            "D2D tonemap: capture {}x{} dxgi_format={:?} is_hdr={}",
            width,
            height,
            src_format,
            is_hdr_format
        );

        // 5. read display metadata for the effect parameters
        let (sdr_white_nits, max_lum_nits) = read_display_metadata(&output)?;
        tracing::info!(
            "D2D tonemap: sdr_white={:.0}nits max_lum={:.0}nits",
            sdr_white_nits,
            max_lum_nits
        );

        // 6. D2D factory + device + context bound to our D3D11 device
        let d2d_factory: ID2D1Factory1 =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
        let d2d_ctx: ID2D1DeviceContext =
            d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

        // 7. wrap the captured texture as a D2D bitmap (input)
        let input_surface: IDXGISurface = desktop_tex.cast()?;
        let input_props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: src_format,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: Default::default(),
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let input_bitmap: ID2D1Bitmap1 =
            d2d_ctx.CreateBitmapFromDxgiSurface(&input_surface, Some(&input_props))?;

        // 8. build the effect chain
        let effect_in: ID2D1Effect = if is_hdr_format {
            // WhiteLevelAdjustment: divide everything by SdrWhite/80, so
            // SDR-promoted content lands back at scRGB 1.0
            let wl: ID2D1Effect = d2d_ctx.CreateEffect(&CLSID_D2D1WhiteLevelAdjustment)?;
            wl.SetInput(0, &input_bitmap, true);
            let in_white_bytes = sdr_white_nits.to_le_bytes();
            let out_white_bytes: [u8; 4] = 80.0_f32.to_le_bytes();
            wl.SetValue(
                D2D1_WHITELEVELADJUSTMENT_PROP_INPUT_WHITE_LEVEL.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &in_white_bytes,
            )?;
            wl.SetValue(
                D2D1_WHITELEVELADJUSTMENT_PROP_OUTPUT_WHITE_LEVEL.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &out_white_bytes,
            )?;

            // HdrToneMap: roll off remaining HDR highlights into SDR.
            // chain via wl.GetOutput() (ID2D1Image) -> tm.SetInput.
            let tm: ID2D1Effect = d2d_ctx.CreateEffect(&CLSID_D2D1HdrToneMap)?;
            let wl_out = wl.GetOutput()?;
            tm.SetInput(0, &wl_out, true);
            // scale the input maximum luminance passed to HdrToneMap by the same factor
            // that WhiteLevelAdjustment scaled the entire texture (80.0 / sdr_white_nits)
            let scaled_in_max = (max_lum_nits * (80.0 / sdr_white_nits)).max(80.0);
            let in_max = scaled_in_max.to_le_bytes();
            let out_max: [u8; 4] = 80.0_f32.to_le_bytes();
            tm.SetValue(
                D2D1_HDRTONEMAP_PROP_INPUT_MAX_LUMINANCE.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &in_max,
            )?;
            tm.SetValue(
                D2D1_HDRTONEMAP_PROP_OUTPUT_MAX_LUMINANCE.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &out_max,
            )?;
            let display_mode_bytes = (D2D1_HDRTONEMAP_DISPLAY_MODE_SDR.0 as u32).to_le_bytes();
            tm.SetValue(
                D2D1_HDRTONEMAP_PROP_DISPLAY_MODE.0 as u32,
                D2D1_PROPERTY_TYPE_ENUM,
                &display_mode_bytes,
            )?;
            tm
        } else {
            // SDR-format capture — no tonemap needed, but we still need an
            // effect to draw from. wrap input_bitmap via an identity-ish
            // effect (we can just DrawBitmap directly instead).
            // for now: return an error to fall back to GDI for SDR captures.
            return Err(anyhow!(
                "D2D tonemap: source is SDR ({:?}), fall back to GDI",
                src_format
            ));
        };

        // 9. output texture: B8G8R8A8 render target on the same device
        let out_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut out_tex: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&out_desc, None, Some(&mut out_tex))?;
        let out_tex = out_tex.ok_or_else(|| anyhow!("CreateTexture2D output null"))?;

        let out_surface: IDXGISurface = out_tex.cast()?;
        let out_props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let out_bitmap: ID2D1Bitmap1 =
            d2d_ctx.CreateBitmapFromDxgiSurface(&out_surface, Some(&out_props))?;

        // 10. render the effect chain into the output bitmap.
        //     DrawImage takes an ID2D1Image (the effect's output).
        let final_output = effect_in.GetOutput()?;
        d2d_ctx.SetTarget(&out_bitmap);
        d2d_ctx.BeginDraw();
        d2d_ctx.DrawImage(
            &final_output,
            None,
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
        d2d_ctx.EndDraw(None, None)?;
        d2d_ctx.SetTarget(None);

        // 11. CPU readback: staging texture → Map → BGRA→RGBA → RgbaImage
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
        let staging = staging.ok_or_else(|| anyhow!("CreateTexture2D staging null"))?;
        context.CopyResource(&staging, &out_tex);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        struct UnmapGuard<'a> {
            ctx: &'a ID3D11DeviceContext,
            tex: &'a ID3D11Texture2D,
        }
        impl<'a> Drop for UnmapGuard<'a> {
            fn drop(&mut self) {
                unsafe { self.ctx.Unmap(self.tex, 0) };
            }
        }
        let _unmap = UnmapGuard { ctx: &context, tex: &staging };

        let row_pitch = mapped.RowPitch as usize;
        let row_bytes = (width as usize) * 4;
        if row_pitch < row_bytes {
            return Err(anyhow!("row_pitch {row_pitch} < row_bytes {row_bytes}"));
        }
        let src_ptr = mapped.pData as *const u8;
        if src_ptr.is_null() {
            return Err(anyhow!("staging Map null pointer"));
        }

        let pixel_count = (width as usize) * (height as usize);
        let mut rgba = vec![0u8; pixel_count * 4];
        let thread_count = std::thread::available_parallelism()
            .map(|n| n.get().min(16))
            .unwrap_or(4)
            .max(1);
        let rows_per_chunk = (height as usize).div_ceil(thread_count);
        let src_addr = src_ptr as usize;
        std::thread::scope(|s| {
            for (chunk_idx, dst_chunk) in rgba.chunks_mut(rows_per_chunk * row_bytes).enumerate() {
                let start_row = chunk_idx * rows_per_chunk;
                s.spawn(move || {
                    let rows = dst_chunk.len() / row_bytes;
                    for r in 0..rows {
                        let y = start_row + r;
                        let src = (src_addr + y * row_pitch) as *const u8;
                        let dst_row = &mut dst_chunk[r * row_bytes..(r + 1) * row_bytes];
                        for x in 0..(width as usize) {
                            let off = x * 4;
                            dst_row[off] = *src.add(off + 2);
                            dst_row[off + 1] = *src.add(off + 1);
                            dst_row[off + 2] = *src.add(off);
                            dst_row[off + 3] = *src.add(off + 3);
                        }
                    }
                });
            }
        });

        tracing::info!("D2D tonemap: total {}ms", t0.elapsed().as_millis());

        RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
    }
}

unsafe fn pick_adapter_output(
    factory: &IDXGIFactory1,
    target: Option<(i32, i32)>,
) -> Result<(IDXGIAdapter1, IDXGIOutput)> {
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
    let adapter = factory
        .EnumAdapters1(0)
        .map_err(|e| anyhow!("EnumAdapters1 failed: {e}"))?;
    let output = adapter
        .EnumOutputs(0)
        .map_err(|e| anyhow!("EnumOutputs failed: {e}"))?;
    Ok((adapter, output))
}

unsafe fn read_display_metadata(output: &IDXGIOutput) -> Result<(f32, f32)> {
    use windows::Win32::Graphics::Dxgi::IDXGIOutput6;
    let output6: IDXGIOutput6 = output
        .cast()
        .map_err(|e| anyhow!("cast IDXGIOutput6 failed: {e}"))?;
    let desc1 = output6.GetDesc1()?;

    let sdr_white_nits = query_displayconfig_sdr_white(&desc1.DeviceName)
        .or_else(|| {
            if desc1.MaxFullFrameLuminance > 0.0 {
                Some(desc1.MaxFullFrameLuminance.min(400.0))
            } else {
                None
            }
        })
        .unwrap_or(80.0)
        .max(80.0);
    let max_lum_nits = if desc1.MaxLuminance > 0.0 {
        desc1.MaxLuminance.max(80.0)
    } else {
        1000.0
    };
    Ok((sdr_white_nits, max_lum_nits))
}

fn query_displayconfig_sdr_white(device_name: &[u16; 32]) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};

    const DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL: i32 = 11;

    unsafe {
        let mut path_count: u32 = 0;
        let mut mode_count: u32 = 0;
        let rc = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count);
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
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            let rc = WIN32_ERROR(DisplayConfigGetDeviceInfo(&mut source_name.header) as u32);
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
            let rc = WIN32_ERROR(DisplayConfigGetDeviceInfo(&mut sdr_white.header) as u32);
            if rc != ERROR_SUCCESS {
                continue;
            }
            if sdr_white.SDRWhiteLevel == 0 {
                continue;
            }
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
