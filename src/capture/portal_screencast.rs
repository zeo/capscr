// xdg-desktop-portal ScreenCast session: the sanctioned recording pixel
// source on desktops with no in-process capture protocol (gnome above all).
// the portal hands back a pipewire node plus a restore token; the token is
// persisted so the second and every later recording starts without the
// source-picker dialog. xcap ships its own screencast recorder but omits
// persist/restore, cursor embedding, and the portal's pipewire fd, so this
// is hand-rolled on the same blocking-zbus pattern as portal.rs.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, Type, Value};

use super::portal::portal_request;

const SOURCE_TYPE_MONITOR: u32 = 1;
const PERSIST_UNTIL_REVOKED: u32 = 2;
const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct CreateSessionResults {
    session_handle: Option<String>,
}

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct StreamProperties {
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
struct StartResults {
    streams: Option<Vec<(u32, StreamProperties)>>,
    restore_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub node_id: u32,
    // logical desktop coordinates of the stream's source, for mapping a
    // capture region onto buffer pixels
    pub position: (i32, i32),
    pub size: (i32, i32),
}

pub struct ScreenCastSession {
    conn: zbus::blocking::Connection,
    session_handle: OwnedObjectPath,
    pub stream: StreamInfo,
    pub pipewire_fd: std::os::fd::OwnedFd,
}

impl Drop for ScreenCastSession {
    fn drop(&mut self) {
        let _ = self.conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            self.session_handle.as_ref(),
            Some("org.freedesktop.portal.Session"),
            "Close",
            &(),
        );
    }
}

// where the restore token lives between runs. deliberately its own file in
// the config dir rather than a config.toml field: the token is a
// machine-local opaque secret the user should never edit or sync
fn token_path() -> Option<std::path::PathBuf> {
    crate::config::Config::config_dir().map(|dir| dir.join("screencast-restore-token"))
}

fn load_restore_token() -> Option<String> {
    let token = std::fs::read_to_string(token_path()?).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn store_restore_token(token: &str) {
    if let Some(path) = token_path() {
        if let Err(e) = std::fs::write(&path, token) {
            tracing::warn!("couldn't persist the screencast restore token: {e:#}");
        }
    }
}

// open a monitor screencast session. shows the desktop's source picker on
// first use; afterwards the stored token restores the same source silently
// (a stale or revoked token is simply ignored by the portal and the picker
// returns). the caller picks which monitor in the picker; a specific one
// can't be requested programmatically.
pub fn open_monitor_session(embed_cursor: bool) -> Result<ScreenCastSession> {
    let conn = zbus::blocking::Connection::session()?;

    let msg = portal_request(
        &conn,
        "org.freedesktop.portal.ScreenCast",
        "CreateSession",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            options.insert(
                "session_handle_token",
                Value::from(format!("capscr_{}", std::process::id())),
            );
            (options,)
        },
    )?;
    let (code, results): (u32, CreateSessionResults) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("screencast session was denied"));
    }
    let session_handle: OwnedObjectPath = results
        .session_handle
        .ok_or_else(|| anyhow!("portal returned no session handle"))?
        .try_into()?;

    let msg = portal_request(
        &conn,
        "org.freedesktop.portal.ScreenCast",
        "SelectSources",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            options.insert("types", Value::from(SOURCE_TYPE_MONITOR));
            options.insert("multiple", Value::from(false));
            options.insert(
                "cursor_mode",
                Value::from(if embed_cursor {
                    CURSOR_EMBEDDED
                } else {
                    CURSOR_HIDDEN
                }),
            );
            options.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
            if let Some(token) = load_restore_token() {
                options.insert("restore_token", Value::from(token));
            }
            (&session_handle, options)
        },
    )?;
    let (code, _): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("screencast source selection was cancelled"));
    }

    let msg = portal_request(
        &conn,
        "org.freedesktop.portal.ScreenCast",
        "Start",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            (&session_handle, "", options)
        },
    )?;
    let (code, results): (u32, StartResults) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("screencast start was cancelled"));
    }
    if let Some(token) = &results.restore_token {
        store_restore_token(token);
    }
    let (node_id, properties) = results
        .streams
        .unwrap_or_default()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("portal returned no screencast stream"))?;
    let stream = StreamInfo {
        node_id,
        position: properties.position.unwrap_or((0, 0)),
        size: properties.size.unwrap_or((0, 0)),
    };

    let pipewire_fd: zbus::zvariant::OwnedFd = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            session_handle.as_ref(),
            Some("org.freedesktop.portal.ScreenCast"),
            "OpenPipeWireRemote",
            &(HashMap::<&str, Value>::new(),),
        )?
        .body()
        .deserialize()?;

    Ok(ScreenCastSession {
        conn,
        session_handle,
        stream,
        pipewire_fd: pipewire_fd.into(),
    })
}
