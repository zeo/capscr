use crate::capture::{MonitorInfo, Rectangle};

const LEFT: u8 = 1;
const TOP: u8 = 2;
const RIGHT: u8 = 4;
const BOTTOM: u8 = 8;
const MIN_SIDE: i32 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameMode {
    Armed,
    Recording,
    Paused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameEvent {
    Moved(Rectangle),
    Record,
    Cancel,
}

// which edges a point on the frame (client coords of a w x h window with a
// `band` wide border) grabs: corners and edge midpoints resize, the rest of
// the band moves (0)
fn hit_edges(x: i32, y: i32, w: i32, h: i32, band: i32) -> u8 {
    let grip = band * 3;
    let (l, t) = (x < grip, y < grip);
    let (r, b) = (!l && x >= w - grip, !t && y >= h - grip);
    let (mid_x, mid_y) = ((x - w / 2).abs() < grip, (y - h / 2).abs() < grip);
    let mut edges = 0;
    if l && (t || b || mid_y) {
        edges |= LEFT;
    }
    if r && (t || b || mid_y) {
        edges |= RIGHT;
    }
    if t && (l || r || mid_x) {
        edges |= TOP;
    }
    if b && (l || r || mid_x) {
        edges |= BOTTOM;
    }
    edges
}

// the rect after dragging `edges` of `start` by (dx, dy); 0 moves it. with
// keep_aspect the axis that changed more leads and the other follows, staying
// centred when that axis isn't being dragged
fn drag_rect(start: Rectangle, edges: u8, dx: i32, dy: i32, keep_aspect: bool) -> Rectangle {
    if edges == 0 {
        return Rectangle::new(start.x + dx, start.y + dy, start.width, start.height);
    }
    let (w0, h0) = (start.width as i32, start.height as i32);
    let grow = |far: u8, near: u8, d: i32| {
        if edges & far != 0 {
            d
        } else if edges & near != 0 {
            -d
        } else {
            0
        }
    };
    let mut w = (w0 + grow(RIGHT, LEFT, dx)).max(MIN_SIDE);
    let mut h = (h0 + grow(BOTTOM, TOP, dy)).max(MIN_SIDE);
    let horizontal = edges & (LEFT | RIGHT) != 0;
    let vertical = edges & (TOP | BOTTOM) != 0;
    if keep_aspect && h0 > 0 {
        let ratio = w0 as f64 / h0 as f64;
        if horizontal && (!vertical || (w - w0).abs() * h0 >= (h - h0).abs() * w0) {
            h = ((w as f64 / ratio).round() as i32).max(MIN_SIDE);
        } else {
            w = ((h as f64 * ratio).round() as i32).max(MIN_SIDE);
        }
    }
    let place = |origin: i32, old: i32, new: i32, near: u8, dragged: bool| {
        if edges & near != 0 {
            origin + old - new
        } else if dragged {
            origin
        } else {
            origin + (old - new) / 2
        }
    };
    Rectangle::new(
        place(start.x, w0, w, LEFT, horizontal),
        place(start.y, h0, h, TOP, vertical),
        w as u32,
        h as u32,
    )
}

// keeps a rect on screen: a moved rect (edges 0) slides back inside, a resized
// one is cut at the bounds so its fixed edges stay put
pub(crate) fn clamp_into(rect: Rectangle, bounds: Rectangle, edges: u8) -> Rectangle {
    let (bx1, by1) = (
        bounds.x + bounds.width as i32,
        bounds.y + bounds.height as i32,
    );
    if edges == 0 {
        let (w, h) = (rect.width.min(bounds.width), rect.height.min(bounds.height));
        return Rectangle::new(
            rect.x.clamp(bounds.x, bx1 - w as i32),
            rect.y.clamp(bounds.y, by1 - h as i32),
            w,
            h,
        );
    }
    let (x0, y0) = (rect.x.max(bounds.x), rect.y.max(bounds.y));
    let x1 = (rect.x + rect.width as i32).min(bx1);
    let y1 = (rect.y + rect.height as i32).min(by1);
    Rectangle::new(
        x0,
        y0,
        (x1 - x0).max(MIN_SIDE) as u32,
        (y1 - y0).max(MIN_SIDE) as u32,
    )
}

// where the control bar sits relative to the recorded region: below its
// bottom-right corner, then above it. a bar excluded from capture otherwise
// tucks inside the region, invisible to the recording. a visible one keeps
// trying beside the region (right, then left, possibly on a neighbouring
// monitor) before the inside fallback, which there only remains for a region
// covering every monitor
pub(crate) fn bar_position(
    region: Rectangle,
    (bar_w, bar_h): (i32, i32),
    monitors: &[MonitorInfo],
    excluded_from_capture: bool,
) -> (i32, i32) {
    let right = region.x + region.width as i32;
    let bottom = region.y + region.height as i32;
    let x_aligned = (right - bar_w).max(region.x);
    let fits = |x: i32, y: i32| {
        monitors.iter().any(|m| {
            x >= m.x
                && y >= m.y
                && x + bar_w <= m.x + m.width as i32
                && y + bar_h <= m.y + m.height as i32
        })
    };
    let mut spots = vec![(x_aligned, bottom + 8), (x_aligned, region.y - bar_h - 8)];
    if !excluded_from_capture {
        spots.push((right + 8, bottom - bar_h));
        spots.push((region.x - bar_w - 8, bottom - bar_h));
    }
    spots
        .into_iter()
        .find(|&(x, y)| fits(x, y))
        .unwrap_or((x_aligned, bottom - bar_h - 8))
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    const BAR: (i32, i32) = (184, 36);

    fn mon(x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo {
            id: 0,
            name: "m".into(),
            x,
            y,
            width: w,
            height: h,
            is_primary: true,
        }
    }

    #[test]
    fn band_regions_map_to_edges() {
        // 400x300 window, 6px band, 18px grips
        assert_eq!(hit_edges(2, 2, 400, 300, 6), LEFT | TOP);
        assert_eq!(hit_edges(397, 297, 400, 300, 6), RIGHT | BOTTOM);
        assert_eq!(hit_edges(2, 150, 400, 300, 6), LEFT);
        assert_eq!(hit_edges(200, 297, 400, 300, 6), BOTTOM);
        assert_eq!(hit_edges(100, 2, 400, 300, 6), 0);
        assert_eq!(hit_edges(2, 80, 400, 300, 6), 0);
    }

    #[test]
    fn tiny_frame_grips_never_claim_both_sides() {
        let edges = hit_edges(30, 30, 56, 56, 12);
        assert_eq!(edges & (LEFT | RIGHT), LEFT);
        assert_eq!(edges & (TOP | BOTTOM), TOP);
    }

    #[test]
    fn dragging_moves_and_resizes() {
        let start = Rectangle::new(100, 100, 400, 300);
        assert_eq!(
            drag_rect(start, 0, 5, -5, false),
            Rectangle::new(105, 95, 400, 300)
        );
        assert_eq!(
            drag_rect(start, LEFT | TOP, -10, 20, false),
            Rectangle::new(90, 120, 410, 280)
        );
        assert_eq!(
            drag_rect(start, RIGHT, -1000, 0, false),
            Rectangle::new(100, 100, MIN_SIDE as u32, 300)
        );
    }

    #[test]
    fn shift_drag_keeps_the_aspect_ratio() {
        let start = Rectangle::new(100, 100, 400, 300);
        assert_eq!(
            drag_rect(start, RIGHT | BOTTOM, 200, 10, true),
            Rectangle::new(100, 100, 600, 450)
        );
        // a side edge grows the other axis around the centre
        assert_eq!(
            drag_rect(start, RIGHT, 200, 0, true),
            Rectangle::new(100, 25, 600, 450)
        );
    }

    #[test]
    fn moves_slide_back_and_resizes_crop() {
        let screen = Rectangle::new(0, 0, 1920, 1080);
        assert_eq!(
            clamp_into(Rectangle::new(-50, 900, 400, 300), screen, 0),
            Rectangle::new(0, 780, 400, 300)
        );
        assert_eq!(
            clamp_into(Rectangle::new(-50, 100, 400, 300), screen, LEFT),
            Rectangle::new(0, 100, 350, 300)
        );
    }

    #[test]
    fn sits_below_when_there_is_room() {
        let region = Rectangle::new(100, 100, 400, 200);
        let (x, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], false);
        assert_eq!(y, 308); // 100 + 200 + 8
        assert_eq!(x, 316); // right edge 500 - 184
    }

    #[test]
    fn flips_above_when_no_room_below() {
        // region hugs the monitor's bottom edge
        let region = Rectangle::new(100, 800, 400, 280);
        let (_, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], false);
        assert_eq!(y, 756); // 800 - 36 - 8
    }

    #[test]
    fn sits_beside_full_height_region() {
        let region = Rectangle::new(0, 0, 400, 1080);
        let (x, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], false);
        assert_eq!((x, y), (408, 1044)); // right of the region, bottom-aligned
    }

    #[test]
    fn escapes_to_second_monitor_when_one_is_fully_recorded() {
        let region = Rectangle::new(0, 0, 1920, 1080);
        let monitors = [mon(0, 0, 1920, 1080), mon(1920, 0, 1920, 1080)];
        let (x, y) = bar_position(region, BAR, &monitors, false);
        assert_eq!((x, y), (1928, 1044)); // beside, on the second monitor
    }

    #[test]
    fn tucks_inside_when_the_region_covers_everything() {
        let region = Rectangle::new(0, 0, 1920, 1080);
        let (x, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], false);
        assert_eq!((x, y), (1736, 1036)); // inside: bottom-right corner
    }

    #[test]
    fn excluded_bar_still_prefers_the_outside_spots() {
        let region = Rectangle::new(100, 100, 400, 200);
        let (x, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], true);
        assert_eq!((x, y), (316, 308));
    }

    #[test]
    fn excluded_bar_goes_inside_instead_of_beside() {
        let region = Rectangle::new(0, 0, 400, 1080);
        let (x, y) = bar_position(region, BAR, &[mon(0, 0, 1920, 1080)], true);
        assert_eq!((x, y), (216, 1036)); // inside: bottom-right of the region
    }

    #[test]
    fn excluded_bar_never_roams_to_a_second_monitor() {
        let region = Rectangle::new(0, 0, 1920, 1080);
        let monitors = [mon(0, 0, 1920, 1080), mon(1920, 0, 1920, 1080)];
        let (x, y) = bar_position(region, BAR, &monitors, true);
        assert_eq!((x, y), (1736, 1036)); // inside the region, not next door
    }
}

