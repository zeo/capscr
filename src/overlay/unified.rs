#![allow(dead_code)]

use crate::capture::Rectangle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionResult {
    Region(Rectangle),
    Window(u32),
    FullScreen,
    Cancelled,
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
            Graphics::Gdi::{
                AlphaBlend, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
                CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetDC,
                GetStockObject, InvalidateRect, ReleaseDC, SelectObject, SetBkColor, SetBkMode,
                SetTextColor, StretchBlt, TextOutW, AC_SRC_OVER, BLENDFUNCTION, HBITMAP, HDC,
                HOLLOW_BRUSH, OPAQUE, PAINTSTRUCT, PS_DASH, PS_SOLID, SRCCOPY, TRANSPARENT,
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
                    SM_YVIRTUALSCREEN, SW_SHOWMAXIMIZED, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN,
                    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                    WS_POPUP, WS_VISIBLE,
                },
            },
        },
    };

    const CLICK_THRESHOLD: i32 = 5;

    static SELECTING: AtomicBool = AtomicBool::new(false);
    static START_X: AtomicI32 = AtomicI32::new(0);
    static START_Y: AtomicI32 = AtomicI32::new(0);
    static END_X: AtomicI32 = AtomicI32::new(0);
    static END_Y: AtomicI32 = AtomicI32::new(0);
    static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);
    static CANCELLED: AtomicBool = AtomicBool::new(false);
    static FULLSCREEN: AtomicBool = AtomicBool::new(false);
    static WINDOW_SELECTED: AtomicU32 = AtomicU32::new(0);

    static SCREEN_BITMAP: Mutex<Option<isize>> = Mutex::new(None);
    static SCREEN_DC: Mutex<Option<isize>> = Mutex::new(None);
    static SCREEN_WIDTH: AtomicI32 = AtomicI32::new(0);
    static SCREEN_HEIGHT: AtomicI32 = AtomicI32::new(0);
    static VIRTUAL_X: AtomicI32 = AtomicI32::new(0);
    static VIRTUAL_Y: AtomicI32 = AtomicI32::new(0);

    static WINDOW_LIST: Mutex<Vec<CachedWindow>> = Mutex::new(Vec::new());
    static HOVERED_WINDOW: AtomicU32 = AtomicU32::new(0);

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

        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
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
        SELECTING.store(true, Ordering::SeqCst);
        START_X.store(0, Ordering::SeqCst);
        START_Y.store(0, Ordering::SeqCst);
        END_X.store(0, Ordering::SeqCst);
        END_Y.store(0, Ordering::SeqCst);
        MOUSE_DOWN.store(false, Ordering::SeqCst);
        CANCELLED.store(false, Ordering::SeqCst);
        FULLSCREEN.store(false, Ordering::SeqCst);
        WINDOW_SELECTED.store(0, Ordering::SeqCst);
        HOVERED_WINDOW.store(0, Ordering::SeqCst);

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
            let bitmap = CreateCompatibleBitmap(screen_dc, virt_width, virt_height);
            let old_bitmap = SelectObject(mem_dc, bitmap);

            BitBlt(mem_dc, 0, 0, virt_width, virt_height, screen_dc, virt_x, virt_y, SRCCOPY).ok();

            SelectObject(mem_dc, old_bitmap);
            ReleaseDC(None, screen_dc);

            *SCREEN_BITMAP.lock().unwrap() = Some(bitmap.0 as isize);
            *SCREEN_DC.lock().unwrap() = Some(mem_dc.0 as isize);

            let instance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(i) => i,
                Err(_) => return SelectionResult::Cancelled,
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
                    Err(_) => return SelectionResult::Cancelled,
                },
                ..Default::default()
            };

            RegisterClassW(&wc);

            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
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
                Err(_) => return SelectionResult::Cancelled,
            };

            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);

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

            if sx == ex || sy == ey {
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

                if let Some(dc) = *SCREEN_DC.lock().unwrap() {
                    if let Some(bmp) = *SCREEN_BITMAP.lock().unwrap() {
                        let mem_dc = HDC(dc as *mut _);
                        let old_bmp = SelectObject(mem_dc, HBITMAP(bmp as *mut _));
                        let _ = StretchBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, width, height, SRCCOPY);
                        SelectObject(mem_dc, old_bmp);
                    }
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

                    let dim_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00000000));

                    let top_rect = RECT { left: 0, top: 0, right: width, bottom: top };
                    let bottom_rect = RECT { left: 0, top: bottom, right: width, bottom: height };
                    let left_rect = RECT { left: 0, top, right: left, bottom };
                    let right_rect = RECT { left: right, top, right: width, bottom };

                    let alpha_dc = CreateCompatibleDC(hdc);
                    let alpha_bmp = CreateCompatibleBitmap(hdc, width, height);
                    let old_alpha_bmp = SelectObject(alpha_dc, alpha_bmp);

                    BitBlt(alpha_dc, 0, 0, width, height, hdc, 0, 0, SRCCOPY).ok();

                    FillRect(alpha_dc, &top_rect, dim_brush);
                    FillRect(alpha_dc, &bottom_rect, dim_brush);
                    FillRect(alpha_dc, &left_rect, dim_brush);
                    FillRect(alpha_dc, &right_rect, dim_brush);

                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: 128,
                        AlphaFormat: 0,
                    };

                    let _ = AlphaBlend(hdc, 0, 0, width, height, alpha_dc, 0, 0, width, height, blend);

                    SelectObject(alpha_dc, old_alpha_bmp);
                    let _ = DeleteObject(alpha_bmp);
                    let _ = DeleteDC(alpha_dc);
                    let _ = DeleteObject(dim_brush);

                    let border_pen = CreatePen(PS_SOLID, 2, windows::Win32::Foundation::COLORREF(0x00FF0000));
                    let dash_pen = CreatePen(PS_DASH, 1, windows::Win32::Foundation::COLORREF(0x00FFFFFF));

                    let old_pen = SelectObject(hdc, border_pen);
                    let hollow = GetStockObject(HOLLOW_BRUSH);
                    let old_brush = SelectObject(hdc, hollow);
                    SetBkMode(hdc, TRANSPARENT);

                    let _ = GdiRectangle(hdc, left, top, right, bottom);

                    SelectObject(hdc, dash_pen);
                    let _ = GdiRectangle(hdc, left + 1, top + 1, right - 1, bottom - 1);

                    SelectObject(hdc, old_pen);
                    SelectObject(hdc, old_brush);
                    let _ = DeleteObject(border_pen);
                    let _ = DeleteObject(dash_pen);

                    let sel_width = (right - left).abs();
                    let sel_height = (bottom - top).abs();
                    let size_text = format!("{}x{}", sel_width, sel_height);

                    let text_x = left + 5;
                    let text_y = if top > 20 { top - 18 } else { bottom + 5 };

                    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
                    SetBkColor(hdc, windows::Win32::Foundation::COLORREF(0x00000000));
                    SetBkMode(hdc, OPAQUE);

                    let text_wide: Vec<u16> = size_text.encode_utf16().collect();
                    let _ = TextOutW(hdc, text_x, text_y, &text_wide);
                } else if !mouse_down {
                    let hovered = HOVERED_WINDOW.load(Ordering::SeqCst);
                    if hovered != 0 {
                        let windows = WINDOW_LIST.lock().unwrap();
                        if let Some(cached) = windows.iter().find(|w| w.hwnd as u32 == hovered) {
                            let left = cached.left - virt_x;
                            let top = cached.top - virt_y;
                            let right = cached.right - virt_x;
                            let bottom = cached.bottom - virt_y;

                            let pen = CreatePen(PS_SOLID, 3, windows::Win32::Foundation::COLORREF(0x0000FF00));
                            let old_pen = SelectObject(hdc, pen);
                            let hollow = GetStockObject(HOLLOW_BRUSH);
                            let old_brush = SelectObject(hdc, hollow);
                            SetBkMode(hdc, TRANSPARENT);

                            let _ = GdiRectangle(hdc, left, top, right, bottom);

                            SelectObject(hdc, old_pen);
                            SelectObject(hdc, old_brush);
                            let _ = DeleteObject(pen);
                        }
                    }
                }

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let mouse_down = MOUSE_DOWN.load(Ordering::SeqCst);
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);

                if mouse_down {
                    END_X.store(pt.x, Ordering::SeqCst);
                    END_Y.store(pt.y, Ordering::SeqCst);
                    let _ = InvalidateRect(hwnd, None, false);
                } else if let Some(cached) = find_window_at_point(pt) {
                    let prev = HOVERED_WINDOW.swap(cached.hwnd as u32, Ordering::SeqCst);
                    if prev != cached.hwnd as u32 {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                } else {
                    let prev = HOVERED_WINDOW.swap(0, Ordering::SeqCst);
                    if prev != 0 {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT::default();
                GetCursorPos(&mut pt).ok();
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
            WM_DESTROY => {
                SELECTING.store(false, Ordering::SeqCst);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
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
