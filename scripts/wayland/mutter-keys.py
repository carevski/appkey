#!/usr/bin/python3
"""Test helper: press key combinations as real input events through Mutter's
RemoteDesktop D-Bus API (GNOME). The session lives as long as the D-Bus
connection that created it, so everything happens in this one process.

    mutter-keys.py ctrl+alt+b [ctrl+alt+g ...]
"""
import sys
import time

from gi.repository import Gio, GLib

KEYSYMS = {"ctrl": 0xFFE3, "alt": 0xFFE9, "shift": 0xFFE1, "super": 0xFFEB}
RD = "org.gnome.Mutter.RemoteDesktop"

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(path, iface, method, args=None, sig=None):
    return bus.call_sync(RD, path, iface, method, args, sig, Gio.DBusCallFlags.NONE, -1, None)


session = call("/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession",
               sig=GLib.VariantType("(o)")).unpack()[0]
call(session, RD + ".Session", "Start")
time.sleep(0.3)


def key(sym, down):
    call(session, RD + ".Session", "NotifyKeyboardKeysym", GLib.Variant("(ub)", (sym, down)))


for combo in sys.argv[1:]:
    *mods, last = combo.split("+")
    syms = [KEYSYMS[m] for m in mods] + [ord(last)]
    for s in syms:
        key(s, True)
    for s in reversed(syms):
        key(s, False)
    time.sleep(0.8)

call(session, RD + ".Session", "Stop")
