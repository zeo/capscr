// pipewire consumer for a portal screencast node. a dedicated thread runs
// the pipewire main loop and copies each arriving frame into a latest-frame
// slot; the recording loop pulls from that slot at its own cadence, so
// memory stays flat no matter how fast the compositor delivers. frames are
// damage-driven on gnome — a static screen delivers nothing — so grabs fall
// back to the previous frame after a bounded wait and the recorder's dedup
// fingerprint absorbs the repeats.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use image::RgbaImage;

use super::portal_screencast::{open_monitor_session, ScreenCastSession, StreamInfo};

// how long the first frame may take (session spin-up, compositor handshake)
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
// how long a grab waits for a fresh frame before re-serving the last one
const FRESH_FRAME_WAIT: Duration = Duration::from_millis(500);

type FrameSlot = Arc<Mutex<Option<(RgbaImage, Instant)>>>;

pub struct PipeWireFrameStream {
    latest: FrameSlot,
    stream: StreamInfo,
    stop_tx: Option<pipewire::channel::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    last_consumed: Option<Instant>,
    // keeps the portal session (and with it the node) alive
    _session: ScreenCastSession,
}

impl PipeWireFrameStream {
    pub fn open(embed_cursor: bool) -> Result<Self> {
        let session = open_monitor_session(embed_cursor)?;
        let stream = session.stream.clone();
        let fd = session.pipewire_fd.try_clone()?;
        let latest: FrameSlot = Arc::new(Mutex::new(None));
        let (stop_tx, stop_rx) = pipewire::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let slot = latest.clone();
        let node_id = stream.node_id;
        let worker = std::thread::Builder::new()
            .name("capscr-screencast".into())
            .spawn(move || run_stream(fd, node_id, slot, stop_rx, ready_tx))?;
        match ready_rx.recv_timeout(FIRST_FRAME_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => bail!("pipewire stream produced no setup result in time"),
        }
        Ok(Self {
            latest,
            stream,
            stop_tx: Some(stop_tx),
            worker: Some(worker),
            last_consumed: None,
            _session: session,
        })
    }

