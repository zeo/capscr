mod gif_encoder;

pub use gif_encoder::GifRecorder;

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
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            fps: 15,
            max_duration: Duration::from_secs(30),
            quality: 80,
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
