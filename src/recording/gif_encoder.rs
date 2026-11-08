use anyhow::{anyhow, Result};
use gif::{Encoder, Frame, Repeat};
use image::RgbaImage;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::{MonitorInfo, Rectangle, ScreenCapture};
use crate::config::{GifEngine, GifOptions, GifPalette};

fn find_best_monitor(rect: Rectangle) -> Option<MonitorInfo> {
    let monitors = crate::capture::list_monitors().ok()?;
    let mut best_monitor: Option<MonitorInfo> = None;
    let mut max_overlap_area = 0i64;

    for m in monitors {
        let rx1 = rect.x;
        let rx2 = rect.x.checked_add(rect.width as i32).unwrap_or(rect.x);
        let ry1 = rect.y;
        let ry2 = rect.y.checked_add(rect.height as i32).unwrap_or(rect.y);

        let mx1 = m.x;
        let mx2 = m.x.checked_add(m.width as i32).unwrap_or(m.x);
        let my1 = m.y;
        let my2 = m.y.checked_add(m.height as i32).unwrap_or(m.y);

        let ox1 = rx1.max(mx1);
        let ox2 = rx2.min(mx2);
        let oy1 = ry1.max(my1);
        let oy2 = ry2.min(my2);

        if ox1 < ox2 && oy1 < oy2 {
            let area = (ox2 - ox1) as i64 * (oy2 - oy1) as i64;
            if area > max_overlap_area {
                max_overlap_area = area;
                best_monitor = Some(m);
            }
        }
    }

    best_monitor
}

fn region_within_monitor(rect: Rectangle, monitor: &MonitorInfo) -> bool {
    let (Ok(rect_width), Ok(rect_height), Ok(monitor_width), Ok(monitor_height)) = (
        i32::try_from(rect.width),
        i32::try_from(rect.height),
        i32::try_from(monitor.width),
        i32::try_from(monitor.height),
    ) else {
        return false;
    };
    let (Some(rect_right), Some(rect_bottom), Some(monitor_right), Some(monitor_bottom)) = (
        rect.x.checked_add(rect_width),
        rect.y.checked_add(rect_height),
        monitor.x.checked_add(monitor_width),
        monitor.y.checked_add(monitor_height),
    ) else {
        return false;
    };

    rect.x >= monitor.x
        && rect.y >= monitor.y
        && rect_right <= monitor_right
        && rect_bottom <= monitor_bottom
}

use super::mp4_stream::{ffmpeg_command, Mp4Streamer};
use super::spool::FrameSpool;
use super::{RecordingFormat, RecordingSettings, RecordingState, StopReason};

// insanity backstop above the theoretical max of 300s * 60fps
const MAX_FRAMES: usize = 21600;
const MAX_GIF_DIMENSION: u32 = 4096;
const MAX_GIF_FILE_SIZE: u64 = 500 * 1024 * 1024;
const MIN_FRAME_INTERVAL_MS: u64 = 16;
const DIRECT_BLACK_RECHECK_FRAMES: u32 = 30;

// where kept frames go during capture. RAM stays flat either way: gif frames
// spool to a temp file for the post-stop encode, mp4 frames stream into a
// live ffmpeg child as they arrive
enum FrameSink {
    Gif {
        spool: FrameSpool,
        capture_end: Option<Duration>,
    },
    Mp4 {
        streamer: Mp4Streamer,
        capture_end: Option<Duration>,
    },
}

pub struct GifRecorder {
    state: Arc<Mutex<RecordingState>>,
    settings: RecordingSettings,
    sink: Arc<Mutex<Option<FrameSink>>>,
    stop_reason: Arc<Mutex<Option<StopReason>>>,
    stop_signal: Option<Sender<()>>,
    // read every tick so the frame can be moved and resized mid-recording
    region: Arc<Mutex<Option<Rectangle>>>,
    paused: Arc<AtomicBool>,
    audio_temp_path: Option<std::path::PathBuf>,
    audio_stop_tx: Option<Sender<()>>,
}

// map capture timestamps and the final horizon onto a discrete output clock.
// Rounding every frame independently drifts by up to one unit per frame — a 15fps gif loses
// 6.7ms each frame and plays ~11% fast — so the schedule tracks the cumulative
// target instead, keeping total drift under one unit for any length. A frame
// whose slot lands below `min_units` gets 0 (the caller drops it and its time
// folds into the next frame); a hold beyond `max_units` is cut there and the
// clock resyncs, so one bad gap can't freeze playback
fn schedule_frames(
    times: &[Duration],
    nominal: Duration,
    capture_end: Option<Duration>,
    units_per_sec: f64,
    min_units: u64,
    max_units: u64,
) -> Vec<u64> {
    let mut emitted: u64 = 0;
    let mut schedule: Vec<u64> = times
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let nominal_end = t.saturating_add(nominal);
            let end = match times.get(i + 1) {
                Some(next) => *next.max(t),
                None => capture_end.unwrap_or(nominal_end).max(nominal_end),
            };
            let target = (end.as_secs_f64() * units_per_sec).round() as u64;
            let slot = target.saturating_sub(emitted);
            if slot < min_units {
                0
            } else if slot > max_units {
                emitted = target;
                max_units
            } else {
                emitted += slot;
                slot
            }
        })
        .collect();

    // never emit an empty recording: if every frame rounded away, keep the
    // last one for the minimum visible hold
    if !schedule.is_empty() && schedule.iter().all(|&u| u == 0) {
        *schedule.last_mut().unwrap() = min_units;
    }
    schedule
}

// gif delays count in hundredths of a second; players treat <2cs as a slow
// 10cs default, so that's the drop threshold. 6000cs caps a single hold at 60s
fn gif_delay_schedule(
    times: &[Duration],
    nominal: Duration,
    capture_end: Option<Duration>,
) -> Vec<u16> {
    schedule_frames(times, nominal, capture_end, 100.0, 2, 6000)
        .into_iter()
        .map(|u| u as u16)
        .collect()
}

// ffmpeg consumes rawvideo at a constant input rate; each frame is written
// once per tick of its slot so wall-clock timing survives dedup and slow
// captures. 0 means the frame is skipped entirely
fn mp4_repeat_schedule(times: &[Duration], nominal: Duration, fps: u32) -> Vec<u64> {
    schedule_frames(times, nominal, None, fps as f64, 1, 60 * fps.max(1) as u64)
}

// recorded time with every pause cut out, so the output plays straight
// through a pause and max duration counts only what was captured
#[derive(Default)]
struct PauseClock {
    paused_total: Duration,
    paused_since: Option<Instant>,
}

impl PauseClock {
    fn update(&mut self, paused: bool, now: Instant) {
        match (paused, self.paused_since) {
            (true, None) => self.paused_since = Some(now),
            (false, Some(since)) => {
                self.paused_total += now.saturating_duration_since(since);
                self.paused_since = None;
            }
            _ => {}
        }
    }

    fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    fn active(&self, start: Instant, now: Instant) -> Duration {
        let pausing = self
            .paused_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        now.saturating_duration_since(start)
            .saturating_sub(self.paused_total + pausing)
    }
}

fn frames_match(previous: Option<&RgbaImage>, current: &RgbaImage) -> bool {
    previous.is_some_and(|previous| {
        previous.dimensions() == current.dimensions() && previous.as_raw() == current.as_raw()
    })
}

fn should_probe_direct_black(cooldown: &mut u32, is_black: bool) -> bool {
    if !is_black {
        *cooldown = 0;
        return false;
    }
    if *cooldown == 0 {
        return true;
    }
    *cooldown -= 1;
    false
}

fn is_exact_black(image: &RgbaImage) -> bool {
    image
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0)
}

fn gif_output_dims(width: u32, height: u32, opts: &GifOptions) -> (u32, u32) {
    let mut scale = opts.scale_percent.clamp(10, 100) as f64 / 100.0;
    if opts.max_width > 0 && width as f64 * scale > opts.max_width as f64 {
        scale = opts.max_width as f64 / width as f64;
    }
    let dim = |v: u32| ((v as f64 * scale).round() as u32).max(1);
    (dim(width), dim(height))
}

