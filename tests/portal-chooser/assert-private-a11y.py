#!/usr/bin/env python3
"""Refuse to continue unless the accessibility bus is ours alone.

This exists because of a real incident while building this harness, and it is
the most important safety check in it.

`dbus-run-session` isolates the SESSION bus. It does not isolate the
ACCESSIBILITY bus, which is a separate bus discovered by its own rules -- an
env var, an X root-window property, an `org.a11y.Bus` lookup, or a path under
XDG_RUNTIME_DIR. On a developer machine that discovery quietly resolved to the
bus of the real desktop, and the harness listed `gnome-shell`, `Brave Browser`,
`code` and `megasync`.

Why that is dangerous rather than merely wrong: the trigger activates a control
*by name*. Pressing "File" on a bus carrying the developer's editor would
activate that editor's File menu. Nothing had happened yet only because the
lookup is scoped to an application named aeroftp -- luck, not design.

So the rule is fail-closed: if anything on this bus was not started by us, stop.
A test that can reach the machine it runs on is not isolated, whatever its
DISPLAY says.

And the rule has a second half, learned the same way: the check must confirm
something POSITIVE. An empty bus satisfies "nothing foreign is present" while
proving nothing at all -- it is what you get when the atk-bridge never loaded, or
when the app is not on the bus yet. The caller greps for the success line as its
evidence that the bus is private, so passing on an empty bus turns absence of
evidence into evidence, which is the exact failure this file argues against.

Exit codes:
    0 - the bus carries at least one of our applications, and nothing else
    6 - a foreign application is visible: the bus is NOT private
    7 - the bus carries nothing of ours, so nothing was verified
    2 - the accessibility stack is unusable
"""

import os
import sys

try:
    import pyatspi
except ImportError:
    print("python3-pyatspi is not installed (apt: at-spi2-core python3-pyatspi)", file=sys.stderr)
    sys.exit(2)

# Everything the harness itself is allowed to put on the bus. Anything else means
# we are looking at somebody's desktop.
ALLOWED_SUBSTRINGS = ("aeroftp",)

# Not a security boundary, just a clear message: these names are unmistakably a
# real desktop and produce a better error than "unexpected application".
DESKTOP_MARKERS = (
    "gnome-shell", "mutter", "gsd-", "ibus", "nautilus", "evolution",
    "xdg-desktop-portal", "gjs", "update-notifier", "code", "brave",
    "firefox", "chrome", "megasync", "thunderbird", "kwin", "plasma",
)


def classify_names(names):
    """Return the exit code for a list of application names already on the bus.

    Pure so a pin test can exercise the fail-closed rules without pyatspi or a
    real accessibility bus. Exit codes match the module docstring.
    """
    foreign = [
        n for n in names
        if not any(a in n.lower() for a in ALLOWED_SUBSTRINGS)
    ]
    if foreign:
        return 6, foreign
    # Nothing foreign AND nothing at all is not a pass. Past this point every
    # name matched ALLOWED_SUBSTRINGS -- anything else would have been caught
    # above as foreign -- so a non-empty list is exactly "one or more of ours".
    if not names:
        return 7, []
    return 0, []


def main():
    try:
        desktop = pyatspi.Registry.getDesktop(0)
    except Exception as exc:
        print(f"cannot reach the accessibility bus: {exc}", file=sys.stderr)
        return 2

    names = []
    for app in desktop:
        if app is None:
            continue
        try:
            names.append(app.name or "<unnamed>")
        except Exception as exc:
            names.append(f"<unreadable: {exc}>")

    bus = os.environ.get("AT_SPI_BUS_ADDRESS", "<unset>")
    print(f"AT_SPI_BUS_ADDRESS={bus}")
    print(f"applications on this accessibility bus: {names!r}")

    code, foreign = classify_names(names)

    if code == 6:
        looks_like_desktop = any(
            any(m in n.lower() for m in DESKTOP_MARKERS) for n in foreign
        )
        print(
            "REFUSING: the accessibility bus is not private.\n"
            f"  foreign applications: {foreign!r}",
            file=sys.stderr,
        )
        if looks_like_desktop:
            print(
                "  This is the real desktop session. Activating a control by name here\n"
                "  could press a button in one of those applications. dbus-run-session\n"
                "  does NOT isolate the a11y bus; a private one must be started and\n"
                "  AT_SPI_BUS_ADDRESS pointed at it.",
                file=sys.stderr,
            )
        return 6

    if code == 7:
        print(
            "REFUSING: this accessibility bus is EMPTY, so nothing was verified.\n"
            f"  Expected at least one application matching {ALLOWED_SUBSTRINGS!r}.\n"
            "  An empty bus is not a private bus. The usual cause is that the\n"
            "  atk-bridge was not loaded before the app started, or that the app\n"
            "  never reached the point of exposing an accessible tree.",
            file=sys.stderr,
        )
        return 7

    print("ok: the accessibility bus carries only our own application(s)")
    return 0


def _selftest():
    """Pin: an empty bus must not print success or return 0.

    The harness greps for the success line as proof the bus is private. If
    classify_names([]) returned 0, absence of evidence would become evidence.
    """
    assert classify_names([]) == (7, []), "empty bus must fail closed (exit 7)"
    assert classify_names(["aeroftp"]) == (0, []), "our app alone is a pass"
    assert classify_names(["AeroFTP Dev"]) == (0, []), "substring match is case-insensitive"
    code, foreign = classify_names(["code", "aeroftp"])
    assert code == 6 and "code" in foreign, "foreign apps must fail closed (exit 6)"
    # The defect under test: success must never be claimed for an empty list.
    assert classify_names([])[0] != 0, "empty bus must never be treated as private"
    print("ok: assert-private-a11y selftest (empty bus fails closed)")
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--selftest":
        sys.exit(_selftest())
    sys.exit(main())
