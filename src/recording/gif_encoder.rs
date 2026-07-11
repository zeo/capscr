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

use super::mp4_stream::{ffmpeg_command, Mp4Streamer};
use super::spool::FrameSpool;
use super::{RecordingFormat, RecordingSettings, RecordingState, StopReason};

// insanity backstop above the theoretical max of 300s * 60fps
const MAX_FRAMES: usize = 21600;
const MAX_GIF_DIMENSION: u32 = 4096;
const MAX_GIF_FILE_SIZE: u64 = 500 * 1024 * 1024;
const MIN_FRAME_INTERVAL_MS: u64 = 16;

// where kept frames go during capture. RAM stays flat either way: gif frames
// spool to a temp file for the post-stop encode, mp4 frames stream into a
// live ffmpeg child as they arrive
enum FrameSink {
    Gif(FrameSpool),
    Mp4(Mp4Streamer),
}

pub struct GifRecorder {
    state: Arc<Mutex<RecordingState>>,
    settings: RecordingSettings,
    sink: Arc<Mutex<Option<FrameSink>>>,
    stop_reason: Arc<Mutex<Option<StopReason>>>,
    stop_signal: Option<Sender<()>>,
    region: Option<Rectangle>,
    audio_temp_path: Option<std::path::PathBuf>,
    audio_stop_tx: Option<Sender<()>>,
}

