// KWin's private, authorized screenshot interface (org.kde.KWin.ScreenShot2).
// on kde+nvidia every public path (xcap, wlr-screencopy/libwayshot, grim, the
// desktop portal) hands back black frames or is denied, so this is the only
// working pixel source — the same one spectacle uses. requires
// `X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2` on our .desktop,
// otherwise kwin answers with NoAuthorized.
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;

use anyhow::{anyhow, Result};
use image::RgbaImage;
use zbus::zvariant::{Fd, OwnedValue, Value};

#[derive(Debug, Clone)]
pub struct KwinWindow {
    pub handle: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn result_u32(results: &HashMap<String, OwnedValue>, key: &str) -> Result<u32> {
    let v = results
        .get(key)
        .ok_or_else(|| anyhow!("ScreenShot2 result missing '{key}'"))?;
    // kwin sends unsigned dimensions but the QImage format as a signed int;
    // accept either without caring which
    if let Ok(n) = v.downcast_ref::<u32>() {
        return Ok(n);
    }
    if let Ok(n) = v.downcast_ref::<i32>() {
        return Ok(n as u32);
    }
    Err(anyhow!("ScreenShot2 result '{key}' is not an integer"))
}

// grab a logical rectangle straight from the compositor. coordinates are in the
// same logical space capscr uses for monitors, so a monitor's x/y/w/h maps 1:1.
pub fn capture_area(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    include_cursor: bool,
) -> Result<RgbaImage> {
    capture_area_with_resolution(x, y, width, height, false, include_cursor)
}

pub fn capture_area_native(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    include_cursor: bool,
) -> Result<RgbaImage> {
    capture_area_with_resolution(x, y, width, height, true, include_cursor)
}

// kwin >= 6.6.1 drops the calling process's windows from every ScreenShot2
// grab by default. windows-parity wants the opposite: pinned screenshots are
// ordinary windows that belong in the user's captures, and only the recording
// bar is excluded (per-window, via exclude_own_windows_from_capture below).
// older kwin never reads the key, so passing it is safe everywhere
fn show_caller_windows(options: &mut HashMap<&str, Value>) {
    options.insert("hide-caller-windows", Value::from(false));
}

pub fn capture_screen(output_name: &str, include_cursor: bool) -> Result<RgbaImage> {
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(include_cursor));
        options.insert("native-resolution", Value::from(true));
        show_caller_windows(&mut options);
        Ok(conn.call_method(
            Some("org.kde.KWin.ScreenShot2"),
            "/org/kde/KWin/ScreenShot2",
            Some("org.kde.KWin.ScreenShot2"),
            "CaptureScreen",
            &(output_name, options, output),
        )?)
    })
}

fn capture_area_with_resolution(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    native_resolution: bool,
    include_cursor: bool,
) -> Result<RgbaImage> {
    if width == 0 || height == 0 {
        return Err(anyhow!("refusing to capture a zero-sized area"));
    }
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(include_cursor));
        options.insert("native-resolution", Value::from(native_resolution));
        show_caller_windows(&mut options);
        Ok(conn.call_method(
            Some("org.kde.KWin.ScreenShot2"),
            "/org/kde/KWin/ScreenShot2",
            Some("org.kde.KWin.ScreenShot2"),
            "CaptureArea",
            &(x, y, width, height, options, output),
        )?)
    })
}

// let kwin perform its own window-under-pointer selection. unlike xcap's xcb
// window list, this sees native wayland clients as well as xwayland windows.
pub fn capture_interactive_window(include_cursor: bool) -> Result<RgbaImage> {
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(include_cursor));
        options.insert("include-decoration", Value::from(true));
        options.insert("include-shadow", Value::from(false));
        options.insert("native-resolution", Value::from(true));
        show_caller_windows(&mut options);
        Ok(conn.call_method(
            Some("org.kde.KWin.ScreenShot2"),
            "/org/kde/KWin/ScreenShot2",
            Some("org.kde.KWin.ScreenShot2"),
            "CaptureInteractive",
            &(0u32, options, output),
        )?)
    })
}

