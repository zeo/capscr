#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen,
            CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, InvalidateRect,
            ReleaseDC, SelectObject, SetBkMode, StretchBlt, Rectangle as GdiRectangle, HBITMAP,
            HDC, PAINTSTRUCT, PS_DASH, PS_SOLID, SRCCOPY, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::VK_ESCAPE,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, GetSystemMetrics, PostQuitMessage, RegisterClassW, ShowWindow,
                TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, SM_CXVIRTUALSCREEN,
                SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWMAXIMIZED,
                WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
                WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
};

use crate::capture::Rectangle;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

static SELECTING: AtomicBool = AtomicBool::new(false);
static START_X: AtomicI32 = AtomicI32::new(0);
static START_Y: AtomicI32 = AtomicI32::new(0);
static END_X: AtomicI32 = AtomicI32::new(0);
static END_Y: AtomicI32 = AtomicI32::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

static SCREEN_BITMAP: Mutex<Option<isize>> = Mutex::new(None);
static SCREEN_DC: Mutex<Option<isize>> = Mutex::new(None);
static SCREEN_WIDTH: AtomicI32 = AtomicI32::new(0);
static SCREEN_HEIGHT: AtomicI32 = AtomicI32::new(0);
static VIRTUAL_X: AtomicI32 = AtomicI32::new(0);
static VIRTUAL_Y: AtomicI32 = AtomicI32::new(0);

pub struct RegionSelector;

impl RegionSelector {
    #[cfg(windows)]
    pub fn select() -> Option<Rectangle> {
        SELECTING.store(false, Ordering::SeqCst);
        START_X.store(0, Ordering::SeqCst);
        START_Y.store(0, Ordering::SeqCst);
        END_X.store(0, Ordering::SeqCst);
        END_Y.store(0, Ordering::SeqCst);
        DRAGGING.store(false, Ordering::SeqCst);
        CANCELLED.store(false, Ordering::SeqCst);

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

            let instance = GetModuleHandleW(PCWSTR::null()).ok()?;
            let class_name: Vec<u16> = "RegionSelectorClass\0".encode_utf16().collect();

            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(region_wnd_proc),
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

            let hwnd = CreateWindowExW(
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
            )
            .ok()?;

            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);

            SELECTING.store(true, Ordering::SeqCst);

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
                DeleteDC(HDC(dc as *mut _));
            }
            if let Some(bmp) = SCREEN_BITMAP.lock().unwrap().take() {
                let _ = DeleteObject(HBITMAP(bmp as *mut _));
            }

            if CANCELLED.load(Ordering::SeqCst) {
                return None;
            }

            let sx = START_X.load(Ordering::SeqCst);
            let sy = START_Y.load(Ordering::SeqCst);
            let ex = END_X.load(Ordering::SeqCst);
            let ey = END_Y.load(Ordering::SeqCst);

            if sx == ex || sy == ey {
                return None;
            }

            Some(Rectangle::normalize(sx, sy, ex, ey))
        }
    }

    #[cfg(not(windows))]
    pub fn select() -> Option<Rectangle> {
        None
    }
}

#[cfg(windows)]
unsafe extern "system" fn region_wnd_proc(
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

            let dim_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00000000));

            if DRAGGING.load(Ordering::SeqCst) {
                let sx = START_X.load(Ordering::SeqCst) - virt_x;
                let sy = START_Y.load(Ordering::SeqCst) - virt_y;
                let ex = END_X.load(Ordering::SeqCst) - virt_x;
                let ey = END_Y.load(Ordering::SeqCst) - virt_y;

                let left = sx.min(ex);
                let top = sy.min(ey);
                let right = sx.max(ex);
                let bottom = sy.max(ey);

                let top_rect = RECT { left: 0, top: 0, right: width, bottom: top };
                let bottom_rect = RECT { left: 0, top: bottom, right: width, bottom: height };
                let left_rect = RECT { left: 0, top, right: left, bottom };
                let right_rect = RECT { left: right, top, right: width, bottom };

                windows::Win32::Graphics::Gdi::SetBkMode(hdc, windows::Win32::Graphics::Gdi::OPAQUE);

                let alpha_dc = CreateCompatibleDC(hdc);
                let alpha_bmp = CreateCompatibleBitmap(hdc, width, height);
                let old_alpha_bmp = SelectObject(alpha_dc, alpha_bmp);

                BitBlt(alpha_dc, 0, 0, width, height, hdc, 0, 0, SRCCOPY).ok();

                FillRect(alpha_dc, &top_rect, dim_brush);
                FillRect(alpha_dc, &bottom_rect, dim_brush);
                FillRect(alpha_dc, &left_rect, dim_brush);
                FillRect(alpha_dc, &right_rect, dim_brush);

                use windows::Win32::Graphics::Gdi::{AC_SRC_OVER, BLENDFUNCTION, AlphaBlend};
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 128,
                    AlphaFormat: 0,
                };

                AlphaBlend(hdc, 0, 0, width, height, alpha_dc, 0, 0, width, height, blend).ok();

                SelectObject(alpha_dc, old_alpha_bmp);
                let _ = DeleteObject(alpha_bmp);
                DeleteDC(alpha_dc);

                let border_pen = CreatePen(PS_SOLID, 2, windows::Win32::Foundation::COLORREF(0x00FF0000));
                let dash_pen = CreatePen(PS_DASH, 1, windows::Win32::Foundation::COLORREF(0x00FFFFFF));

                let old_pen = SelectObject(hdc, border_pen);
                let hollow = windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::HOLLOW_BRUSH);
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

                windows::Win32::Graphics::Gdi::SetTextColor(hdc, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
                windows::Win32::Graphics::Gdi::SetBkColor(hdc, windows::Win32::Foundation::COLORREF(0x00000000));
                windows::Win32::Graphics::Gdi::SetBkMode(hdc, windows::Win32::Graphics::Gdi::OPAQUE);

                let text_wide: Vec<u16> = size_text.encode_utf16().collect();
                windows::Win32::Graphics::Gdi::TextOutW(hdc, text_x, text_y, &text_wide);
            }

            let _ = DeleteObject(dim_brush);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            GetCursorPos(&mut pt).ok();
            START_X.store(pt.x, Ordering::SeqCst);
            START_Y.store(pt.y, Ordering::SeqCst);
            END_X.store(pt.x, Ordering::SeqCst);
            END_Y.store(pt.y, Ordering::SeqCst);
            DRAGGING.store(true, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if DRAGGING.load(Ordering::SeqCst) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                END_X.store(pt.x, Ordering::SeqCst);
                END_Y.store(pt.y, Ordering::SeqCst);
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if DRAGGING.load(Ordering::SeqCst) {
                let mut pt = POINT::default();
                GetCursorPos(&mut pt).ok();
                END_X.store(pt.x, Ordering::SeqCst);
                END_Y.store(pt.y, Ordering::SeqCst);
                DRAGGING.store(false, Ordering::SeqCst);
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
