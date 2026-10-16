#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Isolated native Wayland DMA-BUF rendering lab; no owner desktop access."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
CHECKOUT = HERE.parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, help='new evidence/fixture directory, default a fresh /tmp directory')
parser.add_argument('--port', type=int, default=5188, help='Vite port embedded in the debug binary')
parser.add_argument('--inspector-port', type=int, default=9448)
parser.add_argument('--skip-build', action='store_true', help='binary must already embed the matching --port')
parser.add_argument('--binary', type=Path, help='default CARGO_TARGET_DIR/debug/aeroftp')
parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
args = parser.parse_args()


def checked(command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


if not args.inside:
    if sys.platform != 'linux':
        parser.error('Linux with GNOME Shell headless and a real render node is required')
    for command in ['gnome-shell', 'dbus-run-session', 'node', 'python3', 'gsettings', 'ss']:
        if not shutil.which(command):
            parser.error(f'missing executable: {command}')
    for port in [args.port, args.inspector_port]:
        with socket.socket() as probe:
            # Match reusable server sockets: TIME_WAIT is not a live listener.
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(('127.0.0.1', port))
            except OSError as error:
                parser.error(f'port {port} is unavailable: {error}')
    if not list(Path('/dev/dri').glob('renderD*')):
        parser.error('no DRM render node; Xvfb alone is not GPU acceptance')
    root = args.root.absolute() if args.root else None
    if root is not None and root.exists() and (root.is_symlink() or any(root.iterdir())):
        parser.error('--root must be new or empty and must not be a symlink')
    target = Path(os.environ.get('CARGO_TARGET_DIR', CHECKOUT / 'src-tauri/target')).absolute()
    binary = (args.binary or target / 'debug/aeroftp').absolute()
    if not args.skip_build:
        # Refuse another Rust compile. The same target's Cargo lock also
        # serializes a compiler that starts after this observation.
        for comm in Path('/proc').glob('[0-9]*/comm'):
            try:
                if comm.read_text().strip() in ['rustc', 'clippy-driver', 'cargo-clippy']:
                    parser.error('another Rust compiler is active; wait before building')
            except OSError:
                pass
        build_env = os.environ | {'TAURI_CONFIG': json.dumps({'build': {'devUrl': f'http://127.0.0.1:{args.port}'}}), 'CARGO_BUILD_JOBS': '1'}
        checked(['cargo', 'build', '--manifest-path', str(CHECKOUT / 'src-tauri/Cargo.toml'), '--bin', 'aeroftp'], cwd=CHECKOUT, env=build_env)
    if not binary.is_file():
        parser.error(f'GUI binary missing: {binary}')
    # A PATH entry can be an NVM shell wrapper that breaks with an isolated HOME.
    node_binary = Path(subprocess.check_output(['node', '-p', 'process.execPath'], text=True).strip()).resolve()
    # Allocate fixture storage only after the build/binary preflight succeeds.
    root = root or Path(tempfile.mkdtemp(prefix='aeroftp-dmabuf-lab-'))
    root.mkdir(parents=True, exist_ok=True)
    for name in ['home', 'config', 'cache', 'data', 'evidence', 'stub-bin']:
        (root / name).mkdir()
    stub = root / 'stub-bin/ibus-daemon'
    stub.write_text('#!/bin/sh\nexec sleep infinity\n')
    stub.chmod(0o755)
    runtime = Path(tempfile.mkdtemp(prefix='afdl-'))
    runtime.chmod(0o700)
    # Declare every inherited input: env-i semantics, no owner Wayland/X11,
    # keyring, library override, compositor flag or Mesa override leaks in.
    clean = {
        'HOME': str(root / 'home'), 'XDG_CONFIG_HOME': str(root / 'config'),
        'XDG_CACHE_HOME': str(root / 'cache'), 'XDG_DATA_HOME': str(root / 'data'),
        'XDG_RUNTIME_DIR': str(runtime), 'XDG_DATA_DIRS': '/usr/local/share:/usr/share',
        'PATH': f'{root}/stub-bin:{node_binary.parent}:/usr/local/bin:/usr/bin:/bin',
        'LANG': 'C.UTF-8', 'NO_AT_BRIDGE': '1', 'GTK_IM_MODULE': 'gtk-im-context-simple',
        'AEROFTP_GUI_TEST_ROOT': str(root), 'AEROFTP_GUI_CHECKOUT': str(CHECKOUT),
        'INSPECTOR_PORT': str(args.inspector_port),
    }
    command = ['dbus-run-session', '--', sys.executable, str(__file__), '--inside', '--root', str(root), '--binary', str(binary), '--port', str(args.port), '--inspector-port', str(args.inspector_port)]
    try:
        with (root / 'session.log').open('w') as log:
            result = subprocess.run(command, cwd=CHECKOUT, env=clean, stdout=log, stderr=subprocess.STDOUT)
        print(f'Evidence: {root}\nSession exit: {result.returncode}', flush=True)
        if result.returncode:
            print((root / 'session.log').read_text()[-6000:])
        raise SystemExit(result.returncode)
    finally:
        shutil.rmtree(runtime, ignore_errors=True)

root = args.root
base = dict(os.environ)
owned = []


def start(command, env, log):
    handle = (root / log).open('w')
    process = subprocess.Popen(command, cwd=CHECKOUT, env=env, stdout=handle, stderr=subprocess.STDOUT, start_new_session=True)
    handle.close()
    owned.append(process)
    return process


def stop(process):
    # Descendants can remain in our group after its leader exits.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def gui(*arguments, script=None, capture=None):
    command = ['node', str(CHECKOUT / 'scripts/gui-controller-dev.mjs')]
    command += ['--js', str(script)] if script else list(arguments)
    result = subprocess.run(command, cwd=CHECKOUT, env=base, capture_output=True, text=True)
    if capture:
        (root / 'evidence' / capture).write_text(result.stdout + result.stderr)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    return json.loads(result.stdout)


def screenshot(name):
    checked(['python3', str(HERE / 'screenshot.py'), str(root / 'evidence' / name)], env=base)


try:
    for key, value in [('start-in-overview', 'false')]:
        checked(['gsettings', 'set', 'org.gnome.shell', key, value], env=base)
    for key, value in [('picture-uri', ''), ('picture-uri-dark', ''), ('picture-options', 'none'), ('primary-color', '#ff00ff')]:
        checked(['gsettings', 'set', 'org.gnome.desktop.background', key, value], env=base)
    shell = start(['gnome-shell', '--headless', '--wayland', '--no-x11', '--virtual-monitor', '1600x1000', '--wayland-display', 'wl-dmabuf-lab'], base, 'shell.log')
    deadline = time.monotonic() + 30
    while not (Path(base['XDG_RUNTIME_DIR']) / 'wl-dmabuf-lab').is_socket():
        if shell.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError('private Wayland compositor unavailable; inspect shell.log')
        time.sleep(.2)
    vite = start([str(CHECKOUT / 'node_modules/.bin/vite'), '--host', '127.0.0.1', '--port', str(args.port), '--strictPort'], base, 'vite.log')
    import urllib.request
    deadline = time.monotonic() + 30
    while True:
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{args.port}/', timeout=1).close()
            break
        except OSError:
            if vite.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('owned Vite did not become ready')
            time.sleep(.2)
    hardware = {}
    for label, command in [('kernel', ['uname', '-a']), ('gpu', ['lspci', '-nn']), ('webkit', ['pkg-config', '--modversion', 'webkit2gtk-4.1']), ('shell', ['gnome-shell', '--version'])]:
        if shutil.which(command[0]):
            result = subprocess.run(command, env=base, text=True, capture_output=True)
            hardware[label] = result.stdout
    (root / 'evidence' / 'station.json').write_text(json.dumps(hardware, indent=2) + '\n')
    # Raising only this child session's stack does not touch the owner shell.
    import resource
    soft, hard = resource.getrlimit(resource.RLIMIT_STACK)
    desired = 256 * 1024 * 1024
    resource.setrlimit(resource.RLIMIT_STACK, (desired if hard == resource.RLIM_INFINITY else min(desired, hard), hard))
    for mode, switch in [('default', None), ('on', '0'), ('off', '1')]:
        env = base | {'WAYLAND_DISPLAY': 'wl-dmabuf-lab', 'GDK_BACKEND': 'wayland', 'AEROFTP_MASTER_PASSWORD': 'dmabuf-artificial-fixture', 'WEBKIT_INSPECTOR_HTTP_SERVER': f'127.0.0.1:{args.inspector_port}'}
        if switch is not None:
            env['WEBKIT_DISABLE_DMABUF_RENDERER'] = switch
        manifest_keys = ['GDK_BACKEND', 'DISPLAY', 'WAYLAND_DISPLAY', 'WEBKIT_DISABLE_DMABUF_RENDERER', 'WEBKIT_DISABLE_COMPOSITING_MODE', 'WEBKIT_DMABUF_RENDERER_FORCE_SHM', 'LIBGL_ALWAYS_SOFTWARE', 'MESA_LOADER_DRIVER_OVERRIDE', 'EGL_PLATFORM', 'GBM_BACKEND', '__GLX_VENDOR_LIBRARY_NAME']
        (root / 'evidence' / f'{mode}-environment.json').write_text(json.dumps({k: env.get(k) for k in manifest_keys}, indent=2) + '\n')
        app = start([str(args.binary)], env, f'evidence/{mode}-app.log')
        (root / 'app.pid').write_text(str(app.pid))
        deadline = time.monotonic() + 60
        while True:
            try:
                state = gui(script=HERE / 'inspect-runtime.body')
                if state['value']['viewport'][0] < 500 or not state['value']['body'].strip():
                    raise RuntimeError('waiting for the visible initialized main window')
                break
            except RuntimeError:
                if app.poll() is not None or time.monotonic() > deadline:
                    raise
                time.sleep(.5)  # observation only, never replay a mutation
        gui(script=HERE / 'normalize-window.body', capture=f'{mode}-window.json')
        state = gui(script=HERE / 'inspect-runtime.body')
        (root / 'evidence' / f'{mode}-runtime.json').write_text(json.dumps(state, indent=2) + '\n')
        if state['value']['rounded'] != (mode == 'on'):
            raise RuntimeError(f'{mode}: corner decision differs from the expected GPU-session result')
        if state.get('value', {}).get('locked'):
            base['AEROFTP_GUI_TEST_MASTER'] = 'dmabuf-artificial-fixture'
            gui('--unlock', capture=f'{mode}-unlock.json')
        gui('--begin', '--actor', 'DMA-BUF rendering lab', capture=f'{mode}-begin.json')
        gui('--request', '{"name":"state"}', capture=f'{mode}-state.json')
        screenshot(f'{mode}-startup.png')
        if mode != 'default':
            gui(script=HERE / 'mount-editor-terminal.body', capture=f'{mode}-mount-editor.json')
            checked(['python3', str(HERE / 'measure.py'), f'{mode}-editor-terminal', str(HERE / 'exercise-editor-terminal.body')], env=base)
            editor_result = json.loads((root / 'evidence' / f'{mode}-editor-terminal.json').read_text())['value']
            if not editor_result['terminalHasCompletion'] or editor_result['errors']:
                raise RuntimeError(f'{mode}: terminal completion or page error assertion failed')
            screenshot(f'{mode}-editor-terminal.png')
            gui(script=HERE / 'mount-player.body', capture=f'{mode}-mount-player.json')
            template = (HERE / 'measure-player-template.body').read_text()
            for kind, modes in [('canvas', ['bars', 'waveform', 'radial', 'spectrum', 'fractal', 'vortex', 'plasma', 'kaleidoscope']), ('webgl', ['wave_glitch', 'glitch_vhs', 'fractal_mandelbrot', 'raymarch_tunnel', 'metaball_pulse', 'particles_explosion'])]:
                for name in modes:
                    script = root / 'measure-current.body'
                    script.write_text(template.replace('KIND', json.dumps(kind)).replace('MODE', json.dumps(name)))
                    checked(['python3', str(HERE / 'measure.py'), f'{mode}-{kind}-{name}', str(script)], env=base)
                    receipt = json.loads((root / 'evidence' / f'{mode}-{kind}-{name}.json').read_text())['value']
                    if receipt['newErrors'] or receipt['contextLostEvents']:
                        raise RuntimeError(f'{mode}/{kind}/{name}: page errors or lost GPU context')
                    if kind == 'webgl' and (receipt['glError'] != 0 or not receipt['programLinked'] or not receipt['fragmentCompiled'] or not receipt['fragmentMatchesMode'] or len(receipt['shaderCompiled'] or []) != 2 or not all(receipt['shaderCompiled'] or [])):
                        raise RuntimeError(f'{mode}/{kind}/{name}: shader or GL assertion failed')
                    if kind == 'canvas' and not receipt['pixelSummary']['colouredPixels']:
                        raise RuntimeError(f'{mode}/{kind}/{name}: no coloured canvas content')
                    screenshot(f'{mode}-{kind}-{name}.png')
            gui(script=HERE / 'dispose-bench.body')
        gui('--finish', capture=f'{mode}-finish.json')
        stop(app)
    (root / 'evidence' / 'lab-complete.json').write_text(json.dumps({'complete': True, 'source_sha': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=CHECKOUT, text=True).strip(), 'dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=CHECKOUT))}) + '\n')
finally:
    for process in reversed(owned):
        stop(process)