pub fn capture_window(handle: &str, include_cursor: bool) -> Result<RgbaImage> {
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(include_cursor));
        options.insert("include-decoration", Value::from(true));
        options.insert("include-shadow", Value::from(false));
        options.insert("native-resolution", Value::from(true));
        show_caller_windows(&mut options);
        Ok(conn.call_method(
            Some("org.kde.KWin.ScreenShot2"),
            "/org/kde/KWin/ScreenShot2",
            Some("org.kde.KWin.ScreenShot2"),
            "CaptureWindow",
            &(handle, options, output),
        )?)
    })
}

pub fn list_windows() -> Result<Vec<KwinWindow>> {
    type RunnerMatch = (
        String,
        String,
        String,
        i32,
        f64,
        HashMap<String, OwnedValue>,
    );

    let conn = zbus::blocking::Connection::session()?;
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/WindowsRunner",
        Some("org.kde.krunner1"),
        "Match",
        &("",),
    )?;
    let matches: Vec<RunnerMatch> = reply.body().deserialize()?;
    let own_pid = std::process::id() as i32;
    let mut seen = std::collections::HashSet::new();
    let mut windows = Vec::new();

    for (match_id, _, _, _, _, _) in matches {
        let handle = match_id
            .split_once('_')
            .map_or(match_id.as_str(), |(_, id)| id);
        if !seen.insert(handle.to_owned()) {
            continue;
        }
        let reply = conn.call_method(
            Some("org.kde.KWin"),
            "/KWin",
            Some("org.kde.KWin"),
            "getWindowInfo",
            &(handle,),
        )?;
        let info: HashMap<String, OwnedValue> = reply.body().deserialize()?;
        let number = |key: &str| {
            info.get(key)
                .and_then(|value| value.downcast_ref::<f64>().ok())
        };
        let integer = |key: &str| {
            info.get(key)
                .and_then(|value| value.downcast_ref::<i32>().ok())
        };
        let flag = |key: &str| {
            info.get(key)
                .and_then(|value| value.downcast_ref::<bool>().ok())
                .unwrap_or(false)
        };
        if integer("pid") == Some(own_pid)
            || flag("minimized")
            || flag("skipSwitcher")
            || flag("skipTaskbar")
            || flag("excludeFromCapture")
        {
            continue;
        }
        let Some((x, y, width, height)) = number("x")
            .zip(number("y"))
            .zip(number("width").zip(number("height")))
            .map(|((x, y), (width, height))| (x, y, width, height))
        else {
            continue;
        };
        if width <= 5.0 || height <= 5.0 {
            continue;
        }
        windows.push((
            integer("layer").unwrap_or_default(),
            KwinWindow {
                handle: handle.to_owned(),
                x: x.round() as i32,
                y: y.round() as i32,
                width: width.round() as u32,
                height: height.round() as u32,
            },
        ));
    }
    // krunner's list has no z-order within a layer, so a window covered by
    // another (a fullscreen game over a browser, say) could win hover
    // hit-tests. sort topmost-first by the compositor's real stacking order
    // and fall back to the coarse layer sort when it isn't available.
    let by_stacking = stacking_order().map(|order| {
        order
            .iter()
            .enumerate()
            .map(|(index, uuid)| (uuid.clone(), index + 1))
            .collect::<HashMap<String, usize>>()
    });
    match by_stacking {
        Ok(position)
            if windows
                .iter()
                .any(|(_, window)| position.contains_key(window.handle.as_str())) =>
        {
            windows.sort_by_key(|(_, window)| {
                std::cmp::Reverse(position.get(window.handle.as_str()).copied().unwrap_or(0))
            });
        }
        Ok(_) => {
            tracing::debug!("stacking order shares no ids with the window list; using layers");
            windows.sort_by_key(|(layer, _)| std::cmp::Reverse(*layer));
        }
        Err(e) => {
            tracing::debug!("stacking order unavailable ({e:#}); using layers");
            windows.sort_by_key(|(layer, _)| std::cmp::Reverse(*layer));
        }
    }
    Ok(windows.into_iter().map(|(_, window)| window).collect())
}

