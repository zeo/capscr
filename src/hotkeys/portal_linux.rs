// org.freedesktop.portal.GlobalShortcuts backend: the sanctioned wayland
// hotkey path (plasma 6, gnome 46+). the compositor owns the real triggers,
// persists them per (app_id, shortcut_id), and fires Activated signals; our
// task ids double as shortcut ids so bindings survive restarts. capscr's
// hotkey string travels as the preferred trigger; the desktop may remap it,
// and whatever is effective comes back as a human-readable description
// surfaced in the diagnostics table.
//
// BindShortcuts pops an approval dialog on gnome (kde applies silently), so
// it only runs when the desired set differs from what the compositor
// already knows (ListShortcuts) joined with our persisted record — an
// unchanged set never re-prompts, not on launch, not on unrelated saves.
// the call blocks the hotkey thread until the dialog resolves, which keeps
// the outcome synchronous: bound-with-trigger or failed, no pending state.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::capture::portal_request;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortalBinding {
    pub task_id: String,
    pub label: String,
    pub trigger: Option<String>,
}

struct PortalSession {
    conn: zbus::blocking::Connection,
    session: OwnedObjectPath,
}

static SESSION: Mutex<Option<PortalSession>> = Mutex::new(None);
static EFFECTIVE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static LAST_SYNCED: Mutex<Vec<PortalBinding>> = Mutex::new(Vec::new());
static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn start(app: AppHandle) {
    let _ = APP.set(app);
}

pub fn available() -> bool {
    crate::shell::global_shortcuts_portal().is_some()
}

// task_id -> the compositor's human-readable description of the effective
// trigger, for the diagnostics table
pub fn effective_triggers() -> HashMap<String, String> {
    EFFECTIVE.lock().unwrap().clone().unwrap_or_default()
}

fn record_path() -> Option<std::path::PathBuf> {
    crate::config::Config::config_dir().map(|dir| dir.join("portal-shortcuts.json"))
}

fn load_record() -> Vec<PortalBinding> {
    let Some(path) = record_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

fn store_record(bindings: &[PortalBinding]) {
    if let Some(path) = record_path() {
        if let Ok(body) = serde_json::to_string_pretty(bindings) {
            let _ = std::fs::write(path, body);
        }
    }
}

// bind the given keyboard set, returning per-task failures. an empty set
// clears the effective table and leaves the compositor's persisted bindings
// alone (no task matches their ids, so activations are ignored).
pub fn sync(bindings: Vec<PortalBinding>) -> Vec<(String, String)> {
    *LAST_SYNCED.lock().unwrap() = bindings.clone();
    if bindings.is_empty() {
        *EFFECTIVE.lock().unwrap() = Some(HashMap::new());
        return Vec::new();
    }
    match sync_inner(&bindings, false) {
        Ok(()) => Vec::new(),
        Err(e) => {
            let reason = format!("{e:#}");
            tracing::warn!("portal shortcut sync failed: {reason}");
            bindings
                .into_iter()
                .map(|b| (b.task_id, reason.clone()))
                .collect()
        }
    }
}

// the settings retry button: force a BindShortcuts even for an unchanged
// set, so a user who dismissed the desktop's approval dialog can reopen it
pub fn rebind() -> Result<()> {
    let bindings = LAST_SYNCED.lock().unwrap().clone();
    if bindings.is_empty() {
        return Err(anyhow!("no keyboard shortcuts are configured"));
    }
    sync_inner(&bindings, true)
}

fn sync_inner(bindings: &[PortalBinding], force: bool) -> Result<()> {
    let mut guard = SESSION.lock().unwrap();
    if guard.is_none() {
        *guard = Some(create_session()?);
    }
    let session = guard.as_ref().unwrap();

    // the compositor already knowing every id, with our record agreeing on
    // the preferred triggers, means nothing changed: skip the (potentially
    // dialog-showing) bind entirely
    let known = list_shortcuts(session).unwrap_or_default();
    let record = load_record();
    let unchanged = !force
        && bindings
            .iter()
            .all(|b| known.contains_key(&b.task_id) && record.contains(b))
        && record.len() == bindings.len();
    if unchanged {
        let effective = bindings
            .iter()
            .filter_map(|b| {
                known
                    .get(&b.task_id)
                    .map(|trigger| (b.task_id.clone(), trigger.clone()))
            })
            .collect();
        *EFFECTIVE.lock().unwrap() = Some(effective);
        return Ok(());
    }

    let shortcuts: Vec<(String, HashMap<&str, Value>)> = bindings
        .iter()
        .map(|b| {
            let mut props: HashMap<&str, Value> = HashMap::new();
            props.insert("description", Value::from(b.label.clone()));
            if let Some(trigger) = &b.trigger {
                props.insert("preferred_trigger", Value::from(trigger.clone()));
            }
            (b.task_id.clone(), props)
        })
        .collect();
    let msg = portal_request(
        &session.conn,
        "org.freedesktop.portal.GlobalShortcuts",
        "BindShortcuts",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            (&session.session, shortcuts, "", options)
        },
    )?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!(
            "shortcut binding was not approved — retry from Settings once ready"
        ));
    }
    *EFFECTIVE.lock().unwrap() = Some(parse_shortcut_list(results.get("shortcuts")));
    store_record(bindings);
    tracing::info!("portal bound {} shortcut(s)", bindings.len());
    Ok(())
}

