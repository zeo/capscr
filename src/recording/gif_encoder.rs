use anyhow::{anyhow, Result};
use gif::{Encoder, Frame, Repeat};
use image::RgbaImage;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::{MonitorInfo, Rectangle, ScreenCapture};

fn find_best_monitor(rect: Rectangle) -> Option<MonitorInfo> {
    let monitors = crate::capture::fast_list_monitors().ok()?;
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

use super::{RecordingSettings, RecordingState};

const MAX_FRAMES: usize = 18000;
const MAX_GIF_DIMENSION: u32 = 4096;
const MAX_FRAME_MEMORY_MB: usize = 1024;
const MAX_GIF_FILE_SIZE: u64 = 500 * 1024 * 1024;
const MIN_FRAME_INTERVAL_MS: u64 = 16;

pub struct GifRecorder {
    state: Arc<Mutex<RecordingState>>,
    settings: RecordingSettings,
    frames: Arc<Mutex<Vec<CapturedFrame>>>,
    stop_signal: Option<Sender<()>>,
    region: Option<Rectangle>,
}

struct CapturedFrame {
    image: RgbaImage,
    #[allow(dead_code)]
    fingerprint: u64,
    // capture time relative to recording start; encoding uses the real gaps
    // between frames so dedup-skipped and slow captures don't speed playback
    at: Duration,
}

// display duration of each frame: frame i is on screen until frame i+1 was
// captured; the last frame holds for one nominal interval
fn frame_durations(times: &[Duration], nominal: Duration) -> Vec<Duration> {
    times
        .iter()
        .enumerate()
        .map(|(i, t)| match times.get(i + 1) {
            Some(next) => next.saturating_sub(*t),
            None => nominal,
        })
        .collect()
}

// gif delays count in hundredths of a second; players treat <2cs as a slow
// 10cs default, so floor there. Cap a single frame's hold at 60s so one bad
// gap can't freeze the loop
fn gif_delay_cs(d: Duration) -> u16 {
    (d.as_millis() / 10).clamp(2, 6000) as u16
}

// ffmpeg consumes rawvideo at a constant input rate; holding a frame for
// round(duration * fps) ticks preserves wall-clock timing through dedup
fn mp4_frame_repeats(d: Duration, fps: u32) -> u64 {
    let capped = d.min(Duration::from_secs(60));
    ((capped.as_secs_f64() * fps as f64).round() as u64).max(1)
}

fn compute_frame_fingerprint(image: &RgbaImage) -> u64 {
    let w = image.width();
    let h = image.height();
    if w == 0 || h == 0 {
        return 0;
    }

    let mut hash: u64 = 0;
    let sample_points = [
        (w / 4, h / 4),
        (w / 2, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, h / 2),
        (w / 2, h / 2),
        (3 * w / 4, h / 2),
        (w / 4, 3 * h / 4),
        (w / 2, 3 * h / 4),
        (3 * w / 4, 3 * h / 4),
        (w / 8, h / 8),
        (7 * w / 8, h / 8),
        (w / 8, 7 * h / 8),
        (7 * w / 8, 7 * h / 8),
        (w / 3, h / 3),
        (2 * w / 3, h / 3),
        (w / 3, 2 * h / 3),
    ];

    for (i, &(x, y)) in sample_points.iter().enumerate() {
        let x = x.min(w - 1);
        let y = y.min(h - 1);
        let pixel = image.get_pixel(x, y);
        let val = ((pixel[0] as u64) << 16) | ((pixel[1] as u64) << 8) | (pixel[2] as u64);
        hash ^= val.wrapping_shl((i as u32 * 4) % 64);
    }

    hash
}

impl GifRecorder {
    pub fn new(settings: RecordingSettings) -> Self {
        Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            settings,
            frames: Arc::new(Mutex::new(Vec::new())),
            stop_signal: None,
            region: None,
        }
    }

    pub fn with_region(mut self, region: Rectangle) -> Self {
        self.region = Some(region);
        self
    }

    #[allow(dead_code)]
    pub fn state(&self) -> RecordingState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn start(&mut self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if *state != RecordingState::Idle {
                return Ok(());
            }
            *state = RecordingState::Recording;
        }

        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }

        let (tx, rx): (Sender<()>, Receiver<()>) = channel();
        self.stop_signal = Some(tx);

        let state = Arc::clone(&self.state);
        let frames = Arc::clone(&self.frames);
        let fps = self.settings.fps.max(1);
        let max_duration = self.settings.max_duration;
        let region = self.region;
        let best_monitor = region.and_then(find_best_monitor);
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

            let min_frame_duration = Duration::from_millis(MIN_FRAME_INTERVAL_MS);
            let frame_duration = Duration::from_secs_f64(1.0 / fps as f64).max(min_frame_duration);
            let start_time = Instant::now();
            let mut total_memory: usize = 0;
            let max_memory = MAX_FRAME_MEMORY_MB * 1024 * 1024;
            let mut last_fingerprint: u64 = 0;
            let mut consecutive_dupes: u32 = 0;

            loop {
                if rx.try_recv().is_ok() {
                    break;
                }

                if start_time.elapsed() >= max_duration {
                    break;
                }

                let frame_start = Instant::now();

                let capture_result = if let Some(rect) = region {
                    let single_monitor_capture = if let Some(ref m) = best_monitor {
                        crate::capture::capture_one_monitor(m).map(|img| {
                            let local_x = (rect.x - m.x).max(0) as u32;
                            let local_y = (rect.y - m.y).max(0) as u32;
                            let max_w = img.width().saturating_sub(local_x);
                            let max_h = img.height().saturating_sub(local_y);
                            let w = rect.width.min(max_w).min(MAX_GIF_DIMENSION);
                            let h = rect.height.min(max_h).min(MAX_GIF_DIMENSION);
                            (img, local_x, local_y, w, h)
                        })
                    } else {
                        Err(anyhow!("No monitor matches the region"))
                    };

                    match single_monitor_capture {
                        Ok((img, local_x, local_y, w, h)) => {
                            if w == 0 || h == 0 {
                                Err(anyhow!("Invalid region"))
                            } else {
                                Ok(image::imageops::crop_imm(&img, local_x, local_y, w, h)
                                    .to_image())
                            }
                        }
                        Err(_) => {
                            let full = ScreenCapture::all_monitors();
                            full.and_then(|img| {
                                let x = rect.x.max(0) as u32;
                                let y = rect.y.max(0) as u32;
                                let max_w = img.width().saturating_sub(x);
                                let max_h = img.height().saturating_sub(y);
                                let w = rect.width.min(max_w).min(MAX_GIF_DIMENSION);
                                let h = rect.height.min(max_h).min(MAX_GIF_DIMENSION);
                                if w == 0 || h == 0 {
                                    return Err(anyhow!("Invalid region"));
                                }
                                Ok(image::imageops::crop_imm(&img, x, y, w, h).to_image())
                            })
                        }
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

                if let Ok(mut image) = capture_result {
                    if image.width() <= MAX_GIF_DIMENSION && image.height() <= MAX_GIF_DIMENSION {
                        // grab the cursor once so the same snapshot drives both the
                        // dedup fingerprint and the composite below
                        let cursor_shot = if show_cursor && region.is_some() {
                            crate::capture::capture_cursor_shot()
                        } else {
                            None
                        };

                        let mut fingerprint = compute_frame_fingerprint(&image);
                        if let (Some(shot), Some(rect)) = (&cursor_shot, region) {
                            let (cx, cy) = shot.screen_pos();
                            // only perturb the fingerprint while the cursor is inside the
                            // region: movement within it yields new frames, while a cursor
                            // moving outside the capture must not defeat dedup
                            if cx >= rect.x
                                && cy >= rect.y
                                && cx < rect.x.saturating_add(rect.width as i32)
                                && cy < rect.y.saturating_add(rect.height as i32)
                            {
                                fingerprint ^= ((cx as u32 as u64) << 32) | (cy as u32 as u64);
                            }
                        }

                        if fingerprint == last_fingerprint {
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
                        last_fingerprint = fingerprint;

                        // paint the cursor only onto frames we actually keep
                        if let (Some(shot), Some(rect)) = (&cursor_shot, region) {
                            crate::capture::composite_cursor_shot(
                                &mut image,
                                shot,
                                (rect.x, rect.y),
                            );
                        }

                        let frame_size = (image.width() as usize)
                            .saturating_mul(image.height() as usize)
                            .saturating_mul(4);

                        if let Ok(mut frames_lock) = frames.lock() {
                            if frames_lock.len() >= MAX_FRAMES {
                                break;
                            }
                            if total_memory.saturating_add(frame_size) > max_memory {
                                break;
                            }
                            total_memory = total_memory.saturating_add(frame_size);
                            frames_lock.push(CapturedFrame {
                                image,
                                fingerprint,
                                at: frame_start.duration_since(start_time),
                            });
                        }
                    }
                }

                let elapsed = frame_start.elapsed();
                if elapsed < frame_duration {
                    thread::sleep(frame_duration - elapsed);
                }
            }

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
    }

    #[allow(dead_code)]
    pub fn frame_count(&self) -> usize {
        self.frames.lock().unwrap_or_else(|e| e.into_inner()).len()
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

        let frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());

        if frames.is_empty() {
            return Err(anyhow!("No frames captured"));
        }

        let first_frame = &frames[0].image;
        let orig_width = first_frame.width();
        let orig_height = first_frame.height();

        if orig_width > MAX_GIF_DIMENSION || orig_height > MAX_GIF_DIMENSION {
            return Err(anyhow!("Image dimensions exceed GIF safety limit"));
        }
        if orig_width > u16::MAX as u32 || orig_height > u16::MAX as u32 {
            return Err(anyhow!("Image dimensions too large for GIF format"));
        }
        if orig_width == 0 || orig_height == 0 {
            return Err(anyhow!("Image has zero dimension"));
        }

        let width = orig_width as u16;
        let height = orig_height as u16;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let fps = self.settings.fps.clamp(1, 60);
        let nominal = Duration::from_secs_f64(1.0 / fps as f64);
        let times: Vec<Duration> = frames.iter().map(|f| f.at).collect();
        let durations = frame_durations(&times, nominal);

        let filter = if self.settings.quality >= 80 {
            image::imageops::FilterType::Lanczos3
        } else if self.settings.quality >= 50 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Nearest
        };

        let num_frames = frames.len();
        let sample_step = (num_frames / 15).max(1);
        let mut sample_pixels = Vec::new();

        for i in (0..num_frames).step_by(sample_step) {
            let captured = &frames[i];
            let resized =
                if captured.image.width() != orig_width || captured.image.height() != orig_height {
                    image::imageops::resize(&captured.image, orig_width, orig_height, filter)
                } else {
                    captured.image.clone()
                };

            let rgba = resized.as_raw();
            let total_pixels = resized.width() * resized.height();
            let pixel_step = (total_pixels / 10000).max(1) as usize;

            for chunk in rgba.chunks_exact(4).step_by(pixel_step) {
                sample_pixels.extend_from_slice(chunk);
            }
        }

        if sample_pixels.is_empty() {
            sample_pixels.extend_from_slice(&[0, 0, 0, 255]);
        }

        let nq = color_quant::NeuQuant::new(10, 256, &sample_pixels);
        let colormap_rgba = nq.color_map_rgba();
        let mut global_palette = Vec::with_capacity(256 * 3);
        for chunk in colormap_rgba.chunks_exact(4) {
            global_palette.push(chunk[0]);
            global_palette.push(chunk[1]);
            global_palette.push(chunk[2]);
        }

        {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            let mut encoder = Encoder::new(file, width, height, &global_palette)?;
            encoder.set_repeat(Repeat::Infinite)?;

            for (frame_idx, captured) in frames.iter().enumerate() {
                let resized = if captured.image.width() != orig_width
                    || captured.image.height() != orig_height
                {
                    image::imageops::resize(&captured.image, orig_width, orig_height, filter)
                } else {
                    captured.image.clone()
                };

                let rgba_data = resized.as_raw();
                let pixel_count = (width as usize).saturating_mul(height as usize);

                if pixel_count.saturating_mul(4) > 64 * 1024 * 1024 {
                    return Err(anyhow!("Frame too large to encode"));
                }

                let mut indexed_pixels = vec![0u8; pixel_count];
                let mut last_pixel = [0u8; 4];
                let mut last_index = 0u8;
                let mut cache_valid = false;

                for (idx, chunk) in rgba_data.chunks_exact(4).enumerate() {
                    if cache_valid
                        && chunk[0] == last_pixel[0]
                        && chunk[1] == last_pixel[1]
                        && chunk[2] == last_pixel[2]
                        && chunk[3] == last_pixel[3]
                    {
                        indexed_pixels[idx] = last_index;
                    } else {
                        let color_idx = nq.index_of(chunk) as u8;
                        indexed_pixels[idx] = color_idx;
                        last_pixel.copy_from_slice(chunk);
                        last_index = color_idx;
                        cache_valid = true;
                    }
                }

                let frame = Frame {
                    width,
                    height,
                    delay: gif_delay_cs(durations[frame_idx]),
                    buffer: std::borrow::Cow::Owned(indexed_pixels),
                    ..Default::default()
                };

                encoder.write_frame(&frame)?;
            }
        } // encoder and file handle dropped here — all bytes flushed before size check

        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.len() > MAX_GIF_FILE_SIZE {
                let _ = std::fs::remove_file(path);
                return Err(anyhow!("Generated GIF exceeds maximum file size"));
            }
        }

        Ok(())
    }

    pub fn save_mp4<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(anyhow!("Path contains directory traversal"));
        }
        #[cfg(windows)]
        if path_str.starts_with("\\\\") {
            return Err(anyhow!("Network paths not allowed"));
        }

        let frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());

        if frames.is_empty() {
            return Err(anyhow!("No frames captured"));
        }

        let first_frame = &frames[0].image;
        let orig_width = first_frame.width();
        let orig_height = first_frame.height();

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
        let times: Vec<Duration> = frames.iter().map(|f| f.at).collect();
        let durations = frame_durations(&times, nominal);
        let filter = if self.settings.quality >= 80 {
            image::imageops::FilterType::Lanczos3
        } else if self.settings.quality >= 50 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Nearest
        };

        // spawn ffmpeg child process and write raw rgba video frames to stdin
        let mut child = std::process::Command::new(find_ffmpeg())
            .args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                &format!("{}x{}", orig_width, orig_height),
                "-r",
                &fps.to_string(),
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-crf",
                "23",
                "-y",
                &path.to_string_lossy(),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn ffmpeg: {}", e))?;

        {
            use std::io::Write;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("Failed to open ffmpeg stdin"))?;

            for (frame_idx, captured) in frames.iter().enumerate() {
                let resized = if captured.image.width() != orig_width
                    || captured.image.height() != orig_height
                {
                    image::imageops::resize(&captured.image, orig_width, orig_height, filter)
                } else {
                    captured.image.clone()
                };

                let rgba_data = resized.as_raw();
                // the rawvideo input is constant-rate: repeat each frame to
                // cover its real on-screen duration, otherwise dedup-skipped
                // and slow captures compress time and the video plays sped up
                for _ in 0..mp4_frame_repeats(durations[frame_idx], fps) {
                    stdin
                        .write_all(rgba_data)
                        .map_err(|e| anyhow!("Failed to write to ffmpeg stdin: {}", e))?;
                }
            }
        } // stdin is closed here, signaling eof to ffmpeg

        let status = child
            .wait()
            .map_err(|e| anyhow!("Failed to wait for ffmpeg: {}", e))?;
        if !status.success() {
            return Err(anyhow!("ffmpeg exited with error status: {}", status));
        }

        Ok(())
    }

    pub fn reset(&mut self) {
        self.stop();
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        if let Ok(mut state) = self.state.lock() {
            *state = RecordingState::Idle;
        }
    }
}

