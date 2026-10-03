#!/usr/bin/env python3
"""Mount a release DMG read-only and compare its payload with a known release."""

import argparse
import contextlib
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile


def cargo_binaries(manifest, features=()):
    """Include enabled explicit and implicitly discovered Cargo binaries."""
    metadata = json.loads(subprocess.check_output([
        'cargo', 'metadata', '--offline', '--no-deps', '--format-version', '1',
        '--manifest-path', str(manifest.resolve()),
    ], text=True))
    package = next(package for package in metadata['packages']
                   if Path(package['manifest_path']).resolve() == manifest.resolve())
    enabled = set()
    pending = ['default', *features]
    while pending:
        feature = pending.pop()
        if feature in enabled or feature.startswith('dep:') or '/' in feature:
            continue
        enabled.add(feature)
        pending.extend(package['features'].get(feature, []))
    return {target['name'] for target in package['targets'] if 'bin' in target['kind']
            and set(target.get('required-features', [])).issubset(enabled)}


@contextlib.contextmanager
def mounted(dmg):
    with tempfile.TemporaryDirectory(prefix='aeroftp-dmg-') as directory:
        mount = Path(directory) / 'volume'
        mount.mkdir()
        # Both published and newly bundled installers embed our GPL license.
        # Supply its answer through stdin so CI can mount without a terminal.
        subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint',
                        str(mount), str(dmg.resolve())], input='yes\n', text=True, check=True)
        try:
            yield mount / 'AeroFTP.app'
        finally:
            subprocess.run(['hdiutil', 'detach', str(mount)], check=True)


def minimum_version(app):
    with (app / 'Contents/Info.plist').open('rb') as source:
        value = plistlib.load(source)['LSMinimumSystemVersion']
    print(f'{app}: LSMinimumSystemVersion={value}', flush=True)
    return value


def version_tuple(value):
    if not re.fullmatch(r'[0-9]+(?:\.[0-9]+){0,2}', value):
        raise ValueError(f'Invalid macOS version: {value}')
    parts = tuple(int(part) for part in value.split('.'))
    return parts + (0,) * (3 - len(parts))


def deployment_target(binary):
    """Read the actual Mach-O floor and SDK, including pre-LC_BUILD_VERSION Intel binaries."""
    output = subprocess.check_output(['otool', '-l', str(binary)], text=True)
    targets = []
    for block in re.split(r'(?m)^Load command [0-9]+\s*$', output):
        fields = dict(re.findall(r'^\s*(cmd|minos|version|sdk)\s+(\S+)\s*$', block, re.MULTILINE))
        command = fields.get('cmd')
        if command in ('LC_BUILD_VERSION', 'LC_VERSION_MIN_MACOSX'):
            minimum = fields.get('minos' if command == 'LC_BUILD_VERSION' else 'version')
            sdk = fields.get('sdk')
            if minimum is None or sdk is None:
                raise ValueError(f'{binary}: incomplete Mach-O deployment target')
            version_tuple(minimum)
            version_tuple(sdk)
            targets.append((minimum, sdk))
    if len(targets) != 1:
        raise ValueError(f'{binary}: expected one Mach-O deployment target, found {len(targets)}')
    minimum, sdk = targets[0]
    print(f'{binary}: minimum macOS={minimum}, sdk={sdk}', flush=True)
    return minimum, sdk


def verify_payload(app, required, arch):
    directory = app / 'Contents/MacOS'
    print(f'{directory}: {sorted(path.name for path in directory.iterdir())}', flush=True)
    for name in sorted(required):
        binary = directory / name
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(f'Missing executable: {binary}')
    # Every packaged executable, required or not, must match the image's architecture.
    executables = {path.name for path in directory.iterdir() if path.is_file() and os.access(path, os.X_OK)}
    for name in sorted(executables):
        binary = directory / name
        architectures = subprocess.check_output(['lipo', '-archs', str(binary)], text=True).split()
        if architectures != [arch]:
            raise ValueError(f'{binary}: expected {arch}, found {architectures}')
    return executables


def verify(dmg, reference, manifest, arch, features=()):
    required = cargo_binaries(manifest, features)
    with mounted(reference) as baseline:
        baseline_minimum = minimum_version(baseline)
        baseline_bins = verify_payload(baseline, {'aeroftp', 'aeroftp-cli', 'aeroftp-dispatch'}, arch)
        baseline_targets = {name: deployment_target(baseline / 'Contents/MacOS' / name)
                            for name in sorted(baseline_bins)}
    with mounted(dmg) as app:
        current_minimum = minimum_version(app)
        if current_minimum != baseline_minimum:
            raise ValueError(f'Minimum macOS changed: {baseline_minimum} -> {current_minimum}')
        current_bins = verify_payload(app, required, arch)
        for name in sorted(baseline_bins - current_bins):
            print(f'::warning::Baseline-only executable removed: {name}', flush=True)
        # A new executable is held to the app's own floor on this architecture:
        # the plist advertises 10.13 on both, but Apple Silicon binaries start at 11.0.
        app_floor = baseline_targets['aeroftp'][0]
        for name in sorted(current_bins):
            minimum, _sdk = deployment_target(app / 'Contents/MacOS' / name)
            reference = baseline_targets[name][0] if name in baseline_targets else app_floor
            if name not in baseline_targets:
                print(f'New executable {name}: held to the app floor {app_floor}', flush=True)
            if version_tuple(minimum) > version_tuple(reference):
                raise ValueError(f'Minimum macOS increased for {name}: {reference} -> {minimum}')
    print(f'DMG verification passed: {dmg.name}, {arch}, verified Mach-O deployment targets, plist minimum {baseline_minimum}', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dmg', type=Path, required=True)
    parser.add_argument('--reference', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, default=Path('src-tauri/Cargo.toml'))
    parser.add_argument('--arch', choices=['arm64', 'x86_64'], required=True)
    parser.add_argument('--features', default='', help='Comma-separated release features in addition to defaults')
    args = parser.parse_args()
    features = [feature.strip() for feature in args.features.split(',') if feature.strip()]
    verify(args.dmg, args.reference, args.manifest, args.arch, features)
