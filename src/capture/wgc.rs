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

#![cfg(windows)]

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

pub fn capture_monitor(hmonitor: HMONITOR) -> Result<RgbaImage> {
    unsafe {
        // 1. capture item for the monitor. requires the interop interface
        //    on the GraphicsCaptureItem ABI.
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| anyhow!("get IGraphicsCaptureItemInterop: {e}"))?;
        let item: GraphicsCaptureItem = interop
            .CreateForMonitor(hmonitor)
            .map_err(|e| anyhow!("CreateForMonitor: {e}"))?;
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

        // 4. poll for the first frame. WGC's frame pool is asynchronous —
        //    TryGetNextFrame returns None until the compositor delivers
        //    a frame. spin with short sleeps; in practice this resolves
        //    within one display refresh (~16ms at 60Hz, ~8ms at 120Hz).
        let mut frame = None;
        for _ in 0..120 {
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
        let frame = frame.ok_or_else(|| {
            let _ = session.Close();
            let _ = pool.Close();
            anyhow!("WGC: no frame after 240ms")
        })?;

        // 5. extract the underlying ID3D11Texture2D from the WinRT surface.
        let surface = frame.Surface().map_err(|e| anyhow!("frame.Surface: {e}"))?;
        let access: IDirect3DDxgiInterfaceAccess =
            surface.cast().map_err(|e| anyhow!("cast IDirect3DDxgiInterfaceAccess: {e}"))?;
        let frame_texture: ID3D11Texture2D = access
            .GetInterface()
            .map_err(|e| anyhow!("GetInterface ID3D11Texture2D: {e}"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        frame_texture.GetDesc(&mut desc);
        let width = desc.Width;
        let height = desc.Height;

        // 6. copy to a CPU-readable staging texture, map, read bytes.
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
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
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .map_err(|e| anyhow!("CreateTexture2D staging: {e}"))?;
        let staging = staging.ok_or_else(|| anyhow!("staging texture null"))?;

        context.CopyResource(&staging, &frame_texture);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| anyhow!("Map staging: {e}"))?;

        struct UnmapGuard<'a> {
            ctx: &'a ID3D11DeviceContext,
            tex: &'a ID3D11Texture2D,
        }
        impl<'a> Drop for UnmapGuard<'a> {
            fn drop(&mut self) {
                unsafe { self.ctx.Unmap(self.tex, 0) }
            }
        }
        let _guard = UnmapGuard { ctx: &context, tex: &staging };

        let row_pitch = mapped.RowPitch as usize;
        let src_ptr = mapped.pData as *const u8;
        if src_ptr.is_null() {
            return Err(anyhow!("staging Map returned null pointer"));
        }
        let row_bytes = (width as usize) * 4;
        if row_pitch < row_bytes {
            return Err(anyhow!("row_pitch {row_pitch} < row_bytes {row_bytes}"));
        }

        // 7. swap BGRA → RGBA on the way out. WGC delivers B8G8R8A8 in
        //    little-endian order (B in byte 0, G in 1, R in 2, A in 3);
        //    image::RgbaImage expects RGBA.
        let pixel_count = (width as usize) * (height as usize);
        let mut rgba = vec![0u8; pixel_count * 4];
        for y in 0..(height as usize) {
            let src_row = src_ptr.add(y * row_pitch);
            let dst_row_offset = y * row_bytes;
            for x in 0..(width as usize) {
                let s = src_row.add(x * 4);
                let d = dst_row_offset + x * 4;
                rgba[d]     = *s.add(2); // R from byte 2
                rgba[d + 1] = *s.add(1); // G from byte 1
                rgba[d + 2] = *s.add(0); // B from byte 0
                rgba[d + 3] = *s.add(3); // A from byte 3
            }
        }

        // 8. cleanup. closing the session + pool is critical or WGC will
        //    leak GPU resources and the next capture-active border will
        //    keep showing.
        drop(_guard);
        let _ = session.Close();
        let _ = pool.Close();

        RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
    }
}
