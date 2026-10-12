// appkey helper for GNOME Shell (45+).
//
// GNOME doesn't let apps list or focus other apps' windows, or grab global keys,
// from outside the shell. This extension exposes exactly those operations to the
// appkey daemon over D-Bus, on GNOME Shell's own bus name:
//
//   bus name  org.gnome.Shell
//   path      /io/github/carevski/appkey
//   interface io.github.carevski.Appkey
//
// It holds no state beyond the key grabs appkey asked for, and releases them
// when disabled.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const PATH = '/io/github/carevski/appkey';
const IFACE = `
<node>
  <interface name="io.github.carevski.Appkey">
    <method name="ListWindows"><arg type="s" direction="out" name="json"/></method>
    <method name="ActiveWindow"><arg type="s" direction="out" name="json"/></method>
    <method name="Activate">
      <arg type="s" direction="in" name="id"/>
      <arg type="b" direction="out" name="found"/>
    </method>
    <method name="GrabAccelerators">
      <arg type="as" direction="in" name="accelerators"/>
      <arg type="au" direction="out" name="actions"/>
    </method>
    <method name="UngrabAll"/>
    <property name="Version" type="u" access="read"/>
    <signal name="Activated"><arg type="u" name="action"/></signal>
    <signal name="Started"/>
  </interface>
</node>`;

/** Every id a window goes by, most specific first, for appkey's matching. */
function describe(w) {
    const app = Shell.WindowTracker.get_default().get_window_app(w);
    const ids = [
        w.get_sandboxed_app_id?.(),
        w.get_gtk_application_id?.(),
        // Unknown apps get a placeholder id like `window:3`; it identifies nothing.
        app && !app.get_id().startsWith('window:') ? app.get_id().replace(/\.desktop$/, '') : null,
        w.get_wm_class(),
        w.get_wm_class_instance(),
    ].filter((id, i, all) => id && all.indexOf(id) === i);
    return {id: String(w.get_id()), ids, pid: w.get_pid(), title: w.get_title() ?? ''};
}

/** Normal app windows, most recently used first (the Alt+Tab order). */
function appWindows() {
    return global.display.get_tab_list(Meta.TabList.NORMAL_ALL, null)
        .filter(w => !w.is_skip_taskbar());
}

class Service {
    constructor() {
        this._grabs = [];
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, PATH);
        this._accelId = global.display.connect('accelerator-activated', (_display, action) => {
            if (this._grabs.includes(action))
                this._dbus.emit_signal('Activated', new GLib.Variant('(u)', [action]));
        });
        // Tell a running appkey to (re)apply its shortcuts, e.g. after the lock
        // screen disabled and re-enabled extensions.
        this._dbus.emit_signal('Started', null);
    }

    get Version() {
        return 1;
    }

    ListWindows() {
        return JSON.stringify(appWindows().map(describe));
    }

    ActiveWindow() {
        const w = global.display.focus_window;
        return JSON.stringify(w && !w.is_skip_taskbar() ? describe(w) : null);
    }

    Activate(id) {
        const w = appWindows().find(win => String(win.get_id()) === id);
        if (!w)
            return false;
        // Hiding the Activities overview (if open) restores the previous focus when
        // its animation ends, so activate again once it's gone.
        if (Main.overview.visible) {
            const handler = Main.overview.connect('hidden', () => {
                Main.overview.disconnect(handler);
                w.activate(global.display.get_current_time_roundtrip());
            });
        }
        // A real server timestamp: with none, focus-stealing prevention can keep a
        // just-opened window in front. activateWindow also switches workspace and
        // un-minimizes as needed.
        Main.activateWindow(w, global.display.get_current_time_roundtrip());
        // Mutter sometimes only raises the window on the first request (seen right
        // after windows open); a second activation then moves the focus.
        if (!w.has_focus())
            w.activate(global.display.get_current_time_roundtrip());
        return true;
    }

    GrabAccelerators(accelerators) {
        return accelerators.map(accel => {
            const action = global.display.grab_accelerator(accel, Meta.KeyBindingFlags.NONE);
            if (action !== Meta.KeyBindingAction.NONE) {
                Main.wm.allowKeybinding(Meta.external_binding_name_for_action(action),
                    Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW);
                this._grabs.push(action);
            }
            return action; // 0 = couldn't grab (already taken)
        });
    }

    UngrabAll() {
        for (const action of this._grabs) {
            Main.wm.allowKeybinding(Meta.external_binding_name_for_action(action), Shell.ActionMode.NONE);
            global.display.ungrab_accelerator(action);
        }
        this._grabs = [];
    }

    destroy() {
        this.UngrabAll();
        global.display.disconnect(this._accelId);
        this._dbus.unexport();
    }
}

export default class AppkeyExtension extends Extension {
    enable() {
        this._service = new Service();
    }

    disable() {
        this._service?.destroy();
        this._service = null;
    }
}
