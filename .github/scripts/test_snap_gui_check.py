"""Exercise scripts/snap-gui-check.sh (gate G3) with stub tools.

The real gate needs an installed snap, Xvfb and ImageMagick. Here a stand-in
app prints a log and stays alive, and stub `Xvfb`, `xwd` and `magick` stand
in for the display and the frame, so each case decides on the log alone. The
log in fixtures/snap-gui/ is the real confined GUI log of v4.2.3 from CI
(build-snap, run 38983118424).

The gate used to read EGL only through Mesa and WebKit error strings, and the
frontend only through window diagnostics that the splash safety timeout
prints as well. A fallback to software compositing with the error worded
differently, and a startup finished by the timeout with its line worded
differently, both printed OK. Those cases are below, and the last test pins
the app lines the gate now waits for to the Rust source that prints them.
"""

from pathlib import Path
import os
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/snap-gui-check.sh'
REAL_LOG = (Path(__file__).resolve().parent / 'fixtures/snap-gui/confined-4.2.3.txt').read_text()

ACCELERATED = 'WebKit accelerated compositing enabled: EGL is usable on X11'
FINISHED_BY_FRONTEND = 'Finishing startup (app_ready)'


def stub(path, body):
    path.write_text('#!/usr/bin/env bash\n' + body)
    path.chmod(0o755)


