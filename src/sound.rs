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
            let ok = PlaySoundW(
                PCWSTR(data.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT | SND_NOSTOP,
            );
            if !ok.as_bool() {
                tracing::warn!("PlaySoundW failed for {:?}", self);
            }
        }
    }

    #[cfg(not(windows))]
    pub fn play(self) {
        // audio cues are windows-only at 0.3.1; non-windows builds are silent.
    }
}

/// Warm up the Windows audio subsystem so the first real `Sound::play` cue
/// doesn't have a 200-500 ms startup lag (the user reported the first
/// screenshot beep being noticeably late). PlaySoundW with SND_PURGE and a
/// null pointer kicks waveOut initialisation without actually emitting
/// audio. Cheap, idempotent, no-op on non-Windows.
pub fn warm_audio_subsystem() {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_NODEFAULT, SND_PURGE};
        unsafe {
            let _ = PlaySoundW(PCWSTR::null(), None, SND_PURGE | SND_NODEFAULT);
        }
    }
}
