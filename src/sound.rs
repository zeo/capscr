use std::io::Cursor;

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

    pub fn play(self) {
        let data = self.data();
        std::thread::spawn(move || {
            let Ok((_stream, stream_handle)) = rodio::OutputStream::try_default() else {
                return;
            };

            let cursor = Cursor::new(data);
            let Ok(source) = rodio::Decoder::new(cursor) else {
                return;
            };

            let Ok(sink) = rodio::Sink::try_new(&stream_handle) else {
                return;
            };

            sink.append(source);
            sink.sleep_until_end();
        });
    }
}
