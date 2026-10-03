"""Portable tests for the DMG checks; actual mounts run on macOS CI."""
import importlib.util
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('dmg', Path(__file__).with_name('verify-macos-dmg.py'))
dmg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dmg)


class DmgVerificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.app = self.root / 'AeroFTP.app'
        self.bins = self.app / 'Contents/MacOS'
        self.bins.mkdir(parents=True)
        with (self.app / 'Contents/Info.plist').open('wb') as output:
            plistlib.dump({'LSMinimumSystemVersion': '10.13'}, output)
        self.binary = self.bins / 'aeroftp'
        self.binary.write_bytes(b'binary')
        self.binary.chmod(0o755)

    def test_cargo_inventory_includes_explicit_and_implicit_bins(self):
        manifest = self.root / 'Cargo.toml'
        manifest.write_text('[package]\nname="aeroftp"\nedition="2021"\n[[bin]]\nname="aeroftp-cli"\npath="src/bin/aeroftp_cli.rs"\n')
        bins = self.root / 'src/bin'
        bins.mkdir(parents=True)
        (bins / 'aeroftp_cli.rs').touch()
        (bins / 'new_helper.rs').touch()
        (self.root / 'src/main.rs').touch()
        self.assertEqual(dmg.cargo_binaries(manifest), {'aeroftp', 'aeroftp-cli', 'new_helper'})

    def test_missing_helper_fails(self):
        with self.assertRaisesRegex(ValueError, 'Missing executable'):
            dmg.verify_payload(self.app, {'aeroftp-cli'}, 'arm64')

    def test_non_executable_fails(self):
        self.binary.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'Missing executable'):
            dmg.verify_payload(self.app, {'aeroftp'}, 'arm64')

    def test_wrong_architecture_fails(self):
        with patch.object(dmg.subprocess, 'check_output', return_value='x86_64\n'):
            with self.assertRaisesRegex(ValueError, 'expected arm64'):
                dmg.verify_payload(self.app, {'aeroftp'}, 'arm64')

    def test_complete_payload_passes(self):
        with patch.object(dmg.subprocess, 'check_output', return_value='arm64\n'):
            self.assertEqual(dmg.verify_payload(self.app, {'aeroftp'}, 'arm64'), {'aeroftp'})

    def test_changed_minimum_fails(self):
        with patch.object(dmg, 'mounted', return_value=__import__('contextlib').nullcontext(self.app)), \
             patch.object(dmg, 'cargo_binaries', return_value={'aeroftp'}), \
             patch.object(dmg, 'verify_payload', return_value={'aeroftp'}), \
             patch.object(dmg, 'minimum_version', side_effect=['10.13', '11.0']):
            with self.assertRaisesRegex(ValueError, 'Minimum macOS changed'):
                dmg.verify(self.root / 'new.dmg', self.root / 'old.dmg', self.root / 'Cargo.toml', 'arm64')

    def test_mount_accepts_the_packaged_license_without_a_terminal(self):
        def attach(command, **kwargs):
            if command[:2] == ['hdiutil', 'attach']:
                if kwargs.get('input') != 'yes\n' or not kwargs.get('text'):
                    raise subprocess.CalledProcessError(1, command, stderr='hdiutil: attach canceled')
        with patch.object(dmg.subprocess, 'run', side_effect=attach) as run:
            with dmg.mounted(self.root / 'licensed.dmg'):
                pass
            self.assertEqual(run.call_args_list[-1].args[0][:2], ['hdiutil', 'detach'])

    def test_mount_is_read_only_and_detached_on_validation_failure(self):
        with patch.object(dmg.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'validation failure'):
                with dmg.mounted(self.root / 'fixture.dmg'):
                    raise ValueError('validation failure')
            self.assertIn('-readonly', run.call_args_list[0].args[0])
            self.assertEqual(run.call_args_list[1].args[0][:2], ['hdiutil', 'detach'])


if __name__ == '__main__':
    unittest.main()
