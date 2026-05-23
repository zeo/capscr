const SCREENSHOT_WAV: &[u8] = include_bytes!("../assets/screenshot.wav");
const UPLOAD_WAV: &[u8] = include_bytes!("../assets/upload.wav");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Screenshot,
    Upload,
}

impl Sound {
    pub fn play_if_enabled(self, enabled: bool) {
        if enabled {
            self.play();
        }
    }

    pub fn play(self) {
        #[cfg(windows)]
        {
            engine::play(self);
        }
    }
}

/// Pre-spin the XAudio2 engine on startup so the first capture cue doesn't
/// pay the cold-start cost (engine thread spawn, mastering-voice handshake
/// with WASAPI, source-voice allocation). Subsequent cues just queue a
/// pre-loaded PCM buffer on an already-running source voice, so the audio
/// path is single-digit milliseconds from `submit` to "speaker moves".
pub fn warm_audio_subsystem() {
    #[cfg(windows)]
    {
        engine::ensure_initialized();
    }
}

#[cfg(windows)]
mod engine {
    use super::Sound;
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::Media::Audio::{
        AudioCategory_GameEffects, WAVEFORMATEX, WAVE_FORMAT_PCM,
    };
    use windows::Win32::Media::Audio::XAudio2::{
        IXAudio2, IXAudio2MasteringVoice, IXAudio2SourceVoice, IXAudio2VoiceCallback,
        XAudio2CreateWithVersionInfo, XAUDIO2_BUFFER, XAUDIO2_COMMIT_NOW,
        XAUDIO2_DEFAULT_PROCESSOR, XAUDIO2_END_OF_STREAM, XAUDIO2_VOICE_STATE,
    };

    // NTDDI_WIN8 = 0x06020000 — XAudio2 ships in Windows 8+, and the
    // 2.8 DLL (xaudio2_8.dll) is the one bound by the windows crate.
    const NTDDI_WIN8: u32 = 0x06020000;

    struct PreparedSound {
        pcm: &'static [u8],
        voice: IXAudio2SourceVoice,
    }

    struct Engine {
        _xaudio2: IXAudio2,
        _mastering_voice: IXAudio2MasteringVoice,
        screenshot: Mutex<PreparedSound>,
        upload: Mutex<PreparedSound>,
    }

    // SAFETY: IXAudio2 + voices are COM objects documented to be safe to
    // call from any thread once the engine is initialized. Voice methods
    // (Start, Stop, SubmitSourceBuffer, GetState) are explicitly thread-safe
    // per MSDN's XAudio2 threading model.
    unsafe impl Send for Engine {}
    unsafe impl Sync for Engine {}

    static ENGINE: OnceLock<Option<Engine>> = OnceLock::new();

    pub fn ensure_initialized() {
        ENGINE.get_or_init(|| {
            let started = std::time::Instant::now();
            let result = init();
            let elapsed_ms = started.elapsed().as_millis();
            match &result {
                Some(_) => tracing::info!("xaudio2 engine init ok in {elapsed_ms}ms"),
                None => tracing::warn!(
                    "xaudio2 engine init FAILED after {elapsed_ms}ms — sounds will be dropped"
                ),
            }
            result
        });
    }

    pub fn play(sound: Sound) {
        let start = std::time::Instant::now();
        ensure_initialized();
        let Some(Some(engine)) = ENGINE.get() else {
            tracing::warn!("xaudio2 engine not initialized; dropping sound {sound:?}");
            return;
        };
        let prepared_lock = match sound {
            Sound::Screenshot => &engine.screenshot,
            Sound::Upload => &engine.upload,
        };
        let prepared = match prepared_lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        // pessimistic: if a previous fire is still in flight, drop this
        // trigger rather than queueing. matches the prior behavior and
        // prevents back-to-back hotkey hammering from stuttering audio.
        let mut state = XAUDIO2_VOICE_STATE::default();
        unsafe { prepared.voice.GetState(&mut state, 0) };
        if state.BuffersQueued > 0 {
            return;
        }

        let buffer = XAUDIO2_BUFFER {
            Flags: XAUDIO2_END_OF_STREAM,
            AudioBytes: prepared.pcm.len() as u32,
            pAudioData: prepared.pcm.as_ptr(),
            PlayBegin: 0,
            PlayLength: 0,
            LoopBegin: 0,
            LoopLength: 0,
            LoopCount: 0,
            pContext: std::ptr::null_mut(),
        };

        if let Err(e) = unsafe { prepared.voice.SubmitSourceBuffer(&buffer, None) } {
            let elapsed_us = start.elapsed().as_micros();
            tracing::warn!("SubmitSourceBuffer failed for {sound:?} after {elapsed_us}us: {e}");
            return;
        }
        if let Err(e) = unsafe { prepared.voice.Start(0, XAUDIO2_COMMIT_NOW) } {
            tracing::warn!("SourceVoice::Start failed for {sound:?}: {e}");
            return;
        }
        let elapsed_us = start.elapsed().as_micros();
        tracing::info!("Sound::play({sound:?}) submitted in {elapsed_us}us");
    }

