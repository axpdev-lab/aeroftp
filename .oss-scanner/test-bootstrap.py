#!/usr/bin/env python3
"""Guard scanner toolchain provenance without building the heavyweight image."""
from pathlib import Path
import re
import unittest


class BootstrapPolicy(unittest.TestCase):
    def test_bootstraps_are_content_pinned_and_verified_before_execution(self):
        dockerfile = Path(__file__).with_name("Dockerfile").read_text()
        self.assertRegex(dockerfile, r"FROM ubuntu:24\.04@sha256:[0-9a-f]{64}")
        self.assertNotRegex(dockerfile, r"curl[^\n]*\|\s*(?:bash|sh)")
        self.assertNotIn("sh.rustup.rs", dockerfile)
        self.assertNotIn("deb.nodesource.com/setup_", dockerfile)
        for name in ("node", "rust"):
            self.assertRegex(dockerfile, rf'{name}_sha="[0-9a-f]{{64}}"')
            verify = dockerfile.index(f'sha256sum --check --strict {name}.sha256')
            unpack = dockerfile.index(f'tar -xJf {name}.tar.xz')
            self.assertLess(verify, unpack)
        self.assertIn("node-v24.", dockerfile)
        self.assertIn("rust-1.97.1-", dockerfile)
        self.assertIn('*) echo "Unsupported scanner architecture:', dockerfile)


if __name__ == "__main__":
    unittest.main()
