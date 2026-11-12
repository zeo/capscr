// gtk-layer-shell loaded at runtime via dlopen so wlroots compositors get
// proper surface layering without adding a hard link-time dependency that
// kde and gnome sessions would never use. a layer role must be applied
// before the window is first mapped (init_for_window unrealizes it), which
// every caller satisfies by building windows with visible(false).
//
// raw zwlr_layer_shell_v1 is not an option for these windows: a layer
// surface is a wl_surface role, and gtk's window already holds the
// xdg_toplevel role by the time we see it. gtk-layer-shell exists precisely
// to re-plumb the window through the layer protocol before mapping.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::OnceLock;

// gtk-layer-shell edge / layer / keyboard-mode constants
const EDGE_LEFT: c_int = 0;
const EDGE_RIGHT: c_int = 1;
const EDGE_TOP: c_int = 2;
const EDGE_BOTTOM: c_int = 3;
pub const LAYER_TOP: c_int = 2;
pub const LAYER_OVERLAY: c_int = 3;
const KEYBOARD_NONE: c_int = 0;
const KEYBOARD_ON_DEMAND: c_int = 2;

struct LayerShellApi {
    _library: usize,
    is_supported: unsafe extern "C" fn() -> c_int,
    init_for_window: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow),
    set_layer: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int),
    set_anchor: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int),
    set_margin: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int),
    set_monitor: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *mut gtk::gdk::ffi::GdkMonitor),
    set_keyboard_mode: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int),
    set_namespace: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *const c_char),
}

unsafe impl Send for LayerShellApi {}
unsafe impl Sync for LayerShellApi {}

static LAYER_SHELL: OnceLock<Option<LayerShellApi>> = OnceLock::new();

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn layer_shell_api() -> Option<&'static LayerShellApi> {
    LAYER_SHELL
        .get_or_init(|| unsafe {
            let library_name = CString::new("libgtk-layer-shell.so.0").unwrap();
            let library = dlopen(library_name.as_ptr(), 2);
            if library.is_null() {
                return None;
            }
            macro_rules! symbol {
                ($name:literal, $kind:ty) => {{
                    let name = CString::new($name).unwrap();
                    let symbol = dlsym(library, name.as_ptr());
                    if symbol.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $kind>(symbol)
                }};
            }
            Some(LayerShellApi {
                _library: library as usize,
                is_supported: symbol!("gtk_layer_is_supported", unsafe extern "C" fn() -> c_int),
                init_for_window: symbol!(
                    "gtk_layer_init_for_window",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow)
                ),
                set_layer: symbol!(
                    "gtk_layer_set_layer",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int)
                ),
                set_anchor: symbol!(
                    "gtk_layer_set_anchor",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int)
                ),
                set_margin: symbol!(
                    "gtk_layer_set_margin",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int)
                ),
                set_monitor: symbol!(
                    "gtk_layer_set_monitor",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *mut gtk::gdk::ffi::GdkMonitor)
                ),
                set_keyboard_mode: symbol!(
                    "gtk_layer_set_keyboard_mode",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int)
                ),
                set_namespace: symbol!(
                    "gtk_layer_set_namespace",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *const c_char)
                ),
            })
        })
        .as_ref()
}

