use anyhow::{anyhow, Result};
use image::RgbaImage;
use windows::Win32::Graphics::Gdi::{
    GetDC, CreateCompatibleDC, CreateDIBSection, SelectObject, BitBlt,
    GdiFlush, DeleteDC, DeleteObject, ReleaseDC, DIB_RGB_COLORS,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, SRCCOPY, CAPTUREBLT, HBITMAP,
};
use windows::Win32::Foundation::HWND;

// returns the DIB section bitmap together with the pointer to its pixel bits,
// so the caller can read the blitted pixels directly (after a GdiFlush) instead
// of paying a second full-frame copy through GetDIBits.
fn create_32bpp_dib(width: i32, height: i32) -> Option<(HBITMAP, *mut std::ffi::c_void)> {
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = width;
    bi.bmiHeader.biHeight = -height; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let hbmp = unsafe {
        CreateDIBSection(
            windows::Win32::Graphics::Gdi::HDC::default(),
            &bi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            None,
            0,
        )
    }
    .ok()?;
    if hbmp.is_invalid() || bits_ptr.is_null() {
        return None;
    }
    Some((hbmp, bits_ptr))
}

pub fn fast_gdi_capture(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
    unsafe {
        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            return Err(anyhow!("GetDC failed"));
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            ReleaseDC(HWND::default(), screen_dc);
            return Err(anyhow!("CreateCompatibleDC failed"));
        }
        let (bitmap, bits_ptr) = create_32bpp_dib(width as i32, height as i32)
            .ok_or_else(|| anyhow!("create_32bpp_dib failed"))?;

        let old_bitmap = SelectObject(mem_dc, bitmap);
        let ok = BitBlt(
            mem_dc,
            0,
            0,
            width as i32,
            height as i32,
            screen_dc,
            x,
            y,
            windows::Win32::Graphics::Gdi::ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
        );

        if ok.is_err() {
            SelectObject(mem_dc, old_bitmap);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            return Err(anyhow!("BitBlt failed"));
        }

        // flush the GDI batch so the BitBlt has finished writing the DIB before
        // the CPU reads its bits directly. reading the DIB section pointer skips
        // the extra full-frame copy GetDIBits would otherwise perform.
        let _ = GdiFlush();

        let pixel_count = (width as usize) * (height as usize);
        let mut rgba_data = vec![0u8; pixel_count * 4];
        // read the blitted BGRA bits straight from the DIB section and write
        // RGBA in the same pass — one copy+swap instead of GetDIBits then swap.
        let src = std::slice::from_raw_parts(bits_ptr as *const u8, pixel_count * 4);
        for (dst, s) in rgba_data.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            dst[0] = s[2]; // R
            dst[1] = s[1]; // G
            dst[2] = s[0]; // B
            // screen BitBlt leaves the alpha byte undefined (usually 0); force
            // opaque here since a desktop capture is always opaque. this is what
            // capture_one_monitor's ensure_opaque pass would do anyway, but
            // setting it inline lets that pass early-return instead of scanning
            // and rewriting the whole frame
            dst[3] = 255;
        }

        SelectObject(mem_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);

        RgbaImage::from_raw(width, height, rgba_data)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
    }
}

use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, HDC, MONITORINFOEXW,
};
use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use super::MonitorInfo;

struct MonitorEnumState {
    monitors: Vec<MonitorInfo>,
    count: u32,
}

pub fn fast_list_monitors() -> Result<Vec<MonitorInfo>> {
    unsafe {
        let mut state = MonitorEnumState {
            monitors: Vec::new(),
            count: 0,
        };

        // EnumDisplayMonitors returns BOOL in windows-rs
        let result = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum_proc),
            LPARAM(&mut state as *mut MonitorEnumState as isize),
        );

        if !result.as_bool() {
            return Err(anyhow!("EnumDisplayMonitors failed"));
        }

        Ok(state.monitors)
    }
}

unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let state = &mut *(lparam.0 as *mut MonitorEnumState);

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

    let ok = GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _);
    if ok.as_bool() {
        let r = info.monitorInfo.rcMonitor;
        let is_primary = (info.monitorInfo.dwFlags & 1) != 0; // MONITORINFOF_PRIMARY = 1
        
        let name_len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
        let name = String::from_utf16_lossy(&info.szDevice[..name_len]);

        state.monitors.push(MonitorInfo {
            id: state.count,
            name,
            x: r.left,
            y: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
            is_primary,
        });
        state.count += 1;
    }

    BOOL(1)
}

