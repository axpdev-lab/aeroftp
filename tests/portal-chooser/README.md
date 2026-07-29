# Portal-backed file chooser harness (#464)

Deterministic Linux coverage for AeroFTP's file/folder picker: does the chooser
really leave the process through `xdg-desktop-portal`, and does the app behave
when the portal cancels or refuses?

## Why this exists

`src-tauri/src/lib.rs` sets `GTK_USE_PORTAL=1` unless the user overrides it,
because the in-process `GtkFileChooser` has corrupted the GLib heap under
WebKitGTK. That makes "the chooser runs out-of-process" a **safety property**,
not a preference — and nothing inside the app can prove it holds. Gate G3 proves
the window paints; it never opens a chooser.

Verified while building this, rather than assumed: `rfd 0.16`, which
`tauri-plugin-dialog` uses, resolves against **`gtk-sys`** in this tree, and
`ashpd` is absent. So the chooser really does go GTK → portal, and
`GTK_USE_PORTAL` really is the switch that decides it.

## Layout

| Path | What |
|---|---|
| `fake-portal/` | standalone Rust crate: the portal stand-in and a client probe |
| `selftest-portal.sh` | proves the stand-in itself works, no X needed |

`fake-portal/` is **not** a workspace member and is not referenced by
`src-tauri`. It is built by its own `cargo build` and cannot reach a shipped
binary. That is the constraint from #464: no test-only feature and no automation
backdoor in the production app.

## The stand-in

`aeroftp-fake-portal` owns `org.freedesktop.portal.Desktop` on an **isolated**
session bus, implements `org.freedesktop.portal.FileChooser`, records every call
to a JSONL file, and answers with a scripted outcome:

| `--mode` | Behaviour | What it pins |
|---|---|---|
| `cancel` | `Response(1)` | a dismissed dialog must read as "no selection", not as an error |
| `accept` | `Response(0)` + `uris` | the success path, so `cancel` cannot pass by doing nothing |
| `error` | the D-Bus call fails | a portal that is present but refusing; the call is still recorded and the app survives it |

Its exit code is the verdict: **0** when at least one call was recorded, **3**
when the portal was never asked. That 3 is the point of the whole harness — it
turns "the chooser never went through the portal" from a silent pass into a
failure.

It refuses to start without `DBUS_SESSION_BUS_ADDRESS`. Owning that name on a
developer's real session would hijack the file chooser of every running
application.

## Two things that cost a round each, kept here so they are not rediscovered

**The Request path is caller-specific and predicted by the client.** A portal
answers on `/org/freedesktop/portal/desktop/request/<SENDER>/<TOKEN>`, where
`SENDER` is the caller's unique name without the leading `:` and with `.` mapped
to `_`. The client subscribes to that path *before* the method returns. Get it
wrong and the reply goes somewhere nobody listens: the caller waits forever, and
the hang looks like a bug in the application rather than in the stub.

**`handle_token` is an object path element**, so only `[A-Za-z0-9_]`. A token
with a hyphen produces `InvalidObjectPath` from deep inside zvariant with
nothing naming the token. Both sides sanitise identically.

## What the gate measures, and one finding it produced

`portal-chooser-test.sh` drives the real app through three named controls -
Export / Import, Import Servers, Choose .aeroftp file... - and asserts on the
stand-in record. 17 assertions across three cases.

The proof that the call is genuine is the **handle token**: GTK generates
`gtk<random>`, so a recorded `"handle_token":"gtk573679125"` alongside the app own
dialog title (`Import Servers`) says the request came through the toolkit rather
than through anything the harness could have staged.

**The third case contradicts a claim in the source.** `src-tauri/src/lib.rs` says
that on a host with no portal, GTK falls back to the native chooser. Measured on a
bus that can activate all 65 other session services and only omits the 9 portal
ones - a faithful model of a machine without `xdg-desktop-portal` installed - no
chooser window appears at all and nothing is presented to the user. The window
count after the press is identical to the portal-backed run.

That matters beyond the test: on a minimal WM, in a container, or on any install
without the portal, the file picker does nothing. The frontend does not soften it
either - `ExportImportDialog.tsx` calls `open()` with no `try`/`catch`, so a
refusal becomes an unhandled rejection and the user sees no message. The gate pins
the behaviour as measured rather than as wished, with a comment saying so, so a
future fix has to change the assertion deliberately.

### Owning the portal name is a promise, and a partial stand-in can break the app

The stand-in implements `NetworkMonitor` even though no assertion here reads it.
That is not decoration, and the reasoning is worth keeping because the first
version of this section got it wrong.

Under `GTK_USE_PORTAL=1`, GIO stops using netlink and routes
`g_network_monitor_get_default()` through the portal — the moment something owns
`org.freedesktop.portal.Desktop`, it is answering for the whole desktop, not just
for the interfaces it cares about. A stand-in that owns the name and returns
`UnknownInterface` leaves GIO with a network monitor that cannot report the
network as up, and **WebKitGTK consults exactly that before loading a URL**.

Measured on a CI runner, three cases out of three: with the stand-in present the
app never reached `app_ready` and the splash hit its 10s safety timeout; the
no-portal case in the *same job* started normally and rendered its frontend. The
only portal-related difference was one line:

```
GLib-GIO-WARNING: GDBus.Error:org.freedesktop.DBus.Error.UnknownInterface:
Unknown interface 'org.freedesktop.portal.NetworkMonitor'
```

WebKit was declining to load `http://127.0.0.1:14321` while that URL was serving
a 200 with the real `index.html` — the harness fetched it on the failure path to
prove it. So an incomplete stand-in does not degrade into "no portal": it becomes
a portal that breaks the application under test, and the damage lands nowhere
near the file chooser. It read as "WebKit cannot render headless in CI" and cost
a round of environment flags aimed at the wrong thing.

The lesson generalises past this interface: **implement what owning the name
promises, not only what the test asserts.** `Settings` is still unimplemented and
the app logs `Failed to read portal settings: Unknown interface` for it; that one
has been observed to be harmless, but "harmless" is now a measurement rather than
an assumption.

## Running the self-test

```sh
tests/portal-chooser/selftest-portal.sh
```

Needs only `dbus-run-session`; no display packages, so it runs anywhere. It
builds the crate on first use.

The six cases are **verified as pins, not by watching them pass**: with the
Request path made to ignore the token, 4 of the 11 assertions fail, including
both "a subscribing client received the Response" checks. A stub that answers on
the wrong path is exactly the failure that would otherwise masquerade as an
application hang.

Case 5 was pinned the same way and is worth its own note: exporting `Request` on
the portal's own path instead of on the handle returned to the caller fails that
assertion and **nothing else** — all ten others stay green. `Close()` then
reaches no implementation, and the only symptom is a bus error in the app's log
that reads like the portal misbehaving.

## Running the gate on a desktop machine

`recon-session.sh` **refuses to start when a live Wayland compositor socket is
reachable**, because clearing `WAYLAND_DISPLAY` does not close it: anything that
re-exports it opens a window on the real session, where a name-based trigger can
press a control in somebody's editor. CI is headless and never trips this.

On a Wayland desktop, run it headless or set `RECON_ALLOW_WAYLAND_HOST=1` to
accept the risk deliberately. Note the app also binds `127.0.0.1:14321`, so a
second instance collides with one already running and comes up blank — a symptom
that is easy to misread as a rendering failure.
