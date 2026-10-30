// Watches file drags. When Shift is held mid-drag, asks the app to show its
// wheel and puts the wheel window under the pointer.
//
// Wayland apps can't see other apps' drags or place their own windows, the
// shell can. Everything else (reading the dropped files, converting) is the
// app's job: the wheel is an ordinary drop target.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Clutter from 'gi://Clutter';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const APP_ID = 'dev.jayhemnani.ShiftDrop';
const APP_PATH = '/dev/jayhemnani/ShiftDrop';
const WHEEL_TITLE = 'ShiftDrop Wheel';
const STATUS_TITLE = 'ShiftDrop Status';
const POLL_MS = 30;

// `touch ~/.cache/shiftdrop-debug` turns on one journal line per step:
//   journalctl --user -f -o cat /usr/bin/gnome-shell | grep shiftdrop
const DEBUG_FLAG = GLib.build_filenamev([GLib.get_user_cache_dir(), 'shiftdrop-debug']);
function debug(message) {
    if (GLib.file_test(DEBUG_FLAG, GLib.FileTest.EXISTS))
        console.log(`shiftdrop: ${message}`);
}

export default class ShiftDropExtension extends Extension {
    enable() {
        this._dragging = false;
        this._shown = false;
        this._tools = false;
        this._pollId = 0;
        this._anchor = [0, 0];

        Main.xdndHandler.connectObject(
            'drag-begin', () => this._onDragBegin(),
            'drag-end', () => this._onDragEnd(),
            this);
        global.display.connectObject(
            'window-created', (_display, window) => this._onWindowCreated(window),
            this);
    }

    disable() {
        Main.xdndHandler.disconnectObject(this);
        global.display.disconnectObject(this);
        for (const actor of global.get_window_actors())
            actor.get_meta_window()?.disconnectObject(this);
        this._stopPoll();
    }

    _onDragBegin() {
        debug('drag-begin');
        this._dragging = true;
        this._shown = false;
        this._stopPoll();
        this._pollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, POLL_MS, () => this._poll());
    }

    _onDragEnd() {
        debug(`drag-end (wheel shown: ${this._shown})`);
        this._dragging = false;
        this._stopPoll();
        if (this._shown)
            this._activate('drag-ended', null);
        this._shown = false;
    }

    _poll() {
        if (!this._dragging)
            return GLib.SOURCE_REMOVE;
        const [x, y, mods] = global.get_pointer();
        const shift = (mods & Clutter.ModifierType.SHIFT_MASK) !== 0;
        const tools = (mods & Clutter.ModifierType.CONTROL_MASK) !== 0;
        if (shift && !this._shown) {
            this._shown = true;
            this._tools = tools;
            this._anchor = [x, y];
            debug(`shift at ${x},${y} tools=${tools}`);
            this._activate('show-wheel', new GLib.Variant('b', tools));
        } else if (this._shown && tools !== this._tools) {
            this._tools = tools;
            this._activate('set-tools', new GLib.Variant('b', tools));
        }
        return GLib.SOURCE_CONTINUE;
    }

    _stopPoll() {
        if (this._pollId) {
            GLib.source_remove(this._pollId);
            this._pollId = 0;
        }
    }

    // org.gtk.Actions is exported by every GApplication; D-Bus activation
    // starts the app on the first drag if it isn't running yet.
    _activate(action, parameter) {
        const params = parameter ? [parameter] : [];
        const sent = GLib.get_monotonic_time();
        Gio.DBus.session.call(
            APP_ID, APP_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [action, params, {}]),
            null, Gio.DBusCallFlags.NONE, -1, null,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                    debug(`${action} ok in ${Math.round((GLib.get_monotonic_time() - sent) / 1000)} ms`);
                } catch (e) {
                    console.warn(`ShiftDrop: ${action} failed: ${e.message}`);
                }
            });
    }

    _onWindowCreated(window) {
        // Match on the title, not the app id: GTK sends the app id after the
        // window already exists, so checking it here skipped every window.
        // Both the title and the size can arrive a moment later, so try again
        // when either shows up.
        const place = () => {
            const title = window.get_title();
            if (title !== WHEEL_TITLE && title !== STATUS_TITLE)
                return;
            const rect = window.get_frame_rect();
            if (rect.width === 0)
                return;
            if (title === WHEEL_TITLE) {
                // centre on the pointer: where Shift was pressed, or where Files was clicked
                const [x, y] = this._shown ? this._anchor : global.get_pointer();
                window.move_frame(false, x - Math.round(rect.width / 2), y - Math.round(rect.height / 2));
                window.make_above();
                debug(`wheel placed at ${x},${y} size ${rect.width}x${rect.height}`);
            } else {
                const area = Main.layoutManager.getWorkAreaForMonitor(Main.layoutManager.primaryIndex);
                window.move_frame(false, area.x + Math.round((area.width - rect.width) / 2), area.y + 12);
                window.make_above();
            }
        };
        window.connectObject('shown', place, 'notify::title', place, 'size-changed', place, this);
        place();
    }
}
