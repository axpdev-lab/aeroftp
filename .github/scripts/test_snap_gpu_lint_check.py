"""Exercise the snapcraft lint pass of scripts/snap-gpu-lint-check.sh.

The real `snapcraft lint` needs a build provider, so a stub `snapcraft` on PATH
plays each outcome the gate has to tell apart. A stub `unsquashfs` lists a
clean snap, so the content pass always succeeds and only the lint pass decides.
The stub output follows snapcraft's own report: findings are printed as
"- <linter>: <file>: <text>" under a "Lint warnings:" style header, and a
clean run prints no report at all (snapcraft/linters/linters.py, `report`).
"""

from pathlib import Path
import os
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/snap-gpu-lint-check.sh'

CLEAN_LISTING = """Parallel unsquashfs: Using 4 processors
3 inodes (3 blocks) to write

squashfs-root
squashfs-root/meta
squashfs-root/meta/snap.yaml
squashfs-root/usr/bin/aeroftp
"""

NO_NETWORK = """Running linter.
Checking build provider availability.
Launching instance...
A network related operation failed in a context of no network access.
Recommended resolution: Verify that the environment has internet connectivity.
"""


def stub(path, body):
    path.write_text('#!/usr/bin/env bash\n' + body)
    path.chmod(0o755)


class SnapLintPassTests(unittest.TestCase):
    def run_gate(self, lint_output, lint_rc):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bin_dir = tmp / 'bin'
            bin_dir.mkdir()
            (tmp / 'listing.txt').write_text(CLEAN_LISTING)
            (tmp / 'lint.txt').write_text(lint_output)
            stub(bin_dir / 'unsquashfs', f'cat "{tmp / "listing.txt"}"\n')
            stub(bin_dir / 'snapcraft', f'cat "{tmp / "lint.txt"}"\nexit {lint_rc}\n')
            snap = tmp / 'aeroftp.snap'
            snap.write_bytes(b'not read by the stub')
            env = dict(os.environ, PATH=f'{bin_dir}:{os.environ["PATH"]}')
            result = subprocess.run(
                ['bash', str(SCRIPT), str(snap)],
                capture_output=True, text=True, env=env, check=False,
            )
            return result.returncode, result.stdout + result.stderr

    def test_a_clean_lint_is_reported_clean(self):
        rc, out = self.run_gate('Running linter.\n', 0)
        self.assertEqual(rc, 0, out)
        self.assertIn('OK: snapcraft lint ran and reports no gpu: warnings.', out)
        self.assertNotIn('UNVERIFIED', out)

    def test_a_gpu_finding_fails_the_gate(self):
        report = 'Running linter.\nLint warnings:\n- gpu: usr/lib/dri/iris_dri.so: GPU library primed in the snap.\n'
        rc, out = self.run_gate(report, 0)
        self.assertEqual(rc, 1, out)
        self.assertIn('::error::snapcraft lint still reports gpu: warnings', out)
        self.assertNotIn('OK: snapcraft lint', out)

    def test_a_gpu_finding_with_a_non_zero_exit_fails_the_gate(self):
        report = 'Lint errors:\n- gpu: usr/lib/libgbm.so.1: GPU library primed in the snap.\n'
        rc, out = self.run_gate(report, 1)
        self.assertEqual(rc, 1, out)
        self.assertIn('::error::snapcraft lint still reports gpu: warnings', out)

    def test_a_linter_that_never_ran_is_unverified_not_ok(self):
        rc, out = self.run_gate(NO_NETWORK, 1)
        self.assertEqual(rc, 0, out)
        self.assertIn('::warning::snapcraft lint did not run (Recommended resolution:', out)
        self.assertIn('#465 criterion 5 is UNVERIFIED in this run', out)
        self.assertNotIn('OK: snapcraft lint', out)

    def test_a_linter_that_exits_with_no_output_is_unverified(self):
        rc, out = self.run_gate('', 1)
        self.assertEqual(rc, 0, out)
        self.assertIn('::warning::snapcraft lint did not run (exit 1, no output)', out)
        self.assertNotIn('OK: snapcraft lint', out)

    def test_a_timestamped_gpu_finding_fails_the_gate(self):
        # Debug and trace verbosity put a craft-cli timestamp in front of
        # every line.
        report = (
            '2026-10-04 12:00:01.123 Running linter.\n'
            '2026-10-04 12:00:01.456 Lint warnings:\n'
            '2026-10-04 12:00:01.457 - gpu: usr/lib/dri/iris_dri.so: GPU library primed in the snap.\n'
        )
        rc, out = self.run_gate(report, 0)
        self.assertEqual(rc, 1, out)
        self.assertIn('::error::snapcraft lint still reports gpu: warnings', out)

    def test_a_timestamped_report_header_counts_as_a_run(self):
        report = '2026-10-04 12:00:01.456 Lint warnings:\n2026-10-04 12:00:01.457 - library: libfoo.so.1: unused library.\n'
        rc, out = self.run_gate(report, 1)
        self.assertEqual(rc, 0, out)
        self.assertNotIn('UNVERIFIED', out)
        self.assertIn('OK: snapcraft lint ran and reports no gpu: warnings.', out)

    def test_library_findings_warn_and_still_pass(self):
        report = 'Lint warnings:\n- library: libfoo.so.1: unused library usr/lib/libfoo.so.1.\n'
        rc, out = self.run_gate(report, 0)
        self.assertEqual(rc, 0, out)
        self.assertIn('::warning::snapcraft lint reports library: warnings', out)
        self.assertIn('OK: snapcraft lint ran and reports no gpu: warnings.', out)


if __name__ == '__main__':
    unittest.main()
