// persistent X11 region grabber for the recording loop. the generic per-frame
// path (xcap capture of a whole monitor, then crop) costs seconds per frame on
// some servers; one long-lived connection issuing GetImage for just the
// recorded rectangle keeps a 10-60fps cadence honest.

use anyhow::{anyhow, Result};
use image::RgbaImage;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

pub struct X11RegionGrabber {
    conn: RustConnection,
    root: Window,
}

impl X11RegionGrabber {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let screen = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X11 screen {screen_num} missing"))?;
        Ok(Self {
            root: screen.root,
            conn,
        })
    }

    // grab a root-relative rectangle as opaque RGBA. the rect is clamped to
    // the root's CURRENT bounds (queried per grab — servers like WSLg resize
    // the root dynamically, so the connection-setup dimensions go stale)
    // because GetImage errors on any out-of-bounds pixel
    pub fn grab(&self, x: i32, y: i32, width: u32, height: u32) -> Result<RgbaImage> {
        let geom = self.conn.get_geometry(self.root)?.reply()?;
        let (root_w, root_h) = (geom.width as i32, geom.height as i32);
        let x0 = x.clamp(0, root_w - 1);
        let y0 = y.clamp(0, root_h - 1);
        let w = width.min((root_w - x0) as u32).max(1) as u16;
        let h = height.min((root_h - y0) as u32).max(1) as u16;

        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                x0 as i16,
                y0 as i16,
                w,
                h,
                !0,
            )?
            .reply()?;
        if reply.depth != 24 && reply.depth != 32 {
            return Err(anyhow!("unsupported root depth {}", reply.depth));
        }
        let expected = (w as usize) * (h as usize) * 4;
        if reply.data.len() < expected {
            return Err(anyhow!(
                "short GetImage reply: {} < {expected}",
                reply.data.len()
            ));
        }
        // ZPixmap at depth 24/32 on little-endian servers is BGRX
        let mut rgba = Vec::with_capacity(expected);
        for px in reply.data[..expected].chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        RgbaImage::from_raw(w as u32, h as u32, rgba)
            .ok_or_else(|| anyhow!("frame buffer size mismatch"))
    }
}
