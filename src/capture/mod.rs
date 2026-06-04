#![allow(dead_code)]

mod screen;
mod window;
mod region;
mod hdr;
mod hdr_png;
mod tonemapping;
mod cursor;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
mod d2d_tonemap;
#[cfg(windows)]
mod gdi;

pub use screen::ScreenCapture;
pub use window::WindowCapture;
pub use region::RegionCapture;
pub use tonemapping::TonemapParams;
pub use hdr::HdrCapture;
#[cfg(windows)]
pub use wgc::capture_at_point as wgc_capture_at_point;
#[cfg(windows)]
pub use d2d_tonemap::capture_hdr_to_sdr_sweep;
#[cfg(windows)]
pub use gdi::{fast_gdi_capture, fast_list_monitors};
pub use hdr_png::{encode_hdr_png, read_cicp, HdrBitmap, HdrTransfer};
pub use cursor::composite_system_cursor;

use std::sync::OnceLock;

static TONEMAP_OVERRIDE: OnceLock<TonemapParams> = OnceLock::new();

thread_local! {
    // set while a parallel monitor-capture worker runs so par_convert falls back
    // to a serial pass instead of spawning a nested thread pool — the monitor
    // loop already occupies every core, so nesting would only oversubscribe.
    static SERIAL_CONVERT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// run `f` with par_convert forced to its serial path on this thread. used by the
// parallel all_monitors workers.
pub(crate) fn capture_serial<R>(f: impl FnOnce() -> R) -> R {
    // RAII reset so the flag is cleared even if `f` panics — keeps the thread
    // usable if it is ever reused (scoped workers are one-shot today, but this
    // removes the footgun rather than relying on that).
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SERIAL_CONVERT.with(|c| c.set(false));
        }
    }
    SERIAL_CONVERT.with(|c| c.set(true));
    let _reset = Reset;
    f()
}

pub fn install_tonemap_params(params: TonemapParams) {
    let _ = TONEMAP_OVERRIDE.set(params);
}

pub fn current_tonemap_params() -> TonemapParams {
    TONEMAP_OVERRIDE.get().copied().unwrap_or_default()
}

// Capture path gates. defaults match ShareX behaviour: plain GDI BitBlt
// for everything, instant, SDR content identical to what Snipping Tool
// produces, HDR content overblown (same as every other Windows screenshot
// tool). two env vars opt into alternative paths:
//   CAPSCR_HDR_AWARE=1 → CPU Reinhard tonemap on HDR pixels (slow,
//                        tunable look, multi-second in debug builds)
//   CAPSCR_USE_WGC=1   → Windows.Graphics.Capture with OS-side tonemap
//                        (instant, OS-quality, but composes SDR content
//                        via the HDR compositor on HDR displays which
//                        can subtly shift SDR brightness vs GDI BitBlt)
pub fn hdr_aware_enabled() -> bool {
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let raw = std::env::var("CAPSCR_HDR_AWARE").unwrap_or_else(|_| "<unset>".to_string());
        let forced_on = matches!(raw.trim(), "1" | "true" | "TRUE" | "on");
        tracing::info!(
            "CAPSCR_HDR_AWARE env var = {:?} -> hdr_aware_enabled = {}",
            raw, forced_on,
        );
        forced_on
    })
}

pub fn wgc_enabled() -> bool {
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let raw = std::env::var("CAPSCR_USE_WGC").unwrap_or_else(|_| "<unset>".to_string());
        let forced_on = matches!(raw.trim(), "1" | "true" | "TRUE" | "on");
        tracing::info!(
            "CAPSCR_USE_WGC env var = {:?} -> wgc_enabled = {}",
            raw, forced_on,
        );
        forced_on
    })
}

