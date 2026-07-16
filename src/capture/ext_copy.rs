// ext-image-copy-capture-v1 backend: the standards-track successor to wlr
// screencopy, implemented by kwin >= 6.6 (behind a desktop-file interface
// grant), wlroots >= 0.18, hyprland, cosmic, and mutter >= 49.2. sessions
// are persistent and buffers are client-owned shm, which sidesteps the
// nvidia gbm/egl black-frame class entirely and makes repeat grabs cheap
// enough for the recording loop.
//
// v1 captures whole sources (one output per session); region grabs crop
// client-side. dmabuf negotiation is deliberately not implemented: shm is
// universal, driver-proof, and fast enough for stills and region recording.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use image::RgbaImage;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1, ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1,
};

use super::{apply_output_transform, compose_region, MonitorInfo, Rectangle};

const NEGOTIATE_TIMEOUT: Duration = Duration::from_secs(2);
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct OutputInfo {
    name: Option<String>,
}

// constraints the compositor advertises per session, refreshed whenever the
// source changes (mode switch, scale change); a grab rebuilds its buffer when
// these no longer match it
#[derive(Default, Clone)]
struct SessionConstraints {
    width: u32,
    height: u32,
    shm_formats: Vec<wl_shm::Format>,
    done: bool,
    stopped: bool,
}

#[derive(Clone)]
enum FrameStatus {
    Pending,
    Ready,
    Failed(ext_image_copy_capture_frame_v1::FailureReason),
}

struct FrameState {
    status: FrameStatus,
    transform: wl_output::Transform,
}

struct State {
    outputs: Vec<(wl_output::WlOutput, OutputInfo)>,
    sessions: HashMap<ObjectId, SessionConstraints>,
    frames: HashMap<ObjectId, FrameState>,
}

struct ShmBuffer {
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    pool_size: usize,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct OutputSession {
    session: ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
    source: ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    include_cursor: bool,
    buffer: Option<ShmBuffer>,
}

impl Drop for OutputSession {
    fn drop(&mut self) {
        self.session.destroy();
        self.source.destroy();
    }
}

pub struct ExtCopySession {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    shm: wl_shm::WlShm,
    copy_manager: ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    source_manager:
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    // one live session per output name; recreated when the cursor flag flips
    // or the compositor stops the session
    sessions: HashMap<String, OutputSession>,
}

impl ExtCopySession {
    pub fn new() -> Result<Self> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let qh = queue.handle();
        let copy_manager = globals
            .bind::<ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1, _, _>(
                &qh,
                1..=1,
                (),
            )
            .map_err(|_| anyhow!("compositor doesn't expose ext_image_copy_capture_manager_v1"))?;
        let source_manager = globals
            .bind::<ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1, _, _>(
                &qh,
                1..=1,
                (),
            )
            .map_err(|_| {
                anyhow!("compositor doesn't expose ext_output_image_capture_source_manager_v1")
            })?;
        let shm = globals
            .bind::<wl_shm::WlShm, _, _>(&qh, 1..=1, ())
            .map_err(|_| anyhow!("compositor doesn't expose wl_shm"))?;

        let mut state = State {
            outputs: bind_outputs(&globals, &qh),
            sessions: HashMap::new(),
            frames: HashMap::new(),
        };
        // a couple of roundtrips so every output reports its name
        for _ in 0..3 {
            queue.roundtrip(&mut state)?;
            if state.outputs.iter().all(|(_, info)| info.name.is_some()) {
                break;
            }
        }
        Ok(Self {
            connection,
            queue,
            state,
            shm,
            copy_manager,
            source_manager,
            sessions: HashMap::new(),
        })
    }

    // native-resolution grab of one output, rotated into logical orientation
    pub fn grab_output(&mut self, output_name: &str, include_cursor: bool) -> Result<RgbaImage> {
        self.ensure_session(output_name, include_cursor)?;
        match self.capture_frame(output_name) {
            Ok(img) => Ok(img),
            // constraint changes (mode/scale switch) and compositor-stopped
            // sessions get one clean rebuild before giving up
            Err(retry) if retry.is::<RetryableCapture>() => {
                self.sessions.remove(output_name);
                self.ensure_session(output_name, include_cursor)?;
                self.capture_frame(output_name)
            }
            Err(e) => Err(e),
        }
    }

