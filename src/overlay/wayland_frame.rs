// the blinking red region frame on kde, as a raw wayland client. a gtk
// toplevel gets kwin's server-side titlebar and its plasma role ignored
// (verified in a live recording), so this mirrors the native selector's
// working recipe instead: xdg toplevel + plasma criticalnotification role +
// set_position, an empty input region, and an shm buffer whose interior is
// transparent. the blink alternates the ring buffer with a fully clear one
// so the surface never unmaps.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_plasma::plasma_shell::client::{org_kde_plasma_shell, org_kde_plasma_surface};

use crate::capture::Rectangle;

const BORDER: i32 = 4;
const FLASH_MS: u64 = 500;

pub struct WaylandFrame {
    shutdown: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl WaylandFrame {
    pub fn show(region: Rectangle) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("capscr-region-frame".into())
            .spawn(move || {
                if let Err(error) = run(region, shutdown_rx, &ready_tx) {
                    tracing::debug!("region frame unavailable: {error:#}");
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                shutdown: shutdown_tx,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = shutdown_tx.send(());
                let _ = thread.join();
                Err(anyhow!("region frame timed out before mapping"))
            }
        }
    }
}

impl Drop for WaylandFrame {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    configured: bool,
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore org_kde_plasma_shell::OrgKdePlasmaShell);
delegate_noop!(State: ignore org_kde_plasma_surface::OrgKdePlasmaSurface);

impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, wayland_client::globals::GlobalListContents>
    for State
{
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_registry::WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        _: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// an ARGB ring buffer: 4px red border, fully transparent interior. `clear`
// swaps the ring for nothing, giving the off phase of the blink
fn ring_buffer(
    shm: &wl_shm::WlShm,
    queue: &QueueHandle<State>,
    width: i32,
    height: i32,
    clear: bool,
) -> Result<(wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, std::fs::File)> {
    let stride = width * 4;
    let size = (stride * height) as usize;
    let (mut file, path) = super::wayland_native_selector::create_shm_file(
        if clear { "frame-clear" } else { "frame-ring" },
        size as u32,
    )?;
    let _ = std::fs::remove_file(path);
    let mut row = vec![0u8; stride as usize];
    for y in 0..height {
        let edge_row = y < BORDER || y >= height - BORDER;
        for x in 0..width {
            let edge = edge_row || x < BORDER || x >= width - BORDER;
            let px: [u8; 4] = if edge && !clear {
                // premultiplied ARGB little-endian: opaque pure red
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            row[(x * 4) as usize..(x * 4 + 4) as usize].copy_from_slice(&px);
        }
        file.write_all(&row)?;
    }
    file.flush()?;
    let pool = shm.create_pool(file.as_fd(), size as i32, queue, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wl_shm::Format::Argb8888,
        queue,
        (),
    );
    Ok((pool, buffer, file))
}

fn run(region: Rectangle, shutdown: Receiver<()>, ready: &Sender<Result<()>>) -> Result<()> {
    let width = region.width as i32 + BORDER * 2;
    let height = region.height as i32 + BORDER * 2;
    let (x, y) = (region.x - BORDER, region.y - BORDER);

    let connection = Connection::connect_to_env().context("connect to wayland")?;
    let (globals, mut event_queue) =
        wayland_client::globals::registry_queue_init::<State>(&connection)
            .context("enumerate globals")?;
    let queue = event_queue.handle();
    let compositor = globals
        .bind::<wl_compositor::WlCompositor, _, _>(&queue, 1..=6, ())
        .context("bind wl_compositor")?;
    let shm = globals
        .bind::<wl_shm::WlShm, _, _>(&queue, 1..=1, ())
        .context("bind wl_shm")?;
    let wm_base = globals
        .bind::<xdg_wm_base::XdgWmBase, _, _>(&queue, 1..=6, ())
        .context("bind xdg_wm_base")?;
    // no plasma shell, no exact placement: the caller falls back to the gtk
    // path (layer-shell on wlroots, the companion on gnome)
    let plasma_shell = globals
        .bind::<org_kde_plasma_shell::OrgKdePlasmaShell, _, _>(&queue, 1..=8, ())
        .context("bind org_kde_plasma_shell")?;

    let surface = compositor.create_surface(&queue, ());
    let empty = compositor.create_region(&queue, ());
    surface.set_input_region(Some(&empty));
    let xdg = wm_base.get_xdg_surface(&surface, &queue, ());
    let toplevel = xdg.get_toplevel(&queue, ());
    toplevel.set_app_id("capscr".into());
    toplevel.set_title("capscr frame".into());
    let plasma = plasma_shell.get_surface(&surface, &queue, ());
    // criticalnotification stacks above fullscreen windows and takes kwin's
    // exact-position path; roles below v6 use notification
    let role = if plasma.version() >= 6 {
        org_kde_plasma_surface::Role::Criticalnotification
    } else {
        org_kde_plasma_surface::Role::Notification
    };
    plasma.set_role(role as u32);
    plasma.set_position(x, y);
    if plasma.version() >= 2 {
        plasma.set_skip_taskbar(1);
    }
    if plasma.version() >= 5 {
        plasma.set_skip_switcher(1);
    }
    xdg.set_window_geometry(0, 0, width, height);
    surface.commit();

    let mut state = State { configured: false };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !state.configured {
        if std::time::Instant::now() > deadline {
            return Err(anyhow!("compositor never configured the frame surface"));
        }
        event_queue
            .blocking_dispatch(&mut state)
            .context("dispatch waiting for frame configure")?;
    }

    let (_ring_pool, ring, _ring_file) =
        ring_buffer(&shm, &queue, width, height, false).context("ring buffer")?;
    let (_clear_pool, clear, _clear_file) =
        ring_buffer(&shm, &queue, width, height, true).context("clear buffer")?;

    surface.attach(Some(&ring), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip after mapping the frame")?;
    let _ = ready.send(Ok(()));

    let mut visible = true;
    loop {
        match shutdown.recv_timeout(Duration::from_millis(FLASH_MS)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        visible = !visible;
        surface.attach(Some(if visible { &ring } else { &clear }), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        // a roundtrip (not just flush) so wm_base pings get answered during
        // long recordings; kwin kills clients that stop responding
        event_queue
            .roundtrip(&mut state)
            .context("roundtrip on blink tick")?;
    }
    Ok(())
}
