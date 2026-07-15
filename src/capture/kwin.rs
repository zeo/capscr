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
pub fn capture_area(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
    capture_area_with_resolution(x, y, width, height, false)
}

pub fn capture_area_native(x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
    capture_area_with_resolution(x, y, width, height, true)
}

pub fn capture_screen(output_name: &str) -> Result<RgbaImage> {
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(false));
        options.insert("native-resolution", Value::from(true));
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
) -> Result<RgbaImage> {
    if width == 0 || height == 0 {
        return Err(anyhow!("refusing to capture a zero-sized area"));
    }
    capture_request(|conn, output| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("include-cursor", Value::from(false));
        options.insert("native-resolution", Value::from(native_resolution));
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
        windows.push(KwinWindow {
            handle: handle.to_owned(),
            x: x.round() as i32,
            y: y.round() as i32,
            width: width.round() as u32,
            height: height.round() as u32,
        });
    }
    // the runner follows kwin's bottom-to-top window order; hit-testing needs
    // the last painted window first
    windows.reverse();
    Ok(windows)
}

fn capture_request(
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

    let conn = zbus::blocking::Connection::session()?;
    let reply = call(&conn, Fd::from(writer.as_fd()))?;
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
