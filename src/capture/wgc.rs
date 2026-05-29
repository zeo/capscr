// Windows.Graphics.Capture (WGC) HDR-aware screen capture.
//
// Captures via WGC with B8G8R8A8 output, so the OS does HDR-to-SDR tonemap
// on the GPU before handing pixels back. No CPU-side tonemap pipeline, no
// per-pixel powf, no per-frame quickselect — captures complete in 10-30ms
// regardless of dev/release build, regardless of HDR content brightness.
// The trade-off vs the previous DXGI Desktop Duplication + custom Reinhard
// path: we get the OS's tonemap quality (matches Snipping Tool / Game Bar)
// instead of our own, and we can't tune it. for screen-capture content
// that's the right trade — speed + OS consistency over a custom look.

use anyhow::{anyhow, Result};
use image::RgbaImage;
use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

// Capture a single frame of the given monitor. WGC's frame pool naturally
// gives us one frame per acquisition; we stop the session right after.
// Convenience wrapper: resolve the HMONITOR for a desktop coordinate and
// capture that monitor. used by RegionCapture / ScreenCapture which know
// the selection's (x, y) but not the HMONITOR handle.
pub fn capture_at_point(x: i32, y: i32) -> Result<RgbaImage> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) };
    if hmon.is_invalid() {
        return Err(anyhow!("no monitor at point ({x}, {y})"));
    }
    capture_monitor(hmon)
}

pub fn capture_window(hwnd: HWND) -> Result<RgbaImage> {
    unsafe {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| anyhow!("get IGraphicsCaptureItemInterop: {e}"))?;
        let item: GraphicsCaptureItem = interop
            .CreateForWindow(hwnd)
            .map_err(|e| anyhow!("CreateForWindow: {e}"))?;
        capture_item(item)
    }
}

pub fn capture_monitor(hmonitor: HMONITOR) -> Result<RgbaImage> {
    unsafe {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| anyhow!("get IGraphicsCaptureItemInterop: {e}"))?;
        let item: GraphicsCaptureItem = interop
            .CreateForMonitor(hmonitor)
            .map_err(|e| anyhow!("CreateForMonitor: {e}"))?;
        capture_item(item)
    }
}

