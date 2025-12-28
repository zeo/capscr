#[cfg(windows)]
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY};

const SCREENSHOT_WAV: &[u8] = include_bytes!("../assets/screenshot.wav");
const UPLOAD_WAV: &[u8] = include_bytes!("../assets/upload.wav");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Screenshot,
    Upload,
}

impl Sound {
    #[cfg(windows)]
    pub fn play(self) {
        let data = match self {
            Sound::Screenshot => SCREENSHOT_WAV,
            Sound::Upload => UPLOAD_WAV,
        };

        std::thread::spawn(move || unsafe {
            let _ = PlaySoundW(
                windows::core::PCWSTR(data.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_ASYNC,
            );
        });
    }

    #[cfg(not(windows))]
    pub fn play(self) {
        // Sound not supported on non-Windows platforms
    }
}