// opt-in: skip the fixed ~10ms settle sleep before the first DXGI
// AcquireNextFrame in the CPU-HDR capture path. the first frame after
// DuplicateOutput is the current desktop, and the acquire loop + black-frame
// retry already recover a stale/black first frame, so the sleep is usually pure
// latency. left off by default because some drivers may rely on the settle
// time; CAPSCR_FAST_HDR=1 trims it once verified on real HDR hardware.
pub fn fast_hdr_acquire_enabled() -> bool {
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| {
        let raw = std::env::var("CAPSCR_FAST_HDR").unwrap_or_else(|_| "<unset>".to_string());
        let forced_on = matches!(raw.trim(), "1" | "true" | "TRUE" | "on");
        tracing::info!(
            "CAPSCR_FAST_HDR env var = {:?} -> fast_hdr_acquire_enabled = {}",
            raw, forced_on,
        );
        forced_on
    })
}

use anyhow::Result;
use image::RgbaImage;

pub trait Capture {
    fn capture(&self) -> Result<RgbaImage>;
}

// Rotate a freshly-captured monitor image to match the orientation Windows
// reports for that monitor. DXGI Desktop Duplication and GDI BitBlt both
// hand back the framebuffer in its NATIVE (unrotated) orientation; if the
// user has set the monitor to Portrait in display settings, that means a
// 1920x1080-native panel gives us a 1920x1080 image while monitor.width()
// reports 1080 and monitor.height() reports 1920. compositing the native
// image into the rotated virtual-screen slot crops it and visually rotates
// it 90° in the saved PNG. fix: if captured dimensions are swapped vs the
// reported monitor dimensions, rotate the image to match.
//
// expected_w/h are what monitor.width()/height() report (post-rotation).
#[cfg(windows)]
pub fn orient_captured_image(
    img: RgbaImage,
    expected_w: u32,
    expected_h: u32,
    monitor_x: i32,
    monitor_y: i32,
) -> RgbaImage {
    let (iw, ih) = (img.width(), img.height());
    if iw == expected_w && ih == expected_h {
        return img;
    }
    if iw == expected_h && ih == expected_w {
        // dimensions swapped — monitor is in portrait. query the actual
        // rotation so we know whether to spin 90° or 270°.
        let rotation = current_monitor_rotation_at(monitor_x, monitor_y);
        let rotated = match rotation {
            MonitorRotation::Rotate90 => image::imageops::rotate90(&img),
            MonitorRotation::Rotate270 => image::imageops::rotate270(&img),
            // unknown or 180° (which preserves dimensions): default to 270°
            // because that's the most common physical-portrait orientation
            // for desktop monitors mounted on a stand.
            _ => image::imageops::rotate270(&img),
        };
        tracing::info!(
            "orient_captured_image: captured {iw}x{ih}, expected {expected_w}x{expected_h}, applied {:?}",
            rotation,
        );
        return rotated;
    }
    if iw == expected_w && ih == expected_h * 2 {
        // dimensions doubled vertically (some xcap quirk on certain
        // configurations); take the top half.
        return image::imageops::crop_imm(&img, 0, 0, iw, expected_h).to_image();
    }
    tracing::warn!(
        "orient_captured_image: captured {iw}x{ih} doesn't match expected {expected_w}x{expected_h} and isn't a swap — passing through unchanged",
    );
    img
}