#[cfg(windows)]
pub mod windows_impl {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect,
                MonitorFromRect, ScreenToClient, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            UI::Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT,
                VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
            },
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW, PostMessageW,
                RegisterClassW, SetCursor, SetForegroundWindow, SetLayeredWindowAttributes,
                SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage,
                CS_HREDRAW, CS_VREDRAW, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
                IDC_SIZEWE, LWA_COLORKEY, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
                SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WM_CAPTURECHANGED, WM_DESTROY, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_TIMER,
                WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    };

    const WM_STOP_OVERLAY: u32 = WM_USER + 1;
    const WM_SET_MODE: u32 = WM_USER + 2;
    const WM_SET_RECT: u32 = WM_USER + 3;
    const FLASH_TIMER_ID: usize = 1;
    const FLASH_INTERVAL_MS: u32 = 500;
    const KEY: COLORREF = COLORREF(0x00010101);

    static OVERLAY_HWND: Mutex<Option<isize>> = Mutex::new(None);
    static REGION_X: AtomicI32 = AtomicI32::new(0);
    static REGION_Y: AtomicI32 = AtomicI32::new(0);
    static REGION_W: AtomicI32 = AtomicI32::new(0);
    static REGION_H: AtomicI32 = AtomicI32::new(0);
    static BAND: AtomicI32 = AtomicI32::new(6);
    static MODE: AtomicU8 = AtomicU8::new(0);
    static FLASH_STATE: AtomicBool = AtomicBool::new(true);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
    static DRAG: Mutex<Option<(u8, POINT, Rectangle)>> = Mutex::new(None);
    type EventCallback = Box<dyn Fn(FrameEvent) + Send>;
    static ON_EVENT: Mutex<Option<EventCallback>> = Mutex::new(None);

    pub fn arm(region: Rectangle, on_event: EventCallback) {
        // a frame closed a moment ago may still be tearing down
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while RUNNING.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        store_region(region);
        MODE.store(FrameMode::Armed as u8, Ordering::SeqCst);
        FLASH_STATE.store(true, Ordering::SeqCst);
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        *ON_EVENT.lock().unwrap() = Some(on_event);
        thread::spawn(run_overlay_loop);
    }

    pub fn set_mode(mode: FrameMode) {
        MODE.store(mode as u8, Ordering::SeqCst);
        post(WM_SET_MODE);
    }

    pub fn set_region(region: Rectangle) {
        store_region(region);
        post(WM_SET_RECT);
    }

    pub fn region() -> Rectangle {
        Rectangle::new(
            REGION_X.load(Ordering::SeqCst),
            REGION_Y.load(Ordering::SeqCst),
            REGION_W.load(Ordering::SeqCst) as u32,
            REGION_H.load(Ordering::SeqCst) as u32,
        )
    }

    pub fn band() -> i32 {
        BAND.load(Ordering::SeqCst)
    }

    pub fn stop() {
        if !RUNNING.load(Ordering::SeqCst) {
            return;
        }
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        post(WM_STOP_OVERLAY);
    }

    fn post(msg: u32) {
        if let Some(hwnd) = *OVERLAY_HWND.lock().unwrap() {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn store_region(region: Rectangle) {
        REGION_X.store(region.x, Ordering::SeqCst);
        REGION_Y.store(region.y, Ordering::SeqCst);
        REGION_W.store(region.width as i32, Ordering::SeqCst);
        REGION_H.store(region.height as i32, Ordering::SeqCst);
    }

    fn fire(event: FrameEvent) {
        if let Some(cb) = ON_EVENT.lock().unwrap().as_ref() {
            cb(event);
        }
    }

    fn clear_overlay_state() {
        *OVERLAY_HWND.lock().unwrap() = None;
        *ON_EVENT.lock().unwrap() = None;
        *DRAG.lock().unwrap() = None;
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        RUNNING.store(false, Ordering::SeqCst);
    }

    fn virtual_screen() -> Rectangle {
        unsafe {
            Rectangle::new(
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
            )
        }
    }

    // moves the window around the new region and tells the session
    unsafe fn apply_region(hwnd: HWND, region: Rectangle, edges: u8) {
        let region = clamp_into(region, virtual_screen(), edges);
        store_region(region);
        let b = band();
        let _ = SetWindowPos(
            hwnd,
            None,
            region.x - b,
            region.y - b,
            region.width as i32 + b * 2,
            region.height as i32 + b * 2,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(hwnd), None, false);
        fire(FrameEvent::Moved(region));
    }

    fn run_overlay_loop() {
        unsafe {
            let instance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(i) => i,
                Err(_) => {
                    clear_overlay_state();
                    return;
                }
            };
            let class: Vec<u16> = "RecordingFrameClass\0".encode_utf16().collect();
            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);
            RegisterClassW(&WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(frame_wnd_proc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            });

            let region = region();
            let region_rect = RECT {
                left: region.x,
                top: region.y,
                right: region.x + region.width as i32,
                bottom: region.y + region.height as i32,
            };
            let monitor = MonitorFromRect(&region_rect, MONITOR_DEFAULTTONEAREST);
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            BAND.store(6 * dpi_x.max(96) as i32 / 96, Ordering::SeqCst);
            let b = band();

            // the colour-keyed hole passes clicks through to the app below;
            // only the band takes the mouse
            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                region.x - b,
                region.y - b,
                region.width as i32 + b * 2,
                region.height as i32 + b * 2,
                None,
                None,
                Some(hinstance),
                None,
            ) {
                Ok(h) => h,
                Err(_) => {
                    clear_overlay_state();
                    return;
                }
            };
            let _ = SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY);
            // the band sits outside the recorded rect; exclusion also keeps it
            // out of any other region that overlaps it
            if let Err(error) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
                tracing::warn!("recording frame capture exclusion unavailable: {error}");
            }
            *OVERLAY_HWND.lock().unwrap() = Some(hwnd.0 as isize);
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetTimer(Some(hwnd), FLASH_TIMER_ID, FLASH_INTERVAL_MS, None);

            let mut msg = MSG::default();
            while RUNNING.load(Ordering::SeqCst) && !STOP_REQUESTED.load(Ordering::SeqCst) {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() || msg.message == WM_STOP_OVERLAY {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            KillTimer(Some(hwnd), FLASH_TIMER_ID).ok();
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = DestroyWindow(hwnd);
            clear_overlay_state();
        }
    }

    fn mode() -> u8 {
        MODE.load(Ordering::SeqCst)
    }

    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let b = band();
        let (w, h) = (
            REGION_W.load(Ordering::SeqCst) + b * 2,
            REGION_H.load(Ordering::SeqCst) + b * 2,
        );
        let fill = |left, top, right, bottom, color: COLORREF| {
            let brush = CreateSolidBrush(color);
            let rect = RECT {
                left,
                top,
                right,
                bottom,
            };
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(brush.into());
        };
        fill(0, 0, w, h, KEY);
        let color = match mode() {
            m if m == FrameMode::Recording as u8 => COLORREF(0x002B33E0),
            m if m == FrameMode::Paused as u8 => COLORREF(0x0020A0F0),
            _ => COLORREF(0x00BF6F2A),
        };
        if mode() != FrameMode::Recording as u8 || FLASH_STATE.load(Ordering::SeqCst) {
            fill(0, 0, w, b, color);
            fill(0, h - b, w, h, color);
            fill(0, b, b, h - b, color);
            fill(w - b, b, w, h - b, color);
        }
        // grips mark the corners and edge midpoints, where hit_edges resizes
        let grip = b * 3;
        let white = COLORREF(0x00FFFFFF);
        for (x, y) in [(0, 0), (w - grip, 0), (0, h - b), (w - grip, h - b)] {
            fill(x, y, x + grip, y + b, white);
        }
        for (x, y) in [(0, 0), (w - b, 0), (0, h - grip), (w - b, h - grip)] {
            fill(x, y, x + b, y + grip, white);
        }
        fill(w / 2 - grip / 2, 0, w / 2 + grip / 2, b, white);
        fill(w / 2 - grip / 2, h - b, w / 2 + grip / 2, h, white);
        fill(0, h / 2 - grip / 2, b, h / 2 + grip / 2, white);
        fill(w - b, h / 2 - grip / 2, w, h / 2 + grip / 2, white);
        let _ = EndPaint(hwnd, &ps);
    }

    unsafe fn cursor_edges(hwnd: HWND) -> (u8, POINT) {
        let mut screen = POINT::default();
        let _ = GetCursorPos(&mut screen);
        let mut pt = screen;
        let _ = ScreenToClient(hwnd, &mut pt);
        let b = band();
        let edges = hit_edges(
            pt.x,
            pt.y,
            REGION_W.load(Ordering::SeqCst) + b * 2,
            REGION_H.load(Ordering::SeqCst) + b * 2,
            b,
        );
        (edges, screen)
    }

    fn key_down(vk: u16) -> bool {
        unsafe { GetKeyState(vk as i32) < 0 }
    }

    unsafe extern "system" fn frame_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == FLASH_TIMER_ID => {
                if mode() == FrameMode::Recording as u8 {
                    FLASH_STATE.fetch_xor(true, Ordering::SeqCst);
                    // erase=false: paint fills the whole client, a white
                    // erase first would only add a flicker frame
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_SET_MODE => {
                FLASH_STATE.store(true, Ordering::SeqCst);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_SET_RECT => {
                apply_region(hwnd, region(), 0);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let (edges, _) = cursor_edges(hwnd);
                let id = match edges {
                    0 => IDC_SIZEALL,
                    e if e == LEFT | TOP || e == RIGHT | BOTTOM => IDC_SIZENWSE,
                    e if e == LEFT | BOTTOM || e == RIGHT | TOP => IDC_SIZENESW,
                    LEFT | RIGHT => IDC_SIZEWE,
                    _ => IDC_SIZENS,
                };
                if let Ok(cursor) = LoadCursorW(None, id) {
                    SetCursor(Some(cursor));
                }
                LRESULT(1)
            }
            WM_LBUTTONDOWN => {
                let (edges, screen) = cursor_edges(hwnd);
                *DRAG.lock().unwrap() = Some((edges, screen, region()));
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let drag = *DRAG.lock().unwrap();
                if let Some((edges, origin, start)) = drag {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let next = drag_rect(
                        start,
                        edges,
                        pt.x - origin.x,
                        pt.y - origin.y,
                        key_down(VK_SHIFT.0),
                    );
                    if next != region() {
                        apply_region(hwnd, next, edges);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                *DRAG.lock().unwrap() = None;
                let _ = ReleaseCapture();
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                *DRAG.lock().unwrap() = None;
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                let step = if key_down(VK_SHIFT.0) { 10 } else { 1 };
                let (dx, dy) = match vk {
                    v if v == VK_LEFT.0 => (-step, 0),
                    v if v == VK_RIGHT.0 => (step, 0),
                    v if v == VK_UP.0 => (0, -step),
                    v if v == VK_DOWN.0 => (0, step),
                    _ => (0, 0),
                };
                let armed = mode() == FrameMode::Armed as u8;
                if (dx, dy) != (0, 0) {
                    // ctrl resizes from the bottom-right corner
                    let edges = if key_down(VK_CONTROL.0) {
                        RIGHT | BOTTOM
                    } else {
                        0
                    };
                    apply_region(hwnd, drag_rect(region(), edges, dx, dy, false), edges);
                } else if vk == VK_RETURN.0 && armed {
                    fire(FrameEvent::Record);
                } else if vk == VK_ESCAPE.0 && armed {
                    fire(FrameEvent::Cancel);
                }
                LRESULT(0)
            }
            // RUNNING drops only in clear_overlay_state, after teardown, so a
            // frame armed right after this one can't have its state wiped
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::GetWindowDisplayAffinity;

        static OVERLAY_TEST_LOCK: Mutex<()> = Mutex::new(());

        struct OverlayGuard;

        impl Drop for OverlayGuard {
            fn drop(&mut self) {
                stop();
                let deadline = Instant::now() + Duration::from_secs(2);
                while RUNNING.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        fn wait_for_window() -> isize {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(hwnd) = *OVERLAY_HWND.lock().unwrap() {
                    return hwnd;
                }
                assert!(Instant::now() < deadline, "recording frame did not open");
                thread::sleep(Duration::from_millis(10));
            }
        }

        #[test]
        fn frame_is_excluded_from_capture() {
            let _serial = OVERLAY_TEST_LOCK.lock().unwrap();
            arm(Rectangle::new(32, 32, 160, 90), Box::new(|_| {}));
            let _guard = OverlayGuard;
            let frame = wait_for_window();

            let mut affinity = 0u32;
            unsafe {
                GetWindowDisplayAffinity(HWND(frame as *mut _), &mut affinity).unwrap();
            }
            assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);
        }

        #[test]
        fn preset_moves_the_frame_and_reports_it() {
            let _serial = OVERLAY_TEST_LOCK.lock().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let tx = Mutex::new(tx);
            arm(
                Rectangle::new(32, 32, 160, 90),
                Box::new(move |event| {
                    let _ = tx.lock().unwrap().send(event);
                }),
            );
            let _guard = OverlayGuard;
            wait_for_window();
            set_region(Rectangle::new(40, 50, 320, 240));
            let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(event, FrameEvent::Moved(Rectangle::new(40, 50, 320, 240)));
        }

        #[test]
        fn immediate_stop_request_is_not_lost() {
            let _serial = OVERLAY_TEST_LOCK.lock().unwrap();
            arm(Rectangle::new(32, 32, 160, 90), Box::new(|_| {}));
            let _guard = OverlayGuard;
            stop();

            let deadline = Instant::now() + Duration::from_secs(2);
            while RUNNING.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(!RUNNING.load(Ordering::SeqCst));
        }
    }
}

// linux: a small always-on-top window (label "recbar") with the elapsed
// clock, pause and stop. on plasma >= 6.7 a kwin script marks it
// excludeFromCapture (the compositor-side WDA_EXCLUDEFROMCAPTURE), so it
// places like the windows bar: below the region, above it, or inside it
// without appearing in the frames. everywhere else it stays OUTSIDE the
// recorded region, spilling onto a neighbouring monitor when the region
// fills its own.
#[cfg(target_os = "linux")]
pub mod linux_impl {
    use super::*;
    use ceri::{WindowAction, WindowConfig};
    use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use std::time::Duration;

    const LABEL: &str = "recbar";
    // keep in sync with .recbar in styles.css
    const BAR_W: u32 = 184;
    const BAR_H: u32 = 36;
    // matches the windows overlay: a 4px ring just outside the region,
    // blinking every 500ms
    const FRAME_BORDER: i32 = 4;
    const FRAME_FLASH_MS: u64 = 500;

    // keeps the kwin capture-exclusion and placement script loaded while recording
    static KWIN_GUARD: Mutex<Option<crate::capture::CaptureExclusionGuard>> = Mutex::new(None);
    // the red region frame: kde gets a raw wayland client, x11 gets four
    // override-redirect border strips so clicks pass through the interior
    static WL_FRAME: Mutex<Option<crate::overlay::wayland_frame::WaylandFrame>> = Mutex::new(None);
    static X11_FRAME: Mutex<Option<X11Frame>> = Mutex::new(None);

    struct X11Frame {
        shutdown: Sender<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl X11Frame {
        fn show(region: Rectangle) -> anyhow::Result<Self> {
            use x11rb::connection::Connection;
            use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, WindowClass};

            let (conn, screen_num) = x11rb::connect(None)?;
            let screen = &conn.setup().roots[screen_num];
            let root = screen.root;
            let red = conn
                .alloc_color(screen.default_colormap, 0xffff, 0, 0)?
                .reply()?
                .pixel;

            let x = region.x - FRAME_BORDER;
            let y = region.y - FRAME_BORDER;
            let total_w = region.width as i32 + FRAME_BORDER * 2;
            let total_h = region.height as i32 + FRAME_BORDER * 2;
            let b = FRAME_BORDER;
            let strips = [
                (x, y, total_w, b),
                (x, y + total_h - b, total_w, b),
                (x, y + b, b, (total_h - b * 2).max(1)),
                (x + total_w - b, y + b, b, (total_h - b * 2).max(1)),
            ];

            let aux = CreateWindowAux::new()
                .background_pixel(red)
                .override_redirect(1);
            let mut ids = Vec::with_capacity(strips.len());
            for (sx, sy, sw, sh) in strips {
                let id = conn.generate_id()?;
                conn.create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    id,
                    root,
                    sx.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    sy.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    sw.clamp(1, u16::MAX as i32) as u16,
                    sh.clamp(1, u16::MAX as i32) as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &aux,
                )?;
                conn.map_window(id)?;
                ids.push(id);
            }
            conn.flush()?;

            let (shutdown_tx, shutdown_rx) = channel();
            let thread = std::thread::Builder::new()
                .name("capscr-x11-frame".into())
                .spawn(move || {
                    let mut visible = true;
                    loop {
                        match shutdown_rx.recv_timeout(Duration::from_millis(FRAME_FLASH_MS)) {
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => {}
                        }
                        visible = !visible;
                        for &id in &ids {
                            let _ = if visible {
                                conn.map_window(id)
                            } else {
                                conn.unmap_window(id)
                            };
                        }
                        let _ = conn.flush();
                    }
                    for &id in &ids {
                        let _ = conn.destroy_window(id);
                    }
                    let _ = conn.flush();
                })?;

            Ok(Self {
                shutdown: shutdown_tx,
                thread: Some(thread),
            })
        }
    }

    impl Drop for X11Frame {
        fn drop(&mut self) {
            let _ = self.shutdown.send(());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub fn start(region: Rectangle) {
        let Some(windows) = crate::overlay::linux::app_handle().and_then(|app| app.windows())
        else {
            return;
        };

        let wayland = crate::capture::gui_is_wayland();
        let kde_wayland = wayland && crate::shell::desktop() == crate::shell::DesktopEnv::Kde;
        let can_exclude = kde_wayland && crate::shell::kwin_still_capture_exclusion();

        let monitors = crate::capture::list_monitors().unwrap_or_default();
        let (bar_x, bar_y) =
            bar_position(region, (BAR_W as i32, BAR_H as i32), &monitors, can_exclude);

        // arm the kwin script before the bar maps so windowAdded sets keep-above,
        // position, and plasma 6.7 capture exclusion before the first frame
        let mut excluded = false;
        if kde_wayland {
            match crate::capture::configure_own_window(
                "capscr recording",
                Some((bar_x, bar_y)),
                can_exclude,
            ) {
                Ok(guard) => {
                    *KWIN_GUARD.lock().unwrap() = Some(guard);
                    excluded = can_exclude;
                }
                Err(e) => {
                    tracing::debug!("kwin recbar helper unavailable ({e:#})");
                }
            }
        }
        let (bar_x, bar_y) = if excluded == can_exclude {
            (bar_x, bar_y)
        } else {
            bar_position(region, (BAR_W as i32, BAR_H as i32), &monitors, false)
        };

        if kde_wayland {
            match crate::overlay::wayland_frame::WaylandFrame::show(region) {
                Ok(frame) => *WL_FRAME.lock().unwrap() = Some(frame),
                Err(e) => tracing::debug!("region frame unavailable: {e:#}"),
            }
        } else if !wayland {
            match X11Frame::show(region) {
                Ok(frame) => *X11_FRAME.lock().unwrap() = Some(frame),
                Err(e) => tracing::debug!("x11 region frame unavailable: {e:#}"),
            }
        }

        if wayland && !kde_wayland && crate::capture::gnome_shell::available() {
            let _ = crate::capture::gnome_shell::place_above("capscr recording", bar_x, bar_y);
        }

        if windows.state(LABEL).is_some() {
            windows.act(LABEL, WindowAction::Destroy);
        }
        windows.open(WindowConfig {
            label: LABEL.to_string(),
            title: "capscr recording".to_string(),
            width: BAR_W,
            height: BAR_H,
            minimum_width: BAR_W,
            minimum_height: BAR_H,
            maximum_size: Some((BAR_W, BAR_H)),
            position: Some((bar_x, bar_y)),
            start_url: "/recbar.html".to_string(),
            decorations: false,
            resizable: false,
            visible: true,
            always_on_top: true,
            skip_taskbar: true,
            transparent: true,
            focused: false,
            app_id: Some("capscr".to_string()),
            ..WindowConfig::default()
        });
    }

    pub fn stop() {
        *KWIN_GUARD.lock().unwrap() = None;
        *WL_FRAME.lock().unwrap() = None;
        *X11_FRAME.lock().unwrap() = None;
        if let Some(windows) = crate::overlay::linux::app_handle().and_then(|app| app.windows()) {
            windows.act(LABEL, WindowAction::Destroy);
        }
    }
}

pub struct RecordingOverlay;

// windows arms its frame through windows_impl directly; elsewhere the frame
// and bar appear once recording starts
impl RecordingOverlay {
    #[cfg(target_os = "linux")]
    pub fn start(region: Rectangle) {
        linux_impl::start(region);
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub fn start(_region: Rectangle) {}

    pub fn stop() {
        #[cfg(windows)]
        windows_impl::stop();
        #[cfg(target_os = "linux")]
        linux_impl::stop();
    }
}
