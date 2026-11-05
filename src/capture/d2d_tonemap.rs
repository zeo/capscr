// GPU HDR-to-SDR capture.
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
// The pipeline, all GPU:
//   1. DXGI Desktop Duplication into R16G16B16A16_FLOAT (scRGB FP16)
//   2. hdrkit's tonemap shader: divide by SdrWhite/80 so SDR lands back at
//      1.0, roll highlights above a knee onto white at the panel's peak, and
//      sRGB-encode into 8-bit BGRA
//
// the d2d chain this replaced had the white level effect backwards (input
// must be 80, output sdr white), wrote linear light into a UNORM target
// without a color management step, and HdrToneMap's sdr mode lifted midtones
//
// References:
//   https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range
//   https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api

use anyhow::{anyhow, Result};
use hdrkit::windows::core::Interface;
use hdrkit::windows::Win32::Foundation::HMODULE;
use hdrkit::windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use hdrkit::windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use hdrkit::windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
use hdrkit::windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput5, IDXGIOutput6,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};
use image::RgbaImage;

pub fn capture_hdr_to_sdr(target: Option<(i32, i32)>) -> Result<RgbaImage> {
    let t0 = std::time::Instant::now();
    unsafe {
        // 1. pick adapter+output for the target monitor (multi-GPU safe)
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let (adapter, output) = pick_adapter_output(&factory, target)?;
        let output6: IDXGIOutput6 = output
            .cast()
            .map_err(|e| anyhow!("cast IDXGIOutput6 failed: {e}"))?;
        let panel = hdrkit::display(output6.GetDesc1()?.Monitor)?;
        if !panel.hdr {
            return Err(anyhow!("HDR tonemap: display is SDR, fall back to GDI"));
        }
        tracing::info!(
            "HDR tonemap: sdr_white={:.0}nits peak={:.0}nits",
            panel.sdr_white,
            panel.peak
        );

        // 2. D3D11 device on that adapter
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice null device"))?;
        let context = context.ok_or_else(|| anyhow!("D3D11CreateDevice null context"))?;

        // 3. duplication as scRGB FP16, the only format the tonemap reads
        let output5: IDXGIOutput5 = output
            .cast()
            .map_err(|e| anyhow!("cast IDXGIOutput5 failed: {e}"))?;
        let supported_formats = [DXGI_FORMAT_R16G16B16A16_FLOAT];
        let duplicate = || {
            output5
                .DuplicateOutput1(&device, 0, &supported_formats)
                .map_err(|e| anyhow!("DuplicateOutput1 failed: {e}"))
        };
        let mut duplication: IDXGIOutputDuplication = duplicate()?;

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
                    let desktop_res = match desktop_resource.take() {
                        Some(r) => r,
                        None => {
                            let _ = duplication.ReleaseFrame();
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                    };

                    // only a frame nothing was presented into yet can be all-zeros
                    let check_black =
                        frame_info.LastPresentTime == 0 && frame_info.AccumulatedFrames == 0;
                    let is_black = check_black
                        && desktop_res
                            .cast::<ID3D11Texture2D>()
                            .is_ok_and(|t| is_black_frame(&device, &context, &t));

                    if !is_black {
                        desktop_resource = Some(desktop_res);
                        acquired = true;
                        break;
                    }
                    let _ = duplication.ReleaseFrame();
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST && attempt < 29 => {
                    if let Ok(fresh) = duplicate() {
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
            return Err(anyhow!("HDR tonemap: no frame acquired after 30 attempts"));
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
        if tex_desc.Format != DXGI_FORMAT_R16G16B16A16_FLOAT {
            return Err(anyhow!(
                "HDR tonemap: duplication gave {:?}, fall back to GDI",
                tex_desc.Format
            ));
        }
        tracing::info!("HDR tonemap: capture {}x{}", width, height);

        // 5. tonemap into an 8-bit sRGB BGRA texture of the same size
        let out_tex = hdrkit::Tonemap::new(&device, panel)?.apply(&desktop_tex)?;

        // 6. CPU readback: staging texture → Map → BGRA→RGBA → RgbaImage
        let staging = staging_copy(&device, &context, &out_tex)?;
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
        let _unmap = UnmapGuard {
            ctx: &context,
            tex: &staging,
        };

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

        tracing::info!("HDR tonemap: total {}ms", t0.elapsed().as_millis());

        RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
    }
}

unsafe fn staging_copy(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging: Option<ID3D11Texture2D> = None;
    device.CreateTexture2D(&desc, None, Some(&mut staging))?;
    let staging = staging.ok_or_else(|| anyhow!("CreateTexture2D staging null"))?;
    context.CopyResource(&staging, texture);
    Ok(staging)
}

// first, middle and last rows all zero
unsafe fn is_black_frame(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> bool {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    let (w, h) = (desc.Width as usize, desc.Height as usize);
    if w == 0 || h == 0 {
        return false;
    }
    let Ok(staging) = staging_copy(device, context, texture) else {
        return false;
    };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    if context
        .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .is_err()
    {
        return false;
    }
    let src = mapped.pData as *const u8;
    let row_pitch = mapped.RowPitch as usize;
    let row_bytes = w * 8;
    let black = !src.is_null()
        && row_pitch >= row_bytes
        && [0, h / 2, h - 1].iter().all(|&y| {
            std::slice::from_raw_parts(src.add(row_pitch * y), row_bytes)
                .iter()
                .all(|&b| b == 0)
        });
    context.Unmap(&staging, 0);
    black
}

// diagnostic: capture the HDR monitor at `target` through the GPU tonemap
// and write it to `outdir`, to compare against the GDI and WGC shots
pub fn capture_hdr_to_sdr_sweep(target: Option<(i32, i32)>, outdir: &str) -> Result<()> {
    std::fs::create_dir_all(outdir)?;
    let path = format!("{outdir}/tonemap.png");
    capture_hdr_to_sdr(target)?.save(&path)?;
    eprintln!("sweep: wrote {path}");
    Ok(())
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