    fn init() -> Option<Engine> {
        let (fmt, screenshot_pcm) = parse_wav(super::SCREENSHOT_WAV)?;
        let (fmt2, upload_pcm) = parse_wav(super::UPLOAD_WAV)?;
        if fmt.nChannels != fmt2.nChannels
            || fmt.nSamplesPerSec != fmt2.nSamplesPerSec
            || fmt.wBitsPerSample != fmt2.wBitsPerSample
        {
            tracing::warn!(
                "screenshot.wav and upload.wav formats diverge — sound engine disabled"
            );
            return None;
        }

        unsafe {
            let mut xaudio2_opt: Option<IXAudio2> = None;
            if let Err(e) = XAudio2CreateWithVersionInfo(
                &mut xaudio2_opt,
                0,
                XAUDIO2_DEFAULT_PROCESSOR,
                NTDDI_WIN8,
            ) {
                tracing::warn!("XAudio2CreateWithVersionInfo failed: {e}");
                return None;
            }
            let xaudio2 = xaudio2_opt?;

            // mastering voice: input-channels=0 and input-rate=0 mean
            // "match the default device", letting XAudio2 pick whatever the
            // OS-selected output endpoint runs at.
            let mut mastering_opt: Option<IXAudio2MasteringVoice> = None;
            if let Err(e) = xaudio2.CreateMasteringVoice(
                &mut mastering_opt,
                0,
                0,
                0,
                windows::core::PCWSTR::null(),
                None,
                AudioCategory_GameEffects,
            ) {
                tracing::warn!("CreateMasteringVoice failed: {e}");
                return None;
            }
            let mastering_voice = mastering_opt?;

            let screenshot_voice = create_source_voice(&xaudio2, &fmt)?;
            let upload_voice = create_source_voice(&xaudio2, &fmt)?;

            if let Err(e) = screenshot_voice.Start(0, XAUDIO2_COMMIT_NOW) {
                tracing::warn!("screenshot SourceVoice::Start failed: {e}");
                return None;
            }
            if let Err(e) = upload_voice.Start(0, XAUDIO2_COMMIT_NOW) {
                tracing::warn!("upload SourceVoice::Start failed: {e}");
                return None;
            }

            Some(Engine {
                _xaudio2: xaudio2,
                _mastering_voice: mastering_voice,
                screenshot: Mutex::new(PreparedSound {
                    pcm: screenshot_pcm,
                    voice: screenshot_voice,
                }),
                upload: Mutex::new(PreparedSound {
                    pcm: upload_pcm,
                    voice: upload_voice,
                }),
            })
        }
    }

    unsafe fn create_source_voice(
        xaudio2: &IXAudio2,
        fmt: &WAVEFORMATEX,
    ) -> Option<IXAudio2SourceVoice> {
        let mut voice_opt: Option<IXAudio2SourceVoice> = None;
        if let Err(e) = xaudio2.CreateSourceVoice(
            &mut voice_opt,
            fmt as *const WAVEFORMATEX,
            0,
            1.0,
            None::<&IXAudio2VoiceCallback>,
            None,
            None,
        ) {
            tracing::warn!("CreateSourceVoice failed: {e}");
            return None;
        }
        voice_opt
    }

    fn parse_wav(bytes: &'static [u8]) -> Option<(WAVEFORMATEX, &'static [u8])> {
        if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return None;
        }
        let mut fmt: Option<WAVEFORMATEX> = None;
        let mut pcm: Option<&'static [u8]> = None;
        let mut cursor = 12usize;
        while cursor + 8 <= bytes.len() {
            let id = &bytes[cursor..cursor + 4];
            let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().ok()?) as usize;
            let payload = &bytes[cursor + 8..(cursor + 8 + size).min(bytes.len())];
            match id {
                b"fmt " if payload.len() >= 16 => {
                    let format_tag = u16::from_le_bytes(payload[0..2].try_into().ok()?);
                    if format_tag != WAVE_FORMAT_PCM as u16 {
                        return None;
                    }
                    fmt = Some(WAVEFORMATEX {
                        wFormatTag: format_tag,
                        nChannels: u16::from_le_bytes(payload[2..4].try_into().ok()?),
                        nSamplesPerSec: u32::from_le_bytes(payload[4..8].try_into().ok()?),
                        nAvgBytesPerSec: u32::from_le_bytes(payload[8..12].try_into().ok()?),
                        nBlockAlign: u16::from_le_bytes(payload[12..14].try_into().ok()?),
                        wBitsPerSample: u16::from_le_bytes(payload[14..16].try_into().ok()?),
                        cbSize: 0,
                    });
                }
                b"data" => {
                    pcm = Some(payload);
                }
                _ => {}
            }
            cursor += 8 + size + (size & 1);
        }
        match (fmt, pcm) {
            (Some(f), Some(p)) => Some((f, p)),
            _ => None,
        }
    }
}
