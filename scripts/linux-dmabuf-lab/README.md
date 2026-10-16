# Linux DMA-BUF rendering lab

Developer-only runtime verification for Requested item 56 in tracker #1202. This does not change the application's default or ship a new runtime component.

Requires Linux, a real `/dev/dri/renderD*` device, GNOME Shell with `--headless`, WebKitGTK 4.1, Node 22+, Python 3 with PyGObject, and the checkout's installed npm dependencies. The test uses its own headless Wayland compositor, D-Bus, HOME, XDG storage and runtime directory. It does not use the owner's desktop or Wayland socket. Xvfb alone is not GPU coverage. This procedure covers native Wayland; it does not automatically fall back to XWayland if a driver fails.

From a dedicated worktree of this branch:

```bash
# Link/install node_modules first. Reuse a compatible Cargo target if available.
# Wait for any other Rust compiler on this machine to finish.
python3 scripts/linux-dmabuf-lab/run.py --root /tmp/aeroftp-dmabuf-intel
```

By default the driver compiles the GUI with Vite port 5188 embedded in the debug context and uses inspector port 9448. Both ports must be free. `--port` and `--inspector-port` select alternatives. The existing `CARGO_TARGET_DIR` is respected. To reuse a binary already built with the same Vite port:

```bash
python3 scripts/linux-dmabuf-lab/run.py \
  --root /tmp/aeroftp-dmabuf-intel \
  --skip-build --binary /absolute/target/debug/aeroftp
```

The fixture directory must be new or empty. Startup logs, manifest, native screenshots, native controller receipts, page timing, application CPU/RSS and available per-process DRM counters remain under its `evidence/` folder; session/compositor/Vite diagnostics remain alongside it. The driver stops only the process groups it started and removes its private runtime directory.

Variants are explicitly recorded in `*-environment.json`. A clean environment prevents inherited `GDK_BACKEND`, WebKit flags, Mesa overrides, display names or EGL settings from affecting the result:

| Variant | GDK_BACKEND | WAYLAND_DISPLAY | DISPLAY | WEBKIT_DISABLE_DMABUF_RENDERER | WEBKIT_DISABLE_COMPOSITING_MODE |
|---|---|---|---|---|---|
| default | wayland | private wl-dmabuf-lab | absent | absent | absent |
| on | wayland | private wl-dmabuf-lab | absent | 0 | absent |
| off | wayland | private wl-dmabuf-lab | absent | 1 | absent |

`default` verifies startup only. `on` and `off` run the actual app's CodeEditor and SSHTerminal components, all eight AeroPlayer canvas modes and all six WebGL2 shaders. A labelled DEV overlay mounts production components with artificial data. The terminal runs a local PTY under the fixture HOME; it prints 1200 ANSI/Unicode lines. The audio analyser is a deterministic synthetic fixture, so this is rendering coverage, not audio playback or decoding coverage. The `.body` files are async-function bodies consumed by `gui-controller-dev.mjs --js`, not standalone JavaScript modules.

Read `lab-complete.json` only as successful harness completion. Acceptance also requires reviewing the images and assertions: `terminalHasCompletion`, `programLinked`, `shaderCompiled`, `glError`, `contextLostEvents` and error lists. A driver error must be reported with logs, not silently converted into a fallback success. FPS observes requestAnimationFrame cadence and is exploratory; it is not proof of every frame being presented. CPU percentages are aggregate process CPU, where 100% is one core. Background workstation load and debug builds limit performance comparisons.

Report the exact branch SHA, station GPU/driver/WebKit versions, each environment manifest, startup corner logs and screenshot observations. Preserve a failure. Do not turn the default back on or merge as part of running this lab.