// map each frame's real end time (next frame's capture time; the last frame
// holds one nominal interval) onto a discrete output clock. Rounding every
// frame independently drifts by up to one unit per frame — a 15fps gif loses
// 6.7ms each frame and plays ~11% fast — so the schedule tracks the cumulative
// target instead, keeping total drift under one unit for any length. A frame
// whose slot lands below `min_units` gets 0 (the caller drops it and its time
// folds into the next frame); a hold beyond `max_units` is cut there and the
// clock resyncs, so one bad gap can't freeze playback
fn schedule_frames(
    times: &[Duration],
    nominal: Duration,
    units_per_sec: f64,
    min_units: u64,
    max_units: u64,
) -> Vec<u64> {
    let mut emitted: u64 = 0;
    let mut schedule: Vec<u64> = times
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let end = match times.get(i + 1) {
                Some(next) => *next.max(t),
                None => t.saturating_add(nominal),
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
fn gif_delay_schedule(times: &[Duration], nominal: Duration) -> Vec<u16> {
    schedule_frames(times, nominal, 100.0, 2, 6000)
        .into_iter()
        .map(|u| u as u16)
        .collect()
}

// ffmpeg consumes rawvideo at a constant input rate; each frame is written
// once per tick of its slot so wall-clock timing survives dedup and slow
// captures. 0 means the frame is skipped entirely
fn mp4_repeat_schedule(times: &[Duration], nominal: Duration, fps: u32) -> Vec<u64> {
    schedule_frames(times, nominal, fps as f64, 1, 60 * fps.max(1) as u64)
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
            sink: Arc::new(Mutex::new(None)),
            stop_reason: Arc::new(Mutex::new(None)),
            stop_signal: None,
            region: None,
            audio_temp_path: None,
            audio_stop_tx: None,
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

    /// why the capture loop ended; None while it is still running
    pub fn stop_reason(&self) -> Option<StopReason> {
        *self.stop_reason.lock().unwrap_or_else(|e| e.into_inner())
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
                Ok(spool) => FrameSink::Gif(spool),
                Err(e) => {
                    *self.state.lock().unwrap_or_else(|p| p.into_inner()) = RecordingState::Idle;
                    return Err(e);
                }
            },
            RecordingFormat::Mp4 => FrameSink::Mp4(Mp4Streamer::new(self.settings.fps)),
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
            let mut frames_kept: usize = 0;
            let mut last_fingerprint: u64 = 0;
            let mut consecutive_dupes: u32 = 0;

            let reason = loop {
                if rx.try_recv().is_ok() {
                    break StopReason::Requested;
                }

                if start_time.elapsed() >= max_duration {
                    break StopReason::MaxDuration;
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

                        if frames_kept >= MAX_FRAMES {
                            break StopReason::FrameCap;
                        }

                        let at = frame_start.duration_since(start_time);
                        let mut sink_guard = sink.lock().unwrap_or_else(|e| e.into_inner());
                        match sink_guard.as_mut() {
                            Some(FrameSink::Gif(spool)) => match spool.push(&image, at) {
                                Ok(true) => frames_kept += 1,
                                Ok(false) => break StopReason::DiskFull,
                                Err(e) => {
                                    tracing::error!("frame spool write failed: {e}");
                                    break StopReason::EncoderFailed;
                                }
                            },
                            Some(FrameSink::Mp4(streamer)) => {
                                if let Err(e) = streamer.push(image, at) {
                                    tracing::error!("mp4 stream write failed: {e}");
                                    break StopReason::EncoderFailed;
                                }
                                frames_kept += 1;
                            }
                            // reset() cleared the sink under us — just end
                            None => break StopReason::Requested,
                        }
                    }
                }

                let elapsed = frame_start.elapsed();
                if elapsed < frame_duration {
                    thread::sleep(frame_duration - elapsed);
                }
            };

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
            Some(FrameSink::Gif(spool)) => spool.len(),
            Some(FrameSink::Mp4(streamer)) => streamer.frames_pushed() as usize,
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
        let spool = match sink_guard.as_mut() {
            Some(FrameSink::Gif(spool)) => spool,
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

        let width = orig_width as u16;
        let height = orig_height as u16;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let fps = self.settings.fps.clamp(1, 60);
        let nominal = Duration::from_secs_f64(1.0 / fps as f64);
        let times: Vec<Duration> = spool.metas().iter().map(|m| m.at).collect();
        let delays = gif_delay_schedule(&times, nominal);

        let filter = if self.settings.quality >= 80 {
            image::imageops::FilterType::Lanczos3
        } else if self.settings.quality >= 50 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Nearest
        };

        let num_frames = spool.len();
        let sample_step = (num_frames / 15).max(1);
        let mut sample_pixels = Vec::new();

        for i in (0..num_frames).step_by(sample_step) {
            let frame = spool.read_frame(i)?;
            let resized = if frame.width() != orig_width || frame.height() != orig_height {
                image::imageops::resize(&frame, orig_width, orig_height, filter)
            } else {
                frame
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

            for frame_idx in 0..num_frames {
                // 0-delay frames were folded into a neighbour by the schedule;
                // skipping them here also skips their disk read + quantize cost
                if delays[frame_idx] == 0 {
                    continue;
                }
                let frame = spool.read_frame(frame_idx)?;
                let resized = if frame.width() != orig_width || frame.height() != orig_height {
                    image::imageops::resize(&frame, orig_width, orig_height, filter)
                } else {
                    frame
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
                    delay: delays[frame_idx],
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

        if let Some(ref wav_path) = self.audio_temp_path {
            let _ = std::fs::remove_file(wav_path);
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

        // frames were encoded live during capture; all that's left is closing
        // the stream and muxing in the audio track (or moving the file)
        let temp_video = {
            let mut sink_guard = self.sink.lock().unwrap_or_else(|e| e.into_inner());
            match sink_guard.as_mut() {
                Some(FrameSink::Mp4(streamer)) => {
                    if streamer.frames_pushed() == 0 {
                        return Err(anyhow!("No frames captured"));
                    }
                    streamer.finish()?
                }
                _ => return Err(anyhow!("No frames captured")),
            }
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let audio_exists = self.audio_temp_path.as_ref()
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
                return Ok(());
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

        Ok(())
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
pub fn record_loopback_audio(
    wav_path: &Path,
    stop_rx: Receiver<()>,
) -> Result<()> {
    use std::io::{Seek, Write};
    
    let _ = wasapi::initialize_mta();

    let device = wasapi::get_default_device(&wasapi::Direction::Render)
        .map_err(|e| anyhow!("failed to get default render device: {e:?}"))?;

    let mut client = device.get_iaudioclient()
        .map_err(|e| anyhow!("failed to get audio client: {e:?}"))?;

    let format = client.get_mixformat()
        .map_err(|e| anyhow!("failed to get mix format: {e:?}"))?;

    let sample_rate = format.get_samplespersec();
    let channels = format.get_nchannels() as u16;
    let bits_per_sample = format.get_bitspersample() as u16;
    let block_align = format.get_blockalign() as usize;

    let mode = wasapi::StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: 100_000,
    };
    client.initialize_client(&format, &wasapi::Direction::Capture, &mode)
        .map_err(|e| anyhow!("failed to initialize audio client: {e:?}"))?;

    let capture_client = client.get_audiocaptureclient()
        .map_err(|e| anyhow!("failed to get audio capture client: {e:?}"))?;

    let mut temp_file = std::fs::File::create(wav_path)?;
    temp_file.write_all(&[0u8; 44])?;

    let mut bytes_written: u32 = 0;

    client.start_stream()
        .map_err(|e| anyhow!("failed to start audio stream: {e:?}"))?;

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
                }
            } else {
                break;
            }
        }
    }

    let _ = client.stop_stream();

    temp_file.seek(std::io::SeekFrom::Start(0))?;
    write_wav_header(&mut temp_file, bytes_written, sample_rate, channels, bits_per_sample)?;

    Ok(())
}

#[cfg(not(windows))]
pub fn record_loopback_audio(
    _wav_path: &Path,
    _stop_rx: Receiver<()>,
) -> Result<()> {
    Err(anyhow!("WASAPI loopback audio is only supported on Windows"))
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
        
        use std::io::{Seek, Read};
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
        assert_eq!(u32::from_le_bytes(header[24..28].try_into().unwrap()), 44100);
        assert_eq!(u32::from_le_bytes(header[28..32].try_into().unwrap()), 176400);
        assert_eq!(u16::from_le_bytes(header[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(header[34..36].try_into().unwrap()), 16);
        assert_eq!(&header[36..40], b"data");
        assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()), 1000);
    }

    fn times_at_interval(count: usize, interval_ms: f64) -> Vec<Duration> {
        (0..count)
            .map(|i| Duration::from_secs_f64(i as f64 * interval_ms / 1000.0))
            .collect()
    }

    #[test]
    fn gif_schedule_uses_real_gaps() {
        let times = vec![
            Duration::from_millis(0),
            Duration::from_millis(100),
            Duration::from_millis(600),
        ];
        let nominal = Duration::from_millis(66);
        let delays = gif_delay_schedule(&times, nominal);
        assert_eq!(delays, vec![10, 50, 7]);
    }

    #[test]
    fn gif_schedule_total_matches_wall_clock_at_15fps() {
        // 15fps captures are 66.7ms apart; per-frame truncation used to emit
        // 6cs each (10% fast). the cumulative schedule must land on ~667cs
        let times = times_at_interval(100, 1000.0 / 15.0);
        let delays = gif_delay_schedule(&times, Duration::from_secs_f64(1.0 / 15.0));
        let total: u64 = delays.iter().map(|&d| d as u64).sum();
        assert!((666..=668).contains(&total), "total {total}cs, want ~667");
        assert!(delays.iter().all(|&d| d == 0 || d >= 2));
    }

    #[test]
    fn gif_schedule_drops_frames_below_player_floor() {
        // 60fps capture: 16.7ms/frame is under the 2cs player floor, so some
        // frames must be dropped rather than padding the gif 20% slower
        let times = times_at_interval(120, 1000.0 / 60.0);
        let delays = gif_delay_schedule(&times, Duration::from_secs_f64(1.0 / 60.0));
        let total: u64 = delays.iter().map(|&d| d as u64).sum();
        assert!((199..=201).contains(&total), "total {total}cs, want ~200");
        assert!(delays.iter().any(|&d| d == 0), "expected dropped frames");
        assert!(delays.iter().all(|&d| d == 0 || d >= 2));
    }

    #[test]
    fn gif_schedule_caps_single_hold() {
        let times = vec![Duration::from_millis(0), Duration::from_secs(120)];
        let delays = gif_delay_schedule(&times, Duration::from_millis(66));
        assert_eq!(delays[0], 6000);
    }

    #[test]
    fn gif_schedule_keeps_at_least_one_frame() {
        let times = vec![Duration::from_millis(0), Duration::from_millis(5)];
        let delays = gif_delay_schedule(&times, Duration::from_millis(5));
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
        assert!((149..=151).contains(&total), "total {total} ticks, want ~150");
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
