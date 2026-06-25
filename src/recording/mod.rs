#![allow(dead_code)]

mod gif_encoder;

pub use gif_encoder::{GifRecorder, find_ffmpeg, is_ffmpeg_available};

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Processing,
}

#[derive(Debug, Clone)]
pub struct RecordingSettings {
    pub fps: u32,
    pub max_duration: Duration,
    pub quality: u8,
    // composite the live system cursor into each frame, mirroring the
    // capture.show_cursor toggle used by still captures
    pub show_cursor: bool,
    pub record_audio: bool,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            fps: 15,
            max_duration: Duration::from_secs(30),
            quality: 80,
            show_cursor: false,
            record_audio: false,
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
            show_cursor: false,
            record_audio: false,
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
