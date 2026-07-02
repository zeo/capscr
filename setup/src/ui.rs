//! capscr-setup's face: one frameless window on the same hard greyscale
//! ladder the app uses, painted with GDI so it comes up anywhere. the real
//! work is msiexec's; this window is the part the user actually sees

use std::sync::{Arc, Mutex};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetTextExtentPoint32W, InvalidateRect,
    SelectObject, SetBkMode, SetTextCharacterExtra, SetTextColor, TextOutW, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, DIB_RGB_COLORS, FW_BOLD, FW_NORMAL, HBITMAP,
    HDC, HGDIOBJ, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, KillTimer, LoadCursorW, LoadIconW, MoveWindow,
    PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HTCAPTION, HTCLIENT, IDC_ARROW, MSG, SM_CXSCREEN,
    SM_CYSCREEN, SW_SHOW, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSW, WS_POPUP,
    WS_VISIBLE,
};

use crate::engine;

const INK0: COLORREF = rgb(0x05, 0x05, 0x05);
const BG: COLORREF = rgb(0x14, 0x14, 0x14);
const SURFACE: COLORREF = rgb(0x1c, 0x1c, 0x1c);
const RULE: COLORREF = rgb(0x2a, 0x2a, 0x2a);
const RULE2: COLORREF = rgb(0x3a, 0x3a, 0x3a);
const MUTE: COLORREF = rgb(0x6f, 0x6f, 0x6f);
const TEXT: COLORREF = rgb(0xc4, 0xc4, 0xc4);
const TEXT2: COLORREF = rgb(0xed, 0xed, 0xed);
const PAPER: COLORREF = rgb(0xf5, 0xf5, 0xf5);
const PAPER_DIM: COLORREF = rgb(0xdc, 0xdc, 0xdc);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | r as u32)
}

pub const WIN_W: i32 = 560;
pub const WIN_H: i32 = 380;

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Welcome,
    Progress,
    Done,
    Error,
    RemoveConfirm,
    Removed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Close,
    Install,
    Launch,
    Quit,
    Remove,
    Cancel,
}

pub struct State {
    pub page: Page,
    pub error: String,
    /// indeterminate sweep phase, ticked by a timer while msiexec runs
    pub sweep: f32,
    pub removing: bool,
    hover: Option<Hit>,
}

impl State {
    pub fn new(page: Page) -> Self {
        State { page, error: String::new(), sweep: 0.0, removing: false, hover: None }
    }
}

pub type Shared = Arc<Mutex<State>>;

static CTX: Mutex<Option<Shared>> = Mutex::new(None);

fn hit_rects(dpi: f32, s: &State) -> Vec<(Hit, RECT)> {
    let px = |v: i32| (v as f32 * dpi) as i32;
    let mut out = Vec::new();
    out.push((Hit::Close, RECT { left: px(WIN_W - 44), top: px(12), right: px(WIN_W - 12), bottom: px(44) }));
    match s.page {
        Page::Welcome => {
            out.push((Hit::Install, RECT { left: px(WIN_W / 2 - 100), top: px(266), right: px(WIN_W / 2 + 100), bottom: px(306) }));
        }
        Page::Done => {
            out.push((Hit::Launch, RECT { left: px(WIN_W / 2 - 150), top: px(230), right: px(WIN_W / 2 - 10), bottom: px(270) }));
            out.push((Hit::Quit, RECT { left: px(WIN_W / 2 + 10), top: px(230), right: px(WIN_W / 2 + 150), bottom: px(270) }));
        }
        Page::Error | Page::Removed => {
            out.push((Hit::Quit, RECT { left: px(WIN_W / 2 - 70), top: px(280), right: px(WIN_W / 2 + 70), bottom: px(320) }));
        }
        Page::RemoveConfirm => {
            out.push((Hit::Remove, RECT { left: px(WIN_W / 2 - 150), top: px(230), right: px(WIN_W / 2 - 10), bottom: px(270) }));
            out.push((Hit::Cancel, RECT { left: px(WIN_W / 2 + 10), top: px(230), right: px(WIN_W / 2 + 150), bottom: px(270) }));
        }
        Page::Progress => {}
    }
    out
}

fn in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

struct Painter {
    hdc: HDC,
    dpi: f32,
}

