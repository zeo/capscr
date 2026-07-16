// consumers arrive with the capture-chain, hotkey-portal, and tray phases;
// drop this once they land
#![allow(dead_code)]

// one home for "what can this desktop do for us". every probe is lazy and
// cached for the life of the process, so nothing here costs anything unless a
// caller asks, and repeat callers never pay the bus or registry roundtrip
// twice. capture source ordering, overlay pinning, hotkey backend selection,
// and tray detection all key off these answers instead of probing ad hoc.

pub mod layer_shell;
pub mod tray_detect;

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopEnv {
    Kde,
    Gnome,
    Wlroots,
    Other,
}

// XDG_CURRENT_DESKTOP is a colon-separated list of increasingly generic
// names ("ubuntu:GNOME"). match case-insensitively on any entry. wlroots
// compositors don't share a marker string, so the known family members are
// listed; unknown compositors land in Other and are treated by capability
// (wayland_globals) rather than by name.
fn parse_desktop(raw: &str) -> DesktopEnv {
    for entry in raw.split(':') {
        let entry = entry.trim().to_ascii_lowercase();
        match entry.as_str() {
            "kde" | "plasma" => return DesktopEnv::Kde,
            "gnome" => return DesktopEnv::Gnome,
            "sway" | "hyprland" | "wlroots" | "river" | "wayfire" | "labwc" | "niri" => {
                return DesktopEnv::Wlroots
            }
            _ => {}
        }
    }
    DesktopEnv::Other
}

pub fn desktop() -> DesktopEnv {
    static DESKTOP: OnceLock<DesktopEnv> = OnceLock::new();
    *DESKTOP.get_or_init(|| {
        parse_desktop(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WaylandGlobals {
    pub plasma_shell: bool,
    pub layer_shell: bool,
    pub ext_image_copy: bool,
    pub wlr_screencopy: bool,
    pub color_management: bool,
}

// advertised wayland globals, from one throwaway connection's registry
// roundtrip. an x11 session (or a broken wayland socket) reports everything
// absent, which every caller treats as "use the x11 or portal path".
pub fn wayland_globals() -> WaylandGlobals {
    static GLOBALS: OnceLock<WaylandGlobals> = OnceLock::new();
    *GLOBALS.get_or_init(probe_wayland_globals)
}

fn probe_wayland_globals() -> WaylandGlobals {
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::{Connection, Dispatch, QueueHandle};

    struct Probe;
    impl Dispatch<WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    let Ok(connection) = Connection::connect_to_env() else {
        return WaylandGlobals::default();
    };
    let Ok((globals, _queue)) = registry_queue_init::<Probe>(&connection) else {
        return WaylandGlobals::default();
    };
    let mut caps = WaylandGlobals::default();
    globals.contents().with_list(|list| {
        for global in list {
            match global.interface.as_str() {
                "org_kde_plasma_shell" => caps.plasma_shell = true,
                "zwlr_layer_shell_v1" => caps.layer_shell = true,
                "ext_image_copy_capture_manager_v1" => caps.ext_image_copy = true,
                "zwlr_screencopy_manager_v1" => caps.wlr_screencopy = true,
                "wp_color_manager_v1" => caps.color_management = true,
                _ => {}
            }
        }
    });
    tracing::debug!(
        "wayland globals: plasma_shell={} layer_shell={} ext_image_copy={} wlr_screencopy={} color_management={}",
        caps.plasma_shell,
        caps.layer_shell,
        caps.ext_image_copy,
        caps.wlr_screencopy,
        caps.color_management,
    );
    caps
}

// kwin's authorized screenshot service. presence of the bus name is what
// matters; whether this process is actually granted the interface only
// surfaces on the first call, and the capture chain treats that failure as
// "next source" anyway
pub fn kwin_screenshot2_available() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        let Ok(connection) = zbus::blocking::Connection::session() else {
            return false;
        };
        connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &("org.kde.KWin.ScreenShot2",),
            )
            .ok()
            .and_then(|reply| reply.body().deserialize::<bool>().ok())
            .unwrap_or(false)
    })
}

// kwin's version, parsed out of supportInformation ("KWin version: 6.7.2").
// None off kde or when the call fails
pub fn kwin_version() -> Option<(u32, u32)> {
    static VERSION: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let connection = zbus::blocking::Connection::session().ok()?;
        let reply = connection
            .call_method(Some("org.kde.KWin"), "/KWin", Some("org.kde.KWin"), "supportInformation", &())
            .ok()?;
        let info: String = reply.body().deserialize().ok()?;
        let line = info.lines().find_map(|l| l.strip_prefix("KWin version: "))?;
        let mut parts = line.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some((major, minor))
    })
}

// plasma 6.6 let windows opt out of screencasts (Window.excludeFromCapture);
// 6.7 extended that to still screenshots, which is what the recording loop's
// ScreenShot2 grabs are. only 6.7+ makes an in-region recording bar invisible
pub fn kwin_still_capture_exclusion() -> bool {
    kwin_version().is_some_and(|version| version >= (6, 7))
}

// version of the GlobalShortcuts portal, or None when the desktop's portal
// backend doesn't implement it (plasma < 6, gnome < 46, most wlroots stacks)
pub fn global_shortcuts_portal() -> Option<u32> {
    static VERSION: OnceLock<Option<u32>> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let connection = zbus::blocking::Connection::session().ok()?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
        )
        .ok()?;
        proxy.get_property::<u32>("version").ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_desktop, DesktopEnv};

    #[test]
    fn desktop_string_parsing() {
        assert_eq!(parse_desktop("KDE"), DesktopEnv::Kde);
        assert_eq!(parse_desktop("ubuntu:GNOME"), DesktopEnv::Gnome);
        assert_eq!(parse_desktop("GNOME-Flashback:GNOME"), DesktopEnv::Gnome);
        assert_eq!(parse_desktop("sway"), DesktopEnv::Wlroots);
        assert_eq!(parse_desktop("Hyprland"), DesktopEnv::Wlroots);
        assert_eq!(parse_desktop("niri"), DesktopEnv::Wlroots);
        assert_eq!(parse_desktop("X-Cinnamon"), DesktopEnv::Other);
        assert_eq!(parse_desktop(""), DesktopEnv::Other);
    }
}
