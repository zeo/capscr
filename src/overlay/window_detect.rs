#![allow(dead_code)]

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreatePen, DeleteObject, EndPaint, GetStockObject, InvalidateRect,
            SelectObject, SetBkMode, Rectangle as GdiRectangle, HOLLOW_BRUSH, PAINTSTRUCT,
            PS_SOLID, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::VK_ESCAPE,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
                GetAncestor, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongW,
                GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible,
                KillTimer, PostQuitMessage, RegisterClassW, SetLayeredWindowAttributes, SetTimer,
                ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GA_ROOT, GWL_EXSTYLE,
                GWL_STYLE, LWA_COLORKEY, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, WM_DESTROY, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
            },
        },
    },
};

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(windows)]
static SELECTING: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static HOVERED_HWND: AtomicU32 = AtomicU32::new(0);
#[cfg(windows)]
static SELECTED_HWND: AtomicU32 = AtomicU32::new(0);
#[cfg(windows)]
static CANCELLED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static OVERLAY_HWND: Mutex<Option<isize>> = Mutex::new(None);
#[cfg(windows)]
static WINDOW_LIST: Mutex<Vec<CachedWindow>> = Mutex::new(Vec::new());

#[cfg(windows)]
#[derive(Debug, Clone)]
struct CachedWindow {
    hwnd: isize,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Debug, Clone)]
pub struct DetectedWindow {
    pub hwnd: u32,
    pub title: String,
    pub rect: (i32, i32, u32, u32),
}

pub struct WindowDetector;

impl WindowDetector {
    #[cfg(windows)]
    fn enumerate_windows() -> Vec<CachedWindow> {
        let mut windows = Vec::new();

        unsafe {
            let windows_ptr = &mut windows as *mut Vec<CachedWindow>;
            let _ = EnumWindows(Some(enum_windows_callback), LPARAM(windows_ptr as isize));
        }

        windows
    }

    #[cfg(windows)]
    pub fn select() -> Option<u32> {
        SELECTING.store(true, Ordering::SeqCst);
        HOVERED_HWND.store(0, Ordering::SeqCst);
        SELECTED_HWND.store(0, Ordering::SeqCst);
        CANCELLED.store(false, Ordering::SeqCst);

        let windows = Self::enumerate_windows();
        *WINDOW_LIST.lock().unwrap() = windows;

        unsafe {
            let instance = GetModuleHandleW(PCWSTR::null()).ok()?;
            let class_name: Vec<u16> = "WindowDetectorClass\0".encode_utf16().collect();

            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_detect_wnd_proc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                    None,
                    windows::Win32::UI::WindowsAndMessaging::IDC_CROSS,
                )
                .ok()?,
                ..Default::default()
            };

            RegisterClassW(&wc);

            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                x,
                y,
                width,
                height,
                None,
                None,
                hinstance,
                None,
            )
            .ok()?;

            *OVERLAY_HWND.lock().unwrap() = Some(hwnd.0 as isize);

            SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0x00000001),
                1,
                LWA_COLORKEY,
            )
            .ok()?;

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetTimer(hwnd, 1, 16, None);

            let mut msg = MSG::default();
            while SELECTING.load(Ordering::SeqCst) {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    break;
                }
            }

            KillTimer(hwnd, 1).ok();
            let _ = DestroyWindow(hwnd);
            *OVERLAY_HWND.lock().unwrap() = None;
            WINDOW_LIST.lock().unwrap().clear();

            if CANCELLED.load(Ordering::SeqCst) {
                return None;
            }

            let selected = SELECTED_HWND.load(Ordering::SeqCst);
            if selected != 0 {
                Some(selected)
            } else {
                None
            }
        }
    }

    #[cfg(not(windows))]
    pub fn select() -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn find_window_at_point(pt: POINT) -> Option<CachedWindow> {
        let overlay = *OVERLAY_HWND.lock().unwrap();
        let windows = WINDOW_LIST.lock().unwrap();

        for win in windows.iter() {
            if Some(win.hwnd) == overlay {
                continue;
            }

            if pt.x >= win.left && pt.x < win.right &&
               pt.y >= win.top && pt.y < win.bottom {
                return Some(win.clone());
            }
        }
        None
    }

    #[cfg(windows)]
    pub fn get_window_at_cursor() -> Option<DetectedWindow> {
        unsafe {
            let mut pt = POINT::default();
            GetCursorPos(&mut pt).ok()?;

            let cached = Self::find_window_at_point(pt)?;
            let target_hwnd = HWND(cached.hwnd as *mut _);

            let width = (cached.right - cached.left) as u32;
            let height = (cached.bottom - cached.top) as u32;

            let title_len = GetWindowTextLengthW(target_hwnd);
            let title = if title_len > 0 {
                let mut buf: Vec<u16> = vec![0; (title_len + 1) as usize];
                GetWindowTextW(target_hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..title_len as usize])
            } else {
                String::new()
            };

            Some(DetectedWindow {
                hwnd: target_hwnd.0 as u32,
                title,
                rect: (cached.left, cached.top, width, height),
            })
        }
    }

    #[cfg(not(windows))]
    pub fn get_window_at_cursor() -> Option<DetectedWindow> {
        None
    }
}

#[cfg(windows)]
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
    let target = if !root.0.is_null() && root != hwnd {
        return BOOL(1);
    } else {
        hwnd
    };

    windows.push(CachedWindow {
        hwnd: target.0 as isize,
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    });

    BOOL(1)
}

#[cfg(windows)]
unsafe extern "system" fn window_detect_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_ok() {
                if let Some(cached) = WindowDetector::find_window_at_point(pt) {
                    let prev = HOVERED_HWND.swap(cached.hwnd as u32, Ordering::SeqCst);
                    if prev != cached.hwnd as u32 {
                        let _ = InvalidateRect(hwnd, None, true);
                    }
                } else {
                    let prev = HOVERED_HWND.swap(0, Ordering::SeqCst);
                    if prev != 0 {
                        let _ = InvalidateRect(hwnd, None, true);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let hovered = HOVERED_HWND.load(Ordering::SeqCst);
            if hovered != 0 {
                let windows = WINDOW_LIST.lock().unwrap();
                if let Some(cached) = windows.iter().find(|w| w.hwnd as u32 == hovered) {
                    let offset_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let offset_y = GetSystemMetrics(SM_YVIRTUALSCREEN);

                    let pen = CreatePen(
                        PS_SOLID,
                        3,
                        windows::Win32::Foundation::COLORREF(0x0000FF00),
                    );
                    let old_pen = SelectObject(hdc, pen);
                    let hollow = GetStockObject(HOLLOW_BRUSH);
                    let old_brush = SelectObject(hdc, hollow);
                    SetBkMode(hdc, TRANSPARENT);

                    let _ = GdiRectangle(
                        hdc,
                        cached.left - offset_x,
                        cached.top - offset_y,
                        cached.right - offset_x,
                        cached.bottom - offset_y,
                    );

                    SelectObject(hdc, old_pen);
                    SelectObject(hdc, old_brush);
                    let _ = DeleteObject(pen);
                }
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let hovered = HOVERED_HWND.load(Ordering::SeqCst);
            if hovered != 0 {
                SELECTED_HWND.store(hovered, Ordering::SeqCst);
                SELECTING.store(false, Ordering::SeqCst);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as i32 == VK_ESCAPE.0 as i32 {
                CANCELLED.store(true, Ordering::SeqCst);
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
