use anyhow::{anyhow, Result};
use image::RgbaImage;
use windows::Win32::Graphics::Gdi::{
    GetDC, CreateCompatibleDC, CreateDIBSection, SelectObject, BitBlt,
    GetDIBits, DeleteDC, DeleteObject, ReleaseDC, DIB_RGB_COLORS,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, SRCCOPY, CAPTUREBLT, HBITMAP,
};
use windows::Win32::Foundation::HWND;

fn create_32bpp_dib(width: i32, height: i32) -> Option<HBITMAP> {
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = width;
    bi.bmiHeader.biHeight = -height; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    unsafe {
        CreateDIBSection(
            windows::Win32::Graphics::Gdi::HDC::default(),
            &bi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            None,
            0,
        )
    }
    .ok()
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
        let bitmap = create_32bpp_dib(width as i32, height as i32)
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

        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = width as i32;
        bi.bmiHeader.biHeight = -(height as i32); // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;

        let pixel_count = (width as usize) * (height as usize);
        let mut bgra_data = vec![0u8; pixel_count * 4];

        let scanlines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            height,
            Some(bgra_data.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);

        if scanlines == 0 {
            return Err(anyhow!("GetDIBits failed"));
        }

        // Convert BGRA -> RGBA in place
        for chunk in bgra_data.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
        }

        RgbaImage::from_raw(width, height, bgra_data)
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

