"""Exercise Tauri's real release binary selection without compiling AeroFTP."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / 'src-tauri/Cargo.toml'
CLI = ROOT / 'node_modules/.bin/tauri'


class ReleaseBinaryGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        metadata = json.loads(subprocess.check_output([
            'cargo', 'metadata', '--offline', '--no-deps', '--format-version', '1',
            '--manifest-path', str(MANIFEST),
        ], cwd=MANIFEST.parent, text=True))
        cls.package = next(p for p in metadata['packages'] if Path(p['manifest_path']) == MANIFEST)
        cls.targets = [t for t in cls.package['targets'] if 'bin' in t['kind']]

    def bundle_binaries(self, features=()):
        # Use the real manifest's target names, required features and source
        # locations: the last matters because Tauri also discovers src/bin.
        fixtures = MANIFEST.parent / 'target'
        fixtures.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='release-gate-', dir=fixtures) as temporary:
            root = Path(temporary)
            tauri = root / 'src-tauri'
            tauri.mkdir()
            manifest = ['[package]', 'name="aeroftp"', 'version="0.0.0"',
                        'edition="2021"', 'default-run="aeroftp"', '[features]']
            for name, enabled in self.package['features'].items():
                # No dependencies or builds in this selection fixture. Keep
                # actual defaults; other feature entries only declare names.
                manifest.append(f'{json.dumps(name)} = {json.dumps(enabled if name == "default" else [])}')
            for target in self.targets:
                source = Path(target['src_path']).relative_to(MANIFEST.parent)
                (tauri / source).parent.mkdir(parents=True, exist_ok=True)
                (tauri / source).write_text('fn main() {}\n')
                manifest += ['[[bin]]', f'name={json.dumps(target["name"])}',
                             f'path={json.dumps(str(source))}',
                             f'required-features={json.dumps(target.get("required-features", []))}']
            manifest.append('[workspace]')
            (tauri / 'Cargo.toml').write_text('\n'.join(manifest) + '\n')
            (root / 'dist').mkdir()
            (root / 'dist/index.html').write_text('<html></html>')
            (tauri / 'icons').mkdir()
            shutil.copyfile(MANIFEST.parent / 'icons/32x32.png', tauri / 'icons/icon.png')
            (tauri / 'tauri.conf.json').write_text(json.dumps({
                'productName': 'AeroFTP', 'version': '0.0.0',
                'identifier': 'app.aeroftp.release-gate',
                'build': {'frontendDist': '../dist'},
                'bundle': {'active': True, 'targets': ['deb'], 'icon': ['icons/icon.png']},
            }))
            release = tauri / 'target/release'
            release.mkdir(parents=True)
            # Include even the gated target's stale artifact: the bundler
            # must omit it rather than depend on a clean target directory.
            for target in self.targets:
                shutil.copyfile('/usr/bin/true', release / target['name'])
                (release / target['name']).chmod(0o755)
            env = dict(os.environ)
            env.pop('CARGO_TARGET_DIR', None)
            command = [str(CLI), 'bundle', '--ci', '--bundles', 'deb']
            if features:
                command += ['--features', ','.join(features)]
            result = subprocess.run(command, cwd=root, env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            self.assertEqual(result.returncode, 0, result.stdout)
            images = list((release / 'bundle/deb').glob('*.deb'))
            self.assertEqual(len(images), 1)
            unpack = root / 'unpacked'
            subprocess.run(['dpkg-deb', '--extract', str(images[0]), str(unpack)], check=True)
            return {p.name for p in (unpack / 'usr/bin').iterdir()}

    def test_release_bundle_contains_only_public_binaries(self):
        self.assertEqual(self.bundle_binaries(), {'aeroftp', 'aeroftp-cli', 'aeroftp-dispatch'})

    def test_seed_helper_is_available_when_explicitly_enabled(self):
        self.assertEqual(self.bundle_binaries(['test-seed']),
                         {'aeroftp', 'aeroftp-cli', 'aeroftp-dispatch', 'seed_test_profiles'})


if __name__ == '__main__':
    unittest.main()
