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
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_plasma::plasma_shell::client::{org_kde_plasma_shell, org_kde_plasma_surface};

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

// a full-resolution shm image kept alive for the selector's lifetime
struct Plane {
    buffer: wl_buffer::WlBuffer,
    _pool: wl_shm_pool::WlShmPool,
    _file: File,
}

// subsurface that reveals the undimmed frozen pixels under the selection or
// the hovered window — the wayland analog of the windows selector's bright
// SCREEN_BITMAP blit over the pre-dimmed backdrop
struct Highlight {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    viewport: wp_viewport::WpViewport,
    visible: bool,
}

struct Label {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    buffer: wl_buffer::WlBuffer,
    _pool: wl_shm_pool::WlShmPool,
    file: File,
    visible: bool,
}

struct OutputSurface {
    name: String,
    rect: Rectangle,
    windows: Vec<NativeWindow>,
    image: Arc<RgbaImage>,
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    _plasma: Option<org_kde_plasma_surface::OrgKdePlasmaSurface>,
    wl_output: WlOutput,
    mapped: bool,
    fullscreen_retries: u8,
    last_assert: Option<std::time::Instant>,
    dim: Plane,
    bright: Plane,
    highlight: Highlight,
    label: Label,
    crosshair: Vec<Border>,
    borders: Vec<Border>,
    viewport: wp_viewport::WpViewport,
}

