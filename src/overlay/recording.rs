#![allow(dead_code)]

use crate::capture::Rectangle;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, CreatePen, DeleteObject, EndPaint, GetStockObject, InvalidateRect,
                SelectObject, SetBkMode, HOLLOW_BRUSH, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
                Rectangle as GdiRectangle,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
                KillTimer, PostMessageW, RegisterClassW, SetLayeredWindowAttributes, SetTimer,
                ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, LWA_COLORKEY, MSG,
                SW_HIDE, SW_SHOWNA, WM_DESTROY, WM_PAINT, WM_TIMER, WM_USER, WNDCLASSW,
                WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    };

    const WM_STOP_OVERLAY: u32 = WM_USER + 1;
    const BORDER_WIDTH: i32 = 4;
    const TIMER_ID: usize = 1;
    const FLASH_INTERVAL_MS: u32 = 500;

    static OVERLAY_HWND: Mutex<Option<isize>> = Mutex::new(None);
    static REGION_X: AtomicI32 = AtomicI32::new(0);
    static REGION_Y: AtomicI32 = AtomicI32::new(0);
    static REGION_W: AtomicI32 = AtomicI32::new(0);
    static REGION_H: AtomicI32 = AtomicI32::new(0);
    static FLASH_STATE: AtomicBool = AtomicBool::new(true);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn start(region: Rectangle) {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }

        REGION_X.store(region.x, Ordering::SeqCst);
        REGION_Y.store(region.y, Ordering::SeqCst);
        REGION_W.store(region.width as i32, Ordering::SeqCst);
        REGION_H.store(region.height as i32, Ordering::SeqCst);
        FLASH_STATE.store(true, Ordering::SeqCst);

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

    fn run_overlay_loop() {
        unsafe {
            let instance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(i) => i,
                Err(_) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let class_name: Vec<u16> = "RecordingOverlayClass\0".encode_utf16().collect();
            let hinstance = windows::Win32::Foundation::HINSTANCE(instance.0);

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };

            RegisterClassW(&wc);

            let x = REGION_X.load(Ordering::SeqCst) - BORDER_WIDTH;
            let y = REGION_Y.load(Ordering::SeqCst) - BORDER_WIDTH;
            let w = REGION_W.load(Ordering::SeqCst) + BORDER_WIDTH * 2;
            let h = REGION_H.load(Ordering::SeqCst) + BORDER_WIDTH * 2;

            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                PCWSTR(class_name.as_ptr()),
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

            let _ = SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0x00010101),
                255,
                LWA_COLORKEY,
            );

            let _ = ShowWindow(hwnd, SW_SHOWNA);
            let _ = SetTimer(hwnd, TIMER_ID, FLASH_INTERVAL_MS, None);

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

            KillTimer(hwnd, TIMER_ID).ok();
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = DestroyWindow(hwnd);
            *OVERLAY_HWND.lock().unwrap() = None;
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

                let bg_brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                    windows::Win32::Foundation::COLORREF(0x00010101),
                );
                let bg_rect = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                windows::Win32::Graphics::Gdi::FillRect(hdc, &bg_rect, bg_brush);
                let _ = DeleteObject(bg_brush);

                if FLASH_STATE.load(Ordering::SeqCst) {
                    let red = windows::Win32::Foundation::COLORREF(0x000000FF);
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
                if wparam.0 == TIMER_ID {
                    let current = FLASH_STATE.load(Ordering::SeqCst);
                    FLASH_STATE.store(!current, Ordering::SeqCst);
                    let _ = InvalidateRect(hwnd, None, true);
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
}

#[cfg(not(windows))]
mod fallback_impl {
    use super::*;

    pub fn start(_region: Rectangle) {}
    pub fn stop() {}
}

pub struct RecordingOverlay;

impl RecordingOverlay {
    #[cfg(windows)]
    pub fn start(region: Rectangle) {
        windows_impl::start(region);
    }

    #[cfg(not(windows))]
    pub fn start(region: Rectangle) {
        fallback_impl::start(region);
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