    // the shm formats the compositor offers for one output, for the
    // hdr-readiness diagnostic (a >8-bit entry here is the signal that hdr
    // capture became reachable)
    pub fn offered_formats(&mut self, output_name: &str) -> Result<Vec<wl_shm::Format>> {
        self.ensure_session(output_name, false)?;
        let session_id = self.sessions[output_name].session.id();
        Ok(self
            .state
            .sessions
            .get(&session_id)
            .map(|constraints| constraints.shm_formats.clone())
            .unwrap_or_default())
    }

    // logical-coordinate region grab composed from whole-output captures
    pub fn grab_area(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        include_cursor: bool,
    ) -> Result<RgbaImage> {
        let region = Rectangle {
            x,
            y,
            width,
            height,
        };
        compose_region(region, |monitor: &MonitorInfo| {
            self.grab_output(&monitor.name, include_cursor)
        })
    }

    fn wl_output_by_name(&self, name: &str) -> Result<wl_output::WlOutput> {
        self.state
            .outputs
            .iter()
            .find(|(_, info)| info.name.as_deref() == Some(name))
            .map(|(output, _)| output.clone())
            .ok_or_else(|| anyhow!("wayland output {name} not found"))
    }

    fn ensure_session(&mut self, output_name: &str, include_cursor: bool) -> Result<()> {
        if let Some(existing) = self.sessions.get(output_name) {
            let stopped = self
                .state
                .sessions
                .get(&existing.session.id())
                .map(|c| c.stopped)
                .unwrap_or(true);
            if existing.include_cursor == include_cursor && !stopped {
                return Ok(());
            }
            let stale = self.sessions.remove(output_name);
            if let Some(stale) = &stale {
                self.state.sessions.remove(&stale.session.id());
            }
        }
        let output = self.wl_output_by_name(output_name)?;
        let qh = self.queue.handle();
        let source = self.source_manager.create_source(&output, &qh, ());
        let options = if include_cursor {
            ext_image_copy_capture_manager_v1::Options::PaintCursors
        } else {
            ext_image_copy_capture_manager_v1::Options::empty()
        };
        let session = self.copy_manager.create_session(&source, options, &qh, ());
        self.state
            .sessions
            .insert(session.id(), SessionConstraints::default());
        let session_id = session.id();
        self.sessions.insert(
            output_name.to_string(),
            OutputSession {
                session,
                source,
                include_cursor,
                buffer: None,
            },
        );
        // wait for the first constraints batch so a buffer can be allocated
        self.dispatch_until(NEGOTIATE_TIMEOUT, |state| {
            state
                .sessions
                .get(&session_id)
                .map(|c| c.done || c.stopped)
                .unwrap_or(false)
        })
        .map_err(|e| anyhow!("ext-image-copy session negotiation on {output_name}: {e:#}"))
    }