// best-effort keep-above for this process's windows with exactly `title`,
// via the same restricted window-management interface stacking_order uses.
// xdg always_on_top is a no-op on wayland; this is kde's designed mechanism
// (what taskbar keep-above toggles send). returns how many windows matched.
pub fn keep_own_windows_above(title: &str) -> Result<usize> {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols_plasma::plasma_window_management::client::{
        org_kde_plasma_window::{self, OrgKdePlasmaWindow},
        org_kde_plasma_window_management::{self, OrgKdePlasmaWindowManagement},
    };

    #[derive(Default)]
    struct KeepAbove {
        uuids: Vec<String>,
        info: HashMap<String, (Option<u32>, Option<String>)>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for KeepAbove {
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

    impl Dispatch<OrgKdePlasmaWindowManagement, ()> for KeepAbove {
        fn event(
            state: &mut Self,
            _: &OrgKdePlasmaWindowManagement,
            event: org_kde_plasma_window_management::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let org_kde_plasma_window_management::Event::WindowWithUuid { uuid, .. } = event {
                state.uuids.push(uuid);
            }
        }
    }

    impl Dispatch<OrgKdePlasmaWindow, String> for KeepAbove {
        fn event(
            state: &mut Self,
            _: &OrgKdePlasmaWindow,
            event: org_kde_plasma_window::Event,
            uuid: &String,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let entry = state.info.entry(uuid.clone()).or_default();
            match event {
                org_kde_plasma_window::Event::PidChanged { pid } => entry.0 = Some(pid),
                org_kde_plasma_window::Event::TitleChanged { title } => entry.1 = Some(title),
                _ => {}
            }
        }
    }

    let connection = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<KeepAbove>(&connection)?;
    let queue = event_queue.handle();
    // v13 = window_with_uuid announcements
    let manager = globals.bind::<OrgKdePlasmaWindowManagement, _, _>(&queue, 13..=16, ())?;
    let mut state = KeepAbove::default();
    event_queue.roundtrip(&mut state)?;
    let windows: Vec<(String, OrgKdePlasmaWindow)> = state
        .uuids
        .clone()
        .into_iter()
        .map(|uuid| {
            let window = manager.get_window_by_uuid(uuid.clone(), &queue, uuid.clone());
            (uuid, window)
        })
        .collect();
    // each window streams its initial pid/title events after creation
    event_queue.roundtrip(&mut state)?;
    event_queue.roundtrip(&mut state)?;
    let own_pid = std::process::id();
    let mut flagged = 0usize;
    for (uuid, window) in &windows {
        let matched = state.info.get(uuid).is_some_and(|(pid, window_title)| {
            *pid == Some(own_pid) && window_title.as_deref() == Some(title)
        });
        if matched {
            const KEEP_ABOVE: u32 = 0x10;
            window.set_state(KEEP_ABOVE, KEEP_ABOVE);
            flagged += 1;
        }
    }
    event_queue.roundtrip(&mut state)?;
    Ok(flagged)
}

// kwin >= 6.7 removes a window with excludeFromCapture set from screenshots
// and screencasts alike — the compositor-side equivalent of the
// WDA_EXCLUDEFROMCAPTURE flag the windows recording bar uses. the property is
// only reachable from inside kwin, so a resident script flags this process's
// windows whose caption carries `marker`, and its windowAdded handler runs
// before the window is ever composited into a grab, so a bar mapped after the
// guard is armed never leaks into a frame. dropping the guard unloads the
// script; the property dies with the window.
pub struct CaptureExclusionGuard {
    script_path: std::path::PathBuf,
    file: std::path::PathBuf,
}

const EXCLUSION_PLUGIN: &str = "capscr-capture-exclusion";

pub fn exclude_own_windows_from_capture(marker: &str) -> Result<CaptureExclusionGuard> {
    let pid = std::process::id();
    // kwin appends " — <app name>" to captions, so match on containment
    let script = format!(
        "function apply(w) {{\n    if (w.pid == {pid} && w.caption.includes({marker:?})) {{\n        w.excludeFromCapture = true;\n    }}\n}}\nworkspace.windowList().forEach(apply);\nworkspace.windowAdded.connect(apply);\n"
    );
    let file = std::env::temp_dir().join(format!("capscr_kwin_{pid}.js"));
    std::fs::write(&file, script)?;

    let conn = zbus::blocking::Connection::session()?;
    // a stale copy from a crashed run would make loadScript return -1
    let _ = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "unloadScript",
        &(EXCLUSION_PLUGIN,),
    );
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(file.to_string_lossy().as_ref(), EXCLUSION_PLUGIN),
    )?;
    let id: i32 = reply.body().deserialize()?;
    if id < 0 {
        let _ = std::fs::remove_file(&file);
        return Err(anyhow!("kwin refused the capture-exclusion script"));
    }
    let script_path = format!("/Scripting/Script{id}");
    if let Err(e) = conn.call_method(
        Some("org.kde.KWin"),
        script_path.as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    ) {
        let _ = std::fs::remove_file(&file);
        return Err(e.into());
    }
    Ok(CaptureExclusionGuard {
        script_path: script_path.into(),
        file,
    })
}