    // pull a frame and crop the logical-coordinate rect out of it using the
    // stream's position/size mapping. a region outside the streamed monitor
    // yields an error and the recording loop's generic path takes over.
    pub fn grab(&mut self, x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
        let deadline = Instant::now()
            + match self.last_consumed {
                Some(_) => FRESH_FRAME_WAIT,
                None => FIRST_FRAME_TIMEOUT,
            };
        let (frame, stamp) = loop {
            let current = self.latest.lock().unwrap().clone();
            match current {
                Some((frame, stamp))
                    if Some(stamp) != self.last_consumed || Instant::now() >= deadline =>
                {
                    break (frame, stamp);
                }
                Some(_) => {}
                None if Instant::now() >= deadline => {
                    bail!("screencast delivered no frames")
                }
                None => {}
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        self.last_consumed = Some(stamp);

        let (size_w, size_h) = self.stream.size;
        if size_w <= 0 || size_h <= 0 {
            bail!("portal reported no stream geometry");
        }
        let scale_x = frame.width() as f64 / size_w as f64;
        let scale_y = frame.height() as f64 / size_h as f64;
        let (pos_x, pos_y) = self.stream.position;
        let src_x = ((x - pos_x).max(0) as f64 * scale_x).round() as u32;
        let src_y = ((y - pos_y).max(0) as f64 * scale_y).round() as u32;
        let src_w = (width as f64 * scale_x).round() as u32;
        let src_h = (height as f64 * scale_y).round() as u32;
        let src_w = src_w.min(frame.width().saturating_sub(src_x));
        let src_h = src_h.min(frame.height().saturating_sub(src_y));
        if src_w == 0 || src_h == 0 {
            bail!("capture region lies outside the shared monitor");
        }
        Ok(image::imageops::crop_imm(&frame, src_x, src_y, src_w, src_h).to_image())
    }
}

impl Drop for PipeWireFrameStream {
    fn drop(&mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// one 10-bit PQ frame off a fresh portal screencast, for HDR stills. mutter
// (gnome 50+) only offers the 210LE formats while the shared monitor is in
// HDR mode, so a session that can't negotiate them errors out and the caller
// takes the SDR path; the packed words go out as-is — the same R10G10B10A2
// layout HdrFormat::Hdr10 means everywhere else in the pipeline
pub struct Hdr10Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub position: (i32, i32),
    pub logical_size: (i32, i32),
}

type Hdr10Slot = Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>;

pub fn grab_hdr10_frame(embed_cursor: bool) -> Result<Hdr10Frame> {
    let session = open_monitor_session(embed_cursor)?;
    let stream = session.stream.clone();
    let fd = session.pipewire_fd.try_clone()?;
    let slot: Hdr10Slot = Arc::new(Mutex::new(None));
    let (stop_tx, stop_rx) = pipewire::channel::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let hdr_slot = slot.clone();
    let node_id = stream.node_id;
    let worker = std::thread::Builder::new()
        .name("capscr-hdr-still".into())
        .spawn(move || run_hdr_stream(fd, node_id, hdr_slot, stop_rx, ready_tx))?;
    let finish = |stop_tx: pipewire::channel::Sender<()>, worker: std::thread::JoinHandle<()>| {
        let _ = stop_tx.send(());
        let _ = worker.join();
    };
    match ready_rx.recv_timeout(FIRST_FRAME_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            finish(stop_tx, worker);
            return Err(e);
        }
        Err(_) => {
            finish(stop_tx, worker);
            bail!("hdr screencast produced no setup result in time");
        }
    }
    let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
    let frame = loop {
        if let Some(frame) = slot.lock().unwrap().take() {
            break frame;
        }
        if Instant::now() >= deadline {
            finish(stop_tx, worker);
            bail!("hdr screencast delivered no frames");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    finish(stop_tx, worker);
    let (data, width, height) = frame;
    Ok(Hdr10Frame {
        data,
        width,
        height,
        position: stream.position,
        logical_size: stream.size,
    })
}

// worker for the one-shot HDR grab: offer only the packed 10-bit formats and
// verify the negotiated transfer is PQ before trusting a frame
fn run_hdr_stream(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    slot: Hdr10Slot,
    stop_rx: pipewire::channel::Receiver<()>,
    ready_tx: mpsc::Sender<Result<()>>,
) {
    use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::{self, serialize::PodSerializer, Pod};
    use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
    use pipewire::stream::{StreamFlags, StreamRc};

    // spa_video_transfer_function / spa_video_color_primaries codepoints
    const TRANSFER_UNKNOWN: u32 = 0;
    const TRANSFER_SMPTE2084: u32 = 14;

    let setup = || -> Result<(pipewire::main_loop::MainLoopRc, StreamRc, Box<dyn std::any::Any>)> {
        pipewire::init();
        let main_loop = pipewire::main_loop::MainLoopRc::new(None)?;
        let context = pipewire::context::ContextRc::new(&main_loop, None)?;
        let core = context.connect_fd_rc(fd, None)?;

        #[derive(Clone, Default)]
        struct StreamData {
            format: VideoInfoRaw,
        }

        let stream = StreamRc::new(
            core,
            "capscr-hdr",
            pipewire::properties::properties! {
                *pipewire::keys::MEDIA_TYPE => "Video",
                *pipewire::keys::MEDIA_CATEGORY => "Capture",
                *pipewire::keys::MEDIA_ROLE => "Screen",
            },
        )?;

        let listener = stream
            .add_local_listener_with_user_data(StreamData::default())
            .param_changed(|_, data, id, param| {
                let Some(param) = param else { return };
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let parsed = pipewire::spa::param::format_utils::parse_format(param);
                if let Ok((MediaType::Video, MediaSubtype::Raw)) = parsed {
                    if let Err(e) = data.format.parse(param) {
                        tracing::warn!("hdr screencast format parse failed: {e:?}");
                    }
                    tracing::info!(
                        "hdr screencast format: {:?} transfer={} primaries={}",
                        data.format.format(),
                        data.format.transfer_function(),
                        data.format.color_primaries(),
                    );
                }
            })
            .process(move |stream, data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let format = data.format.format();
                let opaque = format == VideoFormat::xBGR_210LE;
                if format != VideoFormat::xBGR_210LE && format != VideoFormat::ABGR_210LE {
                    tracing::warn!("hdr screencast delivered non-10-bit format {format:?}");
                    return;
                }
                // an SDR-colorimetry 10-bit stream is not an HDR source
                let transfer = data.format.transfer_function();
                if transfer != TRANSFER_SMPTE2084 && transfer != TRANSFER_UNKNOWN {
                    tracing::warn!("hdr screencast transfer {transfer} is not PQ; dropping");
                    return;
                }
                let size = data.format.size();
                let (width, height) = (size.width, size.height);
                if width == 0 || height == 0 {
                    return;
                }
                let stride = datas[0].chunk().stride().unsigned_abs() as usize;
                let Some(raw) = datas[0].data() else { return };
                let row_bytes = width as usize * 4;
                let stride = if stride == 0 { row_bytes } else { stride };
                if raw.len() < stride * (height as usize - 1) + row_bytes {
                    return;
                }
                let mut packed = vec![0u8; row_bytes * height as usize];
                for (row_index, dst_row) in packed.chunks_exact_mut(row_bytes).enumerate() {
                    dst_row
                        .copy_from_slice(&raw[row_index * stride..row_index * stride + row_bytes]);
                    if opaque {
                        // x-formats carry undefined alpha bits; force opaque
                        for px in dst_row.chunks_exact_mut(4) {
                            px[3] |= 0xC0;
                        }
                    }
                }
                *slot.lock().unwrap() = Some((packed, width, height));
            })
            .register()?;

        let format_object = pod::object!(
            SpaTypes::ObjectParamFormat,
            ParamType::EnumFormat,
            pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
            pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
            pod::property!(
                FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                VideoFormat::xBGR_210LE,
                VideoFormat::ABGR_210LE,
            ),
            pod::property!(
                FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                Rectangle {
                    width: 1920,
                    height: 1080
                },
                Rectangle {
                    width: 1,
                    height: 1
                },
                Rectangle {
                    width: 16384,
                    height: 16384
                }
            ),
            pod::property!(
                FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                Fraction { num: 60, denom: 1 },
                Fraction { num: 0, denom: 1 },
                Fraction {
                    num: 1000,
                    denom: 1
                }
            ),
        );
        let serialized = PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pod::Value::Object(format_object),
        )
        .map_err(|e| anyhow!("pod serialization failed: {e:?}"))?
        .0
        .into_inner();
        let mut params = [Pod::from_bytes(&serialized)
            .ok_or_else(|| anyhow!("pod construction failed"))?];

        stream.connect(
            Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;

        Ok((main_loop, stream, Box::new(listener)))
    };

    match setup() {
        Ok((main_loop, _stream, _listener)) => {
            let _ = ready_tx.send(Ok(()));
            let loop_handle = main_loop.clone();
            let _stop_attached = stop_rx.attach(main_loop.loop_(), move |_| {
                loop_handle.quit();
            });
            main_loop.run();
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    }
}

// the pipewire side, entirely on its own thread: connect over the portal's
// fd, negotiate a raw video format, and copy every buffer into the slot.
// format incantations mirror xcap's screencast recorder, which runs against
// this same crate version in production.
fn run_stream(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    slot: FrameSlot,
    stop_rx: pipewire::channel::Receiver<()>,
    ready_tx: mpsc::Sender<Result<()>>,
) {
    use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::{self, serialize::PodSerializer, Pod};
    use pipewire::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
    use pipewire::stream::{StreamFlags, StreamRc};

    let setup = || -> Result<(pipewire::main_loop::MainLoopRc, StreamRc, Box<dyn std::any::Any>)> {
        pipewire::init();
        let main_loop = pipewire::main_loop::MainLoopRc::new(None)?;
        let context = pipewire::context::ContextRc::new(&main_loop, None)?;
        let core = context.connect_fd_rc(fd, None)?;

        #[derive(Clone, Default)]
        struct StreamData {
            format: VideoInfoRaw,
        }

        let stream = StreamRc::new(
            core,
            "capscr",
            pipewire::properties::properties! {
                *pipewire::keys::MEDIA_TYPE => "Video",
                *pipewire::keys::MEDIA_CATEGORY => "Capture",
                *pipewire::keys::MEDIA_ROLE => "Screen",
            },
        )?;

        let listener = stream
            .add_local_listener_with_user_data(StreamData::default())
            .param_changed(|_, data, id, param| {
                let Some(param) = param else { return };
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let parsed = pipewire::spa::param::format_utils::parse_format(param);
                if let Ok((MediaType::Video, MediaSubtype::Raw)) = parsed {
                    if let Err(e) = data.format.parse(param) {
                        tracing::warn!("screencast format parse failed: {e:?}");
                    }
                }
            })
            .process(move |stream, data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let size = data.format.size();
                let (width, height) = (size.width, size.height);
                if width == 0 || height == 0 {
                    return;
                }
                let stride = datas[0].chunk().stride().unsigned_abs() as usize;
                let Some(raw) = datas[0].data() else { return };
                let row_bytes = width as usize * 4;
                let stride = if stride == 0 { row_bytes } else { stride };
                if raw.len() < stride * (height as usize - 1) + row_bytes {
                    return;
                }
                let mut rgba = vec![0u8; row_bytes * height as usize];
                let format = data.format.format();
                for (row_index, dst_row) in rgba.chunks_exact_mut(row_bytes).enumerate() {
                    let src_row = &raw[row_index * stride..row_index * stride + row_bytes];
                    match format {
                        VideoFormat::RGBA | VideoFormat::RGBx => {
                            dst_row.copy_from_slice(src_row);
                        }
                        VideoFormat::BGRA | VideoFormat::BGRx => {
                            for (dst, src) in
                                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4))
                            {
                                dst[0] = src[2];
                                dst[1] = src[1];
                                dst[2] = src[0];
                                dst[3] = src[3];
                            }
                        }
                        other => {
                            tracing::warn!("unsupported screencast format {other:?}");
                            return;
                        }
                    }
                }
                // x-formats carry undefined alpha; force opaque
                if matches!(format, VideoFormat::RGBx | VideoFormat::BGRx) {
                    for px in rgba.chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                }
                if let Some(frame) = RgbaImage::from_raw(width, height, rgba) {
                    *slot.lock().unwrap() = Some((frame, Instant::now()));
                }
            })
            .register()?;

        let format_object = pod::object!(
            SpaTypes::ObjectParamFormat,
            ParamType::EnumFormat,
            pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
            pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
            pod::property!(
                FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                VideoFormat::RGBA,
                VideoFormat::RGBx,
                VideoFormat::BGRx,
                VideoFormat::BGRA,
            ),
            pod::property!(
                FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                Rectangle {
                    width: 1920,
                    height: 1080
                },
                Rectangle {
                    width: 1,
                    height: 1
                },
                Rectangle {
                    width: 16384,
                    height: 16384
                }
            ),
            pod::property!(
                FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                Fraction { num: 60, denom: 1 },
                Fraction { num: 0, denom: 1 },
                Fraction {
                    num: 1000,
                    denom: 1
                }
            ),
        );
        let serialized = PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pod::Value::Object(format_object),
        )
        .map_err(|e| anyhow!("pod serialization failed: {e:?}"))?
        .0
        .into_inner();
        let mut params = [Pod::from_bytes(&serialized)
            .ok_or_else(|| anyhow!("pod construction failed"))?];

        stream.connect(
            Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;

        Ok((main_loop, stream, Box::new(listener)))
    };

    match setup() {
        Ok((main_loop, _stream, _listener)) => {
            let _ = ready_tx.send(Ok(()));
            let loop_handle = main_loop.clone();
            let _stop_attached = stop_rx.attach(main_loop.loop_(), move |_| {
                loop_handle.quit();
            });
            main_loop.run();
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    }
}

#[cfg(test)]
mod tests {
    // needs a live portal with a screencast backend and a user (or rig) to
    // approve the source picker on first run; opt in via env
    #[test]
    fn screencast_delivers_frames() {
        if std::env::var("CAPSCR_TEST_SCREENCAST").is_err() {
            return;
        }
        let mut stream = super::PipeWireFrameStream::open(false).expect("screencast session");
        let img = stream.grab(0, 0, 64, 64).expect("screencast frame");
        eprintln!("screencast returned {}x{}", img.width(), img.height());
        assert!(img.width() > 0 && img.height() > 0);
    }
}
