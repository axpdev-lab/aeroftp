#!/usr/bin/env python3
"""Activate a control in the running AeroFTP window by its accessible name.

This is the trigger the file-chooser test needs, and it is deliberately not a
click. Clicking a coordinate is wrong here for two reasons: the coordinates come
from a layout that changes, and a click that lands on nothing is
indistinguishable from a click that landed and did nothing -- so a harness built
on it reports success while testing nothing.

Activating through AT-SPI removes both problems. The control is found by name,
and the two failure modes separate cleanly:

    exit 4 - no control with that name exists (the UI changed, or we are on the
             wrong screen)
    exit 5 - the control exists but exposes no action to activate

Neither is ever reported as success. That distinction is the whole reason this
file exists rather than a line of xdotool.

It also does not need the window to be mapped or composited: the action goes to
the widget over the accessibility bus, not through X.

Usage:
    press-atspi.py "Export / Import"          # press the first exact match
    press-atspi.py --list                     # print pressable names and exit
    press-atspi.py --contains "Export"        # substring match instead of exact
"""

import sys
import time

try:
    import pyatspi
except ImportError:
    print("python3-pyatspi is not installed (apt: at-spi2-core python3-pyatspi)", file=sys.stderr)
    sys.exit(2)

PRESS_ACTIONS = ("press", "click", "activate", "jump")


def pressable(node):
    try:
        action = node.queryAction()
    except NotImplementedError:
        return None
    for i in range(action.nActions):
        if action.getName(i).lower() in PRESS_ACTIONS:
            return (action, i)
    return None


def walk(node, depth=0, limit=60):
    if depth > limit:
        return
    yield node
    try:
        count = node.childCount
    except Exception:
        return
    for i in range(count):
        try:
            child = node.getChildAtIndex(i)
        except Exception:
            continue
        if child is not None:
            yield from walk(child, depth + 1, limit)


def app_nodes(debug=False):
    desktop = pyatspi.Registry.getDesktop(0)
    seen = []
    matched = False
    for app in desktop:
        if app is None:
            continue
        try:
            name = (app.name or "")
        except Exception as exc:
            seen.append(f"<unreadable: {exc}>")
            continue
        seen.append(name)
        if "aeroftp" in name.lower():
            matched = True
            yield from walk(app)
    if debug and not matched:
        print(f"accessibility bus shows these applications: {seen!r}", file=sys.stderr)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__, file=sys.stderr)
        return 2

    mode_contains = False
    if args[0] == "--list":
        found = []
        for node in app_nodes(debug=True):
            try:
                nm = node.name or ""
                role = node.getRoleName()
            except Exception:
                continue
            if nm and pressable(node):
                found.append(f"{role}\t{nm}")
        if not found:
            print("no pressable named control found; is the app running on this bus?", file=sys.stderr)
            return 4
        print("\n".join(sorted(set(found))))
        return 0

    if args[0] == "--contains":
        mode_contains = True
        args = args[1:]
        if not args:
            print("--contains needs a value", file=sys.stderr)
            return 2

    target = args[0]
    named_but_inert = []

    for node in app_nodes():
        try:
            nm = node.name or ""
        except Exception:
            continue
        if not nm:
            continue
        hit = (target in nm) if mode_contains else (nm == target)
        if not hit:
            continue
        act = pressable(node)
        if act is None:
            named_but_inert.append(nm)
            continue
        action, index = act
        try:
            role = node.getRoleName()
        except Exception:
            role = "?"
        print(f"pressing [{role}] {nm!r} via action {action.getName(index)!r}")
        action.doAction(index)
        # The chooser is opened asynchronously: the press returns immediately and
        # the portal call happens on the app's own loop. The caller decides how
        # long to wait for evidence; this just gives the event a moment to leave.
        time.sleep(0.5)
        return 0

    if named_but_inert:
        print(
            f"found {len(named_but_inert)} control(s) named {target!r} but none exposes a "
            f"press/click action: {named_but_inert[:5]}",
            file=sys.stderr,
        )
        return 5

    print(f"no control named {target!r} in the AeroFTP accessible tree", file=sys.stderr)
    return 4


if __name__ == "__main__":
    sys.exit(main())
