#!/usr/bin/env python3
"""Dump the AT-SPI accessible tree of the running AeroFTP window.

This is reconnaissance for the second half of #464. The trigger that opens the
picker has to find its target somehow, and there are only two honest options:
click a coordinate, or activate an accessible object by name. Coordinates rot
the first time the layout changes and fail silently -- the click lands on
nothing, no picker opens, and a careless harness would report success. A
name-based trigger is stable and, more importantly, can tell "the button is not
there" from "the button did not respond".

WebKitGTK exposes the DOM through AT-SPI, so the buttons in AeroFTP's React UI
should appear here with their accessible names taken from their text or
aria-label. Whether they actually do, and under what names, is exactly what this
script exists to find out -- it is not assumed anywhere.

Run it while the app is up, on the same DISPLAY and session bus.
"""

import sys

try:
    import pyatspi
except ImportError:
    print(
        "python3-pyatspi is not installed; no accessible tree can be read.\n"
        "apt: at-spi2-core python3-pyatspi",
        file=sys.stderr,
    )
    sys.exit(2)

# Anything that could plausibly be activated to open a chooser. Printing the
# whole tree unfiltered buries the answer in thousands of nodes.
INTERESTING = {
    pyatspi.ROLE_PUSH_BUTTON,
    pyatspi.ROLE_TOGGLE_BUTTON,
    pyatspi.ROLE_LINK,
    pyatspi.ROLE_MENU_ITEM,
    pyatspi.ROLE_LIST_ITEM,
    pyatspi.ROLE_ENTRY,
    pyatspi.ROLE_TEXT,
}

MAX_DEPTH = 40


def actions_of(node):
    try:
        action = node.queryAction()
    except NotImplementedError:
        return []
    return [action.getName(i) for i in range(action.nActions)]


def extents_of(node):
    try:
        comp = node.queryComponent()
    except NotImplementedError:
        return None
    try:
        e = comp.getExtents(pyatspi.DESKTOP_COORDS)
        return (e.x, e.y, e.width, e.height)
    except Exception:
        return None


def walk(node, depth, out):
    if depth > MAX_DEPTH:
        return
    try:
        name = node.name or ""
        role = node.getRoleName()
        role_id = node.getRole()
        child_count = node.childCount
    except Exception as exc:  # a node can vanish mid-walk; that is not fatal
        out.append(f"{'  ' * depth}<unreadable: {exc}>")
        return

    # Print every node so the shape is visible, but annotate the ones a trigger
    # could actually use.
    mark = ""
    if role_id in INTERESTING:
        acts = actions_of(node)
        ext = extents_of(node)
        mark = f"   <== actions={acts} extents={ext}"
    out.append(f"{'  ' * depth}[{role}] {name!r}{mark}")

    for i in range(child_count):
        try:
            child = node.getChildAtIndex(i)
        except Exception:
            continue
        if child is not None:
            walk(child, depth + 1, out)


def main():
    desktop = pyatspi.Registry.getDesktop(0)
    out = []
    found_app = False
    for app in desktop:
        if app is None:
            continue
        try:
            app_name = app.name or ""
        except Exception:
            continue
        out.append(f"=== application: {app_name!r} ===")
        if "aeroftp" in app_name.lower():
            found_app = True
        walk(app, 1, out)

    print("\n".join(out))
    if not found_app:
        # Say it out loud rather than leaving an empty file to be read as
        # "nothing to see". An absent app here usually means the at-spi bridge
        # was not loaded before the app started, not that the app has no UI.
        print(
            "\nNOTE: no application whose name contains 'aeroftp' was found on the "
            "accessibility bus. Either the app was not running, or GTK_MODULES did "
            "not include atk-bridge BEFORE it started -- WebKitGTK builds its "
            "accessible tree at widget construction, so enabling the bridge later "
            "yields nothing.",
            file=sys.stderr,
        )
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