impl Drop for CaptureExclusionGuard {
    fn drop(&mut self) {
        if let Ok(conn) = zbus::blocking::Connection::session() {
            let _ = conn.call_method(
                Some("org.kde.KWin"),
                self.script_path.to_string_lossy().as_ref(),
                Some("org.kde.kwin.Script"),
                "stop",
                &(),
            );
            let _ = conn.call_method(
                Some("org.kde.KWin"),
                "/Scripting",
                Some("org.kde.kwin.Scripting"),
                "unloadScript",
                &(EXCLUSION_PLUGIN,),
            );
        }
        let _ = std::fs::remove_file(&self.file);
    }
}

// the compositor announces the full window stacking order (bottom to top,
// `;`-separated internal uuids) once on bind
fn stacking_order() -> Result<Vec<String>> {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};
    use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window_management::{
        self, OrgKdePlasmaWindowManagement,
    };

    struct Stacking {
        uuids: Option<Vec<String>>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Stacking {
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

    impl Dispatch<OrgKdePlasmaWindowManagement, ()> for Stacking {
        fn event(
            state: &mut Self,
            _: &OrgKdePlasmaWindowManagement,
            event: org_kde_plasma_window_management::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let org_kde_plasma_window_management::Event::StackingOrderUuidChanged { uuids } =
                event
            {
                state.uuids = Some(
                    uuids
                        .split(';')
                        .filter(|uuid| !uuid.is_empty())
                        .map(String::from)
                        .collect(),
                );
            }
        }
    }

    let connection = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<Stacking>(&connection)?;
    let _manager = globals.bind::<OrgKdePlasmaWindowManagement, _, _>(
        &event_queue.handle(),
        12..=16,
        (),
    )?;
    let mut state = Stacking { uuids: None };
    for _ in 0..4 {
        event_queue.roundtrip(&mut state)?;
        if state.uuids.is_some() {
            break;
        }
    }
    state
        .uuids
        .ok_or_else(|| anyhow!("compositor sent no stacking order"))
}

// persistent-connection region grabber for recording loops: one session-bus
// connection and region-sized logical captures, instead of a fresh connection
// plus a full-monitor native-resolution grab per frame
pub struct KwinRegionGrabber {
    conn: zbus::blocking::Connection,
}

impl KwinRegionGrabber {
    pub fn new() -> Result<Self> {
        Ok(Self {
            conn: zbus::blocking::Connection::session()?,
        })
    }

    pub fn grab(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        include_cursor: bool,
    ) -> Result<RgbaImage> {
        if width == 0 || height == 0 {
            return Err(anyhow!("refusing to capture a zero-sized area"));
        }
        capture_request_on(&self.conn, |conn, output| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("include-cursor", Value::from(include_cursor));
            options.insert("native-resolution", Value::from(false));
            show_caller_windows(&mut options);
            Ok(conn.call_method(
                Some("org.kde.KWin.ScreenShot2"),
                "/org/kde/KWin/ScreenShot2",
                Some("org.kde.KWin.ScreenShot2"),
                "CaptureArea",
                &(x, y, width, height, options, output),
            )?)
        })
    }
}