// pin a fixed-size window to a global logical position on wlroots
// compositors, the layer-shell counterpart of kde's plasma-shell placement.
// the surface anchors top-left of its output and is offset by margins into
// the target position; the window keeps its own size via geometry hints.
// `keyboard` grants on-demand focus (pins accept clicks), off for the
// recbar. returns false when layer-shell isn't available so the caller can
// fall back to a plain window.
pub fn pin_at(
    window: &gtk::Window,
    monitor: &gtk::gdk::Monitor,
    layer: c_int,
    global_x: i32,
    global_y: i32,
    keyboard: bool,
) -> bool {
    use gtk::gdk::prelude::MonitorExt;
    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::WidgetExt;

    let Some(api) = layer_shell_api() else {
        return false;
    };
    if unsafe { (api.is_supported)() } == 0 {
        return false;
    }
    // margins are output-local, so subtract the monitor's origin
    let geometry = monitor.geometry();
    let left = (global_x - geometry.x()).max(0);
    let top = (global_y - geometry.y()).max(0);
    if window.is_realized() {
        window.unrealize();
    }
    let namespace = CString::new("capscr-pin").unwrap_or_default();
    unsafe {
        let window_ptr = window.to_glib_none().0;
        (api.init_for_window)(window_ptr);
        (api.set_namespace)(window_ptr, namespace.as_ptr());
        (api.set_layer)(window_ptr, layer);
        (api.set_monitor)(window_ptr, monitor.to_glib_none().0);
        // anchor to the top-left corner only, so the surface keeps its size
        // and the margins position it
        (api.set_anchor)(window_ptr, EDGE_TOP, 1);
        (api.set_anchor)(window_ptr, EDGE_LEFT, 1);
        (api.set_anchor)(window_ptr, EDGE_RIGHT, 0);
        (api.set_anchor)(window_ptr, EDGE_BOTTOM, 0);
        (api.set_margin)(window_ptr, EDGE_LEFT, left);
        (api.set_margin)(window_ptr, EDGE_TOP, top);
        (api.set_keyboard_mode)(
            window_ptr,
            if keyboard { KEYBOARD_ON_DEMAND } else { KEYBOARD_NONE },
        );
    }
    true
}

// move an already-pinned layer surface by only updating its margins. init
// (pin_at) unrealizes and re-plumbs the window through the layer protocol,
// which is a once-per-window operation — calling it on every pointer-move
// while dragging would re-create the surface and flicker. this just nudges.
pub fn set_position(window: &gtk::Window, monitor: &gtk::gdk::Monitor, global_x: i32, global_y: i32) {
    use gtk::gdk::prelude::MonitorExt;
    use gtk::glib::translate::ToGlibPtr;

    let Some(api) = layer_shell_api() else {
        return;
    };
    let geometry = monitor.geometry();
    let left = (global_x - geometry.x()).max(0);
    let top = (global_y - geometry.y()).max(0);
    unsafe {
        let window_ptr = window.to_glib_none().0;
        (api.set_margin)(window_ptr, EDGE_LEFT, left);
        (api.set_margin)(window_ptr, EDGE_TOP, top);
    }
}

// the gdk monitor whose geometry contains a global logical point, for
// handing pin_at the right output
pub fn monitor_at(window: &gtk::Window, x: i32, y: i32) -> Option<gtk::gdk::Monitor> {
    use gtk::gdk::prelude::MonitorExt;
    use gtk::prelude::WidgetExt;
    let display = window.display();
    let mut fallback = None;
    for index in 0..display.n_monitors() {
        let Some(monitor) = display.monitor(index) else {
            continue;
        };
        let geometry = monitor.geometry();
        if x >= geometry.x()
            && x < geometry.x() + geometry.width()
            && y >= geometry.y()
            && y < geometry.y() + geometry.height()
        {
            return Some(monitor);
        }
        fallback.get_or_insert(monitor);
    }
    fallback
}

// full-output surface on the overlay layer with on-demand keyboard focus:
// the selector's shape. anchoring all four edges sizes the surface to the
// monitor
pub fn cover_output(window: &gtk::Window, monitor: &gtk::gdk::Monitor, namespace: &str) -> bool {
    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::WidgetExt;

    let Some(api) = layer_shell_api() else {
        return false;
    };
    if unsafe { (api.is_supported)() } == 0 {
        return false;
    }
    if window.is_realized() {
        window.unrealize();
    }
    let namespace = CString::new(namespace).unwrap_or_default();
    unsafe {
        let window_ptr = window.to_glib_none().0;
        (api.init_for_window)(window_ptr);
        (api.set_namespace)(window_ptr, namespace.as_ptr());
        (api.set_layer)(window_ptr, 3);
        for edge in 0..4 {
            (api.set_anchor)(window_ptr, edge, 1);
        }
        (api.set_monitor)(window_ptr, monitor.to_glib_none().0);
        (api.set_keyboard_mode)(window_ptr, 2);
    }
    true
}
