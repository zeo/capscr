mod gif_encoder;
mod mp4_stream;
mod spool;

pub(crate) use gif_encoder::PauseClock;
pub use gif_encoder::{is_ffmpeg_available, GifRecorder};
pub use mp4_stream::{ffmpeg_command, hardware_encoders};

use std::time::Duration;

// tests that need ffmpeg skip visibly without it, and fail under
// CAPSCR_REQUIRE_FFMPEG so ci can't pass them by skipping
#[cfg(test)]
pub(crate) fn ffmpeg_or_skip(name: &str) -> bool {
    if is_ffmpeg_available() {
        return true;
    }
    assert!(
        std::env::var_os("CAPSCR_REQUIRE_FFMPEG").is_none(),
        "{name} needs ffmpeg and CAPSCR_REQUIRE_FFMPEG is set"
    );
    eprintln!("skipped {name}: no ffmpeg");
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingFormat {
    #[default]
    Gif,
    Mp4,
}

// why the capture loop ended; anything but Requested / MaxDuration means the
// recording was cut short by a safety limit and the user should hear about it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Requested,
    MaxDuration,
    FrameCap,
    DiskFull,
    EncoderFailed,
    #[cfg(target_os = "linux")]
    SourceLost,
}

#[derive(Debug, Clone)]
pub struct RecordingSettings {
    pub fps: u32,
    pub max_duration: Duration,
    pub quality: u8,
    // x264 crf for mp4 recordings; ignored by the gif sink
    pub video_crf: u8,
    // composite the live system cursor into each frame, mirroring the
    // capture.show_cursor toggle used by still captures
    pub show_cursor: bool,
    pub record_audio: bool,
    // decides the frame sink: gif spools raw frames to disk for a post-stop
    // encode, mp4 streams into a live ffmpeg child
    pub format: RecordingFormat,
    pub gif: crate::config::GifOptions,
    pub video_encoder: crate::config::VideoEncoder,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            fps: 15,
            max_duration: Duration::from_secs(30),
            quality: 80,
            video_crf: 23,
            show_cursor: false,
            record_audio: false,
            format: RecordingFormat::Gif,
            gif: Default::default(),
            video_encoder: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_settings_quality() {
        let settings = RecordingSettings {
            fps: 30,
            max_duration: Duration::from_secs(10),
            quality: 90,
            video_crf: 18,
            show_cursor: false,
            record_audio: false,
            format: RecordingFormat::Mp4,
            gif: Default::default(),
            video_encoder: Default::default(),
        };
        assert_eq!(settings.quality, 90);
        assert_eq!(settings.fps, 30);
    }

    #[test]
    fn test_recording_state() {
        assert_eq!(RecordingState::Idle, RecordingState::Idle);
        assert_ne!(RecordingState::Idle, RecordingState::Recording);
    }
}