fn capture_item(item: GraphicsCaptureItem) -> Result<RgbaImage> {
    unsafe {
        let size = item.Size().map_err(|e| anyhow!("item.Size: {e}"))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(anyhow!("monitor item has zero size"));
        }

        // 2. D3D11 device + IDirect3DDevice wrapper. WGC requires the
        //    WinRT-flavoured device interface, which wraps our raw D3D11
        //    device via CreateDirect3D11DeviceFromDXGIDevice.
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| anyhow!("D3D11CreateDevice: {e}"))?;
        let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice returned null device"))?;
        let context =
            context.ok_or_else(|| anyhow!("D3D11CreateDevice returned null context"))?;

        let dxgi_device: IDXGIDevice = device.cast().map_err(|e| anyhow!("cast IDXGIDevice: {e}"))?;
        let winrt_device_inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|e| anyhow!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))?;
        let winrt_device: IDirect3DDevice = winrt_device_inspectable
            .cast()
            .map_err(|e| anyhow!("cast IDirect3DDevice: {e}"))?;

        // 3. frame pool with B8G8R8A8 — this is what triggers OS-side HDR
        //    tonemap. requesting R16G16B16A16Float would give us raw scRGB
        //    instead (and we'd be back in CPU-tonemap land).
        let pool = Direct3D11CaptureFramePool::Create(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| anyhow!("FramePool::Create: {e}"))?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| anyhow!("CreateCaptureSession: {e}"))?;

        // suppress capscr's own cursor in the capture — the user is
        // pressing a hotkey, the cursor in the result is rarely wanted.
        let _ = session.SetIsCursorCaptureEnabled(false);

        session.StartCapture().map_err(|e| anyhow!("StartCapture: {e}"))?;

        // poll for a non-black frame as WGC's frame pool is asynchronous
        // and may occasionally deliver all-zero pixels in the first frame
        // so we retry up to 10 attempts to get a valid non-black frame
        let mut rgba = None;
        let mut width = 0;
        let mut height = 0;
        let mut acquired = false;

        'retry: for attempt in 0..10 {
            let mut frame = None;
            for _ in 0..30 {
                match pool.TryGetNextFrame() {
                    Ok(f) => {
                        frame = Some(f);
                        break;
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
            }

            let frame = match frame {
                Some(f) => f,
                None => {
                    tracing::warn!("WGC: TryGetNextFrame returned None on attempt {attempt}");
                    continue 'retry;
                }
            };

            let surface = match frame.Surface() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("WGC: frame.Surface failed on attempt {attempt}: {e}");
                    continue 'retry;
                }
            };
            let access: IDirect3DDxgiInterfaceAccess = match surface.cast() {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("WGC: cast IDirect3DDxgiInterfaceAccess failed on attempt {attempt}: {e}");
                    continue 'retry;
                }
            };
            let frame_texture: ID3D11Texture2D = match access.GetInterface() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("WGC: GetInterface ID3D11Texture2D failed on attempt {attempt}: {e}");
                    continue 'retry;
                }
            };

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            frame_texture.GetDesc(&mut desc);
            let w = desc.Width;
            let h = desc.Height;

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: desc.SampleDesc,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: Default::default(),
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: Default::default(),
            };

            let mut staging: Option<ID3D11Texture2D> = None;
            if let Err(e) = device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) {
                tracing::warn!("WGC: CreateTexture2D staging failed on attempt {attempt}: {e}");
                continue 'retry;
            }
            let staging = match staging {
                Some(s) => s,
                None => {
                    tracing::warn!("WGC: staging texture was null on attempt {attempt}");
                    continue 'retry;
                }
            };

            context.CopyResource(&staging, &frame_texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            if let Err(e) = context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) {
                tracing::warn!("WGC: Map staging failed on attempt {attempt}: {e}");
                continue 'retry;
            }

            let row_pitch = mapped.RowPitch as usize;
            let src_ptr = mapped.pData as *const u8;
            if src_ptr.is_null() {
                context.Unmap(&staging, 0);
                tracing::warn!("WGC: Map returned null pointer on attempt {attempt}");
                continue 'retry;
            }

            let row_bytes = (w as usize) * 4;
            if row_pitch < row_bytes {
                context.Unmap(&staging, 0);
                tracing::warn!("WGC: row_pitch < row_bytes on attempt {attempt}");
                continue 'retry;
            }

            // check if the frame is completely black (all-zeros)
            let is_zero = {
                let first_row = std::slice::from_raw_parts(src_ptr, row_bytes);
                first_row.iter().all(|&b| b == 0) && {
                    let mid_row = src_ptr.add(row_pitch * (h as usize / 2));
                    let mid_slice = std::slice::from_raw_parts(mid_row, row_bytes);
                    mid_slice.iter().all(|&b| b == 0)
                } && {
                    let last_row = src_ptr.add(row_pitch * (h as usize - 1));
                    let last_slice = std::slice::from_raw_parts(last_row, row_bytes);
                    last_slice.iter().all(|&b| b == 0)
                }
            };

            if is_zero {
                context.Unmap(&staging, 0);
                tracing::warn!("WGC: acquired black frame on attempt {attempt}, retrying...");
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue 'retry;
            }

            // swap BGRA -> RGBA on the way out since image::RgbaImage expects RGBA
            let pixel_count = (w as usize) * (h as usize);
            let mut rgba_buf = vec![0u8; pixel_count * 4];
            let thread_count = std::thread::available_parallelism()
                .map(|n| n.get().min(16))
                .unwrap_or(4)
                .max(1);
            let rows_per_chunk = (h as usize).div_ceil(thread_count);
            let src_addr = src_ptr as usize;

            std::thread::scope(|s| {
                for (chunk_idx, dst_chunk) in rgba_buf.chunks_mut(rows_per_chunk * row_bytes).enumerate() {
                    let start_row = chunk_idx * rows_per_chunk;
                    s.spawn(move || {
                        let rows = dst_chunk.len() / row_bytes;
                        for r in 0..rows {
                            let y = start_row + r;
                            let src = (src_addr + y * row_pitch) as *const u8;
                            let dst_row = &mut dst_chunk[r * row_bytes..(r + 1) * row_bytes];
                            for x in 0..(w as usize) {
                                let off = x * 4;
                                dst_row[off]     = *src.add(off + 2);
                                dst_row[off + 1] = *src.add(off + 1);
                                dst_row[off + 2] = *src.add(off);
                                dst_row[off + 3] = *src.add(off + 3);
                            }
                        }
                    });
                }
            });

            context.Unmap(&staging, 0);
            rgba = Some(rgba_buf);
            width = w;
            height = h;
            acquired = true;
            break;
        }

        // cleanup session and pool to avoid leaking GPU resources
        let _ = session.Close();
        let _ = pool.Close();

        if !acquired {
            return Err(anyhow!("WGC: failed to acquire non-black frame after 10 attempts"));
        }

        let rgba_data = rgba.ok_or_else(|| anyhow!("WGC: missing rgba data"))?;
        RgbaImage::from_raw(width, height, rgba_data)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
    }
}
