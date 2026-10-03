#!/usr/bin/env python3
"""Mount a release DMG read-only and compare its payload with a known release."""

import argparse
import contextlib
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import tomllib


def cargo_binaries(manifest):
    """Include explicit binaries and Cargo's implicitly discovered GUI/helpers."""
    with manifest.open('rb') as source:
        cargo = tomllib.load(source)
    names = {binary['name'] for binary in cargo.get('bin', [])}
    if cargo['package'].get('autobins', True):
        if (manifest.parent / 'src/main.rs').is_file():
            names.add(cargo['package']['name'])
        bins = manifest.parent / 'src/bin'
        if bins.is_dir():
            for path in bins.iterdir():
                if path.suffix == '.rs':
                    names.add(path.stem)
                elif (path / 'main.rs').is_file():
                    names.add(path.name)
        # Explicit paths replace the automatically discovered target at that path.
        for binary in cargo.get('bin', []):
            path = Path(binary.get('path', ''))
            if path.parent == Path('src/bin'):
                names.discard(path.stem)
                names.add(binary['name'])
    return names


@contextlib.contextmanager
def mounted(dmg):
    with tempfile.TemporaryDirectory(prefix='aeroftp-dmg-') as directory:
        mount = Path(directory) / 'volume'
        mount.mkdir()
        subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint',
                        str(mount), str(dmg.resolve())], check=True)
        try:
            yield mount / 'AeroFTP.app'
        finally:
            subprocess.run(['hdiutil', 'detach', str(mount)], check=True)


def minimum_version(app):
    with (app / 'Contents/Info.plist').open('rb') as source:
        value = plistlib.load(source)['LSMinimumSystemVersion']
    print(f'{app}: LSMinimumSystemVersion={value}', flush=True)
    return value


def verify_payload(app, required, arch):
    directory = app / 'Contents/MacOS'
    print(f'{directory}: {sorted(path.name for path in directory.iterdir())}', flush=True)
    for name in sorted(required):
        binary = directory / name
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(f'Missing executable: {binary}')
        architectures = subprocess.check_output(['lipo', '-archs', str(binary)], text=True).split()
        if architectures != [arch]:
            raise ValueError(f'{binary}: expected {arch}, found {architectures}')
    return {path.name for path in directory.iterdir() if path.is_file()}


def verify(dmg, reference, manifest, arch):
    required = cargo_binaries(manifest)
    with mounted(reference) as baseline:
        baseline_minimum = minimum_version(baseline)
        baseline_bins = verify_payload(baseline, {'aeroftp', 'aeroftp-cli', 'aeroftp-dispatch'}, arch)
    with mounted(dmg) as app:
        current_minimum = minimum_version(app)
        if current_minimum != baseline_minimum:
            raise ValueError(f'Minimum macOS changed: {baseline_minimum} -> {current_minimum}')
        verify_payload(app, required | baseline_bins, arch)
    print(f'DMG verification passed: {dmg.name}, {arch}, minimum macOS {baseline_minimum}', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dmg', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, default=Path('src-tauri/Cargo.toml'))
    parser.add_argument('--arch', choices=['arm64', 'x86_64'], required=True)
    args = parser.parse_args()
    verify(args.dmg, args.reference, args.manifest, args.arch)