struct State {
    configured: HashMap<String, (u32, u32)>,
    pending_size: HashMap<String, (u32, u32)>,
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
delegate_noop!(State: ignore wp_viewporter::WpViewporter);
delegate_noop!(State: ignore wp_viewport::WpViewport);
delegate_noop!(State: ignore org_kde_plasma_shell::OrgKdePlasmaShell);
delegate_noop!(State: ignore org_kde_plasma_surface::OrgKdePlasmaSurface);

impl
    Dispatch<
        wayland_client::protocol::wl_registry::WlRegistry,
        wayland_client::globals::GlobalListContents,
    > for State
{
    fn event(
        _state: &mut Self,
        _registry: &wayland_client::protocol::wl_registry::WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

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

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _state: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, String> for State {
    fn event(
        state: &mut Self,
        _toplevel: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        output_name: &String,
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state
                    .pending_size
                    .insert(output_name.clone(), (width.max(0) as u32, height.max(0) as u32));
            }
            xdg_toplevel::Event::Close => {
                tracing::warn!("selector window {output_name} closed by the compositor");
                state.finish(NativeOutcome::Cancelled);
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, String> for State {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        output_name: &String,
        _connection: &wayland_client::Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            let size = state
                .pending_size
                .get(output_name)
                .copied()
                .unwrap_or((0, 0));
            tracing::debug!(
                "selector window {output_name} configured at {}x{}",
                size.0,
                size.1,
            );
            state.configured.insert(output_name.clone(), size);
            // kwin moves a freshly mapped fullscreen window to the active
            // output and reconfigures it; asserting the target output again
            // makes it settle where it was asked to go (qt clients win the
            // same tug of war this way)
            let Some(output) = state
                .outputs
                .iter_mut()
                .find(|output| &output.name == output_name)
            else {
                return;
            };
            if !output.mapped || size.0 == 0 || size.1 == 0 {
                return;
            }
            let intended = (output.rect.width, output.rect.height);
            if size == intended {
                output.fullscreen_retries = 0;
                output
                    .viewport
                    .set_destination(intended.0 as i32, intended.1 as i32);
                output
                    .xdg_surface
                    .set_window_geometry(0, 0, intended.0 as i32, intended.1 as i32);
                output.surface.commit();
            } else if output.fullscreen_retries < 8 {
                // don't conform to the wrong grant: keep committing at the
                // intended size and re-assert the target output — kwin then
                // re-evaluates and settles the window where it was asked to
                // go (qt clients win the same tug of war by never conforming).
                // kwin answers a burst instantly with the same grant, so the
                // re-asserts are spaced out instead of spent at once
                let now = std::time::Instant::now();
                let spaced = output
                    .last_assert
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_millis(100));
                if spaced {
                    output.fullscreen_retries += 1;
                    output.last_assert = Some(now);
                    tracing::debug!(
                        "re-asserting fullscreen for {output_name} (compositor granted {}x{})",
                        size.0,
                        size.1,
                    );
                    output.toplevel.set_fullscreen(Some(&output.wl_output));
                }
                output.surface.commit();
            } else {
                // compositor won't budge; show the frame at the granted size
                // rather than leaving a displaced window
                tracing::warn!(
                    "compositor pinned selector {output_name} at {}x{}, expected {}x{}",
                    size.0,
                    size.1,
                    intended.0,
                    intended.1,
                );
                output.viewport.set_destination(size.0 as i32, size.1 as i32);
                output
                    .xdg_surface
                    .set_window_geometry(0, 0, size.0 as i32, size.1 as i32);
                output.surface.commit();
            }
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
                if let Some(index) = state.current_output {
                    tracing::debug!(
                        "pointer entered selector surface {} at {surface_x},{surface_y}",
                        state.outputs[index].name,
                    );
                }
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
        let dragging = self.start.is_some();
        let pointer = (self.pointer_x, self.pointer_y);
        let current = self.current_output;
        for (index, output) in self.outputs.iter_mut().enumerate() {
            let clipped = rect.and_then(|rect| intersect(rect, output.rect));
            for (side, border) in output.borders.iter().enumerate() {
                let (x, y, width, height) = match clipped {
                    Some(rect) => {
                        let left = rect.x - output.rect.x;
                        let top = rect.y - output.rect.y;
                        match side {
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
                    None => (-10000, -10000, 1, 1),
                };
                border.subsurface.set_position(x, y);
                border.viewport.set_destination(width as i32, height as i32);
                border.surface.commit();
            }

            // undimmed frozen pixels inside the outline
            match clipped {
                Some(clip) => {
                    let scale_x = output.image.width() as f64 / output.rect.width as f64;
                    let scale_y = output.image.height() as f64 / output.rect.height as f64;
                    let local_x = clip.x - output.rect.x;
                    let local_y = clip.y - output.rect.y;
                    let source_x = (local_x as f64 * scale_x)
                        .clamp(0.0, output.image.width().saturating_sub(1) as f64);
                    let source_y = (local_y as f64 * scale_y)
                        .clamp(0.0, output.image.height().saturating_sub(1) as f64);
                    let source_width =
                        (clip.width as f64 * scale_x).min(output.image.width() as f64 - source_x);
                    let source_height =
                        (clip.height as f64 * scale_y).min(output.image.height() as f64 - source_y);
                    output.highlight.subsurface.set_position(local_x, local_y);
                    output
                        .highlight
                        .viewport
                        .set_source(source_x, source_y, source_width, source_height);
                    output
                        .highlight
                        .viewport
                        .set_destination(clip.width as i32, clip.height as i32);
                    output
                        .highlight
                        .surface
                        .attach(Some(&output.bright.buffer), 0, 0);
                    output.highlight.surface.damage_buffer(
                        0,
                        0,
                        output.image.width() as i32,
                        output.image.height() as i32,
                    );
                    output.highlight.surface.commit();
                    output.highlight.visible = true;
                }
                None if output.highlight.visible => {
                    output.highlight.surface.attach(None, 0, 0);
                    output.highlight.surface.commit();
                    output.highlight.visible = false;
                }
                None => {}
            }

            // size label on the output holding the selection's top-left corner
            let show_label = dragging
                && rect
                    .zip(clipped)
                    .is_some_and(|(rect, clip)| rect.x == clip.x && rect.y == clip.y);
            if show_label {
                let rect = rect.unwrap();
                let text = format!("{}x{}", rect.width, rect.height);
                if draw_label(&mut output.label.file, &text).is_ok() {
                    let local_x = rect.x - output.rect.x + 5;
                    let local_top = rect.y - output.rect.y;
                    let local_y = if local_top >= LABEL_HEIGHT as i32 + 4 {
                        local_top - LABEL_HEIGHT as i32 - 4
                    } else {
                        local_top + 5
                    };
                    output.label.subsurface.set_position(local_x, local_y);
                    output.label.surface.attach(Some(&output.label.buffer), 0, 0);
                    output.label.surface.damage_buffer(
                        0,
                        0,
                        LABEL_WIDTH as i32,
                        LABEL_HEIGHT as i32,
                    );
                    output.label.surface.commit();
                    output.label.visible = true;
                }
            } else if output.label.visible {
                output.label.surface.attach(None, 0, 0);
                output.label.surface.commit();
                output.label.visible = false;
            }

            // crosshair guides around the pointer on the hovered output
            let local_pointer = (current == Some(index)).then(|| {
                (
                    (pointer.0 - output.rect.x as f64).round() as i32,
                    (pointer.1 - output.rect.y as f64).round() as i32,
                )
            });
            const GAP: i32 = 20;
            let output_width = output.rect.width as i32;
            let output_height = output.rect.height as i32;
            for (line, guide) in output.crosshair.iter().enumerate() {
                let (x, y, width, height) = match local_pointer {
                    Some((px, py)) => match line {
                        0 => (0, py, (px - GAP).max(0), 1),
                        1 => (px + GAP, py, (output_width - px - GAP).max(0), 1),
                        2 => (px, 0, 1, (py - GAP).max(0)),
                        _ => (px, py + GAP, 1, (output_height - py - GAP).max(0)),
                    },
                    None => (-10000, -10000, 1, 1),
                };
                if width <= 0 || height <= 0 {
                    guide.subsurface.set_position(-10000, -10000);
                    guide.viewport.set_destination(1, 1);
                } else {
                    guide.subsurface.set_position(x, y);
                    guide.viewport.set_destination(width.max(1), height.max(1));
                }
                guide.surface.commit();
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

// size label geometry: up to 9 glyphs ("3840x2160") of 5x7 dots at 3x scale
// with 3px tracking and 6px padding
const LABEL_WIDTH: u32 = 176;
const LABEL_HEIGHT: u32 = 33;
const GLYPH_SCALE: u32 = 3;

fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'x' => [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        _ => return None,
    })
}

// white text on an opaque black chip, same look as the windows selector label
fn draw_label(file: &mut File, text: &str) -> std::io::Result<()> {
    let mut pixels = vec![0u8; (LABEL_WIDTH * LABEL_HEIGHT * 4) as usize];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let advance = 5 * GLYPH_SCALE + GLYPH_SCALE;
    let mut pen_x = 6u32;
    for c in text.chars() {
        if let Some(rows) = glyph(c) {
            for (row, bits) in rows.iter().enumerate() {
                for column in 0..5u32 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    for dy in 0..GLYPH_SCALE {
                        for dx in 0..GLYPH_SCALE {
                            let x = pen_x + column * GLYPH_SCALE + dx;
                            let y = 6 + row as u32 * GLYPH_SCALE + dy;
                            if x < LABEL_WIDTH && y < LABEL_HEIGHT {
                                let i = ((y * LABEL_WIDTH + x) * 4) as usize;
                                pixels[i] = 255;
                                pixels[i + 1] = 255;
                                pixels[i + 2] = 255;
                            }
                        }
                    }
                }
            }
        }
        pen_x += advance;
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&pixels)
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
    // a dedicated connection: sharing libwayshot's confuses kwin's per-client
    // output bookkeeping (its screencopy setup binds and releases outputs),
    // which made kwin resolve our wl_output handles to the active output
    let connection =
        wayland_client::Connection::connect_to_env().context("connect to wayland")?;
    let (globals, mut event_queue) =
        wayland_client::globals::registry_queue_init::<State>(&connection)
            .context("enumerate globals")?;
    let queue = event_queue.handle();
    let compositor = globals
        .bind::<wl_compositor::WlCompositor, _, _>(&queue, 1..=6, ())
        .context("bind wl_compositor")?;
    let subcompositor = globals
        .bind::<wl_subcompositor::WlSubcompositor, _, _>(&queue, 1..=1, ())
        .context("bind wl_subcompositor")?;
    let shm = globals
        .bind::<wl_shm::WlShm, _, _>(&queue, 1..=1, ())
        .context("bind wl_shm")?;
    let wm_base = globals
        .bind::<xdg_wm_base::XdgWmBase, _, _>(&queue, 1..=6, ())
        .context("bind xdg_wm_base")?;
    let viewporter = globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&queue, 1..=1, ())
        .context("bind wp_viewporter")?;
    let _seat = globals
        .bind::<wl_seat::WlSeat, _, _>(&queue, 1..=9, ())
        .context("bind wl_seat")?;
    // kde's plasma shell can pin a surface at exact global coordinates; on
    // other compositors the xdg fullscreen hint is honored as-is
    let plasma_shell = globals
        .bind::<org_kde_plasma_shell::OrgKdePlasmaShell, _, _>(&queue, 1..=8, ())
        .ok();
    let (white_pool, white_buffer, white_file) =
        solid_buffer(&shm, &queue, [255; 4]).context("allocate border buffer")?;
    let (gray_pool, gray_buffer, gray_file) =
        solid_buffer(&shm, &queue, [128, 128, 128, 255]).context("allocate guide buffer")?;
    let cursor = cursor(&compositor, &shm, &queue).context("build cursor surface")?;
    let empty_region = compositor.create_region(&queue, ());
    let mut state = State {
        configured: HashMap::new(),
        pending_size: HashMap::new(),
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

    for global in globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|global| global.interface == "wl_output")
    {
        let _: WlOutput = globals.registry().bind(
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
        let output_global = *state
            .output_globals
            .get(&output.output_name)
            .ok_or_else(|| anyhow!("wayland output global {} disappeared", output.output_name))?;
        tracing::debug!(
            "binding selector surface for {} to wl_output global {output_global}",
            output.output_name,
        );
        let wl_output: WlOutput = globals.registry().bind(output_global, 4, &queue, ());
        let output_name = output.output_name.clone();
        state.outputs.push(
            map_output(
                &queue,
                &compositor,
                &subcompositor,
                &wm_base,
                plasma_shell.as_ref(),
                &viewporter,
                &shm,
                &wl_output,
                output,
                &white_buffer,
                &gray_buffer,
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
    for output in &mut state.outputs {
        output.mapped = true;
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
                output
                    .xdg_surface
                    .set_window_geometry(0, 0, width as i32, height as i32);
            }
        }
        output.surface.attach(Some(&output.dim.buffer), 0, 0);
        output.surface.damage_buffer(
            0,
            0,
            output.image.width() as i32,
            output.image.height() as i32,
        );
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
    drop(gray_buffer);
    drop(gray_pool);
    drop(gray_file);
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
    wm_base: &xdg_wm_base::XdgWmBase,
    plasma_shell: Option<&org_kde_plasma_shell::OrgKdePlasmaShell>,
    viewporter: &wp_viewporter::WpViewporter,
    shm: &wl_shm::WlShm,
    wl_output: &WlOutput,
    output: NativeOutput,
    white_buffer: &wl_buffer::WlBuffer,
    gray_buffer: &wl_buffer::WlBuffer,
    empty_region: &wl_region::WlRegion,
) -> Result<OutputSurface> {
    let surface = compositor.create_surface(queue, ());
    let viewport = viewporter.get_viewport(&surface, queue, ());
    viewport.set_destination(output.rect.width as i32, output.rect.height as i32);
    // no buffer may touch the surface before the first configure — the
    // compositor treats that as a fatal protocol error. run() attaches the
    // dimmed frame only after every surface acked its configure.
    // xdg toplevels instead of layer-shell: kwin (6.7) re-arranges layer
    // surfaces bound to a rotated output into the active output's work area
    let xdg_surface = wm_base.get_xdg_surface(&surface, queue, output.output_name.clone());
    let toplevel = xdg_surface.get_toplevel(queue, output.output_name.clone());
    toplevel.set_app_id("capscr".into());
    toplevel.set_title(format!("capscr selector {}", output.output_name));
    // kwin also moves fullscreen toplevels onto the active output no matter
    // which wl_output the request names; its plasma-shell panel role is the
    // one placement path that honors exact global coordinates. other
    // compositors get the plain fullscreen hint.
    let plasma = plasma_shell.map(|shell| {
        let plasma = shell.get_surface(&surface, queue, ());
        plasma.set_role(org_kde_plasma_surface::Role::Panel as u32);
        plasma.set_position(output.rect.x, output.rect.y);
        plasma.set_panel_behavior(org_kde_plasma_surface::PanelBehavior::WindowsGoBelow as u32);
        plasma.set_panel_takes_focus(1);
        if plasma.version() >= 5 {
            plasma.set_skip_taskbar(1);
        }
        if plasma.version() >= 6 {
            plasma.set_skip_switcher(1);
        }
        plasma
    });
    if plasma.is_none() {
        toplevel.set_fullscreen(Some(wl_output));
    }
    // without an explicit window geometry the offscreen-parked subsurfaces
    // inflate the toplevel's bounding box and the compositor sizes the
    // "fullscreen" window around them, displacing the visible content
    xdg_surface.set_window_geometry(0, 0, output.rect.width as i32, output.rect.height as i32);

    // same retained fraction as the windows selector's pre-dimmed bitmap
    let dim = image_buffer(shm, queue, &output.image, Some(95), &output.output_name)?;
    let bright = image_buffer(shm, queue, &output.image, None, &output.output_name)?;

    // stacking is creation order: highlight below crosshair below borders
    // below label
    let highlight = {
        let hl_surface = compositor.create_surface(queue, ());
        hl_surface.set_input_region(Some(empty_region));
        let hl_viewport = viewporter.get_viewport(&hl_surface, queue, ());
        let subsurface = subcompositor.get_subsurface(&hl_surface, &surface, queue, ());
        subsurface.set_position(0, 0);
        hl_surface.commit();
        Highlight {
            surface: hl_surface,
            subsurface,
            viewport: hl_viewport,
            visible: false,
        }
    };

    let mut crosshair = Vec::with_capacity(4);
    for _ in 0..4 {
        let line_surface = compositor.create_surface(queue, ());
        line_surface.set_input_region(Some(empty_region));
        line_surface.attach(Some(gray_buffer), 0, 0);
        let line_viewport = viewporter.get_viewport(&line_surface, queue, ());
        line_viewport.set_destination(1, 1);
        let subsurface = subcompositor.get_subsurface(&line_surface, &surface, queue, ());
        subsurface.set_position(-10000, -10000);
        line_surface.commit();
        crosshair.push(Border {
            surface: line_surface,
            subsurface,
            viewport: line_viewport,
        });
    }

    let mut borders = Vec::with_capacity(4);
    for _ in 0..4 {
        let border_surface = compositor.create_surface(queue, ());
        border_surface.set_input_region(Some(empty_region));
        border_surface.attach(Some(white_buffer), 0, 0);
        let border_viewport = viewporter.get_viewport(&border_surface, queue, ());
        border_viewport.set_destination(1, 1);
        let subsurface = subcompositor.get_subsurface(&border_surface, &surface, queue, ());
        subsurface.set_position(-10000, -10000);
        border_surface.commit();
        borders.push(Border {
            surface: border_surface,
            subsurface,
            viewport: border_viewport,
        });
    }

    let label = {
        let size = LABEL_WIDTH * LABEL_HEIGHT * 4;
        let (mut file, path) = create_shm_file("label", size)?;
        file.write_all(&vec![0u8; size as usize])?;
        file.seek(SeekFrom::Start(0))?;
        let _ = std::fs::remove_file(path);
        let pool = shm.create_pool(file.as_fd(), size as i32, queue, ());
        let buffer = pool.create_buffer(
            0,
            LABEL_WIDTH as i32,
            LABEL_HEIGHT as i32,
            (LABEL_WIDTH * 4) as i32,
            wl_shm::Format::Argb8888,
            queue,
            (),
        );
        let label_surface = compositor.create_surface(queue, ());
        label_surface.set_input_region(Some(empty_region));
        let subsurface = subcompositor.get_subsurface(&label_surface, &surface, queue, ());
        subsurface.set_position(-(LABEL_WIDTH as i32) - 4, -(LABEL_HEIGHT as i32) - 4);
        label_surface.commit();
        Label {
            surface: label_surface,
            subsurface,
            buffer,
            _pool: pool,
            file,
            visible: false,
        }
    };

    Ok(OutputSurface {
        name: output.output_name,
        rect: output.rect,
        windows: output.windows,
        image: output.image,
        surface,
        xdg_surface,
        toplevel,
        _plasma: plasma,
        wl_output: wl_output.clone(),
        mapped: false,
        fullscreen_retries: 0,
        last_assert: None,
        dim,
        bright,
        highlight,
        label,
        crosshair,
        borders,
        viewport,
    })
}

// full-frame shm plane in the ARGB byte order wl_shm expects; dim is the
// retained brightness numerator out of 255, None keeps the pixels untouched
fn image_buffer(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<State>,
    image: &RgbaImage,
    dim: Option<u32>,
    tag: &str,
) -> Result<Plane> {
    let width = image.width();
    let height = image.height();
    let stride = width.checked_mul(4).context("selector frame is too wide")?;
    let size = stride
        .checked_mul(height)
        .context("selector frame is too large")?;
    let (mut file, path) = create_shm_file(tag, size)?;
    let mut bgra = vec![0u8; size as usize];
    match dim {
        Some(keep) => crate::capture::par_convert(image.as_raw(), &mut bgra, move |pixel| {
            let dimmed = |v: u8| ((v as u32 * keep) / 255) as u8;
            [dimmed(pixel[2]), dimmed(pixel[1]), dimmed(pixel[0]), 255]
        }),
        None => crate::capture::par_convert(image.as_raw(), &mut bgra, |pixel| {
            [pixel[2], pixel[1], pixel[0], 255]
        }),
    }
    file.write_all(&bgra)?;
    file.seek(SeekFrom::Start(0))?;
    let _ = std::fs::remove_file(path);
    let pool = shm.create_pool(file.as_fd(), size as i32, queue, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        queue,
        (),
    );
    Ok(Plane {
        buffer,
        _pool: pool,
        _file: file,
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