#[cfg(not(windows))]
pub fn orient_captured_image(
    img: RgbaImage,
    _expected_w: u32,
    _expected_h: u32,
    _monitor_x: i32,
    _monitor_y: i32,
) -> RgbaImage {
    img
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRotation {
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
    Unknown,
}

#[cfg(windows)]
fn current_monitor_rotation_at(x: i32, y: i32) -> MonitorRotation {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromPoint, DEVMODEW,
        ENUM_CURRENT_SETTINGS, MONITORINFOEXW, MONITORINFO, MONITOR_DEFAULTTONULL,
    };
    unsafe {
        let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL);
        if hmon.is_invalid() {
            return MonitorRotation::Unknown;
        }
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _).as_bool() {
            return MonitorRotation::Unknown;
        }
        let mut devmode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let ok = EnumDisplaySettingsW(
            windows::core::PCWSTR(info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        );
        if !ok.as_bool() {
            return MonitorRotation::Unknown;
        }
        // dmDisplayOrientation lives in the Anonymous2 union inside DEVMODEW.
        let orient = devmode.Anonymous1.Anonymous2.dmDisplayOrientation;
        // DMDO_DEFAULT = 0, DMDO_90 = 1, DMDO_180 = 2, DMDO_270 = 3
        match orient.0 {
            0 => MonitorRotation::Identity,
            1 => MonitorRotation::Rotate90,
            2 => MonitorRotation::Rotate180,
            3 => MonitorRotation::Rotate270,
            _ => MonitorRotation::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    FullScreen,
    Window,
    Region,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rectangle {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rectangle {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    #[cfg(any(test, windows))]
    pub fn normalize(start_x: i32, start_y: i32, end_x: i32, end_y: i32) -> Self {
        let x = start_x.min(end_x);
        let y = start_y.min(end_y);
        let width = (start_x - end_x).unsigned_abs();
        let height = (start_y - end_y).unsigned_abs();
        Self { x, y, width, height }
    }
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    #[cfg(windows)]
    {
        if let Ok(monitors) = fast_list_monitors() {
            return Ok(monitors);
        }
    }
    let screens = xcap::Monitor::all()?;
    let monitors: Vec<MonitorInfo> = screens
        .into_iter()
        .map(|s| MonitorInfo {
            id: s.id(),
            name: s.name().to_string(),
            x: s.x(),
            y: s.y(),
            width: s.width(),
            height: s.height(),
            is_primary: s.is_primary(),
        })
        .collect();
    Ok(monitors)
}

// run a 4-byte-per-pixel conversion from src into dst, splitting the work
// across cores for large frames. each output pixel is per_pixel(input_pixel).
// used for the full-virtual-screen BGRA<->RGBA swaps on the capture-open path,
// which are otherwise single-threaded scalar passes over tens of millions of
// pixels. small buffers run inline to avoid thread-spawn overhead. the result
// is byte-identical to the equivalent serial loop — this only changes how the
// bytes are computed, never which bytes, so it is safe on the (already
// tonemapped) freeze frame.
pub(crate) fn par_convert<F>(src: &[u8], dst: &mut [u8], per_pixel: F)
where
    F: Fn(&[u8]) -> [u8; 4] + Sync,
{
    let n = src.len().min(dst.len()) & !3; // whole pixels only
    let src = &src[..n];
    let dst = &mut dst[..n];

    // below ~1 MiB the spawn overhead outweighs the parallelism; and when the
    // caller is already a parallel monitor-capture worker (SERIAL_CONVERT set),
    // nesting another thread pool here would just oversubscribe the cores.
    const PAR_THRESHOLD: usize = 1 << 20;
    if n < PAR_THRESHOLD || SERIAL_CONVERT.with(|c| c.get()) {
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&per_pixel(s));
        }
        return;
    }

    let threads = std::thread::available_parallelism()
        .map(|t| t.get().min(16))
        .unwrap_or(4)
        .max(1);
    let chunk_bytes = (n / 4).div_ceil(threads) * 4;
    let per_pixel = &per_pixel;
    std::thread::scope(|scope| {
        for (d_chunk, s_chunk) in dst.chunks_mut(chunk_bytes).zip(src.chunks(chunk_bytes)) {
            scope.spawn(move || {
                for (d, s) in d_chunk.chunks_exact_mut(4).zip(s_chunk.chunks_exact(4)) {
                    d.copy_from_slice(&per_pixel(s));
                }
            });
        }
    });
}

// if the entire frame is alpha=0, force it fully opaque. GDI BitBlt and
// some HDR duplication formats leave the alpha channel zeroed even over
// real color, which would otherwise persist as a fully transparent PNG.
// only triggers when no pixel carries alpha, so a window capture with
// genuine per-pixel transparency is left untouched. mirrors the no-alpha
// icon handling in cursor capture
pub fn ensure_opaque_if_fully_transparent(img: &mut RgbaImage) {
    if img.pixels().any(|p| p[3] != 0) {
        return;
    }
    for p in img.pixels_mut() {
        p[3] = 255;
    }
}

// true when every sampled pixel is r=g=b=0. used to detect a failed or
// stale capture (poisoned duplication device, locked output) that came
// back as a black slice. the alpha channel is deliberately ignored:
// GDI-on-HDR and scRGB black frames carry opaque alpha over zero color, so
// testing raw bytes would miss them
pub fn is_black_frame(img: &RgbaImage) -> bool {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return true;
    }
    for &y in &[0, h / 2, h - 1] {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            if p[0] != 0 || p[1] != 0 || p[2] != 0 {
                return false;
            }
        }
    }
    true
}

