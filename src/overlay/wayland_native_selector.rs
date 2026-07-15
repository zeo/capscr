use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output::WlOutput, wl_pointer, wl_region, wl_seat,
    wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
};
use wayland_client::{delegate_noop, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, Layer},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity},
};

use crate::capture::Rectangle;

#[derive(Debug, Clone)]
pub struct NativeWindow {
    pub id: u32,
    pub handle: Option<String>,
    pub rect: Rectangle,
}

pub struct NativeOutput {
    pub output_name: String,
    pub image: Arc<RgbaImage>,
    pub rect: Rectangle,
    pub windows: Vec<NativeWindow>,
}

#[derive(Debug)]
pub enum NativeOutcome {
    Region(Rectangle),
    Window(NativeWindow),
    Monitor(Rectangle, String),
    Color(u8, u8, u8),
    Cancelled,
}

pub struct NativeSelector {
    shutdown: Sender<()>,
    outcome: Receiver<NativeOutcome>,
    thread: Option<JoinHandle<()>>,
}

impl NativeSelector {
    pub fn show(outputs: Vec<NativeOutput>) -> Result<Self> {
        let (ready_tx, ready_rx) = channel();
        let (outcome_tx, outcome_rx) = channel();
        let (shutdown_tx, shutdown_rx) = channel();
        // a failure after ready was acked leaves select() waiting on the
        // outcome channel; a cancelled outcome resolves it immediately
        let outcome_on_failure = outcome_tx.clone();
        let thread = std::thread::Builder::new()
            .name("capscr-wayland-selector".into())
            .spawn(move || {
                if let Err(error) = run(outputs, shutdown_rx, outcome_tx, &ready_tx) {
                    tracing::warn!("native wayland selector unavailable: {error:#}");
                    let _ = outcome_on_failure.send(NativeOutcome::Cancelled);
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                shutdown: shutdown_tx,
                outcome: outcome_rx,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = shutdown_tx.send(());
                let _ = thread.join();
                Err(anyhow!("native selector timed out before mapping"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                Err(anyhow!("native selector stopped before mapping"))
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<NativeOutcome> {
        self.outcome.recv_timeout(timeout).map_err(Into::into)
    }
}

impl Drop for NativeSelector {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Border {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    viewport: wp_viewport::WpViewport,
}

struct OutputSurface {
    name: String,
    rect: Rectangle,
    windows: Vec<NativeWindow>,
    image: Arc<RgbaImage>,
    surface: wl_surface::WlSurface,
    layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    borders: Vec<Border>,
    viewport: wp_viewport::WpViewport,
}

struct State {
    configured: HashMap<String, (u32, u32)>,
    output_globals: HashMap<String, u32>,
    outputs: Vec<OutputSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    current_output: Option<usize>,
    pointer_x: f64,
    pointer_y: f64,
    start: Option<(f64, f64)>,
    hovered: Option<NativeWindow>,
    shift: bool,
    alt: bool,
    cursor: Cursor,
    outcome: Sender<NativeOutcome>,
    done: bool,
}

struct Cursor {
    surface: wl_surface::WlSurface,
    _pool: wl_shm_pool::WlShmPool,
    _buffer: wl_buffer::WlBuffer,
    _file: File,
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(State: ignore wl_subsurface::WlSubsurface);
delegate_noop!(State: ignore wayland_client::protocol::wl_output::WlOutput);
delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: ignore wp_viewporter::WpViewporter);
delegate_noop!(State: ignore wp_viewport::WpViewport);

impl Dispatch<wayland_client::protocol::wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _output: &wayland_client::protocol::wl_output::WlOutput,
        event: wayland_client::protocol::wl_output::Event,
        global_name: &u32,
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_output::Event::Name { name } = event {
            state.output_globals.insert(name, *global_name);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, String> for State {
    fn event(
        state: &mut Self,
        surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        output_name: &String,
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                surface.ack_configure(serial);
                state.configured.insert(output_name.clone(), (width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.configured.remove(output_name);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _connection: &wayland_client::Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
            state.pointer = Some(seat.get_pointer(queue, ()));
        }
        if capabilities.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(queue, ()));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(key_state),
            ..
        } = event
        else {
            return;
        };
        let pressed = key_state == wl_keyboard::KeyState::Pressed;
        match key {
            42 | 54 => state.shift = pressed,
            56 | 100 => state.alt = pressed,
            _ => {}
        }
        if !pressed {
            return;
        }
        match key {
            1 => state.finish(NativeOutcome::Cancelled),
            28 | 57 => {
                if let Some((start_x, start_y)) = state.start {
                    state.finish(NativeOutcome::Region(selection_rectangle(
                        start_x,
                        start_y,
                        state.pointer_x,
                        state.pointer_y,
                        state.shift,
                    )));
                } else if let Some(index) = state.current_output {
                    let output = &state.outputs[index];
                    state.finish(NativeOutcome::Monitor(output.rect, output.name.clone()));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                serial,
            } => {
                pointer.set_cursor(serial, Some(&state.cursor.surface), 15, 15);
                state.current_output = state
                    .outputs
                    .iter()
                    .position(|output| output.surface.id() == surface.id());
                state.update_pointer(surface_x, surface_y);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => state.update_pointer(surface_x, surface_y),
            wl_pointer::Event::Leave { .. } => {
                state.current_output = None;
                if state.start.is_none() {
                    state.hovered = None;
                    state.paint_outline(None);
                }
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } => state.button(button, button_state),
            _ => {}
        }
    }
}

impl State {
    fn finish(&mut self, outcome: NativeOutcome) {
        if self.done {
            return;
        }
        tracing::debug!("native selector outcome: {outcome:?}");
        self.done = true;
        let _ = self.outcome.send(outcome);
    }

    fn update_pointer(&mut self, local_x: f64, local_y: f64) {
        let Some(index) = self.current_output else {
            return;
        };
        let output = &self.outputs[index];
        self.pointer_x = output.rect.x as f64 + local_x;
        self.pointer_y = output.rect.y as f64 + local_y;
        let outline = if let Some(start) = self.start {
            Some(selection_rectangle(
                start.0,
                start.1,
                self.pointer_x,
                self.pointer_y,
                self.shift,
            ))
        } else {
            self.hovered = output
                .windows
                .iter()
                .find(|window| {
                    self.pointer_x >= window.rect.x as f64
                        && self.pointer_x
                            < window.rect.x.saturating_add_unsigned(window.rect.width) as f64
                        && self.pointer_y >= window.rect.y as f64
                        && self.pointer_y
                            < window.rect.y.saturating_add_unsigned(window.rect.height) as f64
                })
                .cloned();
            self.hovered.as_ref().map(|window| window.rect)
        };
        self.paint_outline(outline);
    }

    fn button(&mut self, button: u32, state: wl_pointer::ButtonState) {
        if button == 0x111 && state == wl_pointer::ButtonState::Pressed {
            self.finish(NativeOutcome::Cancelled);
            return;
        }
        if button != 0x110 {
            return;
        }
        match state {
            wl_pointer::ButtonState::Pressed => {
                if self.alt {
                    if let Some((r, g, b)) = self.color_at_pointer() {
                        self.finish(NativeOutcome::Color(r, g, b));
                    }
                    return;
                }
                self.start = Some((self.pointer_x, self.pointer_y));
                self.paint_outline(Some(Rectangle::new(
                    self.pointer_x.round() as i32,
                    self.pointer_y.round() as i32,
                    1,
                    1,
                )));
            }
            wl_pointer::ButtonState::Released => {
                let Some((start_x, start_y)) = self.start.take() else {
                    return;
                };
                let raw_width = (start_x - self.pointer_x).abs();
                let raw_height = (start_y - self.pointer_y).abs();
                if raw_width <= 5.0 && raw_height <= 5.0 {
                    if let Some(window) = self.hovered.clone() {
                        self.finish(NativeOutcome::Window(window));
                    } else if let Some(index) = self.current_output {
                        let output = &self.outputs[index];
                        self.finish(NativeOutcome::Monitor(output.rect, output.name.clone()));
                    }
                } else {
                    self.finish(NativeOutcome::Region(selection_rectangle(
                        start_x,
                        start_y,
                        self.pointer_x,
                        self.pointer_y,
                        self.shift,
                    )));
                }
            }
            _ => {}
        }
    }

    fn paint_outline(&mut self, rect: Option<Rectangle>) {
        for output in &mut self.outputs {
            let clipped = rect.and_then(|rect| intersect(rect, output.rect));
            for (index, border) in output.borders.iter().enumerate() {
                let (x, y, width, height) = match clipped {
                    Some(rect) => {
                        let left = rect.x - output.rect.x;
                        let top = rect.y - output.rect.y;
                        match index {
                            0 => (left, top, rect.width, 1),
                            1 => (
                                left,
                                top + rect.height.saturating_sub(1) as i32,
                                rect.width,
                                1,
                            ),
                            2 => (left, top, 1, rect.height),
                            _ => (
                                left + rect.width.saturating_sub(1) as i32,
                                top,
                                1,
                                rect.height,
                            ),
                        }
                    }
                    None => (-2, -2, 1, 1),
                };
                border.subsurface.set_position(x, y);
                border.viewport.set_destination(width as i32, height as i32);
                border.surface.commit();
            }
            output.surface.commit();
        }
    }

    fn color_at_pointer(&self) -> Option<(u8, u8, u8)> {
        let output = self.outputs.get(self.current_output?)?;
        let scale_x = output.image.width() as f64 / output.rect.width as f64;
        let scale_y = output.image.height() as f64 / output.rect.height as f64;
        let x = ((self.pointer_x - output.rect.x as f64) * scale_x)
            .floor()
            .clamp(0.0, output.image.width().saturating_sub(1) as f64) as u32;
        let y = ((self.pointer_y - output.rect.y as f64) * scale_y)
            .floor()
            .clamp(0.0, output.image.height().saturating_sub(1) as f64) as u32;
        let pixel = output.image.get_pixel(x, y);
        Some((pixel[0], pixel[1], pixel[2]))
    }
}

fn selection_rectangle(
    start_x: f64,
    start_y: f64,
    pointer_x: f64,
    pointer_y: f64,
    snap: bool,
) -> Rectangle {
    let mut end_y = pointer_y;
    let width = (pointer_x - start_x).abs();
    let height = (pointer_y - start_y).abs();
    if snap && width > 0.0 && height > 0.0 {
        const RATIOS: [f64; 5] = [1.0, 16.0 / 9.0, 16.0 / 10.0, 4.0 / 3.0, 21.0 / 9.0];
        let ratio = width / height;
        let target = RATIOS
            .into_iter()
            .min_by(|a, b| (ratio - *a).abs().total_cmp(&(ratio - *b).abs()));
        end_y = start_y + (pointer_y - start_y).signum() * width / target.unwrap_or(1.0);
    }
    Rectangle::new(
        start_x.min(pointer_x).round() as i32,
        start_y.min(end_y).round() as i32,
        width.round().max(1.0) as u32,
        (end_y - start_y).abs().round().max(1.0) as u32,
    )
}

fn intersect(a: Rectangle, b: Rectangle) -> Option<Rectangle> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right =
        a.x.saturating_add_unsigned(a.width)
            .min(b.x.saturating_add_unsigned(b.width));
    let bottom =
        a.y.saturating_add_unsigned(a.height)
            .min(b.y.saturating_add_unsigned(b.height));
    (left < right && top < bottom)
        .then(|| Rectangle::new(left, top, (right - left) as u32, (bottom - top) as u32))
}

fn run(
    outputs: Vec<NativeOutput>,
    shutdown: Receiver<()>,
    outcome: Sender<NativeOutcome>,
    ready: &Sender<Result<()>>,
) -> Result<()> {
    let wayshot = libwayshot_xcap::WayshotConnection::new().context("connect to wayland")?;
    let mut event_queue = wayshot.conn.new_event_queue::<State>();
    let queue = event_queue.handle();
    let compositor = wayshot
        .globals
        .bind::<wl_compositor::WlCompositor, _, _>(&queue, 1..=6, ())
        .context("bind wl_compositor")?;
    let subcompositor = wayshot
        .globals
        .bind::<wl_subcompositor::WlSubcompositor, _, _>(&queue, 1..=1, ())
        .context("bind wl_subcompositor")?;
    let shm = wayshot
        .globals
        .bind::<wl_shm::WlShm, _, _>(&queue, 1..=1, ())
        .context("bind wl_shm")?;
    let layer_shell = wayshot
        .globals
        .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(&queue, 1..=5, ())
        .context("bind zwlr_layer_shell_v1")?;
    let viewporter = wayshot
        .globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&queue, 1..=1, ())
        .context("bind wp_viewporter")?;
    let _seat = wayshot
        .globals
        .bind::<wl_seat::WlSeat, _, _>(&queue, 1..=9, ())
        .context("bind wl_seat")?;
    let (white_pool, white_buffer, white_file) =
        solid_buffer(&shm, &queue, [255; 4]).context("allocate border buffer")?;
    let (clear_pool, clear_buffer, clear_file) =
        solid_buffer(&shm, &queue, [0; 4]).context("allocate clear buffer")?;
    let cursor = cursor(&compositor, &shm, &queue).context("build cursor surface")?;
    let empty_region = compositor.create_region(&queue, ());
    let mut state = State {
        configured: HashMap::new(),
        output_globals: HashMap::new(),
        outputs: Vec::with_capacity(outputs.len()),
        pointer: None,
        keyboard: None,
        current_output: None,
        pointer_x: 0.0,
        pointer_y: 0.0,
        start: None,
        hovered: None,
        shift: false,
        alt: false,
        cursor,
        outcome,
        done: false,
    };

    for global in wayshot
        .globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|global| global.interface == "wl_output")
    {
        let _: WlOutput = wayshot.globals.registry().bind(
            global.name,
            global.version.min(4),
            &queue,
            global.name,
        );
    }
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip after wl_output binds")?;

    for output in outputs {
        let output_index = wayshot
            .get_all_outputs()
            .iter()
            .position(|candidate| {
                let region = candidate.logical_region.inner;
                region.position.x == output.rect.x
                    && region.position.y == output.rect.y
                    && region.size.width == output.rect.width
                    && region.size.height == output.rect.height
            })
            .or_else(|| {
                wayshot
                    .get_all_outputs()
                    .iter()
                    .position(|candidate| candidate.name == output.output_name)
            })
            .ok_or_else(|| anyhow!("wayland output {} disappeared", output.output_name))?;
        let wayland_output = &wayshot.get_all_outputs()[output_index];
        tracing::debug!(
            "native selector frame {} at {},{} {}x{} mapped to wayland output {} at {},{} {}x{}",
            output.output_name,
            output.rect.x,
            output.rect.y,
            output.rect.width,
            output.rect.height,
            wayland_output.name,
            wayland_output.logical_region.inner.position.x,
            wayland_output.logical_region.inner.position.y,
            wayland_output.logical_region.inner.size.width,
            wayland_output.logical_region.inner.size.height,
        );
        let output_global = *state
            .output_globals
            .get(&output.output_name)
            .ok_or_else(|| anyhow!("wayland output global {} disappeared", output.output_name))?;
        let wl_output: WlOutput = wayshot
            .globals
            .registry()
            .bind(output_global, 4, &queue, ());
        let output_name = output.output_name.clone();
        state.outputs.push(
            map_output(
                &queue,
                &compositor,
                &subcompositor,
                &layer_shell,
                &viewporter,
                &wl_output,
                output,
                &white_buffer,
                &empty_region,
            )
            .with_context(|| format!("map selector surface on {output_name}"))?,
        );
    }
    for output in &state.outputs {
        output.surface.commit();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while state.configured.len() != state.outputs.len() {
        if std::time::Instant::now() > deadline {
            return Err(anyhow!("compositor never configured the selector surfaces"));
        }
        event_queue.flush()?;
        event_queue
            .dispatch_pending(&mut state)
            .context("dispatch while waiting for layer surface configure")?;
        if let Some(read_guard) = event_queue.prepare_read() {
            if poll_readable(read_guard.connection_fd().as_raw_fd(), 100)? {
                read_guard.read()?;
                event_queue
                    .dispatch_pending(&mut state)
                    .context("dispatch while waiting for layer surface configure")?;
            }
        }
    }
    for output in &state.outputs {
        if let Some(&(width, height)) = state.configured.get(&output.name) {
            if width != 0
                && height != 0
                && (width, height) != (output.rect.width, output.rect.height)
            {
                tracing::warn!(
                    "compositor sized selector on {} to {width}x{height}, expected {}x{}",
                    output.name,
                    output.rect.width,
                    output.rect.height,
                );
                output.viewport.set_destination(width as i32, height as i32);
            }
        }
        output.surface.attach(Some(&clear_buffer), 0, 0);
        output.surface.damage_buffer(0, 0, 1, 1);
        output.surface.commit();
    }
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip after mapping selector surfaces")?;
    tracing::info!(
        "mapped {} native wayland selector surfaces",
        state.outputs.len()
    );
    let _ = ready.send(Ok(()));

    while !state.done && shutdown.try_recv().is_err() {
        event_queue.flush()?;
        event_queue.dispatch_pending(&mut state)?;
        let Some(read_guard) = event_queue.prepare_read() else {
            continue;
        };
        if poll_readable(read_guard.connection_fd().as_raw_fd(), 50)? {
            read_guard.read()?;
            event_queue.dispatch_pending(&mut state)?;
        }
    }
    drop(white_buffer);
    drop(white_pool);
    drop(white_file);
    drop(clear_buffer);
    drop(clear_pool);
    drop(clear_file);
    Ok(())
}

fn poll_readable(fd: i32, timeout_ms: i32) -> Result<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x0001;
    let mut poll_fd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    let ready = unsafe { poll(&mut poll_fd, 1, timeout_ms) };
    if ready < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(ready > 0 && poll_fd.revents & POLLIN != 0)
}

#[allow(clippy::too_many_arguments)]
fn map_output(
    queue: &QueueHandle<State>,
    compositor: &wl_compositor::WlCompositor,
    subcompositor: &wl_subcompositor::WlSubcompositor,
    layer_shell: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
    viewporter: &wp_viewporter::WpViewporter,
    wl_output: &WlOutput,
    output: NativeOutput,
    white_buffer: &wl_buffer::WlBuffer,
    empty_region: &wl_region::WlRegion,
) -> Result<OutputSurface> {
    let surface = compositor.create_surface(queue, ());
    let viewport = viewporter.get_viewport(&surface, queue, ());
    viewport.set_destination(output.rect.width as i32, output.rect.height as i32);
    // no buffer may touch the surface before the first configure — the
    // compositor treats that as a fatal protocol error. run() attaches the
    // content buffer only after every surface acked its configure.
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        Some(wl_output),
        Layer::Overlay,
        format!("capscr-native-selector-{}", output.output_name),
        queue,
        output.output_name.clone(),
    );
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_anchor(Anchor::Top | Anchor::Right | Anchor::Bottom | Anchor::Left);
    // the on-demand keyboard value only exists from layer-shell v4 on
    let keyboard = if layer_shell.version() >= 4 {
        KeyboardInteractivity::OnDemand
    } else {
        KeyboardInteractivity::Exclusive
    };
    layer_surface.set_keyboard_interactivity(keyboard);
    layer_surface.set_size(0, 0);

    let mut borders = Vec::with_capacity(4);
    for _ in 0..4 {
        let border_surface = compositor.create_surface(queue, ());
        border_surface.set_input_region(Some(empty_region));
        border_surface.attach(Some(white_buffer), 0, 0);
        let border_viewport = viewporter.get_viewport(&border_surface, queue, ());
        border_viewport.set_destination(1, 1);
        let subsurface = subcompositor.get_subsurface(&border_surface, &surface, queue, ());
        subsurface.set_position(-2, -2);
        border_surface.commit();
        borders.push(Border {
            surface: border_surface,
            subsurface,
            viewport: border_viewport,
        });
    }

    Ok(OutputSurface {
        name: output.output_name,
        rect: output.rect,
        windows: output.windows,
        image: output.image,
        surface,
        layer_surface,
        borders,
        viewport,
    })
}

fn solid_buffer(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<State>,
    pixel: [u8; 4],
) -> Result<(wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, File)> {
    let (mut file, path) = create_shm_file("border", 4)?;
    file.write_all(&pixel)?;
    file.seek(SeekFrom::Start(0))?;
    let _ = std::fs::remove_file(path);
    let pool = shm.create_pool(file.as_fd(), 4, queue, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, queue, ());
    Ok((pool, buffer, file))
}

fn cursor(
    compositor: &wl_compositor::WlCompositor,
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<State>,
) -> Result<Cursor> {
    const SIZE: usize = 31;
    let mut pixels = vec![0u8; SIZE * SIZE * 4];
    for index in 2..SIZE - 2 {
        for offset in -1..=1 {
            let vertical = ((index * SIZE) + (15i32 + offset) as usize) * 4;
            let horizontal = (((15i32 + offset) as usize * SIZE) + index) * 4;
            let color = if offset == 0 { 255 } else { 0 };
            pixels[vertical..vertical + 4].copy_from_slice(&[color, color, color, 255]);
            pixels[horizontal..horizontal + 4].copy_from_slice(&[color, color, color, 255]);
        }
    }
    let (mut file, path) = create_shm_file("cursor", pixels.len() as u32)?;
    file.write_all(&pixels)?;
    file.seek(SeekFrom::Start(0))?;
    let _ = std::fs::remove_file(path);
    let pool = shm.create_pool(file.as_fd(), pixels.len() as i32, queue, ());
    let buffer = pool.create_buffer(
        0,
        SIZE as i32,
        SIZE as i32,
        (SIZE * 4) as i32,
        wl_shm::Format::Argb8888,
        queue,
        (),
    );
    let surface = compositor.create_surface(queue, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, SIZE as i32, SIZE as i32);
    surface.commit();
    Ok(Cursor {
        surface,
        _pool: pool,
        _buffer: buffer,
        _file: file,
    })
}

fn create_shm_file(output_name: &str, size: u32) -> Result<(File, PathBuf)> {
    for nonce in 0..32 {
        let path = PathBuf::from(format!(
            "/dev/shm/capscr-native-{}-{}-{nonce}",
            std::process::id(),
            output_name.replace('/', "_")
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                file.set_len(size as u64)?;
                return Ok((file, path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("couldn't allocate native selector shared memory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_rectangle_handles_reverse_drag() {
        assert_eq!(
            selection_rectangle(100.0, 80.0, 20.0, 30.0, false),
            Rectangle::new(20, 30, 80, 50)
        );
    }

    #[test]
    fn selection_rectangle_snaps_to_nearest_aspect_ratio() {
        assert_eq!(
            selection_rectangle(0.0, 0.0, 160.0, 95.0, true),
            Rectangle::new(0, 0, 160, 100)
        );
    }

    #[test]
    fn intersection_clips_an_outline_to_each_output() {
        assert_eq!(
            intersect(
                Rectangle::new(900, 100, 400, 300),
                Rectangle::new(0, 0, 1080, 1920)
            ),
            Some(Rectangle::new(900, 100, 180, 300))
        );
    }
}
