// wayland stills through the xdg-desktop-portal Screenshot call. wlroots
// compositors are covered by xcap's screencopy backend, but GNOME and KDE
// expose no wlr protocol, so the portal is the only sanctioned pixel source
// there. the portal hands back a file uri of the full desktop; callers crop
// what they need. desktops prompt for permission on first use and remember
// the grant afterwards.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{anyhow, Result};
use image::RgbaImage;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

static REQUEST_COUNTER: AtomicU32 = AtomicU32::new(0);

pub fn is_wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|t| t.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

// a wayland session also exposes DISPLAY via xwayland, so DISPLAY being set
// does not mean x11. the gtk/webview toolkit runs as a wayland client whenever
// the session is wayland and the backend isn't pinned to x11 — overlay
// placement must match the toolkit, otherwise it takes the x11
// absolute-positioning path and lands misplaced, showing both monitors
// crammed into one window
pub fn gui_is_wayland() -> bool {
    if std::env::var("GDK_BACKEND")
        .map(|backend| backend.eq_ignore_ascii_case("x11"))
        .unwrap_or(false)
    {
        return false;
    }
    is_wayland_session()
}

// run one portal request/response roundtrip on the given interface. the
// request object path is derivable up front, so the Response signal is
// subscribed to before the call — otherwise a fast portal could reply before
// the subscription lands. build_body receives the handle token and returns
// the full argument tuple (its options dict must carry that token). returns
// the raw Response message so callers deserialize their own result shape.
pub(crate) fn portal_request<B>(
    conn: &zbus::blocking::Connection,
    interface: &str,
    method: &str,
    build_body: impl FnOnce(String) -> B,
) -> Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let unique = conn
        .unique_name()
        .ok_or_else(|| anyhow!("session bus connection has no unique name"))?
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!(
        "capscr_{}_{}",
        std::process::id(),
        REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let request_path = format!("/org/freedesktop/portal/desktop/request/{unique}/{token}");
    let request_proxy = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = request_proxy.receive_signal("Response")?;

    let _handle: OwnedObjectPath = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some(interface),
            method,
            &build_body(token.clone()),
        )?
        .body()
        .deserialize()?;

    // the portal always answers the request — with a nonzero code when the
    // user dismisses its dialog — so this blocks only while one is on screen
    responses
        .next()
        .ok_or_else(|| anyhow!("portal connection closed before responding"))
}

pub fn portal_screenshot() -> Result<RgbaImage> {
    screenshot_request(false)
}

// interactive mode surfaces the desktop's own screenshot dialog (on gnome
// that includes its window picker); the result is whatever the user chose
pub fn portal_screenshot_interactive() -> Result<RgbaImage> {
    screenshot_request(true)
}

fn screenshot_request(interactive: bool) -> Result<RgbaImage> {
    let conn = zbus::blocking::Connection::session()?;
    let msg = portal_request(
        &conn,
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            options.insert("interactive", Value::from(interactive));
            ("", options)
        },
    )?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("screenshot request was denied or cancelled"));
    }
    let uri = results
        .get("uri")
        .and_then(|v| v.downcast_ref::<String>().ok())
        .ok_or_else(|| anyhow!("portal response carried no uri"))?;
    let path = url::Url::parse(&uri)?
        .to_file_path()
        .map_err(|_| anyhow!("portal uri is not a local file: {uri}"))?;
    let img = image::open(&path)?.to_rgba8();
    // the portal writes a one-off file (typically under ~/Pictures); it's
    // ours to clean up once the pixels are in memory
    let _ = std::fs::remove_file(&path);
    Ok(img)
}

#[cfg(test)]
mod tests {
    // needs a live portal (real desktop or a compositor test rig), so it only
    // runs when the environment opts in
    #[test]
    fn portal_screenshot_returns_pixels() {
        if std::env::var("CAPSCR_TEST_PORTAL").is_err() {
            return;
        }
        let img = super::portal_screenshot().expect("portal screenshot");
        eprintln!("portal returned {}x{}", img.width(), img.height());
        assert!(img.width() > 0 && img.height() > 0);
    }
}
