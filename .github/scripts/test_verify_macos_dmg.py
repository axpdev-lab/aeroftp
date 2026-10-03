"""Portable tests for the DMG checks; actual mounts run on macOS CI."""
import contextlib
import importlib.util
import io
import json
import os
import shutil
import textwrap
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
        fixtures = Path(__file__).resolve().parents[2] / 'src-tauri/target'
        fixtures.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='dmg-check-', dir=fixtures)
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
             patch.object(dmg, 'minimum_version', side_effect=['10.13', '11.0']), \
             patch.object(dmg, 'deployment_target', return_value=('11.0', '15.5')):
            with self.assertRaisesRegex(ValueError, 'Minimum macOS changed'):
                dmg.verify(self.root / 'new.dmg', self.root / 'old.dmg', self.root / 'Cargo.toml', 'arm64')


    def test_default_inventory_excludes_feature_gated_helper(self):
        manifest = self.root / 'Cargo.toml'
        manifest.write_text('[package]\nname="aeroftp"\nversion="0.0.0"\nedition="2021"\n'
                            '[features]\ndefault=["release-tools"]\nrelease-tools=["public-helper"]\n'
                            'public-helper=[]\ntest-seed=[]\n[[bin]]\nname="seed_test_profiles"\n'
                            'path="test-support/seed.rs"\nrequired-features=["test-seed"]\n'
                            '[[bin]]\nname="public-helper"\npath="test-support/public.rs"\n'
                            'required-features=["public-helper"]\n')
        (self.root / 'src').mkdir()
        (self.root / 'src/main.rs').touch()
        (self.root / 'test-support').mkdir()
        (self.root / 'test-support/seed.rs').touch()
        (self.root / 'test-support/public.rs').touch()
        self.assertEqual(dmg.cargo_binaries(manifest), {'aeroftp', 'public-helper'})

    def test_explicit_feature_enables_gated_helper(self):
        metadata = {'packages': [{'manifest_path': str((self.root / 'Cargo.toml').resolve()),
                    'features': {'default': [], 'test-seed': []}, 'targets': [
                        {'name': 'aeroftp', 'kind': ['bin']},
                        {'name': 'seed_test_profiles', 'kind': ['bin'],
                         'required-features': ['test-seed']},
                    ]}]}
        with patch.object(dmg.subprocess, 'check_output', return_value=json.dumps(metadata)):
            self.assertEqual(dmg.cargo_binaries(self.root / 'Cargo.toml', ['test-seed']),
                             {'aeroftp', 'seed_test_profiles'})

    def verify_with_load_commands(self, old, new, baseline_bins=frozenset({'aeroftp'})):
        output = io.StringIO()
        def commands(command, **kwargs):
            if command[0] == 'lipo':
                return 'arm64\n'
            return old if 'baseline' in str(command[-1]) else new
        with patch.object(dmg, 'mounted', side_effect=[contextlib.nullcontext(self.root / 'baseline.app'),
                                                     contextlib.nullcontext(self.app)]), \
             patch.object(dmg, 'cargo_binaries', return_value={'aeroftp'}), \
             patch.object(dmg, 'verify_payload', side_effect=[baseline_bins, {'aeroftp'}]), \
             patch.object(dmg, 'minimum_version', return_value='10.13'), \
             patch.object(dmg.subprocess, 'check_output', side_effect=commands), \
             contextlib.redirect_stdout(output):
            dmg.verify(self.root / 'new.dmg', self.root / 'old.dmg', self.root / 'Cargo.toml', 'arm64')
        return output.getvalue()

    def test_macho_minimum_increase_fails_with_unchanged_plist(self):
        old = 'Load command 1\n      cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos 11.0\n      sdk 14.5\n'
        new = old.replace('minos 11.0', 'minos 12.0').replace('sdk 14.5', 'sdk 15.5')
        with self.assertRaisesRegex(ValueError, 'Minimum macOS increased'):
            self.verify_with_load_commands(old, new)

    def test_sdk_change_with_same_macho_minimum_passes(self):
        old = 'Load command 1\n      cmd LC_BUILD_VERSION\n    minos 11.0.0\n      sdk 14.5\n'
        new = old.replace('minos 11.0.0', 'minos 11.0').replace('sdk 14.5', 'sdk 15.5')
        output = self.verify_with_load_commands(old, new)
        self.assertIn('sdk=14.5', output)
        self.assertIn('sdk=15.5', output)

    def test_removed_baseline_helper_warns_without_requiring_it(self):
        public = {'aeroftp', 'aeroftp-cli', 'aeroftp-dispatch'}
        for name in public - {'aeroftp'}:
            (self.bins / name).write_bytes(b'binary')
            (self.bins / name).chmod(0o755)
        baseline = self.root / 'baseline.app'
        shutil.copytree(self.app, baseline)
        helper = baseline / 'Contents/MacOS/seed_test_profiles'
        helper.write_bytes(b'binary')
        helper.chmod(0o755)
        output = io.StringIO()
        def commands(command, **kwargs):
            if command[0] == 'lipo':
                return 'arm64\n'
            return 'Load command 1\n cmd LC_BUILD_VERSION\n minos 11.0\n sdk 15.5\n'
        with patch.object(dmg, 'mounted', side_effect=[contextlib.nullcontext(baseline),
                                                     contextlib.nullcontext(self.app)]), \
             patch.object(dmg, 'cargo_binaries', return_value=public), \
             patch.object(dmg.subprocess, 'check_output', side_effect=commands), \
             contextlib.redirect_stdout(output):
            dmg.verify(self.root / 'new.dmg', self.root / 'old.dmg', self.root / 'Cargo.toml', 'arm64')
        self.assertIn('::warning::Baseline-only executable removed: seed_test_profiles', output.getvalue())

    def test_workflow_resolves_latest_release_and_actual_reference_path(self):
        workflow = (Path(__file__).parent.parent / 'workflows/build.yml').read_text()
        step = workflow.split('- name: Verify macOS DMG payload and minimum system version', 1)[1]
        step = step.split('\n      - name:', 1)[0]
        script = textwrap.dedent(step.split('        run: |\n', 1)[1])
        commands = self.root / 'commands'
        commands.mkdir()
        gh = commands / 'gh'
        gh.write_text('#!/bin/sh\nprintf "%s\n" "$*" >> "$GH_LOG"\n'
                      'if [ "$1" = api ]; then echo v9.9.9; else\n'
                      'touch "$RUNNER_TEMP/aeroftp-dmg-baseline/AeroFTP_9.9.9_aarch64.dmg"\nfi\n')
        python = commands / 'python3'
        python.write_text('#!/bin/sh\nprintf "%s\n" "$@" > "$VERIFY_LOG"\n'
                          'while [ "$#" -gt 0 ]; do\n'
                          'if [ "$1" = --reference ]; then shift; test -f "$1" || exit 2; fi\n'
                          'shift\ndone\n')
        gh.chmod(0o755)
        python.chmod(0o755)
        candidate = self.root / 'src-tauri/target/release/bundle/dmg'
        candidate.mkdir(parents=True)
        (candidate / 'AeroFTP_9.9.10_aarch64.dmg').touch()
        env = dict(os.environ, PATH=str(commands) + os.pathsep + os.environ['PATH'],
                   RUNNER_TEMP=str(self.root), PACKAGE_ARCH='aarch64',
                   GITHUB_REPOSITORY='axpdev-lab/aeroftp', GH_LOG=str(self.root / 'gh.log'),
                   VERIFY_LOG=str(self.root / 'verify.log'))
        result = subprocess.run(['bash', '-c', script], cwd=self.root, env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.assertEqual(result.returncode, 0, result.stdout)
        calls = (self.root / 'gh.log').read_text()
        self.assertIn('releases/latest', calls)
        self.assertIn('release download v9.9.9', calls)
        self.assertIn('AeroFTP_9.9.9_aarch64.dmg', (self.root / 'verify.log').read_text())

    def test_an_unlisted_executable_with_the_wrong_architecture_fails(self):
        helper = self.bins / 'helper'
        helper.write_bytes(b'binary')
        helper.chmod(0o755)
        def architectures(command, **kwargs):
            return 'x86_64\n' if command[-1].endswith('helper') else 'arm64\n'
        with patch.object(dmg.subprocess, 'check_output', side_effect=architectures):
            with self.assertRaisesRegex(ValueError, 'helper: expected arm64'):
                dmg.verify_payload(self.app, {'aeroftp'}, 'arm64')

    def verify_new_executable(self, helper_minimum):
        load = 'Load command 1\n      cmd LC_BUILD_VERSION\n    minos {}\n      sdk 15.5\n'
        def commands(command, **kwargs):
            if command[0] == 'lipo':
                return 'arm64\n'
            return load.format(helper_minimum if command[-1].endswith('new_helper') else '11.0')
        current = {'aeroftp', 'new_helper'}
        output = io.StringIO()
        with patch.object(dmg, 'mounted', side_effect=[contextlib.nullcontext(self.root / 'baseline.app'),
                                                     contextlib.nullcontext(self.app)]), \
             patch.object(dmg, 'cargo_binaries', return_value=current), \
             patch.object(dmg, 'verify_payload', side_effect=[{'aeroftp'}, current]), \
             patch.object(dmg, 'minimum_version', return_value='10.13'), \
             patch.object(dmg.subprocess, 'check_output', side_effect=commands), \
             contextlib.redirect_stdout(output):
            dmg.verify(self.root / 'new.dmg', self.root / 'old.dmg', self.root / 'Cargo.toml', 'arm64')
        return output.getvalue()

    def test_a_new_executable_may_not_require_more_than_the_app(self):
        # The plist says 10.13 on both architectures; the arm64 app itself starts at 11.0.
        with self.assertRaisesRegex(ValueError, 'Minimum macOS increased for new_helper: 11.0 -> 12.0'):
            self.verify_new_executable('12.0')
        self.assertIn('new_helper', self.verify_new_executable('11.0'))

    def test_intel_legacy_macho_minimum_is_read(self):
        load = 'Load command 2\n      cmd LC_VERSION_MIN_MACOSX\n  cmdsize 16\n  version 10.13\n      sdk 14.5\n'
        with patch.object(dmg.subprocess, 'check_output', return_value=load):
            self.assertEqual(dmg.deployment_target(self.binary), ('10.13', '14.5'))

    def test_missing_macho_deployment_command_fails(self):
        with patch.object(dmg.subprocess, 'check_output', return_value='Load command 0\n cmd LC_SEGMENT_64\n'):
            with self.assertRaisesRegex(ValueError, 'deployment target'):
                dmg.deployment_target(self.binary)

    def test_invalid_macho_minimum_fails(self):
        load = 'Load command 1\n cmd LC_BUILD_VERSION\n minos unknown\n sdk 15.5\n'
        with patch.object(dmg.subprocess, 'check_output', return_value=load):
            with self.assertRaises(ValueError):
                dmg.deployment_target(self.binary)

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