impl Painter {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.dpi) as i32
    }

    fn fill(&self, x: i32, y: i32, w: i32, h: i32, c: COLORREF) {
        unsafe {
            let b = CreateSolidBrush(c);
            let r = RECT { left: self.px(x), top: self.px(y), right: self.px(x + w), bottom: self.px(y + h) };
            FillRect(self.hdc, &r, b);
            let _ = DeleteObject(b.into());
        }
    }

    fn rule(&self, x: i32, y: i32, w: i32, c: COLORREF) {
        unsafe {
            let b = CreateSolidBrush(c);
            let r = RECT { left: self.px(x), top: self.px(y), right: self.px(x + w), bottom: self.px(y) + self.dpi.max(1.0) as i32 };
            FillRect(self.hdc, &r, b);
            let _ = DeleteObject(b.into());
        }
    }

    fn stroke(&self, x: i32, y: i32, w: i32, h: i32, c: COLORREF) {
        self.fill(x, y, w, 1, c);
        self.fill(x, y + h - 1, w, 1, c);
        self.fill(x, y, 1, h, c);
        self.fill(x + w - 1, y, 1, h, c);
    }

    fn font(&self, px: i32, bold: bool) -> isize {
        unsafe {
            CreateFontW(
                -self.px(px),
                0,
                0,
                0,
                if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
                0,
                0,
                0,
                Default::default(),
                Default::default(),
                Default::default(),
                CLEARTYPE_QUALITY,
                Default::default(),
                w!("Cascadia Mono"),
            )
            .0 as isize
        }
    }

    fn text(&self, x: i32, y: i32, text: &str, px_size: i32, c: COLORREF, bold: bool, track: i32) {
        unsafe {
            let f = self.font(px_size, bold);
            let old: HGDIOBJ = SelectObject(self.hdc, HGDIOBJ(f as *mut core::ffi::c_void));
            SetBkMode(self.hdc, TRANSPARENT);
            SetTextColor(self.hdc, c);
            SetTextCharacterExtra(self.hdc, (track as f32 * self.dpi) as i32);
            let wtext: Vec<u16> = text.encode_utf16().collect();
            let _ = TextOutW(self.hdc, self.px(x), self.px(y), &wtext);
            SelectObject(self.hdc, old);
            let _ = DeleteObject(HGDIOBJ(f as *mut core::ffi::c_void));
        }
    }

    fn text_w(&self, text: &str, px_size: i32, bold: bool, track: i32) -> i32 {
        unsafe {
            let f = self.font(px_size, bold);
            let old = SelectObject(self.hdc, HGDIOBJ(f as *mut core::ffi::c_void));
            SetTextCharacterExtra(self.hdc, (track as f32 * self.dpi) as i32);
            let wtext: Vec<u16> = text.encode_utf16().collect();
            let mut size = windows::Win32::Foundation::SIZE::default();
            let _ = GetTextExtentPoint32W(self.hdc, &wtext, &mut size);
            SelectObject(self.hdc, old);
            let _ = DeleteObject(HGDIOBJ(f as *mut core::ffi::c_void));
            (size.cx as f32 / self.dpi) as i32
        }
    }

    fn text_centered(&self, cx: i32, y: i32, text: &str, px_size: i32, c: COLORREF, bold: bool, track: i32) {
        let w = self.text_w(text, px_size, bold, track);
        self.text(cx - w / 2, y, text, px_size, c, bold, track);
    }

    fn button(&self, x: i32, y: i32, w: i32, h: i32, label: &str, primary: bool, hovered: bool) {
        if primary {
            self.fill(x, y, w, h, if hovered { PAPER } else { PAPER_DIM });
            let tw = self.text_w(label, 13, false, 3);
            self.text(x + (w - tw) / 2, y + (h - 16) / 2, label, 13, INK0, false, 3);
        } else {
            self.stroke(x, y, w, h, if hovered { PAPER } else { RULE2 });
            let tw = self.text_w(label, 13, false, 3);
            self.text(x + (w - tw) / 2, y + (h - 16) / 2, label, 13, if hovered { TEXT2 } else { MUTE }, false, 3);
        }
    }
}