// ~10k pixels per frame is plenty for the palette network to learn from
fn sample_pixels(rgba: &[u8], samples: &mut Vec<u8>) {
    let pixels = rgba.as_chunks::<4>().0;
    let step = (pixels.len() / 10_000).max(1);
    for pixel in pixels.iter().step_by(step) {
        samples.extend_from_slice(pixel);
    }
    if samples.is_empty() {
        samples.extend_from_slice(&[0, 0, 0, 255]);
    }
}

// nearest-color lookups go through a 6-bit-per-channel cache: 262k slots,
// at most 3 levels off per channel, far below the palette's own error
struct Quantizer {
    nq: color_quant::NeuQuant,
    palette: Vec<u8>,
    cache: Vec<u16>,
}

impl Quantizer {
    fn new(samples: &[u8], colors: usize) -> Self {
        let nq = color_quant::NeuQuant::new(10, colors, samples);
        let palette = nq.color_map_rgb();
        Self {
            nq,
            palette,
            cache: vec![u16::MAX; 1 << 18],
        }
    }

    fn index(&mut self, r: u8, g: u8, b: u8) -> u8 {
        let key = ((r as usize >> 2) << 12) | ((g as usize >> 2) << 6) | (b as usize >> 2);
        if self.cache[key] == u16::MAX {
            self.cache[key] = self.nq.index_of(&[r, g, b, 255]) as u16;
        }
        self.cache[key] as u8
    }
}

// floyd-steinberg when dithering. with `keep`, pixels equal to the previous
// emitted frame get the skip index so the decoder leaves them on screen
fn quantize_frame(
    rgba: &[u8],
    width: usize,
    quantizer: &mut Quantizer,
    dither: bool,
    keep: Option<(&[u8], u8)>,
) -> Vec<u8> {
    let pixels = rgba.as_chunks::<4>().0;
    let previous = keep.map(|(prev, skip)| (prev.as_chunks::<4>().0, skip));
    let stride = width + 2;
    let mut error = vec![[0i16; 3]; if dither { stride * 2 } else { 0 }];
    let mut out = vec![0u8; pixels.len()];

    for (i, px) in pixels.iter().enumerate() {
        let x = i % width;
        let row = (i / width) & 1;
        let (cur, next) = (row * stride + x + 1, (row ^ 1) * stride + x + 1);
        if dither && x == 0 {
            error[(row ^ 1) * stride..(row ^ 1) * stride + stride].fill([0; 3]);
        }
        if let Some((prev, skip)) = previous {
            if prev.get(i) == Some(px) {
                out[i] = skip;
                continue;
            }
        }
        if !dither {
            out[i] = quantizer.index(px[0], px[1], px[2]);
            continue;
        }
        let e = error[cur];
        let c = |k: usize| (px[k] as i16 + e[k]).clamp(0, 255);
        let (r, g, b) = (c(0), c(1), c(2));
        let idx = quantizer.index(r as u8, g as u8, b as u8);
        out[i] = idx;
        let p = &quantizer.palette[idx as usize * 3..idx as usize * 3 + 3];
        for (k, v) in [r, g, b].into_iter().enumerate() {
            let d = v - p[k] as i16;
            error[cur + 1][k] += d * 7 / 16;
            error[next - 1][k] += d * 3 / 16;
            error[next][k] += d * 5 / 16;
            error[next + 1][k] += d / 16;
        }
    }
    out
}

// smallest rectangle holding every changed pixel; an unchanged frame still
// needs one pixel to carry its delay
fn crop_changed(
    indexes: &[u8],
    width: u32,
    height: u32,
    skip: u8,
) -> (u32, u32, u32, u32, Vec<u8>) {
    let w = width as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (w, height as usize, 0, 0);
    for (i, _) in indexes.iter().enumerate().filter(|(_, &v)| v != skip) {
        let (x, y) = (i % w, i / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + 1);
        y1 = y1.max(y + 1);
    }
    if x1 == 0 {
        return (0, 0, 1, 1, vec![skip]);
    }
    let buffer = (y0..y1)
        .flat_map(|y| indexes[y * w + x0..y * w + x1].iter().copied())
        .collect();
    (
        x0 as u32,
        y0 as u32,
        (x1 - x0) as u32,
        (y1 - y0) as u32,
        buffer,
    )
}

// ffmpeg's gif muxer rounds each frame to whole centiseconds, so frames go in
// at a rate that divides 100 and every tick lands exactly
fn ffmpeg_gif_rate(fps: u32) -> u32 {
    [1, 2, 4, 5, 10, 20, 25, 50]
        .into_iter()
        .find(|&r| r >= fps)
        .unwrap_or(50)
}

#[allow(clippy::too_many_arguments)]
fn save_gif_ffmpeg(
    spool: &mut FrameSpool,
    times: &[Duration],
    nominal: Duration,
    capture_end: Option<Duration>,
    (in_width, in_height): (u32, u32),
    (out_width, out_height): (u32, u32),
    opts: &GifOptions,
    path: &Path,
) -> Result<()> {
    let rate = ffmpeg_gif_rate((1.0 / nominal.as_secs_f64()).round() as u32);
    let repeats = schedule_frames(
        times,
        nominal,
        capture_end,
        rate as f64,
        1,
        60 * rate as u64,
    );
    let dither = if opts.dither { "sierra2_4a" } else { "none" };
    let diff = if opts.diff_frames {
        "rectangle"
    } else {
        "none"
    };
    let scale = format!("scale={out_width}:{out_height}:flags=lanczos");
    let colors = opts.colors.clamp(2, 256);
    let size = format!("{in_width}x{in_height}");
    let rate_arg = rate.to_string();
    let input = [
        "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", &rate_arg, "-i", "-",
    ];

    let mut run = |extra: &[&str]| -> Result<()> {
        let mut child = ffmpeg_command()
            .args(input)
            .args(extra)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn ffmpeg: {e}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stdin closed"))?;
        let mut written = Ok(());
        'frames: for (i, &count) in repeats.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let frame = match spool.read_frame(i) {
                Ok(frame) => frame,
                Err(e) => {
                    written = Err(e);
                    break;
                }
            };
            let frame = if frame.dimensions() != (in_width, in_height) {
                image::imageops::resize(
                    &frame,
                    in_width,
                    in_height,
                    image::imageops::FilterType::Triangle,
                )
            } else {
                frame
            };
            for _ in 0..count {
                if let Err(e) = std::io::Write::write_all(&mut stdin, frame.as_raw()) {
                    written = Err(e.into());
                    break 'frames;
                }
            }
        }
        drop(stdin);
        let status = child.wait()?;
        written?;
        if !status.success() {
            return Err(anyhow!("ffmpeg exited with {status}"));
        }
        Ok(())
    };

    let out = path.to_string_lossy().into_owned();
    let result = if opts.palette == GifPalette::PerFrame {
        // diff_mode keeps the previous frame's indexes outside the changed
        // rectangle, which point at the wrong colors once each frame has its own palette
        let graph = format!(
            "{scale},split[a][b];[a]palettegen=max_colors={colors}:stats_mode=single[p];[b][p]paletteuse=new=1:dither={dither}"
        );
        run(&["-filter_complex", &graph, "-y", &out])
    } else {
        // a global palette needs every frame first; two passes over the on-disk
        // spool keep ffmpeg from buffering the whole recording in memory
        let palette_path = std::env::temp_dir().join(format!(
            "capscr_palette_{}.png",
            uuid::Uuid::new_v4().as_simple()
        ));
        let palette = palette_path.to_string_lossy().into_owned();
        let pass_one = format!("{scale},palettegen=max_colors={colors}:stats_mode=diff");
        let result = run(&["-vf", &pass_one, "-y", &palette]).and_then(|()| {
            let graph =
                format!("[0:v]{scale}[x];[x][1:v]paletteuse=dither={dither}:diff_mode={diff}");
            run(&["-i", &palette, "-filter_complex", &graph, "-y", &out])
        });
        let _ = std::fs::remove_file(&palette_path);
        result
    };
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