    fn capture_frame(&mut self, output_name: &str) -> Result<RgbaImage> {
        let session_id = self
            .sessions
            .get(output_name)
            .map(|s| s.session.id())
            .ok_or_else(|| anyhow!("no session for output {output_name}"))?;
        let constraints = self
            .state
            .sessions
            .get(&session_id)
            .cloned()
            .ok_or_else(|| anyhow!("session state missing for {output_name}"))?;
        if constraints.stopped {
            bail!(RetryableCapture("session stopped by compositor"));
        }
        if constraints.width == 0 || constraints.height == 0 {
            bail!("compositor advertised a zero-sized capture buffer");
        }
        let format = pick_format(&constraints.shm_formats).ok_or_else(|| {
            anyhow!(
                "no supported shm format among {:?}",
                constraints.shm_formats
            )
        })?;

        // (re)allocate the buffer when constraints moved under us
        let needs_buffer = self.sessions[output_name]
            .buffer
            .as_ref()
            .map(|b| {
                b.width != constraints.width || b.height != constraints.height || b.format != format
            })
            .unwrap_or(true);
        if needs_buffer {
            let buffer = self.create_shm_buffer(constraints.width, constraints.height, format)?;
            self.sessions.get_mut(output_name).unwrap().buffer = Some(buffer);
        }

        let qh = self.queue.handle();
        let output_session = self.sessions.get(output_name).unwrap();
        let frame = output_session.session.create_frame(&qh, ());
        let frame_id = frame.id();
        self.state.frames.insert(
            frame_id.clone(),
            FrameState {
                status: FrameStatus::Pending,
                transform: wl_output::Transform::Normal,
            },
        );
        frame.attach_buffer(&output_session.buffer.as_ref().unwrap().buffer);
        frame.capture();

        let wait = self.dispatch_until(FRAME_TIMEOUT, |state| {
            state
                .frames
                .get(&frame_id)
                .map(|f| !matches!(f.status, FrameStatus::Pending))
                .unwrap_or(false)
        });
        let frame_state = self.state.frames.remove(&frame_id);
        frame.destroy();
        wait.map_err(|e| anyhow!("ext-image-copy frame on {output_name}: {e:#}"))?;
        let frame_state = frame_state.ok_or_else(|| anyhow!("frame state vanished"))?;
        match frame_state.status {
            FrameStatus::Ready => {}
            FrameStatus::Failed(
                ext_image_copy_capture_frame_v1::FailureReason::BufferConstraints,
            ) => bail!(RetryableCapture("buffer constraints changed")),
            FrameStatus::Failed(ext_image_copy_capture_frame_v1::FailureReason::Stopped) => {
                bail!(RetryableCapture("session stopped mid-frame"))
            }
            FrameStatus::Failed(reason) => bail!("capture failed: {reason:?}"),
            FrameStatus::Pending => unreachable!(),
        }

        let buffer = self.sessions[output_name].buffer.as_ref().unwrap();
        let img = read_buffer(buffer)?;
        Ok(apply_output_transform(img, frame_state.transform))
    }

    fn create_shm_buffer(
        &mut self,
        width: u32,
        height: u32,
        format: wl_shm::Format,
    ) -> Result<ShmBuffer> {
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| anyhow!("buffer width overflow"))?;
        let size = stride as usize * height as usize;
        let fd = unsafe { libc::memfd_create(c"capscr-ext-copy".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(size as u64)?;
        let qh = self.queue.handle();
        let pool = self
            .shm
            .create_pool(file.as_fd(), size as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            &qh,
            (),
        );
        Ok(ShmBuffer {
            file,
            pool,
            buffer,
            width,
            height,
            stride,
            format,
            pool_size: size,
        })
    }

    // pump the queue until cond holds or the deadline passes. wayland has no
    // deadline dispatch of its own, so this polls the connection fd with the
    // remaining time and drains whatever arrived
    fn dispatch_until(
        &mut self,
        timeout: Duration,
        cond: impl Fn(&State) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.queue.dispatch_pending(&mut self.state)?;
            if cond(&self.state) {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("timed out after {}ms", timeout.as_millis());
            }
            self.connection.flush()?;
            if let Some(guard) = self.queue.prepare_read() {
                let mut pollfd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let remaining = deadline.duration_since(now).as_millis().min(1000) as i32;
                let ready = unsafe { libc::poll(&mut pollfd, 1, remaining.max(1)) };
                if ready > 0 {
                    let _ = guard.read();
                } else {
                    drop(guard);
                }
            }
        }
    }
}

// error marker for the one-shot session rebuild in grab_output
#[derive(Debug)]
struct RetryableCapture(&'static str);

impl std::fmt::Display for RetryableCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RetryableCapture {}

fn bind_outputs(
    globals: &GlobalList,
    qh: &QueueHandle<State>,
) -> Vec<(wl_output::WlOutput, OutputInfo)> {
    let registry = globals.registry();
    let mut outputs = Vec::new();
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == "wl_output" && global.version >= 4 {
                let output =
                    registry.bind::<wl_output::WlOutput, _, _>(global.name, 4, qh, ());
                outputs.push((output, OutputInfo::default()));
            }
        }
    });
    outputs
}

