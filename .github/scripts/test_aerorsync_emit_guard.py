"""Run scripts/aerorsync-emit-guard-tests.sh with a git whose status fails.

Two scenarios of the guard ask git whether the emitter left tracked files
alone: `public/` after the emitter refused it, and the repository root after
the `..` bypass. They used to read `git status --porcelain ... | wc -l`, so a
git that failed printed nothing, wc counted 0 lines, and both read "intact".
A stub git on PATH fails `status` and hands every other command to the real
git; the emitter itself does not call git.

The guard runs the emitter for real, as the aerorsync-standalone lane does
(about three seconds), and cleans up after itself.
"""

from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
GUARD = ROOT / 'scripts/aerorsync-emit-guard-tests.sh'
INTACT = ("e quell'albero e' intatto", "e la radice del repository e' intatta")


class EmitGuardTests(unittest.TestCase):
    def run_guard(self, git_status=None):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(os.environ)
            if git_status is not None:
                bin_dir = Path(tmp) / 'bin'
                bin_dir.mkdir()
                git = bin_dir / 'git'
                git.write_text(
                    '#!/bin/sh\n'
                    f'if [ "$1" = status ]; then {git_status}; fi\n'
                    f'exec "{shutil.which("git")}" "$@"\n'
                )
                git.chmod(0o755)
                env['PATH'] = f'{bin_dir}:{env["PATH"]}'
            return subprocess.run(['bash', str(GUARD)], cwd=ROOT, env=env,
                                  capture_output=True, text=True, check=False)

    def test_guard_passes_with_git(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r'scenari: \d+ superati, 0 falliti')
        for name in INTACT:
            self.assertIn(f'  ok    {name}\n', result.stdout)

    def test_failing_git_status_is_not_intact(self):
        result = self.run_guard('echo "fatal: not a git repository" >&2; exit 128')
        self.assertEqual(result.returncode, 1, result.stdout)
        for name in INTACT:
            self.assertIn(f'  FALLITO {name} (atteso 0, ottenuto git fallito)\n', result.stdout)
        self.assertRegex(result.stdout, r'scenari: \d+ superati, 2 falliti')


if __name__ == '__main__':
    unittest.main()