impl GifRecorder {
    pub fn new(settings: RecordingSettings) -> Self {
        Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            settings,
            sink: Arc::new(Mutex::new(None)),
            stop_reason: Arc::new(Mutex::new(None)),
            stop_signal: None,
            region: Arc::new(Mutex::new(None)),
            paused: Arc::new(AtomicBool::new(false)),
            audio_temp_path: None,
            audio_stop_tx: None,
        }
    }

    pub fn with_region(self, region: Rectangle) -> Self {
        self.set_region(region);
        self
    }

    pub fn set_region(&self, region: Rectangle) {
        if region.width > 0 && region.height > 0 {
            *self.region.lock().unwrap_or_else(|e| e.into_inner()) = Some(region);
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn state(&self) -> RecordingState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// why the capture loop ended; None while it is still running
    pub fn stop_reason(&self) -> Option<StopReason> {
        *self.stop_reason.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn format(&self) -> RecordingFormat {
        self.settings.format
    }

    pub fn start(&mut self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if *state != RecordingState::Idle {
                return Ok(());
            }
            *state = RecordingState::Recording;
        }

        let new_sink = match self.settings.format {
            RecordingFormat::Gif => match FrameSpool::create() {
                Ok(spool) => FrameSink::Gif {
                    spool,
                    capture_end: None,
                },
                Err(e) => {
                    *self.state.lock().unwrap_or_else(|p| p.into_inner()) = RecordingState::Idle;
                    return Err(e);
                }
            },
            RecordingFormat::Mp4 => FrameSink::Mp4 {
                streamer: Mp4Streamer::new(self.settings.fps, self.settings.video_crf),
                capture_end: None,
            },
        };
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(new_sink);
        *self.stop_reason.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let (tx, rx): (Sender<()>, Receiver<()>) = channel();
        self.stop_signal = Some(tx);

        // gifs have no audio track; only mp4 recordings pay for the wasapi tap
        if self.settings.record_audio && self.settings.format == RecordingFormat::Mp4 {
            let temp_dir = std::env::temp_dir();
            let audio_filename = format!("capscr_audio_{}.wav", uuid::Uuid::new_v4().as_simple());
            let audio_path = temp_dir.join(audio_filename);
            self.audio_temp_path = Some(audio_path.clone());

            let (audio_tx, audio_rx) = channel();
            self.audio_stop_tx = Some(audio_tx);

            thread::spawn(move || {
                if let Err(e) = record_loopback_audio(&audio_path, audio_rx) {
                    tracing::error!("WASAPI Audio loopback record error: {e}");
                }
            });
        }

        let state = Arc::clone(&self.state);
        let sink = Arc::clone(&self.sink);
        let stop_reason = Arc::clone(&self.stop_reason);
        let fps = self.settings.fps.max(1);
        let max_duration = self.settings.max_duration;
        let live_region = Arc::clone(&self.region);
        let paused = Arc::clone(&self.paused);
        let mut region = *live_region.lock().unwrap_or_else(|e| e.into_inner());
        let mut best_monitor = region.and_then(find_best_monitor);
        let virtual_origin = crate::capture::list_monitors()
            .ok()
            .and_then(|monitors| {
                Some((
                    monitors.iter().map(|monitor| monitor.x).min()?,
                    monitors.iter().map(|monitor| monitor.y).min()?,
                ))
            })
            .unwrap_or((0, 0));
        let show_cursor = self.settings.show_cursor;

        thread::spawn(move || {
            #[cfg(windows)]
            struct TimerGuard;
            #[cfg(windows)]
            impl Drop for TimerGuard {
                fn drop(&mut self) {
                    unsafe {
                        let _ = windows::Win32::Media::timeEndPeriod(1);
                    }
                }
            }
            #[cfg(windows)]
            let _timer_guard = unsafe {
                let _ = windows::Win32::Media::timeBeginPeriod(1);
                TimerGuard
            };

            // one persistent region source per recording: reusable GDI on
            // windows, or the compositor and x11 source chain on linux
            // failures fall back to the generic monitor capture path
            #[cfg(target_os = "linux")]
            let mut direct_source = match region {
                Some(rect) => match crate::capture::RecordingSource::new(rect, show_cursor) {
                    Ok(source) => Some(source),
                    Err(e) => {
                        tracing::warn!(
                            "direct region source unavailable ({e:#}); using generic path"
                        );
                        None
                    }
                },
                None => None,
            };

            #[cfg(windows)]
            let open_direct = |rect: Rectangle, monitor: Option<&MonitorInfo>| {
                if !region_within_monitor(rect, monitor?) {
                    return None;
                }
                match crate::capture::GdiRegionGrabber::new(
                    rect.x,
                    rect.y,
                    rect.width.min(MAX_GIF_DIMENSION),
                    rect.height.min(MAX_GIF_DIMENSION),
                ) {
                    Ok(source) => Some(source),
                    Err(error) => {
                        tracing::warn!(
                            "direct Windows region source unavailable ({error:#}); using generic path"
                        );
                        None
                    }
                }
            };
            #[cfg(windows)]
            let mut direct_source =
                region.and_then(|rect| open_direct(rect, best_monitor.as_ref()));
            #[cfg(windows)]
            let mut direct_black_probe_cooldown = 0;

            let min_frame_duration = Duration::from_millis(MIN_FRAME_INTERVAL_MS);
            let frame_duration = Duration::from_secs_f64(1.0 / fps as f64).max(min_frame_duration);
            let start_time = Instant::now();
            let mut clock = PauseClock::default();
            let mut frames_kept: usize = 0;
            let mut last_frame: Option<Arc<RgbaImage>> = None;
            let mut consecutive_dupes: u32 = 0;

            let reason = loop {
                if rx.try_recv().is_ok() {
                    break StopReason::Requested;
                }

                clock.update(paused.load(Ordering::SeqCst), Instant::now());
                if clock.is_paused() {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }

                if clock.active(start_time, Instant::now()) >= max_duration {
                    break StopReason::MaxDuration;
                }

                let latest = *live_region.lock().unwrap_or_else(|e| e.into_inner());
                if latest != region {
                    region = latest;
                    best_monitor = region.and_then(find_best_monitor);
                    #[cfg(windows)]
                    {
                        direct_source =
                            region.and_then(|rect| open_direct(rect, best_monitor.as_ref()));
                    }
                }

                let frame_start = Instant::now();

                let capture_result = if let Some(rect) = region {
                    // fast path: direct region grab from the persistent
                    // source; any failure falls through to the generic path
                    #[cfg(target_os = "linux")]
                    let direct = direct_source.as_mut().and_then(|source| {
                        source
                            .grab(
                                rect.x,
                                rect.y,
                                rect.width.min(MAX_GIF_DIMENSION),
                                rect.height.min(MAX_GIF_DIMENSION),
                                show_cursor,
                            )
                            .ok()
                    });

                    #[cfg(windows)]
                    let direct_result = direct_source.as_mut().map(|source| source.grab());
                    #[cfg(windows)]
                    let direct = match direct_result {
                        Some(Ok(image)) => Some(image),
                        Some(Err(error)) => {
                            tracing::warn!(
                                "direct Windows region source failed ({error:#}); using generic path"
                            );
                            direct_source = None;
                            None
                        }
                        None => None,
                    };

                    #[cfg(not(any(windows, target_os = "linux")))]
                    let direct: Option<RgbaImage> = None;

                    let capture_fallback = || -> Result<RgbaImage> {
                        let width = rect.width.min(MAX_GIF_DIMENSION);
                        let height = rect.height.min(MAX_GIF_DIMENSION);
                        if width == 0 || height == 0 {
                            return Err(anyhow!("Invalid region"));
                        }

                        if let Some(monitor) = best_monitor
                            .as_ref()
                            .filter(|monitor| region_within_monitor(rect, monitor))
                        {
                            if let Ok(image) = crate::capture::capture_one_monitor(monitor) {
                                let local_x = (rect.x - monitor.x) as u32;
                                let local_y = (rect.y - monitor.y) as u32;
                                let fits = local_x
                                    .checked_add(width)
                                    .is_some_and(|right| right <= image.width())
                                    && local_y
                                        .checked_add(height)
                                        .is_some_and(|bottom| bottom <= image.height());
                                if fits {
                                    return Ok(image::imageops::crop_imm(
                                        &image, local_x, local_y, width, height,
                                    )
                                    .to_image());
                                }
                            }
                        }

                        let full = ScreenCapture::all_monitors()?;
                        let x = rect.x.saturating_sub(virtual_origin.0).max(0) as u32;
                        let y = rect.y.saturating_sub(virtual_origin.1).max(0) as u32;
                        let fits = x
                            .checked_add(width)
                            .is_some_and(|right| right <= full.width())
                            && y.checked_add(height)
                                .is_some_and(|bottom| bottom <= full.height());
                        if !fits {
                            return Err(anyhow!("Region extends beyond the captured desktop"));
                        }
                        Ok(image::imageops::crop_imm(&full, x, y, width, height).to_image())
                    };

                    if let Some(img) = direct {
                        #[cfg(windows)]
                        {
                            let probe_black = should_probe_direct_black(
                                &mut direct_black_probe_cooldown,
                                crate::capture::is_black_frame(&img) && is_exact_black(&img),
                            );
                            if probe_black {
                                match capture_fallback() {
                                    Ok(fallback) if !is_exact_black(&fallback) => {
                                        tracing::warn!(
                                            "direct Windows region source returned black; using monitor fallback"
                                        );
                                        direct_source = None;
                                        Ok(fallback)
                                    }
                                    Ok(_) => {
                                        direct_black_probe_cooldown = DIRECT_BLACK_RECHECK_FRAMES;
                                        Ok(img)
                                    }
                                    Err(_) => {
                                        direct_black_probe_cooldown = DIRECT_BLACK_RECHECK_FRAMES;
                                        Ok(img)
                                    }
                                }
                            } else {
                                Ok(img)
                            }
                        }
                        #[cfg(not(windows))]
                        Ok(img)
                    } else {
                        capture_fallback()
                    }
                } else {
                    ScreenCapture::all_monitors().map(|img| {
                        if img.width() > MAX_GIF_DIMENSION || img.height() > MAX_GIF_DIMENSION {
                            let scale_w = MAX_GIF_DIMENSION as f32 / img.width() as f32;
                            let scale_h = MAX_GIF_DIMENSION as f32 / img.height() as f32;
                            let scale = scale_w.min(scale_h);
                            let new_w = ((img.width() as f32) * scale) as u32;
                            let new_h = ((img.height() as f32) * scale) as u32;
                            image::imageops::resize(
                                &img,
                                new_w.max(1),
                                new_h.max(1),
                                image::imageops::FilterType::Triangle,
                            )
                        } else {
                            img
                        }
                    })
                };

                tracing::debug!(
                    "recording frame capture took {}ms (ok={})",
                    frame_start.elapsed().as_millis(),
                    capture_result.is_ok(),
                );

                if let Ok(mut image) = capture_result {
                    if image.width() <= MAX_GIF_DIMENSION && image.height() <= MAX_GIF_DIMENSION {
                        // grab the cursor once so the same snapshot drives both the
                        // frame comparison and the composite below
                        let cursor_shot = if show_cursor && region.is_some() {
                            crate::capture::capture_cursor_shot()
                        } else {
                            None
                        };

                        if let (Some(shot), Some(rect)) = (&cursor_shot, region) {
                            crate::capture::composite_cursor_shot(
                                &mut image,
                                shot,
                                (rect.x, rect.y),
                            );
                        }

                        if frames_match(last_frame.as_deref(), &image) {
                            consecutive_dupes += 1;
                            if consecutive_dupes < 30 {
                                let elapsed = frame_start.elapsed();
                                if elapsed < frame_duration {
                                    thread::sleep(frame_duration - elapsed);
                                }
                                continue;
                            }
                        }
                        consecutive_dupes = 0;

                        if frames_kept >= MAX_FRAMES {
                            break StopReason::FrameCap;
                        }

                        let at = clock.active(start_time, frame_start);
                        let image = Arc::new(image);
                        let mut sink_guard = sink.lock().unwrap_or_else(|e| e.into_inner());
                        match sink_guard.as_mut() {
                            Some(FrameSink::Gif { spool, .. }) => {
                                match spool.push(image.as_ref(), at) {
                                    Ok(true) => frames_kept += 1,
                                    Ok(false) => break StopReason::DiskFull,
                                    Err(e) => {
                                        tracing::error!("frame spool write failed: {e}");
                                        break StopReason::EncoderFailed;
                                    }
                                }
                            }
                            Some(FrameSink::Mp4 { streamer, .. }) => {
                                if let Err(e) = streamer.push(Arc::clone(&image), at) {
                                    tracing::error!("mp4 stream write failed: {e}");
                                    break StopReason::EncoderFailed;
                                }
                                frames_kept += 1;
                            }
                            // reset() cleared the sink under us — just end
                            None => break StopReason::Requested,
                        }
                        last_frame = Some(image);
                    }
                }

                let elapsed = frame_start.elapsed();
                if elapsed < frame_duration {
                    thread::sleep(frame_duration - elapsed);
                }
            };

            let capture_stopped_at = clock.active(start_time, Instant::now());
            let mut sink_guard = sink.lock().unwrap_or_else(|e| e.into_inner());
            match sink_guard.as_mut() {
                Some(FrameSink::Gif { capture_end, .. })
                | Some(FrameSink::Mp4 { capture_end, .. }) => {
                    *capture_end = Some(capture_stopped_at);
                }
                None => {}
            }
            drop(sink_guard);

            *stop_reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
            if let Ok(mut state_lock) = state.lock() {
                *state_lock = RecordingState::Processing;
            }
        });

        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_signal.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.audio_stop_tx.take() {
            let _ = tx.send(());
        }
    }

    #[allow(dead_code)]
    pub fn frame_count(&self) -> usize {
        match self.sink.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(FrameSink::Gif { spool, .. }) => spool.len(),
            Some(FrameSink::Mp4 { streamer, .. }) => streamer.frames_pushed() as usize,
            None => 0,
        }
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(anyhow!("Path contains directory traversal"));
        }
        #[cfg(windows)]
        if path_str.starts_with("\\\\") {
            return Err(anyhow!("Network paths not allowed"));
        }

        let mut sink_guard = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        let (spool, capture_end) = match sink_guard.as_mut() {
            Some(FrameSink::Gif { spool, capture_end }) => (spool, *capture_end),
            _ => return Err(anyhow!("No frames captured")),
        };

        if spool.is_empty() {
            return Err(anyhow!("No frames captured"));
        }

        let orig_width = spool.metas()[0].width;
        let orig_height = spool.metas()[0].height;

        if orig_width > MAX_GIF_DIMENSION || orig_height > MAX_GIF_DIMENSION {
            return Err(anyhow!("Image dimensions exceed GIF safety limit"));
        }
        if orig_width > u16::MAX as u32 || orig_height > u16::MAX as u32 {
            return Err(anyhow!("Image dimensions too large for GIF format"));
        }
        if orig_width == 0 || orig_height == 0 {
            return Err(anyhow!("Image has zero dimension"));
        }

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let fps = self.settings.fps.clamp(1, 60);
        let nominal = Duration::from_secs_f64(1.0 / fps as f64);
        let times: Vec<Duration> = spool.metas().iter().map(|m| m.at).collect();
        let opts = &self.settings.gif;
        let (out_width, out_height) = gif_output_dims(orig_width, orig_height, opts);

        if opts.engine == GifEngine::Ffmpeg {
            if !is_ffmpeg_available() {
                tracing::warn!("ffmpeg gif engine selected but ffmpeg is missing; using built-in");
            } else {
                match save_gif_ffmpeg(
                    spool,
                    &times,
                    nominal,
                    capture_end,
                    (orig_width, orig_height),
                    (out_width, out_height),
                    opts,
                    path,
                ) {
                    Ok(()) => return self.finish_gif(path),
                    Err(e) => tracing::warn!("ffmpeg gif encode failed ({e:#}); using built-in"),
                }
            }
        }

        let delays = gif_delay_schedule(&times, nominal, capture_end);
        let filter = if self.settings.quality >= 80 {
            image::imageops::FilterType::Lanczos3
        } else if self.settings.quality >= 50 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Nearest
        };

        // diff mode reserves the last palette slot for "keep what's on screen"
        let colors = (opts.colors.clamp(2, 256) as usize - opts.diff_frames as usize).max(2);
        let skip = colors as u8;
        let with_skip = |mut palette: Vec<u8>| {
            if opts.diff_frames {
                palette.extend_from_slice(&[0, 0, 0]);
            }
            palette
        };

        let mut global = None;
        if opts.palette == GifPalette::Global {
            let num_frames = spool.len();
            let mut samples = Vec::new();
            for i in (0..num_frames).step_by((num_frames / 15).max(1)) {
                sample_pixels(spool.read_frame(i)?.as_raw(), &mut samples);
            }
            global = Some(Quantizer::new(&samples, colors));
        }
        let global_palette = global
            .as_ref()
            .map(|q| with_skip(q.palette.clone()))
            .unwrap_or_default();

        {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            let mut encoder =
                Encoder::new(file, out_width as u16, out_height as u16, &global_palette)?;
            encoder.set_repeat(Repeat::Infinite)?;

            let mut previous: Option<RgbaImage> = None;
            for (frame_idx, &delay) in delays.iter().enumerate() {
                // 0-delay frames were folded into a neighbour by the schedule;
                // skipping them here also skips their disk read + quantize cost
                if delay == 0 {
                    continue;
                }
                let frame = spool.read_frame(frame_idx)?;
                let frame = if frame.dimensions() != (out_width, out_height) {
                    image::imageops::resize(&frame, out_width, out_height, filter)
                } else {
                    frame
                };

                let mut local = None;
                let quantizer = match global.as_mut() {
                    Some(q) => q,
                    None => {
                        let mut samples = Vec::new();
                        sample_pixels(frame.as_raw(), &mut samples);
                        local.insert(Quantizer::new(&samples, colors))
                    }
                };
                let keep = previous
                    .as_ref()
                    .filter(|_| opts.diff_frames)
                    .map(|p| (p.as_raw().as_slice(), skip));
                let indexes = quantize_frame(
                    frame.as_raw(),
                    out_width as usize,
                    quantizer,
                    opts.dither,
                    keep,
                );
                let (left, top, width, height, buffer) = if keep.is_some() {
                    crop_changed(&indexes, out_width, out_height, skip)
                } else {
                    (0, 0, out_width, out_height, indexes)
                };

                encoder.write_frame(&Frame {
                    width: width as u16,
                    height: height as u16,
                    left: left as u16,
                    top: top as u16,
                    delay,
                    dispose: gif::DisposalMethod::Keep,
                    transparent: opts.diff_frames.then_some(skip),
                    palette: local.map(|q| with_skip(q.palette)),
                    buffer: std::borrow::Cow::Owned(buffer),
                    ..Default::default()
                })?;
                previous = Some(frame);
            }
        } // encoder and file handle dropped here, so every byte is flushed before the size check

        self.finish_gif(path)
    }

    fn finish_gif(&self, path: &Path) -> Result<()> {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.len() > MAX_GIF_FILE_SIZE {
                let _ = std::fs::remove_file(path);
                return Err(anyhow!("Generated GIF exceeds maximum file size"));
            }
        }

        if let Some(ref wav_path) = self.audio_temp_path {
            let _ = std::fs::remove_file(wav_path);
        }

        Ok(())
    }

    /// Ok(true) means the recording saved but the system-audio track the user
    /// asked for is missing (capture produced nothing usable, or the mux
    /// failed) — the caller should tell them it saved without audio.
    pub fn save_mp4<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let path = path.as_ref();

        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(anyhow!("Path contains directory traversal"));
        }
        #[cfg(windows)]
        if path_str.starts_with("\\\\") {
            return Err(anyhow!("Network paths not allowed"));
        }

        // frames were encoded live during capture; all that's left is closing
        // the stream and muxing in the audio track (or moving the file)
        let temp_video = {
            let mut sink_guard = self.sink.lock().unwrap_or_else(|e| e.into_inner());
            match sink_guard.as_mut() {
                Some(FrameSink::Mp4 {
                    streamer,
                    capture_end,
                }) => {
                    if streamer.frames_pushed() == 0 {
                        return Err(anyhow!("No frames captured"));
                    }
                    streamer.finish(*capture_end)?
                }
                _ => return Err(anyhow!("No frames captured")),
            }
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let audio_exists = self
            .audio_temp_path
            .as_ref()
            .map(|p| p.exists() && std::fs::metadata(p).map(|m| m.len() > 44).unwrap_or(false))
            .unwrap_or(false);

        if audio_exists {
            let wav_path = self.audio_temp_path.as_ref().unwrap();
            let mux_ok = ffmpeg_command()
                .args([
                    "-i",
                    &temp_video.to_string_lossy(),
                    "-i",
                    &wav_path.to_string_lossy(),
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-shortest",
                    "-y",
                    &path.to_string_lossy(),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            let _ = std::fs::remove_file(wav_path);
            if mux_ok {
                let _ = std::fs::remove_file(&temp_video);
                return Ok(false);
            }
            // a broken wav must not cost the user their video — fall through
            // and keep the silent recording
            tracing::warn!("audio mux failed; saving recording without audio");
        }

        if std::fs::rename(&temp_video, path).is_err() {
            // temp dir and output dir may sit on different volumes
            std::fs::copy(&temp_video, path)
                .map_err(|e| anyhow!("Failed to move recording into place: {}", e))?;
            let _ = std::fs::remove_file(&temp_video);
        }

        // reaching here with audio requested means the track was lost (no usable
        // wav, or the mux above failed and we fell through)
        Ok(self.settings.record_audio)
    }

    pub fn reset(&mut self) {
        self.stop();
        // dropping the sink removes spool/stream temp files and reaps any
        // live ffmpeg child; a mid-capture thread sees None and ends
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Ok(mut state) = self.state.lock() {
            *state = RecordingState::Idle;
        }
        if let Some(ref wav_path) = self.audio_temp_path {
            let _ = std::fs::remove_file(wav_path);
        }
        self.audio_temp_path = None;
        self.audio_stop_tx = None;
    }
}

impl Drop for GifRecorder {
    fn drop(&mut self) {
        // save_mp4 removes the audio wav only on its mux path; its early error
        // returns (no frames, bad path, stream finish failure) skip cleanup, so
        // reap the temp wav here to hold the temp-hygiene invariant on every exit
        if let Some(ref wav_path) = self.audio_temp_path {
            let _ = std::fs::remove_file(wav_path);
        }
    }
}

pub fn find_ffmpeg() -> std::path::PathBuf {
    // on linux a distro-installed ffmpeg on PATH wins over any copy capscr
    // downloaded, so a later `apt/dnf install ffmpeg` takes over and stays
    // security-updated. on windows there is no system ffmpeg, so the bundled
    // and downloaded copies come first.
    #[cfg(target_os = "linux")]
    if system_ffmpeg_runnable() {
        return std::path::PathBuf::from("ffmpeg");
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let local_ffmpeg = parent.join("ffmpeg.exe");
            if local_ffmpeg.exists() {
                return local_ffmpeg;
            }
            let local_ffmpeg_no_ext = parent.join("ffmpeg");
            if local_ffmpeg_no_ext.exists() {
                return local_ffmpeg_no_ext;
            }
        }
    }

    if let Some(proj_dirs) = directories::ProjectDirs::from("com", "capscr", "capscr") {
        let app_data_ffmpeg = proj_dirs.data_dir().join("ffmpeg.exe");
        if app_data_ffmpeg.exists() {
            return app_data_ffmpeg;
        }
        let app_data_ffmpeg_no_ext = proj_dirs.data_dir().join("ffmpeg");
        if app_data_ffmpeg_no_ext.exists() {
            return app_data_ffmpeg_no_ext;
        }
    }

    std::path::PathBuf::from("ffmpeg")
}

