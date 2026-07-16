// ordered wayland pixel-source selection. the order is computed once from
// what the desktop actually offers (kwin's screenshot service, advertised
// globals) instead of hardcoding compositor names, so a compositor gaining a
// protocol upgrades capscr for free. every hop stays black-frame-guarded by
// the callers' is_black_frame checks exactly as before; this module only
// decides who gets asked, in which order, and reports who answered.

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use image::RgbaImage;

use super::ext_copy::ExtCopySession;
use super::{
    include_cursor, is_black_frame, kwin, list_monitors, portal_grab_monitor,
    wlroots_freeze_output, wlroots_grab, MonitorInfo, Rectangle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    KwinScreenshot2,
    ExtImageCopy,
    WlrScreencopy,
    PortalScreenshot,
}

impl SourceKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::KwinScreenshot2 => "kwin-screenshot2",
            Self::ExtImageCopy => "ext-image-copy",
            Self::WlrScreencopy => "wlr-screencopy",
            Self::PortalScreenshot => "portal-screenshot",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "kwin-screenshot2" => Some(Self::KwinScreenshot2),
            "ext-image-copy" => Some(Self::ExtImageCopy),
            "wlr-screencopy" => Some(Self::WlrScreencopy),
            "portal-screenshot" => Some(Self::PortalScreenshot),
            _ => None,
        }
    }
}

fn compute_order(kwin_service: bool, caps: crate::shell::WaylandGlobals) -> Vec<SourceKind> {
    let mut order = Vec::new();
    if kwin_service {
        order.push(SourceKind::KwinScreenshot2);
    }
    if caps.ext_image_copy {
        order.push(SourceKind::ExtImageCopy);
    }
    if caps.wlr_screencopy {
        order.push(SourceKind::WlrScreencopy);
    }
    // the portal answers everywhere a portal backend runs; keep it as the
    // unconditional last resort
    order.push(SourceKind::PortalScreenshot);
    order
}

// CAPSCR_FORCE_SOURCE pins the chain to one source, bypassing the capability
// probe, so a rig can prove a fallback path stays alive on a compositor that
// also offers better ones
pub(crate) fn still_order() -> &'static [SourceKind] {
    static ORDER: OnceLock<Vec<SourceKind>> = OnceLock::new();
    ORDER.get_or_init(|| {
        if let Ok(forced) = std::env::var("CAPSCR_FORCE_SOURCE") {
            if let Some(kind) = SourceKind::from_name(forced.trim()) {
                tracing::info!("CAPSCR_FORCE_SOURCE pins the wayland source to {}", kind.name());
                return vec![kind];
            }
            tracing::warn!("CAPSCR_FORCE_SOURCE={forced:?} names no known source; ignoring");
        }
        let order = compute_order(
            crate::shell::kwin_screenshot2_available(),
            crate::shell::wayland_globals(),
        );
        tracing::info!(
            "wayland still-source order: {:?}",
            order.iter().map(|k| k.name()).collect::<Vec<_>>(),
        );
        order
    })
}

// persistent ext-image-copy session shared by the still paths; recreated once
// on failure so a compositor restart doesn't wedge the source
static EXT_STILL: Mutex<Option<ExtCopySession>> = Mutex::new(None);

fn ext_grab_output(name: &str, cursor: bool) -> Result<RgbaImage> {
    let mut slot = EXT_STILL.lock().unwrap();
    if slot.is_none() {
        *slot = Some(ExtCopySession::new()?);
    }
    match slot.as_mut().unwrap().grab_output(name, cursor) {
        Ok(img) => Ok(img),
        Err(_) => {
            *slot = Some(ExtCopySession::new()?);
            slot.as_mut().unwrap().grab_output(name, cursor)
        }
    }
}

fn monitor_by_name(name: &str) -> Result<MonitorInfo> {
    list_monitors()?
        .into_iter()
        .find(|monitor| monitor.name == name)
        .ok_or_else(|| anyhow!("output {name} is not in the monitor list"))
}

// run one still grab through the ordered chain. grab returns the image for a
// given source kind; all-black frames demote to the next source like the
// hand-rolled chains this replaces
fn chain_grab(
    what: &str,
    grab: impl Fn(SourceKind) -> Result<RgbaImage>,
) -> Result<RgbaImage> {
    let order = still_order();
    let mut last_error = anyhow!("no wayland pixel source available");
    for kind in order {
        match grab(*kind) {
            Ok(img) if !is_black_frame(&img) => {
                tracing::info!("wayland still source={} {what}", kind.name());
                return Ok(img);
            }
            Ok(_) => {
                tracing::warn!("{} returned all-black for {what}; trying next", kind.name());
                last_error = anyhow!("{} returned an all-black frame", kind.name());
            }
            Err(e) => {
                tracing::debug!("{} unavailable for {what} ({e:#}); trying next", kind.name());
                last_error = e;
            }
        }
    }
    Err(last_error)
}

