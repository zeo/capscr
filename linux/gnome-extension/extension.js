// companion extension for capscr. mutter gives ordinary wayland clients no
// window enumeration, no per-window pixels, and no keep-above, so the parts
// of capscr that need those run here, inside the shell, and answer over
// d-bus. the panel button stands in for the tray on sessions without a
// StatusNotifier host.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

Gio._promisify(Shell.Screenshot, 'composite_to_stream');
Gio._promisify(Gio.Subprocess.prototype, 'communicate_utf8_async');

const IFACE = `
<node>
  <interface name="org.gnome.Shell.Extensions.Capscr">
    <property name="Version" type="u" access="read"/>
    <method name="ListWindows">
      <arg type="s" direction="out" name="windows"/>
    </method>
    <method name="CaptureWindow">
      <arg type="t" direction="in" name="id"/>
      <arg type="s" direction="out" name="path"/>
    </method>
    <method name="PlaceAbove">
      <arg type="s" direction="in" name="titleToken"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="b" direction="out" name="placedNow"/>
    </method>
  </interface>
</node>`;

const CAPSCR_WM_CLASSES = ['capscr', 'io.rot.capscr'];

function isCapscrWindow(win) {
    const wmClass = (win.get_wm_class() ?? '').toLowerCase();
    const appId = (win.get_sandboxed_app_id() ?? '').toLowerCase();
    return CAPSCR_WM_CLASSES.includes(wmClass) || CAPSCR_WM_CLASSES.includes(appId);
}

class CapscrService {
    constructor() {
        // capscr windows awaiting placement: two pins share one title, so
        // each PlaceAbove call binds to the next matching window that hasn't
        // been placed yet, queueing when the window hasn't mapped yet
        this._pending = [];
        this._placed = new WeakMap();
        this._windowCreatedId = global.display.connect(
            'window-created', (_display, win) => this._watchNewWindow(win));
    }

    destroy() {
        global.display.disconnect(this._windowCreatedId);
        this._pending = [];
    }

    get Version() {
        return 1;
    }

    _eligible() {
        const workspace = global.workspace_manager.get_active_workspace();
        const windows = global.get_window_actors()
            .map(actor => actor.get_meta_window())
            .filter(win => win
                && win.get_window_type() === Meta.WindowType.NORMAL
                && !win.minimized
                && win.located_on_workspace(workspace));
        // lowest first from mutter; the picker wants topmost first
        return global.display.sort_windows_by_stacking(windows).reverse();
    }

    ListWindows() {
        const list = this._eligible().map(win => {
            const frame = win.get_frame_rect();
            return {
                id: win.get_id(),
                pid: win.get_pid(),
                title: win.get_title() ?? '',
                wmClass: win.get_wm_class() ?? '',
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: frame.height,
            };
        });
        return JSON.stringify(list);
    }

    // paint the window's own actor offscreen, so the pixels are the window's
    // content even where another window covers it on screen, then crop the
    // shadow margin away by compositing only the frame rect
    async CaptureWindowAsync(params, invocation) {
        try {
            const [id] = params;
            const win = this._eligible().find(w => w.get_id() === Number(id));
            if (!win) {
                invocation.return_dbus_error(
                    'org.gnome.Shell.Extensions.Capscr.Error', `no window with id ${id}`);
                return;
            }
            const actor = win.get_compositor_private();
            const content = actor.paint_to_content(null);
            const texture = content.get_texture();
            const frame = win.get_frame_rect();
            const buffer = win.get_buffer_rect();
            const scale = actor.get_resource_scale();
            const path = GLib.build_filenamev([
                GLib.get_user_runtime_dir(),
                `capscr-window-${id}-${GLib.get_monotonic_time()}.png`,
            ]);
            const file = Gio.File.new_for_path(path);
            const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
            await Shell.Screenshot.composite_to_stream(
                texture,
                Math.round((frame.x - buffer.x) * scale),
                Math.round((frame.y - buffer.y) * scale),
                Math.round(frame.width * scale),
                Math.round(frame.height * scale),
                scale,
                null, 0, 0, 1,
                stream);
            stream.close(null);
            invocation.return_value(new GLib.Variant('(s)', [path]));
        } catch (e) {
            invocation.return_dbus_error(
                'org.gnome.Shell.Extensions.Capscr.Error', String(e));
        }
    }

