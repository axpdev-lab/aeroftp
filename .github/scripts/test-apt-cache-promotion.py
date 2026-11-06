#!/usr/bin/env python3
"""Exercise the apt-install action: finish with unchanged archives (sudo is a
local, unprivileged double), and start with a hostile cache prefix."""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ACTION_DIR = Path(__file__).resolve().parents[1] / "actions/apt-install"
SCRIPT = ACTION_DIR / "apt-install.sh"


class CachePromotion(unittest.TestCase):
    def finish(self, complete):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            archives = root / "archives"
            archives.mkdir()
            (archives / "package.deb").write_bytes(b"test")
            fake_bin = root / "bin"
            fake_bin.mkdir()
            sudo = fake_bin / "sudo"
            sudo.write_text('#!/bin/sh\nexec "$@"\n')
            sudo.chmod(0o755)
            fingerprint = hashlib.sha256(b"./package.deb 4\n").hexdigest()[:16]
            (root / "apt-install.state").write_text(
                f"before={fingerprint}\n" + ("result=complete\n" if complete else "")
            )
            output = root / "output"
            env = dict(os.environ, APT_ARCHIVES=str(archives), RUNNER_TEMP=str(root),
                       GITHUB_OUTPUT=str(output), PATH=str(fake_bin) + os.pathsep + os.environ["PATH"])
            subprocess.run(["bash", str(SCRIPT), "finish"], env=env, check=True)
            return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_unchanged_success_is_save_worthy(self):
        result = self.finish(True)
        self.assertEqual(result["kind"], "complete")
        self.assertEqual(result["changed"], "true")

    def test_unchanged_partial_stays_unsaved(self):
        result = self.finish(False)
        self.assertEqual(result["kind"], "partial")
        self.assertEqual(result["changed"], "false")


def start_step():
    """The `start` step of action.yml, as text (no YAML parser on every runner)."""
    text = (ACTION_DIR / "action.yml").read_text()
    match = re.search(r"\n\s+id: start\n(.*?)(?=\n\s*\n|\n\s+- name:|\Z)", text, re.S)
    assert match, "action.yml has no step with id: start"
    return match.group(1)


class CachePrefixInput(unittest.TestCase):
    """`inputs.cache-prefix` reaches the shell as data, never as script text."""

    def test_input_goes_through_env_not_the_script_body(self):
        step = start_step()
        self.assertIn("CACHE_PREFIX: ${{ inputs.cache-prefix }}", step)
        run = re.search(r"^\s*run: (.*)$", step, re.M).group(1)
        self.assertNotIn("${{", run)

    def run_start(self, prefix):
        step = start_step()
        run = re.search(r"^\s*run: '(.*)'$", step, re.M).group(1).replace("''", "'")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            output = root / "output"
            output.touch()
            env = dict(os.environ, GITHUB_ACTION_PATH=str(ACTION_DIR), RUNNER_TEMP=str(root),
                       GITHUB_OUTPUT=str(output), CACHE_PREFIX=prefix,
                       ImageOS="ubuntu24", RUNNER_ARCH="X64")
            done = subprocess.run(["bash", "-c", run], env=env, cwd=root,
                                  capture_output=True, text=True)
            canaries = sorted(p.name for p in root.iterdir() if p.name.startswith("CANARY"))
            return done.returncode, output.read_text(), canaries

    def test_quotes_and_substitutions_stay_literal(self):
        for prefix in ['x"; touch CANARY1; echo "', "$(touch CANARY2)", "`touch CANARY3`",
                       "it's", "${HOME}"]:
            with self.subTest(prefix=prefix):
                code, output, canaries = self.run_start(prefix)
                self.assertEqual(code, 0)
                self.assertEqual(canaries, [])
                self.assertEqual(output, f"prefix={prefix}-ubuntu24-X64-\n")

    def test_newline_is_rejected_before_any_output(self):
        for prefix in ["apt\nkind=complete", "apt\rkind=complete"]:
            with self.subTest(prefix=prefix):
                code, output, _ = self.run_start(prefix)
                self.assertNotEqual(code, 0)
                self.assertEqual(output, "")


if __name__ == "__main__":
    unittest.main()