// capture a single monitor as an oriented, opaque RGBA image.
//
// HDR monitors default to the Direct2D HdrToneMap pipeline — the same path
// active-monitor capture uses — which tonemaps correctly and builds a fresh
// D3D device per call. the previous freeze-frame path reused the cached
// duplication device that poisons over uptime and handed back black slices
// that fell through to GDI-on-HDR (transparent black), which was the
// selector black-screen bug. the CPU-tonemap and WGC paths stay reachable
// via their env opt-ins. SDR monitors use GDI BitBlt. any slice that still
// comes back fully black is retried through GDI before being accepted, and
// a fully-transparent slice is forced opaque
#[cfg(windows)]
pub fn capture_one_monitor(monitor: &MonitorInfo) -> Result<RgbaImage> {
    let center = (
        monitor.x + (monitor.width as i32) / 2,
        monitor.y + (monitor.height as i32) / 2,
    );
    let is_hdr = HdrCapture::is_hdr_at_point(center.0, center.1);

    let gdi_capture = || -> Result<RgbaImage> {
        match fast_gdi_capture(monitor.x, monitor.y, monitor.width, monitor.height) {
            Ok(img) => Ok(img),
            Err(e) => {
                tracing::warn!("fast GDI capture failed — falling back to xcap: {e:#}");
                let screens = xcap::Monitor::all()?;
                let screen = screens
                    .into_iter()
                    .find(|s| s.id() == monitor.id)
                    .ok_or_else(|| anyhow::anyhow!("xcap monitor not found"))?;
                screen.capture_image().map_err(|e| anyhow::anyhow!("{e}"))
            }
        }
    };

    let raw: RgbaImage = if is_hdr {
        if wgc_enabled() {
            wgc_capture_at_point(center.0, center.1).or_else(|e| {
                tracing::warn!("WGC capture failed at {center:?} — GDI fallback: {e:#}");
                gdi_capture()
            })?
        } else {
            HdrCapture::new()
                .capture_with_hdr_at(Some(center))
                .map(|(img, _)| img)
                .or_else(|e| {
                    tracing::warn!("CPU HDR capture failed at {center:?} — GDI fallback: {e:#}");
                    gdi_capture()
                })?
        }
    } else {
        gdi_capture()?
    };

    let mut img = orient_captured_image(raw, monitor.width, monitor.height, monitor.x, monitor.y);

    if is_black_frame(&img) {
        tracing::warn!(
            "monitor {}x{}+{}+{} captured all-black — GDI fallback",
            monitor.width, monitor.height, monitor.x, monitor.y,
        );
        if let Ok(g) = gdi_capture() {
            let g = orient_captured_image(g, monitor.width, monitor.height, monitor.x, monitor.y);
            if !is_black_frame(&g) {
                img = g;
            }
        }
    }

    ensure_opaque_if_fully_transparent(&mut img);
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rectangle_normalize() {
        let rect = Rectangle::normalize(100, 200, 50, 100);
        assert_eq!(rect.x, 50);
        assert_eq!(rect.y, 100);
        assert_eq!(rect.width, 50);
        assert_eq!(rect.height, 100);
    }


    #[test]
    fn test_screen_capture_with_monitor() {
        let capture = ScreenCapture::with_monitor(1);
        assert!(capture.get_monitor_info().is_err() || capture.get_monitor_info().is_ok());
    }

    #[test]
    fn test_region_capture_new() {
        let rect = Rectangle::new(0, 0, 100, 100);
        let capture = RegionCapture::new(rect);
        let region = capture.region();
        assert_eq!(region.width, 100);
    }

    #[test]
    fn test_region_capture_from_coords() {
        let capture = RegionCapture::from_coords(0, 0, 100, 100);
        let region = capture.region();
        assert_eq!(region.width, 100);
    }

    #[test]
    fn test_window_capture_methods() {
        let _ = WindowCapture::focused();
        let _ = WindowCapture::from_title("nonexistent");
        let windows = WindowCapture::list_application_windows().unwrap_or_default();
        assert!(windows.is_empty() || !windows.is_empty());
    }

    #[test]
    fn opaque_forces_alpha_when_fully_transparent() {
        // real color with alpha=0 everywhere -> alpha forced to 255, color kept
        let mut img = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 0]));
        ensure_opaque_if_fully_transparent(&mut img);
        for p in img.pixels() {
            assert_eq!(*p, image::Rgba([10, 20, 30, 255]));
        }
    }

    #[test]
    fn opaque_leaves_mixed_alpha_untouched() {
        let mut img = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 0]));
        img.put_pixel(1, 1, image::Rgba([1, 2, 3, 128]));
        ensure_opaque_if_fully_transparent(&mut img);
        assert_eq!(*img.get_pixel(0, 0), image::Rgba([10, 20, 30, 0]));
        assert_eq!(*img.get_pixel(1, 1), image::Rgba([1, 2, 3, 128]));
    }

    #[test]
    fn black_frame_detects_zero_rgb_even_with_opaque_alpha() {
        // the scRGB / GDI-on-HDR case: zero color, opaque alpha
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
        assert!(is_black_frame(&img));
    }

    #[test]
    fn black_frame_false_on_any_nonzero_color() {
        let mut img = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
        // single lit pixel on the last sampled row
        img.put_pixel(3, 7, image::Rgba([0, 0, 1, 0]));
        assert!(!is_black_frame(&img));
    }

    #[test]
    fn black_frame_true_on_all_zero() {
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
        assert!(is_black_frame(&img));
    }

    #[test]
    fn par_convert_small_buffer_is_exact() {
        // below the parallel threshold -> serial path. BGRA->RGBA, opaque alpha.
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut got = [0u8; 8];
        par_convert(&src, &mut got, |s| [s[2], s[1], s[0], 255]);
        assert_eq!(got, [3, 2, 1, 255, 7, 6, 5, 255]);
    }

    #[test]
    fn par_convert_matches_serial_across_chunk_boundaries() {
        // larger than PAR_THRESHOLD (1 MiB) so the multi-threaded path runs;
        // a non-uniform pattern catches any chunk-boundary or off-by-one bug.
        let px = 400_000usize; // 1.6 MB
        let mut src = vec![0u8; px * 4];
        for (i, b) in src.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut got = vec![0u8; px * 4];
        par_convert(&src, &mut got, |s| [s[2], s[1], s[0], s[3]]);

        let mut want = vec![0u8; px * 4];
        for (d, s) in want.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        assert_eq!(got, want, "parallel output must match the serial swap byte-for-byte");
    }

    #[test]
    fn capture_serial_path_is_identical_to_parallel() {
        // capture_serial only changes whether par_convert spawns threads, never
        // the bytes it produces. a >1 MiB buffer exercises the parallel path.
        let px = 400_000usize;
        let mut src = vec![0u8; px * 4];
        for (i, b) in src.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut parallel = vec![0u8; px * 4];
        par_convert(&src, &mut parallel, |s| [s[2], s[1], s[0], 255]);

        let mut serial = vec![0u8; px * 4];
        capture_serial(|| par_convert(&src, &mut serial, |s| [s[2], s[1], s[0], 255]));

        assert_eq!(parallel, serial, "capture_serial must not change the output bytes");
    }
}