// probe the literal `ffmpeg` on PATH directly (not through ffmpeg_command,
// which would recurse into find_ffmpeg). the result is cached: find_ffmpeg
// runs several times per recording (availability check, encoder, audio, mux)
// and a spawn-per-call would add avoidable latency to recording start.
// caching false is safe — a later `apt install ffmpeg` is picked up on the
// next process, and the data_dir download path still resolves within a run.
#[cfg(target_os = "linux")]
fn system_ffmpeg_runnable() -> bool {
    use std::sync::OnceLock;
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| child.wait())
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

pub fn is_ffmpeg_available() -> bool {
    let path = find_ffmpeg();
    if path != std::path::Path::new("ffmpeg") {
        return path.exists();
    }
    // check if system ffmpeg is runnable
    ffmpeg_command()
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| child.wait().is_ok())
        .unwrap_or(false)
}

#[cfg(windows)]
fn write_wav_header(
    writer: &mut std::fs::File,
    data_size: u32,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
) -> std::io::Result<()> {
    use std::io::Write;
    writer.write_all(b"RIFF")?;
    writer.write_all(&(data_size + 36).to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    let audio_format: u16 = if bits_per_sample == 32 { 3 } else { 1 };
    writer.write_all(&audio_format.to_le_bytes())?;
    writer.write_all(&channels.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&block_align.to_le_bytes())?;
    writer.write_all(&bits_per_sample.to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_size.to_le_bytes())?;
    Ok(())
}

#[cfg(windows)]
pub fn record_loopback_audio(wav_path: &Path, stop_rx: Receiver<()>) -> Result<()> {
    use std::io::{Seek, Write};

    let _ = wasapi::initialize_mta();

    let device = wasapi::get_default_device(&wasapi::Direction::Render)
        .map_err(|e| anyhow!("failed to get default render device: {e:?}"))?;

    let mut client = device
        .get_iaudioclient()
        .map_err(|e| anyhow!("failed to get audio client: {e:?}"))?;

    let format = client
        .get_mixformat()
        .map_err(|e| anyhow!("failed to get mix format: {e:?}"))?;

    let sample_rate = format.get_samplespersec();
    let channels = format.get_nchannels() as u16;
    let bits_per_sample = format.get_bitspersample() as u16;
    let block_align = format.get_blockalign() as usize;

    // 200ms of device buffer: the polling thread shares a core with capture
    // and encoding, and a 10ms buffer overruns whenever it gets descheduled
    let mode = wasapi::StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: 2_000_000,
    };
    client
        .initialize_client(&format, &wasapi::Direction::Capture, &mode)
        .map_err(|e| anyhow!("failed to initialize audio client: {e:?}"))?;

    let capture_client = client
        .get_audiocaptureclient()
        .map_err(|e| anyhow!("failed to get audio capture client: {e:?}"))?;

    let mut temp_file = std::fs::File::create(wav_path)?;
    temp_file.write_all(&[0u8; 44])?;

    let mut bytes_written: u32 = 0;

    client
        .start_stream()
        .map_err(|e| anyhow!("failed to start audio stream: {e:?}"))?;

    let started = std::time::Instant::now();
    // half a wall-clock second of slack before padding first kicks in: real
    // packets lag the clock by device latency, and padding must never race a
    // stream that is merely delayed. once a silent stretch is confirmed the
    // pad tracks the clock every poll so resumed audio lands on time
    let pad_threshold_frames = sample_rate as u64 / 2;
    let mut padding_active = false;

    while stop_rx.try_recv().is_err() {
        std::thread::sleep(std::time::Duration::from_millis(10));

        while let Ok(Some(packet_size)) = capture_client.get_next_packet_size() {
            if packet_size == 0 {
                break;
            }
            let mut chunk = vec![0u8; packet_size as usize * block_align];
            if let Ok((frames, _info)) = capture_client.read_from_device(&mut chunk) {
                if frames > 0 {
                    let size = frames as usize * block_align;
                    temp_file.write_all(&chunk[..size])?;
                    bytes_written = bytes_written.saturating_add(size as u32);
                    padding_active = false;
                }
            } else {
                break;
            }
        }

        // wasapi loopback stops delivering packets while the system is silent;
        // without zero-fill the gaps collapse, the wav runs short, the
        // -shortest mux truncates the video, and audio after the gap plays
        // early. zeros keep the track wall-clock aligned
        let expected_frames = (started.elapsed().as_secs_f64() * sample_rate as f64) as u64;
        let written_frames = bytes_written as u64 / block_align as u64;
        let gap = expected_frames.saturating_sub(written_frames);
        if gap > 0 && (padding_active || gap > pad_threshold_frames) {
            padding_active = true;
            let silence = vec![0u8; gap as usize * block_align];
            temp_file.write_all(&silence)?;
            bytes_written = bytes_written.saturating_add(silence.len() as u32);
        }
    }

    let _ = client.stop_stream();

    temp_file.seek(std::io::SeekFrom::Start(0))?;
    write_wav_header(
        &mut temp_file,
        bytes_written,
        sample_rate,
        channels,
        bits_per_sample,
    )?;

    Ok(())
}

