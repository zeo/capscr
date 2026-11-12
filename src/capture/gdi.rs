use anyhow::{anyhow, Result};
use image::RgbaImage;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};

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
    let hbmp =
        unsafe { CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) }.ok()?;
    if hbmp.is_invalid() {
        return None;
    }
    if bits_ptr.is_null() {
        unsafe {
            let _ = DeleteObject(hbmp.into());
        }
        return None;
    }
    Some((hbmp, bits_ptr))
}

pub(crate) struct GdiRegionGrabber {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits_ptr: *mut std::ffi::c_void,
    byte_len: usize,
}

impl GdiRegionGrabber {
    pub(crate) fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(anyhow!("invalid GDI capture dimensions {width}x{height}"));
        }
        let byte_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| anyhow!("GDI capture dimensions overflow"))?;
        if byte_len > isize::MAX as usize {
            return Err(anyhow!("GDI capture buffer is too large"));
        }

        unsafe {
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err(anyhow!("GetDC failed"));
            }
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err(anyhow!("CreateCompatibleDC failed"));
            }
            let (bitmap, bits_ptr) = match create_32bpp_dib(width as i32, height as i32) {
                Some(value) => value,
                None => {
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(None, screen_dc);
                    return Err(anyhow!("create_32bpp_dib failed"));
                }
            };
            let old_bitmap = SelectObject(mem_dc, bitmap.into());
            if old_bitmap.is_invalid() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err(anyhow!("SelectObject failed"));
            }

            Ok(Self {
                x,
                y,
                width,
                height,
                screen_dc,
                mem_dc,
                bitmap,
                old_bitmap,
                bits_ptr,
                byte_len,
            })
        }
    }

    pub(crate) fn grab(&mut self) -> Result<RgbaImage> {
        unsafe {
            BitBlt(
                self.mem_dc,
                0,
                0,
                self.width as i32,
                self.height as i32,
                Some(self.screen_dc),
                self.x,
                self.y,
                windows::Win32::Graphics::Gdi::ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
            )
            .map_err(|_| anyhow!("BitBlt failed"))?;

            let _ = GdiFlush();

            let mut rgba_data = vec![0u8; self.byte_len];
            let src = std::slice::from_raw_parts(self.bits_ptr as *const u8, self.byte_len);
            super::par_convert(src, &mut rgba_data, |pixel| {
                [pixel[2], pixel[1], pixel[0], 255]
            });

            RgbaImage::from_raw(self.width, self.height, rgba_data)
                .ok_or_else(|| anyhow!("RgbaImage::from_raw failed"))
        }
    }
}

impl Drop for GdiRegionGrabber {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(None, self.screen_dc);
        }
    }
}

pub fn fast_gdi_capture(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
    GdiRegionGrabber::new(x, y, width, height)?.grab()
}

use super::MonitorInfo;
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, MONITORINFOEXW,
};

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
            None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn persistent_region_capture_rejects_empty_dimensions() {
        assert!(GdiRegionGrabber::new(0, 0, 0, 64).is_err());
        assert!(GdiRegionGrabber::new(0, 0, 64, 0).is_err());
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn persistent_region_capture_produces_requested_frames() {
        let monitor = fast_list_monitors()
            .expect("list monitors")
            .into_iter()
            .find(|monitor| monitor.is_primary)
            .expect("primary monitor");
        let width = monitor.width.min(686);
        let height = monitor.height.min(726);
        let center = (
            monitor.x + monitor.width as i32 / 2,
            monitor.y + monitor.height as i32 / 2,
        );
        let is_hdr = crate::capture::HdrCapture::is_hdr_at_point(center.0, center.1);
        let mut source = GdiRegionGrabber::new(monitor.x, monitor.y, width, height)
            .expect("create persistent region source");

        let started = Instant::now();
        for _ in 0..30 {
            let frame = source.grab().expect("capture region frame");
            assert_eq!(frame.dimensions(), (width, height));
        }
        let elapsed = started.elapsed();
        eprintln!(
            "captured 30 {width}x{height} frames from {}x{} in {:.1}ms ({:.2}ms/frame), hdr={is_hdr}",
            monitor.width,
            monitor.height,
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1000.0 / 30.0
        );
    }
}
