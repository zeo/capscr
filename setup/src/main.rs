//! capscr's installer: a small branded window wrapped around the signed MSI
//! tauri builds — the pretty first-install experience, while the in-app
//! updater keeps consuming the very same MSI chain underneath. modes:
//!   (none)        interactive install
//!   /S            fully silent install (no window), scripting-friendly
//!   /uninstall    confirm + remove
//!   --preview P.png [--page NAME] [--scale N]   headless design render
#![windows_subsystem = "windows"]

mod engine;
mod ui;

use std::sync::{Arc, Mutex};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));
    let val = |f: &str| -> Option<String> {
        args.iter()
            .position(|a| a.eq_ignore_ascii_case(f))
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    if let Some(out) = val("--preview") {
        let page = match val("--page").as_deref() {
            Some("progress") => ui::Page::Progress,
            Some("done") => ui::Page::Done,
            Some("error") => ui::Page::Error,
            Some("remove") => ui::Page::RemoveConfirm,
            Some("removed") => ui::Page::Removed,
            _ => ui::Page::Welcome,
        };
        let scale: f32 = val("--scale").and_then(|v| v.parse().ok()).unwrap_or(2.0);
        let mut s = ui::State::new(page);
        s.sweep = 0.55;
        s.error = "msiexec exited with code 1603".into();
        let _ = ui::preview(&out, &s, scale);
        return;
    }

    // classic silent switch so scripts and tools can drive this exe directly
    if has("/S") {
        std::process::exit(match engine::install() {
            Ok(()) => 0,
            Err(_) => 1,
        });
    }

    let state = if has("/uninstall") || has("--uninstall") {
        ui::State::new(ui::Page::RemoveConfirm)
    } else {
        ui::State::new(ui::Page::Welcome)
    };
    ui::run(Arc::new(Mutex::new(state)));
}