// linux: pulseaudio (or pipewire-pulse) exposes the default output's monitor
// as @DEFAULT_MONITOR@, and monitor sources deliver continuous silence while
// nothing plays — the wall-clock alignment the wasapi path zero-fills for
// comes free. a second ffmpeg child records it straight to the wav.
#[cfg(target_os = "linux")]
pub fn record_loopback_audio(wav_path: &Path, stop_rx: Receiver<()>) -> Result<()> {
    use std::io::Write;

    let mut child = ffmpeg_command()
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "pulse",
            "-i",
            "@DEFAULT_MONITOR@",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-c:a",
            "pcm_s16le",
            "-y",
        ])
        .arg(wav_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("audio ffmpeg spawn failed: {e}"))?;

    // block until the recording stops (signal or sender drop), then ask
    // ffmpeg to finish cleanly so the wav header gets finalized
    let _ = stop_rx.recv();
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
    }
    // bounded wait so a wedged ffmpeg can't outlive the recording
    for _ in 0..40 {
        if let Ok(Some(_)) = child.try_wait() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn record_loopback_audio(_wav_path: &Path, _stop_rx: Receiver<()>) -> Result<()> {
    Err(anyhow!("loopback audio is not supported on this platform"))
}

impl Default for GifRecorder {
    fn default() -> Self {
        Self::new(RecordingSettings::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn test_write_wav_header_structure() {
        let mut buffer = tempfile::tempfile().unwrap();
        let res = write_wav_header(&mut buffer, 1000, 44100, 2, 16);
        assert!(res.is_ok());

        use std::io::{Read, Seek};
        buffer.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut header = [0u8; 44];
        buffer.read_exact(&mut header).unwrap();

        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 1036);
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(header[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(header[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(header[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            44100
        );
        assert_eq!(
            u32::from_le_bytes(header[28..32].try_into().unwrap()),
            176400
        );
        assert_eq!(u16::from_le_bytes(header[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(header[34..36].try_into().unwrap()), 16);
        assert_eq!(&header[36..40], b"data");
        assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()), 1000);
    }

    #[test]
    fn format_reports_configured_sink() {
        // exit_app and finalize branch save() vs save_mp4() on this; if it ever
        // stops reflecting the configured format, mp4 recordings get lost on quit
        let gif = GifRecorder::new(RecordingSettings::default());
        assert_eq!(gif.format(), RecordingFormat::Gif);
        let mp4 = GifRecorder::new(RecordingSettings {
            format: RecordingFormat::Mp4,
            ..Default::default()
        });
        assert_eq!(mp4.format(), RecordingFormat::Mp4);
    }

    fn times_at_interval(count: usize, interval_ms: f64) -> Vec<Duration> {
        (0..count)
            .map(|i| Duration::from_secs_f64(i as f64 * interval_ms / 1000.0))
            .collect()
    }

    #[test]
    fn frame_comparison_detects_a_localized_change() {
        let base = RgbaImage::from_pixel(320, 180, image::Rgba([20, 30, 40, 255]));
        let mut changed = base.clone();
        for y in 2..10 {
            for x in 2..10 {
                changed.put_pixel(x, y, image::Rgba([220, 40, 30, 255]));
            }
        }

        assert!(frames_match(Some(&base), &base));
        assert!(!frames_match(Some(&base), &changed));
    }

    #[test]
    fn black_source_confirmation_expires_after_nonblack_frame() {
        let mut cooldown = 0;

        assert!(should_probe_direct_black(&mut cooldown, true));
        cooldown = DIRECT_BLACK_RECHECK_FRAMES;
        assert!(!should_probe_direct_black(&mut cooldown, true));
        assert!(!should_probe_direct_black(&mut cooldown, false));
        assert_eq!(cooldown, 0);
        assert!(should_probe_direct_black(&mut cooldown, true));
    }

    #[test]
    fn exact_black_check_detects_sparse_content() {
        let mut image = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
        image.put_pixel(3, 1, image::Rgba([0, 0, 1, 255]));

        assert!(crate::capture::is_black_frame(&image));
        assert!(!is_exact_black(&image));
    }

    #[test]
    fn monitor_containment_rejects_spanning_region() {
        let monitor = MonitorInfo {
            id: 1,
            name: "left".to_string(),
            x: -1080,
            y: 0,
            width: 1080,
            height: 1920,
            is_primary: false,
        };

        assert!(region_within_monitor(
            Rectangle {
                x: -1000,
                y: 100,
                width: 800,
                height: 600,
            },
            &monitor,
        ));
        assert!(!region_within_monitor(
            Rectangle {
                x: -100,
                y: 100,
                width: 200,
                height: 600,
            },
            &monitor,
        ));
    }

    #[test]
    #[ignore = "performance probe"]
    fn full_frame_comparison_fits_a_fifteen_fps_tick() {
        let previous = RgbaImage::from_pixel(4096, 4096, image::Rgba([20, 30, 40, 255]));
        let current = previous.clone();
        let started = Instant::now();
        for _ in 0..10 {
            assert!(frames_match(
                Some(std::hint::black_box(&previous)),
                std::hint::black_box(&current),
            ));
        }
        let per_frame = started.elapsed() / 10;
        eprintln!("4096x4096 full-frame comparison: {per_frame:?}/frame");
        assert!(per_frame < Duration::from_millis(66));
    }

    #[test]
    fn gif_schedule_uses_real_gaps() {
        let times = vec![
            Duration::from_millis(0),
            Duration::from_millis(100),
            Duration::from_millis(600),
        ];
        let nominal = Duration::from_millis(66);
        let delays = gif_delay_schedule(&times, nominal, None);
        assert_eq!(delays, vec![10, 50, 7]);
    }

    #[test]
    fn gif_schedule_total_matches_wall_clock_at_15fps() {
        // 15fps captures are 66.7ms apart; per-frame truncation used to emit
        // 6cs each (10% fast). the cumulative schedule must land on ~667cs
        let times = times_at_interval(100, 1000.0 / 15.0);
        let delays = gif_delay_schedule(&times, Duration::from_secs_f64(1.0 / 15.0), None);
        let total: u64 = delays.iter().map(|&d| d as u64).sum();
        assert!((666..=668).contains(&total), "total {total}cs, want ~667");
        assert!(delays.iter().all(|&d| d == 0 || d >= 2));
    }

    #[test]
    fn gif_schedule_drops_frames_below_player_floor() {
        // 60fps capture: 16.7ms/frame is under the 2cs player floor, so some
        // frames must be dropped rather than padding the gif 20% slower
        let times = times_at_interval(120, 1000.0 / 60.0);
        let delays = gif_delay_schedule(&times, Duration::from_secs_f64(1.0 / 60.0), None);
        let total: u64 = delays.iter().map(|&d| d as u64).sum();
        assert!((199..=201).contains(&total), "total {total}cs, want ~200");
        assert!(delays.contains(&0), "expected dropped frames");
        assert!(delays.iter().all(|&d| d == 0 || d >= 2));
    }

    #[test]
    fn gif_schedule_caps_single_hold() {
        let times = vec![Duration::from_millis(0), Duration::from_secs(120)];
        let delays = gif_delay_schedule(&times, Duration::from_millis(66), None);
        assert_eq!(delays[0], 6000);
    }

    #[test]
    fn gif_schedule_keeps_at_least_one_frame() {
        let times = vec![Duration::from_millis(0), Duration::from_millis(5)];
        let delays = gif_delay_schedule(&times, Duration::from_millis(5), None);
        assert_eq!(delays.iter().filter(|&&d| d > 0).count(), 1);
        assert!(delays.iter().all(|&d| d == 0 || d >= 2));
    }

    #[test]
    fn mp4_schedule_preserves_wall_clock() {
        // 100ms captures at a 15fps output used to round to 2 ticks each
        // (33% slow); cumulative mapping alternates 1 and 2 for 1.5 average
        let times = times_at_interval(100, 100.0);
        let repeats = mp4_repeat_schedule(&times, Duration::from_millis(100), 15);
        let total: u64 = repeats.iter().sum();
        assert!(
            (149..=151).contains(&total),
            "total {total} ticks, want ~150"
        );
    }

    #[test]
    fn mp4_schedule_holds_through_gaps() {
        // a 10s static span keeps its duration through dedup
        let times = vec![Duration::from_millis(0), Duration::from_secs(10)];
        let repeats = mp4_repeat_schedule(&times, Duration::from_secs_f64(1.0 / 15.0), 15);
        assert_eq!(repeats[0], 150);
    }

    #[test]
    fn mp4_schedule_skips_subtick_frames() {
        // two frames inside one tick: only one may be written
        let times = vec![
            Duration::from_millis(0),
            Duration::from_millis(10),
            Duration::from_millis(1000),
        ];
        let repeats = mp4_repeat_schedule(&times, Duration::from_millis(66), 15);
        let total: u64 = repeats.iter().sum();
        assert!(repeats.contains(&0), "expected a skipped frame");
        assert!((15..=17).contains(&total), "total {total} ticks");
    }

    #[test]
    fn gif_save_preserves_wall_clock_duration() {
        // 45 frames arriving every 222ms — slower than the 15fps nominal,
        // the exact shape that used to compress playback time
        let recorder = GifRecorder::new(RecordingSettings::default());
        {
            let mut spool = FrameSpool::create().unwrap();
            for i in 0..45u32 {
                let shade = (i * 5) as u8;
                let img = RgbaImage::from_pixel(32, 24, image::Rgba([shade, 0, 255 - shade, 255]));
                spool
                    .push(&img, Duration::from_millis(i as u64 * 222))
                    .unwrap();
            }
            *recorder.sink.lock().unwrap() = Some(FrameSink::Gif {
                spool,
                capture_end: None,
            });
        }

        let path = std::env::temp_dir().join(format!(
            "capscr_test_{}.gif",
            uuid::Uuid::new_v4().as_simple()
        ));
        recorder.save(&path).unwrap();

        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::Indexed);
        let mut decoder = options
            .read_info(std::fs::File::open(&path).unwrap())
            .unwrap();
        let mut total_cs: u64 = 0;
        let mut frames = 0;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            total_cs += frame.delay as u64;
            frames += 1;
        }
        let _ = std::fs::remove_file(&path);

        assert_eq!(frames, 45);
        // 44 gaps * 222ms + one nominal 66.7ms hold = 9834.7ms
        assert!(
            (982..=985).contains(&total_cs),
            "total {total_cs}cs, want ~983"
        );
    }

    #[test]
    fn gif_save_preserves_static_tail_until_capture_end() {
        let recorder = GifRecorder::new(RecordingSettings::default());
        {
            let mut spool = FrameSpool::create().unwrap();
            let image = RgbaImage::from_pixel(32, 24, image::Rgba([40, 80, 120, 255]));
            spool.push(&image, Duration::ZERO).unwrap();
            *recorder.sink.lock().unwrap() = Some(FrameSink::Gif {
                spool,
                capture_end: Some(Duration::from_secs(1)),
            });
        }

        let path = std::env::temp_dir().join(format!(
            "capscr_static_test_{}.gif",
            uuid::Uuid::new_v4().as_simple()
        ));
        recorder.save(&path).unwrap();

        let mut decoder = gif::DecodeOptions::new()
            .read_info(std::fs::File::open(&path).unwrap())
            .unwrap();
        let mut total_cs = 0u64;
        let mut frames = 0;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            total_cs += frame.delay as u64;
            frames += 1;
        }
        let _ = std::fs::remove_file(&path);

        assert_eq!(frames, 1);
        assert_eq!(total_cs, 100);
    }

    #[test]
    fn pause_clock_cuts_paused_time() {
        let start = Instant::now();
        let mut clock = PauseClock::default();
        clock.update(true, start + Duration::from_secs(2));
        assert!(clock.is_paused());
        assert_eq!(
            clock.active(start, start + Duration::from_secs(5)),
            Duration::from_secs(2)
        );
        clock.update(false, start + Duration::from_secs(5));
        assert_eq!(
            clock.active(start, start + Duration::from_secs(6)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn output_dims_follow_scale_and_max_width() {
        let mut opts = GifOptions::default();
        assert_eq!(gif_output_dims(800, 600, &opts), (800, 600));
        opts.scale_percent = 50;
        assert_eq!(gif_output_dims(800, 600, &opts), (400, 300));
        opts.max_width = 200;
        assert_eq!(gif_output_dims(800, 600, &opts), (200, 150));
    }

    #[test]
    fn crop_covers_only_changed_pixels() {
        let mut indexes = vec![9u8; 6 * 4];
        indexes[6 + 2] = 1;
        indexes[2 * 6 + 4] = 2;
        let (left, top, width, height, buffer) = crop_changed(&indexes, 6, 4, 9);
        assert_eq!((left, top, width, height), (2, 1, 3, 2));
        assert_eq!(buffer, vec![1, 9, 9, 9, 9, 2]);
        assert_eq!(crop_changed(&[9; 4], 2, 2, 9).4, vec![9]);
    }

    fn moving_square(i: u32, width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            let sx = (i * 2) % (width - 6);
            if (sx..sx + 6).contains(&x) && (8..14).contains(&y) {
                image::Rgba([250, 250, 250, 255])
            } else {
                image::Rgba([(x * 8) as u8, (y * 10) as u8, 90, 255])
            }
        })
    }

    fn recorder_with(frames: &[RgbaImage], gif: GifOptions) -> GifRecorder {
        let recorder = GifRecorder::new(RecordingSettings {
            gif,
            ..Default::default()
        });
        let mut spool = FrameSpool::create().unwrap();
        for (i, frame) in frames.iter().enumerate() {
            spool
                .push(frame, Duration::from_millis(i as u64 * 100))
                .unwrap();
        }
        *recorder.sink.lock().unwrap() = Some(FrameSink::Gif {
            spool,
            capture_end: Some(Duration::from_millis(frames.len() as u64 * 100)),
        });
        recorder
    }

    // composited frames plus total delay in centiseconds
    fn decode_composited(recorder: &GifRecorder) -> (Vec<RgbaImage>, u64) {
        use image::AnimationDecoder;
        let path = std::env::temp_dir().join(format!(
            "capscr_test_{}.gif",
            uuid::Uuid::new_v4().as_simple()
        ));
        recorder.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let mut total_cs = 0;
        let frames = decoder
            .into_frames()
            .map(|frame| {
                let frame = frame.unwrap();
                let (num, den) = frame.delay().numer_denom_ms();
                total_cs += (num / den / 10) as u64;
                frame.into_buffer()
            })
            .collect();
        (frames, total_cs)
    }

    fn mean_error(a: &RgbaImage, b: &RgbaImage) -> f64 {
        let sum: u64 = a
            .as_raw()
            .iter()
            .zip(b.as_raw())
            .map(|(x, y)| x.abs_diff(*y) as u64)
            .sum();
        sum as f64 / a.as_raw().len() as f64
    }

    #[test]
    fn every_builtin_mode_round_trips() {
        let frames: Vec<_> = (0..12).map(|i| moving_square(i, 32, 24)).collect();
        for dither in [false, true] {
            for palette in [GifPalette::Global, GifPalette::PerFrame] {
                for diff_frames in [false, true] {
                    let opts = GifOptions {
                        dither,
                        palette,
                        diff_frames,
                        ..Default::default()
                    };
                    let label = format!("{opts:?}");
                    let (decoded, total_cs) = decode_composited(&recorder_with(&frames, opts));
                    assert_eq!(decoded.len(), frames.len(), "{label}");
                    assert_eq!(total_cs, 120, "{label}");
                    for (got, want) in decoded.iter().zip(&frames) {
                        assert_eq!(got.dimensions(), (32, 24), "{label}");
                        let err = mean_error(got, want);
                        assert!(err < 12.0, "{label}: mean error {err}");
                    }
                }
            }
        }
    }

    #[test]
    fn few_colors_and_scaling_still_encode() {
        let frames: Vec<_> = (0..4).map(|i| moving_square(i, 32, 24)).collect();
        let opts = GifOptions {
            colors: 2,
            scale_percent: 50,
            ..Default::default()
        };
        let (decoded, _) = decode_composited(&recorder_with(&frames, opts));
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0].dimensions(), (16, 12));
    }

    #[test]
    fn resized_region_keeps_the_first_canvas() {
        let frames = vec![
            moving_square(0, 32, 24),
            moving_square(1, 32, 24),
            moving_square(2, 48, 36),
            moving_square(3, 20, 30),
        ];
        let (decoded, _) = decode_composited(&recorder_with(&frames, GifOptions::default()));
        assert_eq!(decoded.len(), 4);
        assert!(decoded.iter().all(|f| f.dimensions() == (32, 24)));
    }

    #[test]
    fn ffmpeg_engine_matches_builtin_timing() {
        if !is_ffmpeg_available() {
            return;
        }
        let frames: Vec<_> = (0..12).map(|i| moving_square(i, 32, 24)).collect();
        for palette in [GifPalette::Global, GifPalette::PerFrame] {
            let opts = GifOptions {
                engine: GifEngine::Ffmpeg,
                palette,
                ..Default::default()
            };
            let (decoded, total_cs) = decode_composited(&recorder_with(&frames, opts));
            assert!(!decoded.is_empty());
            assert_eq!(decoded[0].dimensions(), (32, 24));
            assert!((115..=125).contains(&total_cs), "{palette:?}: {total_cs}cs");
            let err = mean_error(decoded.last().unwrap(), frames.last().unwrap());
            assert!(err < 12.0, "{palette:?}: mean error {err}");
        }
    }

    #[test]
    fn ffmpeg_rate_divides_centiseconds() {
        assert_eq!(ffmpeg_gif_rate(15), 20);
        assert_eq!(ffmpeg_gif_rate(30), 50);
        assert_eq!(ffmpeg_gif_rate(60), 50);
        assert_eq!(ffmpeg_gif_rate(10), 10);
    }

    #[test]
    fn test_find_best_monitor_overlap() {
        if let Ok(monitors) = crate::capture::list_monitors() {
            if let Some(m) = monitors.first() {
                let rect = Rectangle {
                    x: m.x + 10,
                    y: m.y + 10,
                    width: 100,
                    height: 100,
                };
                let best = find_best_monitor(rect);
                assert!(best.is_some());
                assert_eq!(best.unwrap().id, m.id);
            }
        }
    }
}
