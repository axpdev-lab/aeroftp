#!/usr/bin/env python3
"""Exercise finish with unchanged archives; sudo is a local, unprivileged double."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "actions/apt-install/apt-install.sh"


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


if __name__ == "__main__":
    unittest.main()