fn create_session() -> Result<PortalSession> {
    let conn = zbus::blocking::Connection::session()?;
    let msg = portal_request(
        &conn,
        "org.freedesktop.portal.GlobalShortcuts",
        "CreateSession",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token.clone()));
            options.insert("session_handle_token", Value::from(token));
            (options,)
        },
    )?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("global-shortcuts session was denied"));
    }
    let session: OwnedObjectPath = results
        .get("session_handle")
        .and_then(|v| v.downcast_ref::<String>().ok())
        .ok_or_else(|| anyhow!("portal returned no session handle"))?
        .try_into()?;
    spawn_listeners(&conn, &session);
    Ok(PortalSession { conn, session })
}

fn list_shortcuts(session: &PortalSession) -> Result<HashMap<String, String>> {
    let msg = portal_request(
        &session.conn,
        "org.freedesktop.portal.GlobalShortcuts",
        "ListShortcuts",
        |token| {
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert("handle_token", Value::from(token));
            (&session.session, options)
        },
    )?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(anyhow!("shortcut listing failed"));
    }
    Ok(parse_shortcut_list(results.get("shortcuts")))
}

// shortcuts come back as a(sa{sv}); pull id -> trigger_description
fn parse_shortcut_list(value: Option<&OwnedValue>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(value) = value else {
        return map;
    };
    let Ok(entries) =
        <Vec<(String, HashMap<String, OwnedValue>)>>::try_from(value.try_clone().unwrap_or_else(
            |_| OwnedValue::from(0u32),
        ))
    else {
        return map;
    };
    for (id, props) in entries {
        let trigger = props
            .get("trigger_description")
            .and_then(|v| v.downcast_ref::<String>().ok())
            .unwrap_or_default();
        map.insert(id, trigger);
    }
    map
}

// one thread per signal: Activated fires tasks with the same kill-switch and
// auto-repeat dedupe the other dispatchers apply; Closed drops the session so
// the next reload recreates and rebinds it
fn spawn_listeners(conn: &zbus::blocking::Connection, session: &OwnedObjectPath) {
    let activated_conn = conn.clone();
    let our_session = session.clone();
    std::thread::Builder::new()
        .name("capscr-portal-activated".into())
        .spawn(move || {
            let Ok(proxy) = zbus::blocking::Proxy::new(
                &activated_conn,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.GlobalShortcuts",
            ) else {
                return;
            };
            let Ok(signals) = proxy.receive_signal("Activated") else {
                return;
            };
            let mut last_fire: HashMap<String, std::time::Instant> = HashMap::new();
            for msg in signals {
                let Ok((session, shortcut_id, _timestamp, _options)) = msg
                    .body()
                    .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
                else {
                    continue;
                };
                if session != our_session {
                    continue;
                }
                let Some(app) = APP.get() else { continue };
                let state = app.state::<crate::state::AppState>();
                if state
                    .hotkeys_disabled
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    continue;
                }
                let now = std::time::Instant::now();
                let allow = last_fire
                    .get(&shortcut_id)
                    .map(|t| now.duration_since(*t).as_millis() > 250)
                    .unwrap_or(true);
                if !allow {
                    continue;
                }
                last_fire.insert(shortcut_id.clone(), now);
                crate::commands::trigger_task(app, &shortcut_id);
            }
        })
        .ok();

    let closed_conn = conn.clone();
    let closed_session = session.clone();
    std::thread::Builder::new()
        .name("capscr-portal-closed".into())
        .spawn(move || {
            let Ok(proxy) = zbus::blocking::Proxy::new(
                &closed_conn,
                "org.freedesktop.portal.Desktop",
                closed_session.as_ref(),
                "org.freedesktop.portal.Session",
            ) else {
                return;
            };
            let Ok(mut signals) = proxy.receive_signal("Closed") else {
                return;
            };
            if signals.next().is_some() {
                tracing::warn!("global-shortcuts session closed by the portal; rebinding");
                *SESSION.lock().unwrap() = None;
                let bindings = LAST_SYNCED.lock().unwrap().clone();
                if !bindings.is_empty() {
                    let _ = sync_inner(&bindings, false);
                }
            }
        })
        .ok();
}