pub fn find_ffmpeg() -> std::path::PathBuf {
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

pub fn is_ffmpeg_available() -> bool {
    let path = find_ffmpeg();
    if path != std::path::Path::new("ffmpeg") {
        return path.exists();
    }
    // check if system ffmpeg is runnable
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| child.wait().is_ok())
        .unwrap_or(false)
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
    fn frame_durations_use_real_gaps() {
        let times = vec![
            Duration::from_millis(0),
            Duration::from_millis(100),
            Duration::from_millis(600),
        ];
        let nominal = Duration::from_millis(66);
        let durations = frame_durations(&times, nominal);
        assert_eq!(durations[0], Duration::from_millis(100));
        assert_eq!(durations[1], Duration::from_millis(500));
        assert_eq!(durations[2], nominal);
    }

    #[test]
    fn gif_delay_floors_and_caps() {
        assert_eq!(gif_delay_cs(Duration::from_millis(5)), 2);
        assert_eq!(gif_delay_cs(Duration::from_millis(500)), 50);
        assert_eq!(gif_delay_cs(Duration::from_secs(120)), 6000);
    }

    #[test]
    fn mp4_repeats_preserve_wall_clock() {
        // a 500ms gap at 15fps must hold the frame ~7-8 ticks, not 1
        assert_eq!(mp4_frame_repeats(Duration::from_millis(500), 15), 8);
        // fast frames still emit at least one tick
        assert_eq!(mp4_frame_repeats(Duration::from_millis(1), 15), 1);
        // a 10s static span keeps its duration
        assert_eq!(mp4_frame_repeats(Duration::from_secs(10), 15), 150);
    }

    #[test]
    fn test_find_best_monitor_overlap() {
        if let Ok(monitors) = crate::capture::fast_list_monitors() {
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