pub fn paint(hdc: HDC, dpi: f32, s: &State) {
    let p = Painter { hdc, dpi };
    p.fill(0, 0, WIN_W, WIN_H, BG);
    p.fill(0, WIN_H - 36, WIN_W, 36, INK0);
    p.rule(0, WIN_H - 36, WIN_W, RULE);

    // the mark: a capture frame — a paper chip with a hollow "screen" inside
    p.fill(24, 18, 40, 26, PAPER);
    p.fill(31, 24, 26, 14, INK0);
    p.fill(33, 26, 22, 10, PAPER);
    p.text(76, 21, "capscr", 17, TEXT2, false, 0);
    let close_hover = s.hover == Some(Hit::Close);
    if close_hover {
        p.fill(WIN_W - 44, 12, 32, 32, SURFACE);
    }
    p.text_centered(WIN_W - 28, 19, "\u{00d7}", 15, if close_hover { TEXT2 } else { MUTE }, false, 0);

    p.rule(0, 64, WIN_W, RULE);
    p.text(24, 80, "HDR-AWARE SCREEN CAPTURE", 11, MUTE, false, 3);
    let ver = engine::APP_VERSION.to_uppercase();
    let vw = p.text_w(&ver, 11, false, 3);
    p.text(WIN_W - 24 - vw, 80, &ver, 11, MUTE, false, 3);

    p.text(24, WIN_H - 27, "MIT \u{b7} SIGNED INSTALLER INSIDE \u{b7} NO TELEMETRY", 10, MUTE, false, 2);
    let rw = p.text_w("ROT", 10, false, 2);
    p.text(WIN_W - 24 - rw, WIN_H - 27, "ROT", 10, MUTE, false, 2);

    match s.page {
        Page::Welcome => {
            p.text(24, 126, "tray-first screen capture with HDR tone mapping,", 13, TEXT, false, 0);
            p.text(24, 148, "per-hotkey task chains, and a plugin marketplace.", 13, TEXT, false, 0);
            p.text(24, 184, "INSTALLS FOR ALL USERS \u{b7} WINDOWS ASKS ONCE", 10, MUTE, false, 2);
            p.text(24, 202, "UPDATES ARRIVE SIGNED, IN THE APP", 10, MUTE, false, 2);
            p.button(WIN_W / 2 - 100, 266, 200, 40, "INSTALL", true, s.hover == Some(Hit::Install));
        }
        Page::Progress => {
            p.text_centered(WIN_W / 2, 140, if s.removing { "REMOVING" } else { "INSTALLING" }, 13, TEXT2, false, 4);
            // indeterminate sweep: a paper segment gliding along a hairline
            let tx = 60;
            let tw = WIN_W - 120;
            p.fill(tx, 184, tw, 3, RULE);
            let seg = tw / 4;
            let range = tw + seg;
            let pos = ((s.sweep * range as f32) as i32 % range) - seg;
            let x0 = tx + pos.max(0);
            let x1 = (tx + pos + seg).min(tx + tw);
            if x1 > x0 {
                p.fill(x0, 184, x1 - x0, 3, PAPER);
            }
            p.text_centered(WIN_W / 2, 208, "windows installer is working", 11, MUTE, false, 0);
        }
        Page::Done => {
            p.text_centered(WIN_W / 2, 134, "INSTALLED", 13, TEXT2, false, 4);
            p.text_centered(WIN_W / 2, 162, "capscr lives in the tray \u{2014} look for the frame", 11, MUTE, false, 0);
            p.button(WIN_W / 2 - 150, 230, 140, 40, "LAUNCH", true, s.hover == Some(Hit::Launch));
            p.button(WIN_W / 2 + 10, 230, 140, 40, "CLOSE", false, s.hover == Some(Hit::Quit));
        }
        Page::Error => {
            p.text_centered(WIN_W / 2, 134, "INSTALL FAILED", 13, TEXT2, false, 4);
            let mut y = 166;
            let mut line = String::new();
            for word in s.error.split_whitespace() {
                if line.len() + word.len() + 1 > 64 {
                    p.text_centered(WIN_W / 2, y, &line, 11, MUTE, false, 0);
                    y += 18;
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            if !line.is_empty() {
                p.text_centered(WIN_W / 2, y, &line, 11, MUTE, false, 0);
            }
            p.button(WIN_W / 2 - 70, 280, 140, 40, "CLOSE", false, s.hover == Some(Hit::Quit));
        }
        Page::RemoveConfirm => {
            p.text_centered(WIN_W / 2, 134, "REMOVE CAPSCR?", 13, TEXT2, false, 4);
            p.text_centered(WIN_W / 2, 162, "captures and settings stay on disk", 11, MUTE, false, 0);
            p.button(WIN_W / 2 - 150, 230, 140, 40, "REMOVE", true, s.hover == Some(Hit::Remove));
            p.button(WIN_W / 2 + 10, 230, 140, 40, "CANCEL", false, s.hover == Some(Hit::Cancel));
        }
        Page::Removed => {
            p.text_centered(WIN_W / 2, 140, "REMOVED", 13, TEXT2, false, 4);
            p.button(WIN_W / 2 - 70, 280, 140, 40, "CLOSE", false, s.hover == Some(Hit::Quit));
        }
    }
}

const WM_ENGINE_DONE: u32 = WM_APP + 1;
const TIMER_SWEEP: usize = 3;

pub fn run(shared: Shared) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let class = w!("capscr_setup");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        *CTX.lock().unwrap() = Some(shared.clone());

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("capscr setup"),
            WS_POPUP | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W,
            WIN_H,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let dpi = GetDpiForWindow(hwnd) as f32 / 96.0;
        let w = (WIN_W as f32 * dpi) as i32;
        let h = (WIN_H as f32 * dpi) as i32;
        let _ = MoveWindow(hwnd, (sw - w) / 2, (sh - h) / 2, w, h, true);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        *CTX.lock().unwrap() = None;
    }
}

