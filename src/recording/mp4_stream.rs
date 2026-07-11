use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::time::Duration;

use super::gif_encoder::find_ffmpeg;

// encodes the recording as it happens: frames go straight into a live ffmpeg
// child, so an hour-long capture costs one frame of RAM and saving is a remux
// instead of a full re-encode. ffmpeg spawns lazily on the first frame because
// the pipe needs pixel dimensions up front
pub struct Mp4Streamer {
    fps: u32,
    temp_path: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    width: u32,
    height: u32,
    emitted_ticks: u64,
    frames_written: u64,
    frames_pushed: u64,
    // the frame currently on screen; its tick count is only known once the
    // next frame's capture time arrives
    pending: Option<(RgbaImage, Duration)>,
}

impl Mp4Streamer {
    pub fn new(fps: u32) -> Self {
        let temp_path = std::env::temp_dir().join(format!(
            "capscr_video_{}.mp4",
            uuid::Uuid::new_v4().as_simple()
        ));
        Self {
            fps: fps.clamp(1, 60),
            temp_path,
            child: None,
            stdin: None,
            width: 0,
            height: 0,
            emitted_ticks: 0,
            frames_written: 0,
            frames_pushed: 0,
            pending: None,
        }
    }

    pub fn frames_pushed(&self) -> u64 {
        self.frames_pushed
    }

    pub fn push(&mut self, image: RgbaImage, at: Duration) -> Result<()> {
        if self.child.is_none() {
            self.spawn_encoder(image.width(), image.height())?;
        }
        if let Some((prev, prev_at)) = self.pending.take() {
            self.write_span(&prev, prev_at, at)?;
        }
        self.frames_pushed += 1;
        self.pending = Some((image, at));
        Ok(())
    }

    /// flushes the pending frame with one nominal hold, closes the pipe, and
    /// waits for ffmpeg. returns the finished video-only temp file
    pub fn finish(&mut self) -> Result<PathBuf> {
        if let Some((img, at)) = self.pending.take() {
            let nominal = Duration::from_secs_f64(1.0 / self.fps as f64);
            self.write_span(&img, at, at.saturating_add(nominal))?;
            if self.frames_written == 0 {
                // sub-tick recording: still emit one frame so the file is valid
                self.write_raw(&img, 1)?;
            }
        }
        drop(self.stdin.take());
        let mut child = self
            .child
            .take()
            .ok_or_else(|| anyhow!("No frames captured"))?;
        let status = child
            .wait()
            .map_err(|e| anyhow!("Failed to wait for ffmpeg: {}", e))?;
        if !status.success() {
            return Err(anyhow!("ffmpeg exited with error status: {}", status));
        }
        Ok(self.temp_path.clone())
    }

    fn spawn_encoder(&mut self, width: u32, height: u32) -> Result<()> {
        (self.width, self.height) = even_dims(width, height);

        let args = [
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{}x{}", self.width, self.height),
            "-r",
            &self.fps.to_string(),
            "-i",
            "-",
            "-c:v",
            "libx264",
            // realtime encode must keep up with capture or the pipe stalls
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "23",
            "-y",
            &self.temp_path.to_string_lossy(),
        ];

        let mut child = ffmpeg_command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn ffmpeg: {}", e))?;
        self.stdin = Some(
            child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("Failed to open ffmpeg stdin"))?,
        );
        self.child = Some(child);
        Ok(())
    }

    // same cumulative schedule as the offline gif path: the frame occupies
    // ticks up to round(end * fps), so rounding error never accumulates. a
    // span past 60s is cut and the clock resyncs; a sub-tick span writes
    // nothing and folds into the next frame
    fn write_span(&mut self, image: &RgbaImage, start: Duration, end: Duration) -> Result<()> {
        let target = (end.max(start).as_secs_f64() * self.fps as f64).round() as u64;
        let max_ticks = 60 * self.fps as u64;
        let mut slot = target.saturating_sub(self.emitted_ticks);
        if slot > max_ticks {
            slot = max_ticks;
            self.emitted_ticks = target;
        } else {
            self.emitted_ticks += slot;
        }
        if slot == 0 {
            return Ok(());
        }
        self.write_raw(image, slot)
    }

    fn write_raw(&mut self, image: &RgbaImage, repeats: u64) -> Result<()> {
        let resized = if image.width() != self.width || image.height() != self.height {
            image::imageops::resize(
                image,
                self.width,
                self.height,
                image::imageops::FilterType::Triangle,
            )
        } else {
            image.clone()
        };
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("ffmpeg stdin closed"))?;
        for _ in 0..repeats {
            stdin
                .write_all(resized.as_raw())
                .map_err(|e| anyhow!("Failed to write to ffmpeg stdin: {}", e))?;
        }
        self.frames_written += repeats;
        Ok(())
    }
}

impl Drop for Mp4Streamer {
    fn drop(&mut self) {
        // abandoned mid-recording (reset / app exit): close the pipe, reap the
        // child, and remove the partial file
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.temp_path);
    }
}

// libx264 needs even dimensions for yuv420p
fn even_dims(width: u32, height: u32) -> (u32, u32) {
    (width.max(2) & !1, height.max(2) & !1)
}

pub fn ffmpeg_command() -> std::process::Command {
    let cmd = std::process::Command::new(find_ffmpeg());
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        // CREATE_NO_WINDOW — the app runs with windows_subsystem = "windows",
        // so a console child would otherwise flash a console window
        cmd.creation_flags(0x0800_0000);
        cmd
    };
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_dimension_rounding() {
        assert_eq!(even_dims(101, 55), (100, 54));
        assert_eq!(even_dims(1920, 1080), (1920, 1080));
        assert_eq!(even_dims(1, 1), (2, 2));
    }

    #[test]
    fn abandoned_streamer_leaves_no_temp_file() {
        let path;
        {
            let s = Mp4Streamer::new(15);
            path = s.temp_path.clone();
        }
        assert!(!path.exists());
    }

    // duration from the mvhd box: version byte decides 32- vs 64-bit fields
    fn mp4_duration_secs(bytes: &[u8]) -> Option<f64> {
        let pos = bytes.windows(4).position(|w| w == b"mvhd")?;
        let body = &bytes[pos + 4..];
        let (timescale, duration) = if body.first()? == &1 {
            (
                u32::from_be_bytes(body.get(20..24)?.try_into().ok()?),
                u64::from_be_bytes(body.get(24..32)?.try_into().ok()?),
            )
        } else {
            (
                u32::from_be_bytes(body.get(12..16)?.try_into().ok()?),
                u32::from_be_bytes(body.get(16..20)?.try_into().ok()?) as u64,
            )
        };
        if timescale == 0 {
            return None;
        }
        Some(duration as f64 / timescale as f64)
    }

    #[test]
    fn stream_preserves_wall_clock_duration() {
        if !super::super::is_ffmpeg_available() {
            return;
        }
        // 40 frames every 100ms: per-frame rounding at 15fps used to stretch
        // this to 5.3s; the cumulative schedule must land on 4.0s
        let mut streamer = Mp4Streamer::new(15);
        for i in 0..40u32 {
            let shade = (i * 6) as u8;
            let img = RgbaImage::from_pixel(64, 48, image::Rgba([shade, 30, 60, 255]));
            streamer
                .push(img, Duration::from_millis(i as u64 * 100))
                .unwrap();
        }
        let path = streamer.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let secs = mp4_duration_secs(&bytes).expect("mvhd box present");
        assert!((3.8..=4.2).contains(&secs), "duration {secs}s, want ~4.0");
    }
}
