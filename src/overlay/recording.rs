#![allow(dead_code)]

use crate::capture::Rectangle;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Instant;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW,
                Ellipse, EndPaint, FillRect, FrameRect, GetMonitorInfoW, GetStockObject,
                InvalidateRect, MonitorFromRect, Rectangle as GdiRectangle, ScreenToClient,
                SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
                DEFAULT_CHARSET, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_SEMIBOLD, HOLLOW_BRUSH,
                MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID,
                TRANSPARENT,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
                GetCursorPos, GetMessageW, KillTimer, LoadCursorW, PostMessageW, RegisterClassW,
                SetCursor, SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage,
                CS_HREDRAW, CS_VREDRAW, IDC_ARROW, IDC_HAND, LWA_COLORKEY, MSG, SW_HIDE, SW_SHOWNA,
                WM_DESTROY, WM_LBUTTONUP, WM_PAINT, WM_SETCURSOR, WM_TIMER, WM_USER, WNDCLASSW,
                WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    };

    const WM_STOP_OVERLAY: u32 = WM_USER + 1;
    const BORDER_WIDTH: i32 = 4;
    const BORDER_TIMER_ID: usize = 1;
    const BAR_TIMER_ID: usize = 2;
    const FLASH_INTERVAL_MS: u32 = 500;
    const BAR_REFRESH_MS: u32 = 250;
    // logical (96-dpi) control bar metrics, scaled by the monitor dpi at runtime
    const BAR_W: i32 = 176;
    const BAR_H: i32 = 32;
    const BAR_GAP: i32 = 6;
    const STOP_W: i32 = 52;

    static OVERLAY_HWND: Mutex<Option<isize>> = Mutex::new(None);
    static CONTROL_HWND: Mutex<Option<isize>> = Mutex::new(None);
    static REGION_X: AtomicI32 = AtomicI32::new(0);
    static REGION_Y: AtomicI32 = AtomicI32::new(0);
    static REGION_W: AtomicI32 = AtomicI32::new(0);
    static REGION_H: AtomicI32 = AtomicI32::new(0);
    static FLASH_STATE: AtomicBool = AtomicBool::new(true);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static BAR_DPI: AtomicI32 = AtomicI32::new(96);
    static MAX_SECS: AtomicU64 = AtomicU64::new(0);
    static START_TIME: Mutex<Option<Instant>> = Mutex::new(None);
    type StopCallback = Box<dyn Fn() + Send>;
    static ON_STOP: Mutex<Option<StopCallback>> = Mutex::new(None);

    pub fn start(region: Rectangle, max_secs: u64, on_stop: StopCallback) {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }

        REGION_X.store(region.x, Ordering::SeqCst);
        REGION_Y.store(region.y, Ordering::SeqCst);
        REGION_W.store(region.width as i32, Ordering::SeqCst);
        REGION_H.store(region.height as i32, Ordering::SeqCst);
        FLASH_STATE.store(true, Ordering::SeqCst);
        MAX_SECS.store(max_secs, Ordering::SeqCst);
        *START_TIME.lock().unwrap() = Some(Instant::now());
        *ON_STOP.lock().unwrap() = Some(on_stop);

        thread::spawn(|| {
            run_overlay_loop();
        });
    }

    pub fn stop() {
        if !RUNNING.load(Ordering::SeqCst) {
            return;
        }

        if let Some(hwnd) = *OVERLAY_HWND.lock().unwrap() {
            unsafe {
                let _ = PostMessageW(HWND(hwnd as *mut _), WM_STOP_OVERLAY, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn fire_stop_callback() {
        let cb = ON_STOP.lock().unwrap().take();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn scaled(v: i32) -> i32 {
        v * BAR_DPI.load(Ordering::SeqCst) / 96
    }

    fn stop_button_rect(client: &RECT) -> RECT {
        let margin = scaled(5);
        RECT {
            left: client.right - scaled(STOP_W) - margin,
            top: client.top + margin,
            right: client.right - margin,
            bottom: client.bottom - margin,
        }
    }

    fn elapsed_label() -> Vec<u16> {
        let elapsed = START_TIME
            .lock()
            .unwrap()
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let max = MAX_SECS.load(Ordering::SeqCst);
        let fmt = |s: u64| format!("{:02}:{:02}", s / 60, s % 60);
        let text = if max > 0 {
            format!("{} / {}", fmt(elapsed.min(max)), fmt(max))
        } else {
            fmt(elapsed)
        };
        text.encode_utf16().collect()
    }

    // bar sits right-aligned under the recorded region so it never appears in
    // the frames; falls back to above the region, then clamps to the work area
    fn bar_placement(region_rect: &RECT, bar_w: i32, bar_h: i32) -> (i32, i32) {
        let mut x = region_rect.right - bar_w;
        let mut y = region_rect.bottom + BORDER_WIDTH + scaled(BAR_GAP);

        unsafe {
            let monitor = MonitorFromRect(region_rect, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let work = info.rcWork;
                if y + bar_h > work.bottom {
                    y = region_rect.top - BORDER_WIDTH - scaled(BAR_GAP) - bar_h;
                }
                if y < work.top {
                    y = work.bottom - bar_h;
                }
                x = x.clamp(work.left, (work.right - bar_w).max(work.left));
            }
        }

        (x, y)
    }

    fn run_overlay_loop() {
        unsafe {
            let instance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(i) => i,
                Err(_) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let border_class: Vec<u16> = "RecordingOverlayClass\0".encode_utf16().collect();
            let bar_class: Vec<u16> = "RecordingControlClass\0".encode_utf16().collect();
            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);

            let wc_border = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(border_class.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc_border);

            let wc_bar = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(control_wnd_proc),
                hInstance: hinstance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: PCWSTR(bar_class.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc_bar);

            let rx = REGION_X.load(Ordering::SeqCst);
            let ry = REGION_Y.load(Ordering::SeqCst);
            let rw = REGION_W.load(Ordering::SeqCst);
            let rh = REGION_H.load(Ordering::SeqCst);

            let x = rx - BORDER_WIDTH;
            let y = ry - BORDER_WIDTH;
            let w = rw + BORDER_WIDTH * 2;
            let h = rh + BORDER_WIDTH * 2;

            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                PCWSTR(border_class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                None,
                None,
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(_) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            };

            *OVERLAY_HWND.lock().unwrap() = Some(hwnd.0 as isize);

            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0x00010101), 255, LWA_COLORKEY);

            let _ = ShowWindow(hwnd, SW_SHOWNA);
            let _ = SetTimer(hwnd, BORDER_TIMER_ID, FLASH_INTERVAL_MS, None);

            // dpi of the monitor hosting the region drives the bar's pixel size
            let region_rect = RECT {
                left: rx,
                top: ry,
                right: rx + rw,
                bottom: ry + rh,
            };
            let monitor = MonitorFromRect(&region_rect, MONITOR_DEFAULTTONEAREST);
            let mut dpi_x = 96u32;
            let mut dpi_y = 96u32;
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
                BAR_DPI.store(dpi_x.max(96) as i32, Ordering::SeqCst);
            }

            let bar_w = scaled(BAR_W);
            let bar_h = scaled(BAR_H);
            let (bar_x, bar_y) = bar_placement(&region_rect, bar_w, bar_h);

            let bar_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PCWSTR(bar_class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                bar_x,
                bar_y,
                bar_w,
                bar_h,
                None,
                None,
                hinstance,
                None,
            )
            .ok();

            if let Some(bar) = bar_hwnd {
                *CONTROL_HWND.lock().unwrap() = Some(bar.0 as isize);
                let _ = ShowWindow(bar, SW_SHOWNA);
                let _ = SetTimer(bar, BAR_TIMER_ID, BAR_REFRESH_MS, None);
            }

            let mut msg = MSG::default();
            while RUNNING.load(Ordering::SeqCst) {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.message == WM_STOP_OVERLAY {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    break;
                }
            }

            KillTimer(hwnd, BORDER_TIMER_ID).ok();
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = DestroyWindow(hwnd);
            if let Some(bar) = bar_hwnd {
                KillTimer(bar, BAR_TIMER_ID).ok();
                let _ = ShowWindow(bar, SW_HIDE);
                let _ = DestroyWindow(bar);
            }
            *OVERLAY_HWND.lock().unwrap() = None;
            *CONTROL_HWND.lock().unwrap() = None;
            *START_TIME.lock().unwrap() = None;
            *ON_STOP.lock().unwrap() = None;
            RUNNING.store(false, Ordering::SeqCst);
        }
    }

    unsafe extern "system" fn overlay_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let w = REGION_W.load(Ordering::SeqCst) + BORDER_WIDTH * 2;
                let h = REGION_H.load(Ordering::SeqCst) + BORDER_WIDTH * 2;

                let bg_brush = CreateSolidBrush(COLORREF(0x00010101));
                let bg_rect = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                FillRect(hdc, &bg_rect, bg_brush);
                let _ = DeleteObject(bg_brush);

                if FLASH_STATE.load(Ordering::SeqCst) {
                    let red = COLORREF(0x000000FF);
                    let pen = CreatePen(PS_SOLID, BORDER_WIDTH, red);
                    let old_pen = SelectObject(hdc, pen);
                    let hollow = GetStockObject(HOLLOW_BRUSH);
                    let old_brush = SelectObject(hdc, hollow);
                    SetBkMode(hdc, TRANSPARENT);

                    let half = BORDER_WIDTH / 2;
                    let _ = GdiRectangle(hdc, half, half, w - half, h - half);

                    SelectObject(hdc, old_pen);
                    SelectObject(hdc, old_brush);
                    let _ = DeleteObject(pen);
                }

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == BORDER_TIMER_ID {
                    let current = FLASH_STATE.load(Ordering::SeqCst);
                    FLASH_STATE.store(!current, Ordering::SeqCst);
                    // erase=false: WM_PAINT already FillRects the entire client
                    // area each frame, so letting GDI erase to white first just
                    // adds a flicker frame.
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_STOP_OVERLAY => {
                RUNNING.store(false, Ordering::SeqCst);
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                LRESULT(0)
            }
            WM_DESTROY => {
                RUNNING.store(false, Ordering::SeqCst);
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe extern "system" fn control_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);

                let bg = CreateSolidBrush(COLORREF(0x001A1A1A));
                FillRect(hdc, &client, bg);
                let _ = DeleteObject(bg);
                let frame = CreateSolidBrush(COLORREF(0x003C3C3C));
                FrameRect(hdc, &client, frame);
                let _ = DeleteObject(frame);

                // flashing red recording dot
                if FLASH_STATE.load(Ordering::SeqCst) {
                    let dot = CreateSolidBrush(COLORREF(0x003C3CE6));
                    let old_brush = SelectObject(hdc, dot);
                    let pen = CreatePen(PS_SOLID, 1, COLORREF(0x003C3CE6));
                    let old_pen = SelectObject(hdc, pen);
                    let cy = (client.bottom - client.top) / 2;
                    let r = scaled(5);
                    let cx = scaled(13);
                    let _ = Ellipse(hdc, cx - r, cy - r, cx + r, cy + r);
                    SelectObject(hdc, old_pen);
                    SelectObject(hdc, old_brush);
                    let _ = DeleteObject(pen);
                    let _ = DeleteObject(dot);
                }

                let font_name: Vec<u16> = "Consolas\0".encode_utf16().collect();
                let font = CreateFontW(
                    -scaled(13),
                    0,
                    0,
                    0,
                    FW_SEMIBOLD.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32,
                    CLEARTYPE_QUALITY.0 as u32,
                    0,
                    PCWSTR(font_name.as_ptr()),
                );
                let old_font = SelectObject(hdc, font);
                SetBkMode(hdc, TRANSPARENT);

                // elapsed / max time
                SetTextColor(hdc, COLORREF(0x00E6E6E6));
                let mut label = elapsed_label();
                let stop = stop_button_rect(&client);
                let mut time_rect = RECT {
                    left: scaled(24),
                    top: client.top,
                    right: stop.left - scaled(4),
                    bottom: client.bottom,
                };
                DrawTextW(
                    hdc,
                    &mut label,
                    &mut time_rect,
                    DT_SINGLELINE | DT_VCENTER | DT_CENTER,
                );

                // stop button
                let btn_bg = CreateSolidBrush(COLORREF(0x00282828));
                FillRect(hdc, &stop, btn_bg);
                let _ = DeleteObject(btn_bg);
                let btn_frame = CreateSolidBrush(COLORREF(0x00505050));
                FrameRect(hdc, &stop, btn_frame);
                let _ = DeleteObject(btn_frame);
                SetTextColor(hdc, COLORREF(0x00F5F5F5));
                let mut stop_label: Vec<u16> = "■ stop".encode_utf16().collect();
                let mut stop_text_rect = stop;
                DrawTextW(
                    hdc,
                    &mut stop_label,
                    &mut stop_text_rect,
                    DT_SINGLELINE | DT_VCENTER | DT_CENTER,
                );

                SelectObject(hdc, old_font);
                let _ = DeleteObject(font);

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == BAR_TIMER_ID {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let stop = stop_button_rect(&client);
                if x >= stop.left && x < stop.right && y >= stop.top && y < stop.bottom {
                    fire_stop_callback();
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let stop = stop_button_rect(&client);
                let over_button = pt.x >= stop.left
                    && pt.x < stop.right
                    && pt.y >= stop.top
                    && pt.y < stop.bottom;
                if let Ok(cursor) =
                    LoadCursorW(None, if over_button { IDC_HAND } else { IDC_ARROW })
                {
                    SetCursor(cursor);
                }
                LRESULT(1)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(not(windows))]
mod fallback_impl {
    use super::*;

    pub fn start(_region: Rectangle, _max_secs: u64, _on_stop: Box<dyn Fn() + Send>) {}
    pub fn stop() {}
}

pub struct RecordingOverlay;

impl RecordingOverlay {
    #[cfg(windows)]
    pub fn start(region: Rectangle, max_secs: u64, on_stop: Box<dyn Fn() + Send>) {
        windows_impl::start(region, max_secs, on_stop);
    }

    #[cfg(not(windows))]
    pub fn start(region: Rectangle, max_secs: u64, on_stop: Box<dyn Fn() + Send>) {
        fallback_impl::start(region, max_secs, on_stop);
    }

    #[cfg(windows)]
    pub fn stop() {
        windows_impl::stop();
    }

    #[cfg(not(windows))]
    pub fn stop() {
        fallback_impl::stop();
    }
}