fn capture_request(
    call: impl FnOnce(&zbus::blocking::Connection, Fd<'_>) -> Result<zbus::Message>,
) -> Result<RgbaImage> {
    let conn = zbus::blocking::Connection::session()?;
    capture_request_on(&conn, call)
}

fn capture_request_on(
    conn: &zbus::blocking::Connection,
    call: impl FnOnce(&zbus::blocking::Connection, Fd<'_>) -> Result<zbus::Message>,
) -> Result<RgbaImage> {
    let (mut reader, writer) = std::io::pipe()?;

    // kwin blocks on the pipe write once its buffer fills (images are MBs, the
    // pipe buffer is 64k), so the dbus reply won't land until someone drains it.
    // read on a side thread while the blocking call is in flight.
    let reader_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map(|_| buf)
    });

    let reply = call(conn, Fd::from(writer.as_fd()))?;
    // drop our write end so the reader sees EOF once kwin closes its copy
    drop(writer);

    let results: HashMap<String, OwnedValue> = reply.body().deserialize()?;
    let w = result_u32(&results, "width")?;
    let h = result_u32(&results, "height")?;
    let stride = result_u32(&results, "stride")?;
    let format = result_u32(&results, "format").unwrap_or(5); // QImage::Format_ARGB32

    let buf = reader_thread
        .join()
        .map_err(|_| anyhow!("ScreenShot2 reader thread panicked"))??;
    let needed = stride as usize * h as usize;
    if buf.len() < needed {
        return Err(anyhow!(
            "ScreenShot2 short read: got {} bytes, expected {}",
            buf.len(),
            needed
        ));
    }
    decode(&buf, w, h, stride, format)
}

// the buffers kwin writes are native-endian QImage rows. the ARGB32/RGB32
// family (formats 4/5/6) is byte-order BGRA on little-endian; RGBA8888 (17/18)
// is already RGBA. rebuild as opaque RGBA8, honouring the row stride.
fn decode(buf: &[u8], w: u32, h: u32, stride: u32, format: u32) -> Result<RgbaImage> {
    if stride < w.saturating_mul(4) {
        return Err(anyhow!(
            "ScreenShot2 stride {stride} is too short for {w} pixels"
        ));
    }
    let bgra = match format {
        4..=6 => true,
        17 | 18 => false,
        _ => return Err(anyhow!("unsupported ScreenShot2 QImage format {format}")),
    };
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        let row = &buf[(y * stride) as usize..];
        for x in 0..w {
            let p = &row[(x as usize) * 4..];
            let px = if bgra {
                [p[2], p[1], p[0], 255]
            } else {
                [p[0], p[1], p[2], 255]
            };
            out.put_pixel(x, y, image::Rgba(px));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_padded_argb32_rows() {
        let raw = [3, 2, 1, 255, 0, 0, 0, 0];
        let image = decode(&raw, 1, 1, 8, 5).unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [1, 2, 3, 255]);
    }

    #[test]
    fn rejects_short_stride() {
        assert!(decode(&[0; 4], 2, 1, 4, 5).is_err());
    }

    #[test]
    fn rejects_unknown_qimage_format() {
        assert!(decode(&[0; 4], 1, 1, 4, 99).is_err());
    }
}
