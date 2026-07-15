#![allow(dead_code, unused_imports)]

#[cfg(target_os = "linux")]
pub mod linux;
pub mod recording;
mod unified;
#[cfg(target_os = "linux")]
mod wayland_native_selector;

pub use recording::RecordingOverlay;
pub use unified::{SelectionResult, UnifiedSelector};
