#![allow(dead_code)]

use crate::capture::Rectangle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionResult {
    Region(Rectangle),
    Window(u32),
    FullScreen,
    Cancelled,
    PickedColor(u8, u8, u8),
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
    use std::sync::Mutex;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Graphics::Gdi::{
                AlphaBlend, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
                CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetDC,
                GetStockObject, InvalidateRect, ReleaseDC, SelectObject, SetBkColor, SetBkMode,
                SetTextColor, StretchBlt, TextOutW, AC_SRC_OVER, BLENDFUNCTION, HBITMAP, HDC,
                HOLLOW_BRUSH, OPAQUE, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
                Rectangle as GdiRectangle,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN, VK_SPACE},
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
                    GetAncestor, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongW,
                    GetWindowRect, IsIconic, IsWindowVisible, PostQuitMessage, RegisterClassW,
                    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GA_ROOT, GWL_EXSTYLE,
                    GWL_STYLE, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                    SM_YVIRTUALSCREEN, SetForegroundWindow, SW_SHOWNORMAL, WM_DESTROY, WM_KEYDOWN,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
                    WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
                    SetLayeredWindowAttributes, LWA_ALPHA, LWA_COLORKEY,
                },
            },
        },
    };

    const CLICK_THRESHOLD: i32 = 5;
    const MIN_SELECTION_SIZE: i32 = 5;
    const MAGNIFIER_SIZE: i32 = 120;
    const MAGNIFIER_ZOOM: i32 = 2;

    // layered-window dim alpha (0=fully transparent, 255=fully opaque).
    // 180 = ~70% dim — matches the previous AlphaBlend SourceConstantAlpha
    // value, so the perceived dim level is the same as before the
    // BitBlt-snapshot approach was replaced by DWM compositing.
    const OVERLAY_ALPHA: u8 = 180;
    // colorkey for "punch-through" areas in the overlay (selection rect
    // interior). RGB 0x00FF80 — an unnatural mid-green not likely to be
    // produced by any actual rendered content, so we don't accidentally
    // make legitimate green pixels in the dim layer transparent.
    const OVERLAY_COLORKEY: u32 = 0x0080FF00; // BGR for COLORREF: 00, 80, FF -> green channel high

    static SELECTING: AtomicBool = AtomicBool::new(false);
    static START_X: AtomicI32 = AtomicI32::new(0);
    static START_Y: AtomicI32 = AtomicI32::new(0);
    static END_X: AtomicI32 = AtomicI32::new(0);
    static END_Y: AtomicI32 = AtomicI32::new(0);
    static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);
    static CANCELLED: AtomicBool = AtomicBool::new(false);
    static FULLSCREEN: AtomicBool = AtomicBool::new(false);
    static WINDOW_SELECTED: AtomicU32 = AtomicU32::new(0);
    static PICKED_COLOR_SET: AtomicBool = AtomicBool::new(false);
    static PICKED_R: AtomicU32 = AtomicU32::new(0);
    static PICKED_G: AtomicU32 = AtomicU32::new(0);
    static PICKED_B: AtomicU32 = AtomicU32::new(0);

    fn alt_held() -> bool {
        unsafe {
            let state = windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU.0 as i32,
            );
            state < 0
        }
    }

    static SCREEN_BITMAP: Mutex<Option<isize>> = Mutex::new(None);
    static SCREEN_DC: Mutex<Option<isize>> = Mutex::new(None);
    // pre-darkened copy of SCREEN_BITMAP used to paint the "outside selection"
    // dim during a drag without a per-frame AlphaBlend. AlphaBlend on a 4K
    // back buffer goes through GDI's software path (~10-30ms per call) and
    // is the reason region selection felt locked to 60 Hz / lower. A cached
    // darken bitmap reduces per-frame work to two BitBlts.
    static DIM_BITMAP: Mutex<Option<isize>> = Mutex::new(None);
    static DIM_DC: Mutex<Option<isize>> = Mutex::new(None);
    // persistent back buffer for double-buffered WM_PAINT. Without this, every
    // mouse move allocated/freed a screen-size GDI bitmap (~32 MB on a 4K
    // display × ~100 mouse-events/sec = visible flicker / stutter).
    static BACK_BITMAP: Mutex<Option<isize>> = Mutex::new(None);
    static BACK_DC: Mutex<Option<isize>> = Mutex::new(None);
    static SCREEN_WIDTH: AtomicI32 = AtomicI32::new(0);
    static SCREEN_HEIGHT: AtomicI32 = AtomicI32::new(0);
    static VIRTUAL_X: AtomicI32 = AtomicI32::new(0);
    static VIRTUAL_Y: AtomicI32 = AtomicI32::new(0);

    static WINDOW_LIST: Mutex<Vec<CachedWindow>> = Mutex::new(Vec::new());
    static HOVERED_WINDOW: AtomicU32 = AtomicU32::new(0);
    static CURSOR_X: AtomicI32 = AtomicI32::new(0);
    static CURSOR_Y: AtomicI32 = AtomicI32::new(0);

    #[derive(Debug, Clone)]
    struct CachedWindow {
        hwnd: isize,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    fn enumerate_windows() -> Vec<CachedWindow> {
        let mut windows = Vec::new();
        unsafe {
            let windows_ptr = &mut windows as *mut Vec<CachedWindow>;
            let _ = EnumWindows(Some(enum_windows_callback), LPARAM(windows_ptr as isize));
        }
        windows
    }

    unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = &mut *(lparam.0 as *mut Vec<CachedWindow>);

        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        if IsIconic(hwnd).as_bool() {
            return BOOL(1);
        }

        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_VISIBLE.0 == 0 {
            return BOOL(1);
        }

        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return BOOL(1);
        }

        // GetWindowRect includes the ~7px transparent DWM shadow extent;
        // DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS) gives the
        // tight visible rectangle the user actually perceives as "the
        // window". Use the tighter one so the hover highlight + final
        // crop hug the window instead of overshooting into empty space.
        let mut rect = RECT::default();
        let dwm_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok();
        if !dwm_ok && GetWindowRect(hwnd, &mut rect).is_err() {
            return BOOL(1);
        }

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        if width < 50 || height < 50 {
            return BOOL(1);
        }

        let root = GetAncestor(hwnd, GA_ROOT);
        if !root.0.is_null() && root != hwnd {
            return BOOL(1);
        }

        windows.push(CachedWindow {
            hwnd: hwnd.0 as isize,
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        });

        BOOL(1)
    }

    fn find_window_at_point(pt: POINT) -> Option<CachedWindow> {
        let windows = WINDOW_LIST.lock().unwrap();
        for win in windows.iter() {
            if pt.x >= win.left && pt.x < win.right && pt.y >= win.top && pt.y < win.bottom {
                return Some(win.clone());
            }
        }
        None
    }

    pub fn select() -> SelectionResult {
        // single-flight: the windows_impl module backs the entire selector with
        // process-wide statics (START_X / SCREEN_BITMAP / etc.). A second
        // simultaneous select() call from e.g. tray-click while a hotkey-bound
        // capture is mid-drag would scramble those, so we reject overlap and
        // let the caller treat it as cancelled.
        if SELECTING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::info!("selector already active — dropping overlapping invocation");
            return SelectionResult::Cancelled;
        }
        START_X.store(0, Ordering::SeqCst);
        START_Y.store(0, Ordering::SeqCst);
        END_X.store(0, Ordering::SeqCst);
        END_Y.store(0, Ordering::SeqCst);
        MOUSE_DOWN.store(false, Ordering::SeqCst);
        CANCELLED.store(false, Ordering::SeqCst);
        FULLSCREEN.store(false, Ordering::SeqCst);
        WINDOW_SELECTED.store(0, Ordering::SeqCst);
        HOVERED_WINDOW.store(0, Ordering::SeqCst);
        PICKED_COLOR_SET.store(false, Ordering::SeqCst);

        let windows = enumerate_windows();
        *WINDOW_LIST.lock().unwrap() = windows;

        unsafe {
            let virt_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let virt_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let virt_width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let virt_height = GetSystemMetrics(SM_CYVIRTUALSCREEN);

            VIRTUAL_X.store(virt_x, Ordering::SeqCst);
            VIRTUAL_Y.store(virt_y, Ordering::SeqCst);
            SCREEN_WIDTH.store(virt_width, Ordering::SeqCst);
            SCREEN_HEIGHT.store(virt_height, Ordering::SeqCst);

            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(screen_dc);
            if !mem_dc.is_invalid() {
                // HDR-aware preview disabled — the DXGI desktop duplication
                // path added multi-second latency to overlay open on HDR
                // setups and on some configurations produced zeroed
                // textures. fall back to GDI BitBlt unconditionally: HDR
                // pixels read as overblown in the preview, matching what
                // the saved capture looks like.
                let (bitmap, needs_bitblt) = (
                    CreateCompatibleBitmap(screen_dc, virt_width, virt_height),
                    true,
                );
                if !bitmap.is_invalid() {
                    let old_bitmap = SelectObject(mem_dc, bitmap);
                    if needs_bitblt {
                        BitBlt(mem_dc, 0, 0, virt_width, virt_height, screen_dc, virt_x, virt_y, SRCCOPY).ok();
                    }
                    SelectObject(mem_dc, old_bitmap);
                    *SCREEN_BITMAP.lock().unwrap() = Some(bitmap.0 as isize);

                    // build a dim copy once, used per-frame during drag to
                    // paint the "outside selection" area without AlphaBlend.
                    let dim_dc = CreateCompatibleDC(screen_dc);
                    let dim_bmp = CreateCompatibleBitmap(screen_dc, virt_width, virt_height);
                    if !dim_dc.is_invalid() && !dim_bmp.is_invalid() {
                        let old_dim = SelectObject(dim_dc, dim_bmp);
                        // start with a copy of the screen
                        let _ = BitBlt(dim_dc, 0, 0, virt_width, virt_height, mem_dc, 0, 0, SRCCOPY);
                        // darken via one AlphaBlend at startup (62% black overlay).
                        // amortised across the lifetime of the selector instead
                        // of paying it on every WM_PAINT.
                        let dim_brush_dc = CreateCompatibleDC(screen_dc);
                        let dim_brush_bmp = CreateCompatibleBitmap(screen_dc, virt_width, virt_height);
                        if !dim_brush_dc.is_invalid() && !dim_brush_bmp.is_invalid() {
                            let old_db = SelectObject(dim_brush_dc, dim_brush_bmp);
                            let black = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00000000));
                            let full = RECT { left: 0, top: 0, right: virt_width, bottom: virt_height };
                            FillRect(dim_brush_dc, &full, black);
                            let _ = DeleteObject(black);
                            let blend = BLENDFUNCTION {
                                BlendOp: AC_SRC_OVER as u8,
                                BlendFlags: 0,
                                SourceConstantAlpha: 160,
                                AlphaFormat: 0,
                            };
                            let _ = AlphaBlend(
                                dim_dc, 0, 0, virt_width, virt_height,
                                dim_brush_dc, 0, 0, virt_width, virt_height,
                                blend,
                            );
                            SelectObject(dim_brush_dc, old_db);
                            let _ = DeleteObject(dim_brush_bmp);
                            let _ = DeleteDC(dim_brush_dc);
                        }
                        SelectObject(dim_dc, old_dim);
                        *DIM_BITMAP.lock().unwrap() = Some(dim_bmp.0 as isize);
                        *DIM_DC.lock().unwrap() = Some(dim_dc.0 as isize);
                    }
                }
                *SCREEN_DC.lock().unwrap() = Some(mem_dc.0 as isize);
            }
            ReleaseDC(None, screen_dc);

            let instance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(i) => i,
                Err(_) => {
                    if let Some(dc) = SCREEN_DC.lock().unwrap().take() {
                        let _ = DeleteDC(HDC(dc as *mut _));
                    }
                    if let Some(bmp) = SCREEN_BITMAP.lock().unwrap().take() {
                        let _ = DeleteObject(HBITMAP(bmp as *mut _));
                    }
                    SELECTING.store(false, Ordering::SeqCst);
                    return SelectionResult::Cancelled;
                }
            };
            let class_name: Vec<u16> = "UnifiedSelectorClass\0".encode_utf16().collect();

            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(unified_wnd_proc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: match windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                    None,
                    windows::Win32::UI::WindowsAndMessaging::IDC_CROSS,
                ) {
                    Ok(c) => c,
                    Err(_) => {
                        if let Some(dc) = SCREEN_DC.lock().unwrap().take() {
                            let _ = DeleteDC(HDC(dc as *mut _));
                        }
                        if let Some(bmp) = SCREEN_BITMAP.lock().unwrap().take() {
                            let _ = DeleteObject(HBITMAP(bmp as *mut _));
                        }
                        SELECTING.store(false, Ordering::SeqCst);
                        return SelectionResult::Cancelled;
                    }
                },
                ..Default::default()
            };

            RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                virt_x,
                virt_y,
                virt_width,
                virt_height,
                None,
                None,
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(_) => {
                    if let Some(dc) = SCREEN_DC.lock().unwrap().take() {
                        let _ = DeleteDC(HDC(dc as *mut _));
                    }
                    if let Some(bmp) = SCREEN_BITMAP.lock().unwrap().take() {
                        let _ = DeleteObject(HBITMAP(bmp as *mut _));
                    }
                    SELECTING.store(false, Ordering::SeqCst);
                    return SelectionResult::Cancelled;
                }
            };

            // layered-window attributes: black background paints at 70%
            // alpha (dim layer over live HDR desktop), magic green colorkey
            // (#00FF80) becomes fully transparent (live desktop fully
            // visible inside the selection rectangle). before this, the
            // overlay BitBlt'd a snapshot of the desktop and displayed it
            // as a dim layer — on HDR displays that snapshot was the
            // SDR-tonemapped (overblown) view, and the user perceived the
            // entire screen as overblown the instant screenshot mode
            // started. now DWM composites the dim over the live HDR
            // framebuffer directly.
            let _ = SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(OVERLAY_COLORKEY),
                OVERLAY_ALPHA,
                LWA_ALPHA | LWA_COLORKEY,
            );

            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            // grab foreground + keyboard focus so VK_ESCAPE actually reaches
            // WM_KEYDOWN. without this the overlay paints but key input goes
            // to whatever window had focus before the hotkey fired.
            let _ = SetForegroundWindow(hwnd);

            let mut msg = MSG::default();
            while SELECTING.load(Ordering::SeqCst) {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    break;
                }
            }

            let _ = DestroyWindow(hwnd);

            if let Some(dc) = SCREEN_DC.lock().unwrap().take() {
                let _ = DeleteDC(HDC(dc as *mut _));
            }
            if let Some(bmp) = SCREEN_BITMAP.lock().unwrap().take() {
                let _ = DeleteObject(HBITMAP(bmp as *mut _));
            }

            WINDOW_LIST.lock().unwrap().clear();

            if CANCELLED.load(Ordering::SeqCst) {
                return SelectionResult::Cancelled;
            }

            if PICKED_COLOR_SET.load(Ordering::SeqCst) {
                let r = PICKED_R.load(Ordering::SeqCst) as u8;
                let g = PICKED_G.load(Ordering::SeqCst) as u8;
                let b = PICKED_B.load(Ordering::SeqCst) as u8;
                return SelectionResult::PickedColor(r, g, b);
            }

            if FULLSCREEN.load(Ordering::SeqCst) {
                return SelectionResult::FullScreen;
            }

            let window_id = WINDOW_SELECTED.load(Ordering::SeqCst);
            if window_id != 0 {
                return SelectionResult::Window(window_id);
            }

            let sx = START_X.load(Ordering::SeqCst);
            let sy = START_Y.load(Ordering::SeqCst);
            let ex = END_X.load(Ordering::SeqCst);
            let ey = END_Y.load(Ordering::SeqCst);

            let width = (ex - sx).abs();
            let height = (ey - sy).abs();

            if width < MIN_SELECTION_SIZE || height < MIN_SELECTION_SIZE {
                return SelectionResult::Cancelled;
            }

            SelectionResult::Region(Rectangle::normalize(sx, sy, ex, ey))
        }
    }

    unsafe extern "system" fn unified_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let virt_x = VIRTUAL_X.load(Ordering::SeqCst);
                let virt_y = VIRTUAL_Y.load(Ordering::SeqCst);
                let width = SCREEN_WIDTH.load(Ordering::SeqCst);
                let height = SCREEN_HEIGHT.load(Ordering::SeqCst);

                // compose the whole frame into a back buffer, then BitBlt once
                // to the window. The back buffer is cached for the life of the
                // selector — recreating it every paint allocated ~32 MB of GDI
                // memory per frame on 4K displays and caused visible flicker
                // under fast mouse movement.
                let (back_dc, _back_bmp) = {
                    let mut dc_guard = BACK_DC.lock().unwrap();
                    let mut bmp_guard = BACK_BITMAP.lock().unwrap();
                    if dc_guard.is_none() {
                        let dc = CreateCompatibleDC(hdc);
                        let bmp = CreateCompatibleBitmap(hdc, width, height);
                        SelectObject(dc, bmp);
                        *dc_guard = Some(dc.0 as isize);
                        *bmp_guard = Some(bmp.0 as isize);
                    }
                    (
                        HDC(dc_guard.unwrap() as *mut _),
                        HBITMAP(bmp_guard.unwrap() as *mut _),
                    )
                };

                // base = solid black. layered-window LWA_ALPHA makes it
                // 70% opaque over the live HDR desktop (composited by DWM),
                // so the user sees the *actual* screen tinted dark instead
                // of an SDR-tonemapped (overblown) BitBlt snapshot.
                {
                    let black_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00000000));
                    let full = RECT { left: 0, top: 0, right: width, bottom: height };
                    FillRect(back_dc, &full, black_brush);
                    let _ = DeleteObject(black_brush);
                }

                let mouse_down = MOUSE_DOWN.load(Ordering::SeqCst);
                let sx = START_X.load(Ordering::SeqCst);
                let sy = START_Y.load(Ordering::SeqCst);
                let ex = END_X.load(Ordering::SeqCst);
                let ey = END_Y.load(Ordering::SeqCst);

                let is_dragging = mouse_down && ((ex - sx).abs() > CLICK_THRESHOLD || (ey - sy).abs() > CLICK_THRESHOLD);

                if is_dragging {
                    let left = (sx.min(ex)) - virt_x;
                    let top = (sy.min(ey)) - virt_y;
                    let right = (sx.max(ex)) - virt_x;
                    let bottom = (sy.max(ey)) - virt_y;

                    // back buffer is already solid black above, so the
                    // dim layer is in place. paint the selection-rect
                    // interior with the layered-window colorkey colour so
                    // DWM punches through to the live HDR desktop inside
                    // the selection (no SDR tonemap, what-you-see-is-
                    // what-you-get).
                    let key_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(OVERLAY_COLORKEY));
                    let sel_rect = RECT { left, top, right, bottom };
                    FillRect(back_dc, &sel_rect, key_brush);
                    let _ = DeleteObject(key_brush);

                    // 1px solid white selection border — greyscale, no chroma.
                    let border_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00FFFFFF));

                    let old_pen = SelectObject(back_dc, border_pen);
                    let hollow = GetStockObject(HOLLOW_BRUSH);
                    let old_brush = SelectObject(back_dc, hollow);
                    SetBkMode(back_dc, TRANSPARENT);

                    let _ = GdiRectangle(back_dc, left, top, right, bottom);

                    SelectObject(back_dc, old_pen);
                    SelectObject(back_dc, old_brush);
                    let _ = DeleteObject(border_pen);

                    let sel_width = (right - left).abs();
                    let sel_height = (bottom - top).abs();
                    let size_text = format!("{}x{}", sel_width, sel_height);

                    let text_x = left + 5;
                    let text_y = if top > 20 { top - 18 } else { bottom + 5 };

                    SetTextColor(back_dc, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
                    SetBkColor(back_dc, windows::Win32::Foundation::COLORREF(0x00000000));
                    SetBkMode(back_dc, OPAQUE);

                    let text_wide: Vec<u16> = size_text.encode_utf16().collect();
                    let _ = TextOutW(back_dc, text_x, text_y, &text_wide);
                } else if !mouse_down {
                    let hovered = HOVERED_WINDOW.load(Ordering::SeqCst);
                    if hovered != 0 {
                        let windows = WINDOW_LIST.lock().unwrap();
                        if let Some(cached) = windows.iter().find(|w| w.hwnd as u32 == hovered) {
                            let left = cached.left - virt_x;
                            let top = cached.top - virt_y;
                            let right = cached.right - virt_x;
                            let bottom = cached.bottom - virt_y;

                            // 1px white outline only — no fill. the previous 12%-alpha
                            // white wash inside the hovered window made bright UI look
                            // hazy and made the cursor target less obvious.
                            let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
                            let old_pen = SelectObject(back_dc, pen);
                            let hollow = GetStockObject(HOLLOW_BRUSH);
                            let old_brush = SelectObject(back_dc, hollow);
                            SetBkMode(back_dc, TRANSPARENT);

                            let _ = GdiRectangle(back_dc, left, top, right, bottom);

                            SelectObject(back_dc, old_pen);
                            SelectObject(back_dc, old_brush);
                            let _ = DeleteObject(pen);
                        }
                    }
                }

                let cursor_x = CURSOR_X.load(Ordering::SeqCst) - virt_x;
                let cursor_y = CURSOR_Y.load(Ordering::SeqCst) - virt_y;

                if cursor_x >= 0 && cursor_x < width && cursor_y >= 0 && cursor_y < height {
                    let crosshair_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00808080));
                    let old_pen = SelectObject(back_dc, crosshair_pen);
                    SetBkMode(back_dc, TRANSPARENT);

                    let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, 0, cursor_y, None);
                    let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, cursor_x - 20, cursor_y);
                    let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, cursor_x + 20, cursor_y, None);
                    let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, width, cursor_y);

                    let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, cursor_x, 0, None);
                    let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, cursor_x, cursor_y - 20);
                    let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, cursor_x, cursor_y + 20, None);
                    let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, cursor_x, height);

                    SelectObject(back_dc, old_pen);
                    let _ = DeleteObject(crosshair_pen);

                    if let Some(dc) = *SCREEN_DC.lock().unwrap() {
                        if let Some(bmp) = *SCREEN_BITMAP.lock().unwrap() {
                            let mem_dc = HDC(dc as *mut _);
                            let old_bmp = SelectObject(mem_dc, HBITMAP(bmp as *mut _));

                            let mag_x = cursor_x + 30;
                            let mag_y = cursor_y + 30;
                            let mag_x = if mag_x + MAGNIFIER_SIZE > width { cursor_x - MAGNIFIER_SIZE - 30 } else { mag_x };
                            let mag_y = if mag_y + MAGNIFIER_SIZE > height { cursor_y - MAGNIFIER_SIZE - 30 } else { mag_y };

                            let src_size = MAGNIFIER_SIZE / MAGNIFIER_ZOOM;
                            let src_x = (cursor_x - src_size / 2).max(0).min(width - src_size);
                            let src_y = (cursor_y - src_size / 2).max(0).min(height - src_size);

                            let _ = StretchBlt(
                                back_dc, mag_x, mag_y, MAGNIFIER_SIZE, MAGNIFIER_SIZE,
                                mem_dc, src_x, src_y, src_size, src_size, SRCCOPY
                            );

                            SelectObject(mem_dc, old_bmp);

                            let border_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
                            let old_pen = SelectObject(back_dc, border_pen);
                            let hollow = GetStockObject(HOLLOW_BRUSH);
                            let old_brush = SelectObject(back_dc, hollow);
                            let _ = GdiRectangle(back_dc, mag_x, mag_y, mag_x + MAGNIFIER_SIZE, mag_y + MAGNIFIER_SIZE);

                            let center_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00808080));
                            SelectObject(back_dc, center_pen);
                            let cx = mag_x + MAGNIFIER_SIZE / 2;
                            let cy = mag_y + MAGNIFIER_SIZE / 2;
                            let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, cx - 10, cy, None);
                            let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, cx + 10, cy);
                            let _ = windows::Win32::Graphics::Gdi::MoveToEx(back_dc, cx, cy - 10, None);
                            let _ = windows::Win32::Graphics::Gdi::LineTo(back_dc, cx, cy + 10);

                            SelectObject(back_dc, old_pen);
                            SelectObject(back_dc, old_brush);
                            let _ = DeleteObject(border_pen);
                            let _ = DeleteObject(center_pen);
                        }
                    }
                }

                // single blit of the composited frame.
                let _ = BitBlt(hdc, 0, 0, width, height, back_dc, 0, 0, SRCCOPY);

                // back_dc / back_bmp are cached for the life of the selector
                // window; they're freed in WM_DESTROY, not here.

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let mouse_down = MOUSE_DOWN.load(Ordering::SeqCst);
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);

                CURSOR_X.store(pt.x, Ordering::SeqCst);
                CURSOR_Y.store(pt.y, Ordering::SeqCst);

                if mouse_down {
                    END_X.store(pt.x, Ordering::SeqCst);
                    END_Y.store(pt.y, Ordering::SeqCst);
                } else if let Some(cached) = find_window_at_point(pt) {
                    HOVERED_WINDOW.store(cached.hwnd as u32, Ordering::SeqCst);
                } else {
                    HOVERED_WINDOW.store(0, Ordering::SeqCst);
                }
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT::default();
                GetCursorPos(&mut pt).ok();
                if alt_held() {
                    if let Some(dc) = *SCREEN_DC.lock().unwrap() {
                        let virt_x = VIRTUAL_X.load(Ordering::SeqCst);
                        let virt_y = VIRTUAL_Y.load(Ordering::SeqCst);
                        let mem_dc = HDC(dc as *mut _);
                        if let Some(bmp) = *SCREEN_BITMAP.lock().unwrap() {
                            let old_bmp = SelectObject(mem_dc, HBITMAP(bmp as *mut _));
                            let color = windows::Win32::Graphics::Gdi::GetPixel(
                                mem_dc,
                                pt.x - virt_x,
                                pt.y - virt_y,
                            );
                            SelectObject(mem_dc, old_bmp);
                            let r = (color.0 & 0xFF) as u32;
                            let g = ((color.0 >> 8) & 0xFF) as u32;
                            let b = ((color.0 >> 16) & 0xFF) as u32;
                            PICKED_R.store(r, Ordering::SeqCst);
                            PICKED_G.store(g, Ordering::SeqCst);
                            PICKED_B.store(b, Ordering::SeqCst);
                            PICKED_COLOR_SET.store(true, Ordering::SeqCst);
                        }
                    }
                    SELECTING.store(false, Ordering::SeqCst);
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                START_X.store(pt.x, Ordering::SeqCst);
                START_Y.store(pt.y, Ordering::SeqCst);
                END_X.store(pt.x, Ordering::SeqCst);
                END_Y.store(pt.y, Ordering::SeqCst);
                MOUSE_DOWN.store(true, Ordering::SeqCst);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if MOUSE_DOWN.load(Ordering::SeqCst) {
                    let mut pt = POINT::default();
                    GetCursorPos(&mut pt).ok();
                    END_X.store(pt.x, Ordering::SeqCst);
                    END_Y.store(pt.y, Ordering::SeqCst);
                    MOUSE_DOWN.store(false, Ordering::SeqCst);

                    let sx = START_X.load(Ordering::SeqCst);
                    let sy = START_Y.load(Ordering::SeqCst);
                    let ex = END_X.load(Ordering::SeqCst);
                    let ey = END_Y.load(Ordering::SeqCst);

                    let dx = (ex - sx).abs();
                    let dy = (ey - sy).abs();

                    if dx <= CLICK_THRESHOLD && dy <= CLICK_THRESHOLD {
                        if let Some(cached) = find_window_at_point(pt) {
                            WINDOW_SELECTED.store(cached.hwnd as u32, Ordering::SeqCst);
                        }
                    }

                    SELECTING.store(false, Ordering::SeqCst);
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as i32;
                if key == VK_ESCAPE.0 as i32 {
                    CANCELLED.store(true, Ordering::SeqCst);
                    SELECTING.store(false, Ordering::SeqCst);
                    PostQuitMessage(0);
                } else if key == VK_RETURN.0 as i32 || key == VK_SPACE.0 as i32 {
                    FULLSCREEN.store(true, Ordering::SeqCst);
                    SELECTING.store(false, Ordering::SeqCst);
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                CANCELLED.store(true, Ordering::SeqCst);
                SELECTING.store(false, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_DESTROY => {
                // release the cached back buffer + dim bitmap that the
                // selector allocated on demand — leaks ~64 MB of GDI
                // memory per selector invocation otherwise.
                if let Some(dc) = BACK_DC.lock().unwrap().take() {
                    let _ = DeleteDC(HDC(dc as *mut _));
                }
                if let Some(bmp) = BACK_BITMAP.lock().unwrap().take() {
                    let _ = DeleteObject(HBITMAP(bmp as *mut _));
                }
                if let Some(dc) = DIM_DC.lock().unwrap().take() {
                    let _ = DeleteDC(HDC(dc as *mut _));
                }
                if let Some(bmp) = DIM_BITMAP.lock().unwrap().take() {
                    let _ = DeleteObject(HBITMAP(bmp as *mut _));
                }
                SELECTING.store(false, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    // Capture each monitor via the DXGI HDR pipeline and composite into a
    // single virtual-screen DIB. Used as the overlay preview snapshot
    // when the user is on an HDR display so the preview doesn't show the
    // GDI-clipped (washed-out white) version. Returns None on any failure
    // so the caller can fall back to GDI BitBlt.
    fn capture_virtual_screen_hdr_to_dib(
        virt_x: i32,
        virt_y: i32,
        virt_width: i32,
        virt_height: i32,
    ) -> Option<HBITMAP> {
        use crate::capture::HdrCapture;
        use windows::Win32::Graphics::Gdi::{
            CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };

        let monitors = xcap::Monitor::all().ok()?;
        if monitors.is_empty() {
            return None;
        }

        // top-down 32bpp DIB so memcpy lands at predictable offsets and
        // GDI reads pixels in the right order for SelectObject + BitBlt.
        let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: virt_width,
            biHeight: -virt_height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbmp = unsafe {
            CreateDIBSection(
                HDC::default(),
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

        let row_stride = (virt_width as usize) * 4;
        let total_bytes = row_stride * (virt_height as usize);
        // initialise to opaque black so monitors that fail capture don't
        // show random heap noise.
        unsafe {
            std::ptr::write_bytes(bits_ptr as *mut u8, 0, total_bytes);
        }

        let mut any_ok = false;
        for monitor in &monitors {
            let mx = monitor.x();
            let my = monitor.y();
            let mw = monitor.width() as i32;
            let mh = monitor.height() as i32;
            let center = (mx + mw / 2, my + mh / 2);
            let img = match HdrCapture::new().capture_with_hdr_at(Some(center)) {
                Ok((img, _)) => img,
                Err(_) => continue,
            };
            // blit the tonemapped pixels into the right place in the DIB.
            // RgbaImage is RGBA; GDI 32bpp DIB is BGRA. swap on copy.
            let img_w = img.width() as i32;
            let img_h = img.height() as i32;
            let dst_x0 = mx - virt_x;
            let dst_y0 = my - virt_y;
            for row in 0..img_h.min(mh) {
                let dst_y = dst_y0 + row;
                if dst_y < 0 || dst_y >= virt_height {
                    continue;
                }
                let src_row_offset = (row as usize) * (img_w as usize) * 4;
                for col in 0..img_w.min(mw) {
                    let dst_x = dst_x0 + col;
                    if dst_x < 0 || dst_x >= virt_width {
                        continue;
                    }
                    let src = src_row_offset + (col as usize) * 4;
                    let dst = (dst_y as usize) * row_stride + (dst_x as usize) * 4;
                    let pixels = img.as_raw();
                    if src + 3 >= pixels.len() {
                        continue;
                    }
                    unsafe {
                        let p = bits_ptr as *mut u8;
                        *p.add(dst) = pixels[src + 2]; // B
                        *p.add(dst + 1) = pixels[src + 1]; // G
                        *p.add(dst + 2) = pixels[src]; // R
                        *p.add(dst + 3) = 255;
                    }
                }
            }
            any_ok = true;
        }

        if !any_ok {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp);
            }
            return None;
        }

        Some(hbmp)
    }
}

#[cfg(not(windows))]
mod fallback_impl {
    use super::*;

    pub fn select() -> SelectionResult {
        SelectionResult::FullScreen
    }
}

pub struct UnifiedSelector;

impl UnifiedSelector {
    #[cfg(windows)]
    pub fn select() -> SelectionResult {
        windows_impl::select()
    }

    #[cfg(not(windows))]
    pub fn select() -> SelectionResult {
        fallback_impl::select()
    }
}