// logical-rect grab of one monitor (the capture_one_monitor path)
pub(crate) fn grab_monitor(monitor: &MonitorInfo) -> Result<RgbaImage> {
    chain_grab(&format!("monitor {}", monitor.name), |kind| match kind {
        SourceKind::KwinScreenshot2 => kwin::capture_area(
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
            include_cursor(),
        ),
        SourceKind::ExtImageCopy => ext_grab_output(&monitor.name, include_cursor()),
        SourceKind::WlrScreencopy => wlroots_grab(monitor),
        SourceKind::PortalScreenshot => portal_grab_monitor(monitor),
    })
}

// native-resolution freeze of one output by name (the selector path)
pub(crate) fn freeze_output(name: &str) -> Result<RgbaImage> {
    chain_grab(&format!("output {name}"), |kind| match kind {
        SourceKind::KwinScreenshot2 => kwin::capture_screen(name, include_cursor()),
        SourceKind::ExtImageCopy => ext_grab_output(name, include_cursor()),
        SourceKind::WlrScreencopy => wlroots_freeze_output(name),
        SourceKind::PortalScreenshot => portal_grab_monitor(&monitor_by_name(name)?),
    })
}

// persistent per-recording frame source, replacing the split x11/wayland
// grabber pair in the recording loop. the constructor probes one frame so a
// broken backend fails the whole source and the loop falls back to the
// generic grab-and-crop path
pub(crate) enum RecordingSource {
    X11(super::X11RegionGrabber),
    Kwin(kwin::KwinRegionGrabber),
    ExtCopy(ExtCopySession),
    Screencopy(libwayshot_xcap::WayshotConnection),
}

impl RecordingSource {
    pub(crate) fn new(region: Rectangle, cursor: bool) -> Result<Self> {
        if !super::is_wayland_session() {
            // the XWayland root only contains X clients, so this arm must
            // never run under wayland
            let grabber = super::X11RegionGrabber::new()?;
            let probe = grabber.grab(region.x, region.y, region.width, region.height)?;
            if is_black_frame(&probe) {
                return Err(anyhow!("x11 region grab is all-black"));
            }
            tracing::info!("recording source=x11-getimage");
            return Ok(Self::X11(grabber));
        }
        let mut last_error = anyhow!("no wayland recording source available");
        for kind in still_order() {
            let candidate = match kind {
                SourceKind::KwinScreenshot2 => {
                    kwin::KwinRegionGrabber::new().map(Self::Kwin)
                }
                SourceKind::ExtImageCopy => ExtCopySession::new().map(Self::ExtCopy),
                SourceKind::WlrScreencopy => libwayshot_xcap::WayshotConnection::new()
                    .map(Self::Screencopy)
                    .map_err(Into::into),
                // a one-shot screenshot dialog per frame is no recording
                // source; the generic fallback path handles portal-only
                // desktops until the screencast source lands
                SourceKind::PortalScreenshot => continue,
            };
            let mut candidate = match candidate {
                Ok(candidate) => candidate,
                Err(e) => {
                    last_error = e;
                    continue;
                }
            };
            match candidate.grab(region.x, region.y, region.width, region.height, cursor) {
                Ok(probe) if !is_black_frame(&probe) => {
                    tracing::info!("recording source={}", kind.name());
                    return Ok(candidate);
                }
                Ok(_) => last_error = anyhow!("{} probe frame is all-black", kind.name()),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    pub(crate) fn grab(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        cursor: bool,
    ) -> Result<RgbaImage> {
        match self {
            Self::X11(grabber) => grabber.grab(x, y, width, height),
            Self::Kwin(grabber) => grabber.grab(x, y, width, height, cursor),
            Self::ExtCopy(session) => session.grab_area(x, y, width, height, cursor),
            Self::Screencopy(conn) => {
                use libwayshot_xcap::region::{LogicalRegion, Position, Region, Size};
                let region = LogicalRegion {
                    inner: Region {
                        position: Position { x, y },
                        size: Size { width, height },
                    },
                };
                let img = conn.screenshot(region, cursor)?;
                let rgba = img.to_rgba8();
                RgbaImage::from_raw(rgba.width(), rgba.height(), rgba.into_vec())
                    .ok_or_else(|| anyhow!("screencopy buffer size mismatch"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_order, SourceKind};
    use crate::shell::WaylandGlobals;

    #[test]
    fn order_follows_capabilities() {
        // kde: screenshot2 first, ext/screencopy never advertised
        let kde = compute_order(true, WaylandGlobals::default());
        assert_eq!(
            kde,
            vec![SourceKind::KwinScreenshot2, SourceKind::PortalScreenshot],
        );
        // wlroots: modern protocol ahead of deprecated screencopy
        let wlroots = compute_order(
            false,
            WaylandGlobals {
                ext_image_copy: true,
                wlr_screencopy: true,
                ..Default::default()
            },
        );
        assert_eq!(
            wlroots,
            vec![
                SourceKind::ExtImageCopy,
                SourceKind::WlrScreencopy,
                SourceKind::PortalScreenshot,
            ],
        );
        // gnome: nothing advertised, portal carries stills
        let gnome = compute_order(false, WaylandGlobals::default());
        assert_eq!(gnome, vec![SourceKind::PortalScreenshot]);
    }
}