fn start_engine(hwnd: HWND, shared: Shared, removing: bool) {
    {
        let mut s = shared.lock().unwrap();
        s.page = Page::Progress;
        s.removing = removing;
        s.sweep = 0.0;
    }
    unsafe {
        SetTimer(Some(hwnd), TIMER_SWEEP, 33, None);
    }
    let hwnd_val = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = if removing { engine::uninstall() } else { engine::install() };
        {
            let mut s = shared.lock().unwrap();
            match result {
                Ok(()) if removing => s.page = Page::Removed,
                Ok(()) => s.page = Page::Done,
                Err(e) => {
                    s.page = Page::Error;
                    s.error = e;
                }
            }
        }
        unsafe {
            let hw = HWND(hwnd_val as *mut core::ffi::c_void);
            let _ = PostMessageW(Some(hw), WM_ENGINE_DONE, WPARAM(0), LPARAM(0));
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let shared = CTX.lock().unwrap().clone();
    let Some(shared) = shared else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let mem = CreateCompatibleDC(Some(hdc));
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(hdc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
                let old = SelectObject(mem, bmp.into());
                {
                    let s = shared.lock().unwrap();
                    let dpi = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
                    paint(mem, dpi, &s);
                }
                let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
            }
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            if wp.0 == TIMER_SWEEP {
                let mut still = false;
                {
                    let mut s = shared.lock().unwrap();
                    if s.page == Page::Progress {
                        s.sweep = (s.sweep + 0.012) % 1.0;
                        still = true;
                    }
                }
                if still {
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                } else {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_SWEEP);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lp.0 & 0xffff) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
            let dpi = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
            let mut changed = false;
            {
                let mut s = shared.lock().unwrap();
                let hit = hit_rects(dpi, &s).into_iter().find(|(_, r)| in_rect(r, x, y)).map(|(h, _)| h);
                if s.hover != hit {
                    s.hover = hit;
                    changed = true;
                }
            }
            if changed {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lp.0 & 0xffff) as i16 as i32;
            let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
            let dpi = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
            let hit = {
                let s = shared.lock().unwrap();
                hit_rects(dpi, &s).into_iter().find(|(_, r)| in_rect(r, x, y)).map(|(h, _)| h)
            };
            if let Some(hit) = hit {
                let page = shared.lock().unwrap().page;
                match hit {
                    Hit::Close | Hit::Quit | Hit::Cancel => {
                        if page != Page::Progress {
                            unsafe { PostQuitMessage(0) };
                        }
                    }
                    Hit::Install => start_engine(hwnd, shared.clone(), false),
                    Hit::Remove => start_engine(hwnd, shared.clone(), true),
                    Hit::Launch => {
                        engine::launch_app();
                        unsafe { PostQuitMessage(0) };
                    }
                }
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let r = unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            if r.0 == HTCLIENT as isize {
                let mut pt = windows::Win32::Foundation::POINT {
                    x: (lp.0 & 0xffff) as i16 as i32,
                    y: ((lp.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) };
                let dpi = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
                if pt.y < (64.0 * dpi) as i32 && pt.x < ((WIN_W - 52) as f32 * dpi) as i32 {
                    return LRESULT(HTCAPTION as isize);
                }
            }
            r
        }
        WM_ENGINE_DONE => {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_CLOSE | WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// ---- headless preview -------------------------------------------------------

pub fn preview(path: &str, state: &State, scale: f32) -> Result<(), String> {
    unsafe {
        let w = (WIN_W as f32 * scale) as i32;
        let h = (WIN_H as f32 * scale) as i32;
        let mem = CreateCompatibleDC(None);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp: HBITMAP = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        let old = SelectObject(mem, bmp.into());
        paint(mem, scale, state);
        windows::Win32::Graphics::Gdi::GdiFlush();
        let n = (w * h) as usize;
        let px = std::slice::from_raw_parts(bits as *const u8, n * 4);
        let mut rgba = vec![0u8; n * 4];
        for i in 0..n {
            rgba[i * 4] = px[i * 4 + 2];
            rgba[i * 4 + 1] = px[i * 4 + 1];
            rgba[i * 4 + 2] = px[i * 4];
            rgba[i * 4 + 3] = 255;
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        write_png(path, w as u32, h as u32, &rgba)
    }
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (n, t) in table.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
            *t = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_in = Vec::with_capacity(4 + data.len());
        crc_in.extend_from_slice(kind);
        crc_in.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for row in 0..h as usize {
        raw.push(0);
        let s = row * w as usize * 4;
        raw.extend_from_slice(&rgba[s..s + w as usize * 4]);
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png).map_err(|e| e.to_string())
}
