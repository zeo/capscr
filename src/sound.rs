const SCREENSHOT_WAV: &[u8] = include_bytes!("../assets/screenshot.wav");
const UPLOAD_WAV: &[u8] = include_bytes!("../assets/upload.wav");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Screenshot,
    Upload,
}

impl Sound {
    fn data(self) -> &'static [u8] {
        match self {
            Sound::Screenshot => SCREENSHOT_WAV,
            Sound::Upload => UPLOAD_WAV,
        }
    }

    pub fn play_if_enabled(self, enabled: bool) {
        if enabled {
            self.play();
        }
    }

    #[cfg(windows)]
    pub fn play(self) {
        use windows::core::PCWSTR;
        use windows::Win32::Media::Audio::{
            PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT, SND_NOSTOP,
        };

        let data = self.data();
        // SND_MEMORY: pszsound is interpreted as a pointer to an in-memory WAVE image,
        // regardless of the PCWSTR type. SND_ASYNC returns immediately; SND_NODEFAULT
        // suppresses the system "ding" if the data is invalid; SND_NOSTOP avoids
        // cutting off a currently-playing capscr cue.
        unsafe {
            let _ = PlaySoundW(
                PCWSTR(data.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT | SND_NOSTOP,
            );
        }
    }

    #[cfg(not(windows))]
    pub fn play(self) {
        // audio cues are windows-only at 0.3.1; non-windows builds are silent.
    }
}
