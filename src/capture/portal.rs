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
        || std::env::var("XDG_SESSION_TYPE").map(|t| t == "wayland").unwrap_or(false)
}

pub fn portal_screenshot() -> Result<RgbaImage> {
    let conn = zbus::blocking::Connection::session()?;
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
    // the request object path is derivable up front, so the Response signal
    // can be subscribed to before the call — otherwise a fast portal could
    // reply before the subscription lands
    let request_path = format!("/org/freedesktop/portal/desktop/request/{unique}/{token}");
    let request_proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = request_proxy.receive_signal("Response")?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("interactive", Value::from(false));
    let _handle: OwnedObjectPath = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Screenshot"),
            "Screenshot",
            &("", options),
        )?
        .body()
        .deserialize()?;

    // the portal always answers the request — with code 1 when the user
    // dismisses the permission prompt — so this blocks only for as long as
    // the prompt is on screen
    let msg = responses
        .next()
        .ok_or_else(|| anyhow!("portal connection closed before responding"))?;
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
