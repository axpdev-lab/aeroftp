# Start an accessibility bus that belongs to this session alone, and export
# AT_SPI_BUS_ADDRESS pointing at it. Sourced, not executed: it exports into the
# caller and sets A11Y_PID so the caller can reap it.
#
# Why this file exists at all: `dbus-run-session` isolates the SESSION bus and
# nothing else. AT-SPI runs on its own bus, found through its own chain of
# fallbacks -- an env var, an X root-window property, an org.a11y.Bus lookup, a
# path under XDG_RUNTIME_DIR. On a developer machine that chain quietly resolved
# to the bus of the live desktop, and the harness could enumerate gnome-shell,
# the browser and the editor.
#
# That is not a cosmetic leak. The trigger activates controls BY NAME, so a
# press of "File" on that bus could have opened a menu in the developer own
# editor. It had not happened only because the lookup is scoped to an
# application named aeroftp: luck, not design.
#
# Unsetting AT_SPI_BUS_ADDRESS is NOT sufficient, because the remaining
# fallbacks do not need it. The fix has to be positive: start a bus, point at
# it, and then verify with assert-private-a11y.py before touching any control.
#
# It is a separate file rather than inline because the caller runs inside a
# single-quoted `bash -c` block, where the quoting this needs cannot be written.

A11Y_LAUNCHER=""
for candidate in /usr/libexec/at-spi-bus-launcher \
                 /usr/lib/at-spi2-core/at-spi-bus-launcher \
                 /usr/libexec/at-spi2-core/at-spi-bus-launcher; do
  [ -x "$candidate" ] && { A11Y_LAUNCHER="$candidate"; break; }
done
if [ -z "$A11Y_LAUNCHER" ]; then
  echo "::error::at-spi-bus-launcher not found (apt: at-spi2-core)" >&2
  return 1 2>/dev/null || exit 1
fi

unset AT_SPI_BUS_ADDRESS

# XDG_RUNTIME_DIR is deliberately NOT overridden, and that is a measured decision
# rather than an omission. Two facts, both established by experiment here:
#
#  1. Overriding it breaks the app outright: GTK fails to initialise and tao
#     panics with "Failed to initialize gtk backend!" before any window exists.
#  2. It is not needed for isolation. The launcher creates its bus per DISPLAY --
#     the socket is at-spi/bus_<display number> -- so a private Xvfb display
#     already yields a private bus. The original leak was not caused by the
#     runtime dir: it happened because nothing started a launcher and nothing set
#     AT_SPI_BUS_ADDRESS, so the client library walked its fallback chain all the
#     way to the real desktop bus (bus_0).
#
# Isolation therefore rests on two things that are both explicit: we start the
# launcher on our own display, and assert-private-a11y.py verifies what is on the
# bus before any control is touched. Set RECON_PRIVATE_RUNTIME=1 to override
# anyway, but expect the GTK failure above.
if [ "${RECON_PRIVATE_RUNTIME:-0}" = "1" ]; then
  export XDG_RUNTIME_DIR="${RECON_RUNTIME:?RECON_RUNTIME must be set}"
fi

"$A11Y_LAUNCHER" --launch-immediately >"${OUT:-/tmp}/a11y-launcher.log" 2>&1 &
A11Y_PID=$!

A11Y_ADDR=""
for _ in $(seq 1 60); do
  # gdbus prints the address wrapped as a D-Bus tuple. Parsing is done in python
  # rather than sed so the caller does not have to survive nested quoting.
  A11Y_ADDR="$(gdbus call --session --dest org.a11y.Bus \
      --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress 2>/dev/null |
      python3 -c "import sys,re; m=re.search(r'[\"\\']([^\"\\']+)[\"\\']', sys.stdin.read()); print(m.group(1) if m else '')")"
  [ -n "$A11Y_ADDR" ] && break
  kill -0 "$A11Y_PID" 2>/dev/null || break
  sleep 0.2
done

if [ -z "$A11Y_ADDR" ]; then
  echo "::error::could not start a private accessibility bus" >&2
  cat "${OUT:-/tmp}/a11y-launcher.log" >&2
  kill -TERM "$A11Y_PID" 2>/dev/null
  return 1 2>/dev/null || exit 1
fi

export AT_SPI_BUS_ADDRESS="$A11Y_ADDR"
echo "private a11y bus: $AT_SPI_BUS_ADDRESS (launcher pid $A11Y_PID)"