    // keep-above (TOP layer beats fullscreen-in-NORMAL) plus positioning,
    // neither of which a wayland client can do for itself under mutter
    PlaceAbove(titleToken, x, y) {
        const win = global.get_window_actors()
            .map(actor => actor.get_meta_window())
            .find(w => w && !this._placed.has(w) && this._matches(w, titleToken));
        if (win) {
            this._apply(win, x, y);
            return true;
        }
        this._pending.push({token: titleToken, x, y});
        return false;
    }

    _matches(win, token) {
        return isCapscrWindow(win) && (win.get_title() ?? '').includes(token);
    }

    _apply(win, x, y) {
        this._placed.set(win, {x, y});
        win.make_above();
        win.move_frame(false, x, y);
    }

    _watchNewWindow(win) {
        if (this._pending.length === 0)
            return;
        // title and wm-class arrive after creation; re-check as they land
        const tryPending = () => {
            if (this._placed.has(win))
                return true;
            const index = this._pending.findIndex(p => this._matches(win, p.token));
            if (index < 0)
                return false;
            const [{x, y}] = this._pending.splice(index, 1);
            this._apply(win, x, y);
            return true;
        };
        if (!tryPending()) {
            const titleId = win.connect('notify::title', () => {
                if (tryPending())
                    win.disconnect(titleId);
            });
        }
        // mutter's own placement runs around the first frame and can
        // override a pre-map move; re-assert the spot once the window shows
        const actor = win.get_compositor_private();
        actor?.connect_after('first-frame', () => {
            const spot = this._placed.get(win);
            if (spot)
                win.move_frame(false, spot.x, spot.y);
            else
                tryPending();
        });
    }
}

class CapscrIndicator extends PanelMenu.Button {
    static {
        GObject.registerClass(this);
    }

    _init(appInfo) {
        super._init(0.0, 'capscr');
        this.add_child(new St.Icon({
            icon_name: 'camera-photo-symbolic',
            style_class: 'system-status-icon',
        }));
        const actions = [
            ['capture-region', 'Capture region'],
            ['capture-window', 'Capture window'],
            ['capture-fullscreen', 'Capture full screen'],
        ];
        for (const [action, label] of actions) {
            this.menu.addAction(label, () => this._launch(appInfo, action));
        }
        this._executable = appInfo.get_executable();
        this._cancellable = new Gio.Cancellable();
        this._tasks = new PopupMenu.PopupSubMenuMenuItem('Tasks');
        this._tasks.visible = false;
        this.menu.addMenuItem(this._tasks);
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._refreshTasks().catch(e => console.warn(`capscr: task list failed: ${e}`));
        });
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addAction('Captures folder', () => this._launch(appInfo, 'open-captures'));
        this.menu.addAction('Open capscr', () => this._launch(appInfo, 'open-hub'));
    }

    destroy() {
        this._cancellable.cancel();
        super.destroy();
    }

    // capscr prints one "<id>\t<name>" line per saved task
    async _refreshTasks() {
        const proc = Gio.Subprocess.new(
            [this._executable, '--list-tasks'], Gio.SubprocessFlags.STDOUT_PIPE);
        let stdout;
        try {
            [stdout] = await proc.communicate_utf8_async(null, this._cancellable);
        } catch (e) {
            if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                return;
            throw e;
        }
        const tasks = (stdout ?? '').split('\n')
            .map(line => line.split('\t'))
            .filter(([id, name]) => id && name);
        this._tasks.menu.removeAll();
        for (const [id, name] of tasks) {
            this._tasks.menu.addAction(name, () => {
                Gio.Subprocess.new([this._executable, `--task=${id}`], Gio.SubprocessFlags.NONE);
            });
        }
        this._tasks.visible = tasks.length > 0;
    }

    _launch(appInfo, action) {
        const context = global.create_app_launch_context(0, -1);
        if (appInfo.list_actions().includes(action))
            appInfo.launch_action(action, context);
        else
            appInfo.launch([], context);
    }
}

export default class CapscrExtension extends Extension {
    enable() {
        this._service = new CapscrService();
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this._service);
        this._dbus.export(Gio.DBus.session, '/org/gnome/Shell/Extensions/Capscr');

        const appInfo = Gio.DesktopAppInfo.new('capscr.desktop');
        if (appInfo) {
            this._indicator = new CapscrIndicator(appInfo);
            Main.panel.addToStatusArea('capscr', this._indicator);
        }
    }

    disable() {
        this._dbus?.unexport();
        this._dbus = null;
        this._service?.destroy();
        this._service = null;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