class SnapGuiCheckTests(unittest.TestCase):
    def run_gate(self, log, *, colours=3221, colours_rc=0, wait_seconds=5):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bin_dir = tmp / 'bin'
            bin_dir.mkdir()
            for tool in ('bash', 'cat', 'grep', 'sed', 'seq', 'sleep', 'mktemp',
                         'rm', 'tr', 'setsid'):
                (bin_dir / tool).symlink_to(shutil.which(tool))
            stub(bin_dir / 'Xvfb', 'exec sleep 60\n')
            stub(bin_dir / 'xwd', 'echo xwd-frame\n')
            stub(bin_dir / 'magick', (
                f'for a in "$@"; do [ "$a" = info: ] && {{ printf "%s\\n" "{colours}"; exit {colours_rc}; }}; done\n'
                'out="${@: -1}"; echo png > "${out#png:}"\n'
            ))
            (tmp / 'app.log').write_text(log)
            app = tmp / 'aeroftp'
            stub(app, f'cat "{tmp / "app.log"}"\nexec sleep 30\n')
            env = {
                'PATH': str(bin_dir),
                'HOME': str(tmp),
                'SNAP_GUI_WAIT_SECONDS': str(wait_seconds),
                'SNAP_GUI_SETTLE_SECONDS': '0',
                'SNAP_GUI_SCREENSHOT': str(tmp / 'shot.png'),
            }
            result = subprocess.run(
                ['bash', str(SCRIPT), str(app)],
                capture_output=True, text=True, env=env, check=False, timeout=60,
            )
            return result.returncode, result.stdout + result.stderr

    def assert_red(self, log, message, **kwargs):
        rc, out = self.run_gate(log, **kwargs)
        self.assertEqual(rc, 1, out)
        self.assertIn(message, out)
        self.assertNotIn('OK: confined snap GUI', out)

    def test_the_real_log_passes(self):
        self.assertIn(ACCELERATED, REAL_LOG)
        rc, out = self.run_gate(REAL_LOG)
        self.assertEqual(rc, 0, out)
        self.assertIn('Captured frame:', out)
        self.assertIn('OK: confined snap GUI got a usable EGL, reached app_ready', out)

    # -- EGL: the app's own answer, not the absence of an error string -------

    def test_a_fallback_to_software_compositing_fails(self):
        # EGL failed, Mesa said so in words the error grep does not know, and
        # WebKit painted on the CPU: every other check passes.
        log = REAL_LOG.replace(
            ACCELERATED, 'WebKit software compositing: eglInitialize failed (asked about X11)',
        ).replace(
            'libEGL warning: DRI3 error: Could not get DRI3 device',
            'libEGL error: no usable display for platform x11',
        )
        self.assert_red(
            log, '::error::the app did not get a usable EGL inside the snap: '
            'WebKit software compositing: eglInitialize failed (asked about X11)')

    def test_a_fallback_after_a_crashed_probe_fails(self):
        log = REAL_LOG.replace(
            f'[INFO] {ACCELERATED}',
            '[WARN] WebKit software compositing: a previous GPU probe never came back. '
            'Delete /home/runner/snap/aeroftp/x1/.local/share/aeroftp/gpu-probe-incomplete to try the GPU again.',
        )
        self.assert_red(log, 'did not get a usable EGL inside the snap: WebKit software compositing: a previous GPU probe')

    def test_compositing_forced_by_the_environment_fails(self):
        log = REAL_LOG.replace(
            ACCELERATED,
            'WebKit compositing left to the environment: WEBKIT_DISABLE_COMPOSITING_MODE is already set',
        )
        self.assert_red(log, 'did not get a usable EGL inside the snap: WebKit compositing left to the environment')

    def test_no_egl_decision_fails(self):
        log = ''.join(line for line in REAL_LOG.splitlines(keepends=True) if ACCELERATED not in line)
        self.assert_red(log, '::error::the app logged no EGL compositing decision')

    def test_a_known_egl_error_still_fails(self):
        log = REAL_LOG + 'Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...\n'
        self.assert_red(log, '::error::WebKit could not initialize EGL inside the snap')

    # -- the frontend: only the app_ready command proves it booted ----------

    def test_a_startup_finished_by_the_safety_timeout_fails(self):
        # The timeout runs the same finish_startup sequence, so the window
        # diagnostics the gate used to accept are all there.
        log = REAL_LOG.replace(
            FINISHED_BY_FRONTEND,
            'Splash safety timeout fired, force-closing\n'
            '[2026-10-17][01:46:34][ftp_client_gui_lib][INFO] Finishing startup (safety timeout)',
        )
        self.assertIn('[diag #290] app_ready +300ms: is_visible=true', log)
        self.assert_red(
            log, "::error::the frontend never signalled app_ready: the app logged 'Finishing startup (safety timeout)'",
            wait_seconds=2)

    def test_a_window_that_never_became_visible_fails(self):
        log = REAL_LOG.replace('is_visible=true', 'is_visible=false')
        self.assert_red(log, '::error::the snap GUI did not reach a visible app_ready state', wait_seconds=2)

    def test_a_blank_frame_fails(self):
        self.assert_red(REAL_LOG, '::error::the snap GUI window is blank: only 3 distinct colours', colours=3)

    def test_a_colour_count_the_gate_cannot_read_fails(self):
        # Digits picked out of a notice would read this blank frame as 73.
        for printed in ('IMv7 notice\n3', '3 colours', '', 'unknown'):
            with self.subTest(printed=printed):
                self.assert_red(REAL_LOG, '::error::could not measure the captured frame', colours=printed)

    def test_a_colour_count_from_a_failed_measurement_fails(self):
        # A number on stdout is not a measurement when ImageMagick failed.
        self.assert_red(REAL_LOG, '::error::ImageMagick failed to measure the captured frame',
                        colours=250000, colours_rc=1)

    # -- the lines above are printed by the app ------------------------------

    def test_the_app_prints_the_lines_the_gate_waits_for(self):
        egl = (ROOT / 'src-tauri/src/linux_egl.rs').read_text()
        for line in (
            '"WebKit accelerated compositing enabled: EGL is usable on {}"',
            '"WebKit software compositing: {reason}',
            '"WebKit compositing left to the environment: {DISABLE_COMPOSITING} is already set"',
        ):
            self.assertIn(line, egl, 'scripts/snap-gui-check.sh reads this line; change both together')

        lib = (ROOT / 'src-tauri/src/lib.rs').read_text()
        for line in (
            'info!("Finishing startup ({by})");',
            'info!("[diag #290] {ctx}: is_visible={v}")',
            '"app_ready +300ms"',
            'warn!("Splash screen safety timeout reached',
        ):
            self.assertIn(line, lib, 'scripts/snap-gui-check.sh reads this line; change both together')

        # "Finishing startup (app_ready)" has to mean the frontend called:
        # every finish_startup call that passes "app_ready" sits in the body
        # of the app_ready command, and there is one.
        command = re.search(r'\nasync fn app_ready\(.*?\n\}\n', lib, re.S)
        self.assertIsNotNone(command, 'the app_ready command was not found in lib.rs')
        calls = [m for m in re.finditer(r'finish_startup\(([^;]*?)\)\s*\.await', lib, re.S)
                 if m.group(1).rstrip().rstrip(',').endswith('"app_ready"')]
        self.assertEqual(len(calls), 1, [m.group(0) for m in calls])
        self.assertTrue(command.start() <= calls[0].start() < command.end(), calls[0].group(0))


if __name__ == '__main__':
    unittest.main()
