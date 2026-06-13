// captures the system cursor from Win32 and alpha-composites it onto a
// captured RgbaImage at the right screen-relative position. used by the
// capture pipeline when config.capture.show_cursor is enabled, for both still
// captures and recordings. non-windows falls back to a no-op

use image::RgbaImage;

// a snapshot of the system cursor: its bitmap plus the screen-space position
// of its top-left pixel, with the hotspot already subtracted. taken once at
// capture time so a still capture composites the cursor where it was when the
// frame was frozen rather than where the mouse drifted during region selection
pub struct CursorShot {
    image: RgbaImage,
    screen_x: i32,
    screen_y: i32,
}

impl CursorShot {
    pub fn screen_pos(&self) -> (i32, i32) {
        (self.screen_x, self.screen_y)
    }
}

// snapshot the current system cursor. returns None if the cursor is hidden,
// unresolvable, or any Win32 call fails — cursor capture must never take down a
// screen capture. always None off windows
pub fn capture_cursor_shot() -> Option<CursorShot> {
    #[cfg(windows)]
    {
        let (image, screen_x, screen_y) = windows_impl::fetch_cursor()?;
        Some(CursorShot {
            image,
            screen_x,
            screen_y,
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// composite a previously captured cursor onto `image`. `screen_origin` is the
// (x, y) of the image's top-left pixel in virtual desktop coordinates. returns
// true when any cursor pixel falls inside the image bounds
pub fn composite_cursor_shot(
    image: &mut RgbaImage,
    shot: &CursorShot,
    screen_origin: (i32, i32),
) -> bool {
    let rel_x = shot.screen_x - screen_origin.0;
    let rel_y = shot.screen_y - screen_origin.1;
    composite_at(image, &shot.image, rel_x, rel_y)
}

// composite the live system cursor onto `image` at its screen-relative
// position. used by instant captures with no selection overlay, where the
// cursor has not moved since the trigger. quietly no-ops if it can't be grabbed
pub fn composite_system_cursor(image: &mut RgbaImage, screen_origin: (i32, i32)) {
    if let Some(shot) = capture_cursor_shot() {
        composite_cursor_shot(image, &shot, screen_origin);
    }
}

fn composite_at(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) -> bool {
    let dst_w = dst.width() as i32;
    let dst_h = dst.height() as i32;
    let src_w = src.width() as i32;
    let src_h = src.height() as i32;
    // trim to destination bounds — handles cursors hanging off the capture edge.
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + src_w).min(dst_w);
    let y1 = (y + src_h).min(dst_h);
    if x0 >= x1 || y0 >= y1 {
        return false;
    }
    for dy in y0..y1 {
        for dx in x0..x1 {
            let sx = (dx - x) as u32;
            let sy = (dy - y) as u32;
            let sp = src.get_pixel(sx, sy).0;
            let sa = sp[3] as u16;
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dst.put_pixel(dx as u32, dy as u32, image::Rgba(sp));
                continue;
            }
            let dp = dst.get_pixel(dx as u32, dy as u32).0;
            let inv = 255 - sa;
            let r = ((sp[0] as u16 * sa + dp[0] as u16 * inv) / 255) as u8;
            let g = ((sp[1] as u16 * sa + dp[1] as u16 * inv) / 255) as u8;
            let b = ((sp[2] as u16 * sa + dp[2] as u16 * inv) / 255) as u8;
            let a = dp[3].max(sp[3]);
            dst.put_pixel(dx as u32, dy as u32, image::Rgba([r, g, b, a]));
        }
    }
    true
}

#[cfg(windows)]
mod windows_impl {
    use image::RgbaImage;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CopyIcon, DestroyIcon, DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING,
        DI_NORMAL, ICONINFO,
    };

    pub fn fetch_cursor() -> Option<(RgbaImage, i32, i32)> {
        unsafe {
            let mut info = CURSORINFO {
                cbSize: std::mem::size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut info).is_err() {
                return None;
            }
            if info.flags.0 & CURSOR_SHOWING.0 == 0 {
                return None;
            }
            if info.hCursor.is_invalid() {
                return None;
            }

            // CopyIcon hands us an owned HICON we can pass to GetIconInfo
            // without racing against the cursor handle the OS might recycle.
            let hicon = CopyIcon(info.hCursor).ok()?;
            // RAII so we never leak the icon on any early return below.
            struct IconGuard(windows::Win32::UI::WindowsAndMessaging::HICON);
            impl Drop for IconGuard {
                fn drop(&mut self) {
                    unsafe {
                        let _ = DestroyIcon(self.0);
                    }
                }
            }
            let _icon_guard = IconGuard(hicon);

            let mut icon_info = ICONINFO::default();
            if GetIconInfo(hicon, &mut icon_info).is_err() {
                return None;
            }
            // both bitmaps need cleanup regardless of which one we use to size.
            struct BmpGuard(HBITMAP);
            impl Drop for BmpGuard {
                fn drop(&mut self) {
                    if !self.0.is_invalid() {
                        unsafe {
                            let _ = DeleteObject(self.0);
                        }
                    }
                }
            }
            let _color_guard = BmpGuard(icon_info.hbmColor);
            let _mask_guard = BmpGuard(icon_info.hbmMask);

            // cursor dimensions: when hbmColor is set the cursor is a true
            // 32-bit icon; when only hbmMask is set the cursor is a 1-bit
            // AND/XOR mask stacked vertically (so height is doubled).
            let (width, height) = {
                use windows::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
                let bmp_handle = if !icon_info.hbmColor.is_invalid() {
                    icon_info.hbmColor
                } else {
                    icon_info.hbmMask
                };
                let mut bmp = BITMAP::default();
                let n = GetObjectW(
                    bmp_handle,
                    std::mem::size_of::<BITMAP>() as i32,
                    Some(&mut bmp as *mut _ as *mut _),
                );
                if n == 0 || bmp.bmWidth <= 0 || bmp.bmHeight == 0 {
                    return None;
                }
                let mut h = bmp.bmHeight.unsigned_abs();
                if icon_info.hbmColor.is_invalid() {
                    // AND+XOR stacked → halve to get cursor height.
                    h /= 2;
                }
                let w = bmp.bmWidth.unsigned_abs();
                // cap pathological sizes so we never allocate a runaway buffer
                // on a custom-driver cursor.
                if w == 0 || h == 0 || w > 256 || h > 256 {
                    return None;
                }
                (w, h)
            };

            // create a 32-bit DIB section, draw the cursor into it via
            // DrawIconEx (which handles AND-mask + XOR for arrow cursors and
            // alpha for modern Aero ones), then read the bits out.
            let screen_dc = GetDC(HWND::default());
            if screen_dc.is_invalid() {
                return None;
            }
            struct DcGuard(HDC);
            impl Drop for DcGuard {
                fn drop(&mut self) {
                    unsafe {
                        ReleaseDC(HWND::default(), self.0);
                    }
                }
            }
            let _screen_guard = DcGuard(screen_dc);

            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_invalid() {
                return None;
            }
            struct MemDcGuard(HDC);
            impl Drop for MemDcGuard {
                fn drop(&mut self) {
                    unsafe {
                        let _ = DeleteDC(self.0);
                    }
                }
            }
            let _mem_guard = MemDcGuard(mem_dc);

            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width as i32;
            // negative height → top-down DIB so the byte order matches RgbaImage.
            bmi.bmiHeader.biHeight = -(height as i32);
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut pixels_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
            let dib =
                CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut pixels_ptr, None, 0).ok()?;
            let _dib_guard = BmpGuard(dib);
            if pixels_ptr.is_null() {
                return None;
            }

            let old = SelectObject(mem_dc, dib);
            if old.is_invalid() {
                return None;
            }

            // zero the buffer ourselves — CreateDIBSection initialises to zero
            // on Windows, but DrawIconEx only writes the visible portion of an
            // alpha cursor, so untouched pixels stay alpha=0 (transparent).

            if DrawIconEx(
                mem_dc,
                0,
                0,
                hicon,
                width as i32,
                height as i32,
                0,
                None,
                DI_NORMAL,
            )
            .is_err()
            {
                SelectObject(mem_dc, old);
                return None;
            }
            SelectObject(mem_dc, old);

            // read the DIB pixels into an owned Vec. They're in BGRA order;
            // convert to RGBA for the RgbaImage.
            let stride = (width as usize) * 4;
            let total = stride * (height as usize);
            let src_slice = std::slice::from_raw_parts(pixels_ptr as *const u8, total);
            let mut rgba = Vec::with_capacity(total);
            for chunk in src_slice.chunks_exact(4) {
                rgba.push(chunk[2]); // R
                rgba.push(chunk[1]); // G
                rgba.push(chunk[0]); // B
                rgba.push(chunk[3]); // A
            }

            // for non-alpha cursors (the classic black-and-white arrow on
            // pre-Aero apps) DrawIconEx leaves alpha=0 everywhere despite
            // having drawn opaque pixels. Detect that and force alpha=255 on
            // any non-black pixel as a heuristic recovery — better than an
            // invisible cursor.
            let any_alpha = rgba.chunks_exact(4).any(|p| p[3] != 0);
            if !any_alpha {
                for p in rgba.chunks_exact_mut(4) {
                    if p[0] != 0 || p[1] != 0 || p[2] != 0 {
                        p[3] = 255;
                    }
                }
            }

            let img = RgbaImage::from_raw(width, height, rgba)?;
            // subtract the hotspot so the rendered cursor lines up with the
            // tip / hot pixel rather than the bitmap's top-left corner.
            let screen_x = info.ptScreenPos.x - icon_info.xHotspot as i32;
            let screen_y = info.ptScreenPos.y - icon_info.yHotspot as i32;
            Some((img, screen_x, screen_y))
        }
    }
}
