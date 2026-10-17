# SPDX-License-Identifier: GPL-3.0-or-later
import sys, time
import gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
owned = []
Gio.bus_own_name_on_connection(bus, 'org.gnome.SettingsDaemon.MediaKeys', Gio.BusNameOwnerFlags.NONE, lambda *a: owned.append(1), None)
ctx = GLib.MainContext.default()
end = time.time() + 3
while time.time() < end:
    ctx.iteration(False); time.sleep(0.05)
for path in sys.argv[1:]:
    if path.startswith('sleep:'):
        t_end = time.time() + float(path[6:])
        while time.time() < t_end:
            ctx.iteration(False); time.sleep(0.05)
        continue
    r = bus.call_sync('org.gnome.Shell.Screenshot', '/org/gnome/Shell/Screenshot', 'org.gnome.Shell.Screenshot',
                      'Screenshot', GLib.Variant('(bbs)', (False, False, path)), GLib.VariantType('(bs)'),
                      Gio.DBusCallFlags.NONE, 10000, None)
    print(path, r.unpack(), flush=True)
