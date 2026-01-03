use anyhow::{anyhow, Result};
use gif::{Encoder, Frame, Repeat};
use image::RgbaImage;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::{Rectangle, ScreenCapture};

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

        thread::spawn(move || {
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
                    let full = ScreenCapture::all_monitors();
                    full.and_then(|img| {
                        let x = rect.x.max(0) as u32;
                        let y = rect.y.max(0) as u32;
                        let max_w = img.width().saturating_sub(x);
                        let max_h = img.height().saturating_sub(y);
                        let w = rect.width.min(max_w).min(MAX_GIF_DIMENSION);
                        let h = rect.height.min(max_h).min(MAX_GIF_DIMENSION);
                        if w == 0 || h == 0 {
                            return Err(anyhow::anyhow!("Invalid region"));
                        }
                        Ok(image::imageops::crop_imm(&img, x, y, w, h).to_image())
                    })
                } else {
                    ScreenCapture::all_monitors().map(|img| {
                        if img.width() > MAX_GIF_DIMENSION || img.height() > MAX_GIF_DIMENSION {
                            let scale_w = MAX_GIF_DIMENSION as f32 / img.width() as f32;
                            let scale_h = MAX_GIF_DIMENSION as f32 / img.height() as f32;
                            let scale = scale_w.min(scale_h);
                            let new_w = ((img.width() as f32) * scale) as u32;
                            let new_h = ((img.height() as f32) * scale) as u32;
                            image::imageops::resize(&img, new_w.max(1), new_h.max(1), image::imageops::FilterType::Triangle)
                        } else {
                            img
                        }
                    })
                };

                if let Ok(image) = capture_result {
                    if image.width() <= MAX_GIF_DIMENSION && image.height() <= MAX_GIF_DIMENSION {
                        let fingerprint = compute_frame_fingerprint(&image);

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
                            frames_lock.push(CapturedFrame { image, fingerprint });
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

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        let mut encoder = Encoder::new(file, width, height, &[])?;
        encoder.set_repeat(Repeat::Infinite)?;

        let fps = self.settings.fps.clamp(1, 60);
        let delay = (100.0 / fps as f64).clamp(2.0, 100.0) as u16;

        let filter = if self.settings.quality >= 80 {
            image::imageops::FilterType::Lanczos3
        } else if self.settings.quality >= 50 {
            image::imageops::FilterType::Triangle
        } else {
            image::imageops::FilterType::Nearest
        };

        for captured in frames.iter() {
            let resized = if captured.image.width() != orig_width
                || captured.image.height() != orig_height
            {
                image::imageops::resize(
                    &captured.image,
                    orig_width,
                    orig_height,
                    filter,
                )
            } else {
                captured.image.clone()
            };

            let rgba_data: Vec<u8> = resized.into_raw();
            let pixel_count = (width as usize).saturating_mul(height as usize);
            let rgb_capacity = pixel_count.saturating_mul(3);

            if rgb_capacity > 64 * 1024 * 1024 {
                return Err(anyhow!("Frame too large to encode"));
            }

            let mut rgb_data: Vec<u8> = Vec::with_capacity(rgb_capacity);

            for chunk in rgba_data.chunks_exact(4) {
                rgb_data.push(chunk[0]);
                rgb_data.push(chunk[1]);
                rgb_data.push(chunk[2]);
            }

            let mut frame = Frame::from_rgb(width, height, &rgb_data);
            frame.delay = delay;
            encoder.write_frame(&frame)?;
        }

        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.len() > MAX_GIF_FILE_SIZE {
                let _ = std::fs::remove_file(path);
                return Err(anyhow!("Generated GIF exceeds maximum file size"));
            }
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

impl Default for GifRecorder {
    fn default() -> Self {
        Self::new(RecordingSettings::default())
    }
}