// every format here is 4 bytes per pixel; preference order is arbitrary
// beyond "alpha-carrying first" since conversion cost is identical
fn pick_format(offered: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    const PREFERRED: [wl_shm::Format; 4] = [
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xbgr8888,
    ];
    PREFERRED.into_iter().find(|f| offered.contains(f))
}

fn read_buffer(buffer: &ShmBuffer) -> Result<RgbaImage> {
    use std::os::unix::fs::FileExt;
    let mut raw = vec![0u8; buffer.pool_size];
    buffer.file.read_exact_at(&mut raw, 0)?;
    let mut rgba = vec![0u8; buffer.width as usize * buffer.height as usize * 4];
    // buffers are allocated with stride == width * 4, so the raw bytes are
    // already densely packed and convert in one pass
    match buffer.format {
        wl_shm::Format::Argb8888 => {
            super::par_convert(&raw, &mut rgba, |s| [s[2], s[1], s[0], s[3]])
        }
        wl_shm::Format::Xrgb8888 => {
            super::par_convert(&raw, &mut rgba, |s| [s[2], s[1], s[0], 255])
        }
        wl_shm::Format::Abgr8888 => {
            super::par_convert(&raw, &mut rgba, |s| [s[0], s[1], s[2], s[3]])
        }
        wl_shm::Format::Xbgr8888 => {
            super::par_convert(&raw, &mut rgba, |s| [s[0], s[1], s[2], 255])
        }
        other => bail!("unhandled shm format {other:?}"),
    }
    RgbaImage::from_raw(buffer.width, buffer.height, rgba)
        .ok_or_else(|| anyhow!("shm buffer size mismatch"))
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some((_, info)) = state.outputs.iter_mut().find(|(o, _)| o == output) {
                info.name = Some(name);
            }
        }
    }
}

impl Dispatch<ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        session: &ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let Some(constraints) = state.sessions.get_mut(&session.id()) else {
            return;
        };
        match event {
            Event::BufferSize { width, height } => {
                // a fresh constraints batch replaces the previous one
                if constraints.done {
                    constraints.done = false;
                    constraints.shm_formats.clear();
                }
                constraints.width = width;
                constraints.height = height;
            }
            Event::ShmFormat {
                format: WEnum::Value(format),
            } => {
                if constraints.done {
                    constraints.done = false;
                    constraints.shm_formats.clear();
                }
                constraints.shm_formats.push(format);
            }
            Event::Done => constraints.done = true,
            Event::Stopped => constraints.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let Some(frame_state) = state.frames.get_mut(&frame.id()) else {
            return;
        };
        match event {
            Event::Transform {
                transform: WEnum::Value(transform),
            } => frame_state.transform = transform,
            Event::Ready => frame_state.status = FrameStatus::Ready,
            Event::Failed {
                reason: WEnum::Value(reason),
            } => frame_state.status = FrameStatus::Failed(reason),
            Event::Failed { .. } => {
                frame_state.status =
                    FrameStatus::Failed(ext_image_copy_capture_frame_v1::FailureReason::Unknown)
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore ext_image_capture_source_v1::ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(State: ignore ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(State: ignore ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1);

#[cfg(test)]
mod tests {
    // needs a compositor that advertises ext-image-copy-capture (a granted
    // kwin session or the sway rig); opt in via env like the portal test
    #[test]
    fn ext_copy_grabs_pixels() {
        if std::env::var("CAPSCR_TEST_EXT_COPY").is_err() {
            return;
        }
        let mut session = super::ExtCopySession::new().expect("ext-copy session");
        let monitors = crate::capture::list_monitors().expect("monitors");
        let monitor = monitors.first().expect("at least one output");
        let img = session
            .grab_output(&monitor.name, false)
            .expect("ext-copy grab");
        eprintln!("ext-copy returned {}x{}", img.width(), img.height());
        assert!(img.width() > 0 && img.height() > 0);
        assert!(!crate::capture::is_black_frame(&img), "frame is all black");
    }
}
