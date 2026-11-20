"""Exercise scripts/archive-appimage-sources.py with a stub gh, a stub AppImage
and a stub Launchpad.

The AppImage's written offer promises the source of each bundled library for
at least three years, and names the release `appimage-sources` that keeps it.
The publish job runs the script before it publishes: a source it could not
keep, or a release it could not read back, must stop the job, never pass.

The stub gh keeps the release in a folder (one file per asset, plus
release.json), the stub AppImage answers `--appimage-extract` with the list a
test gives it, and the stub fetch serves a Launchpad laid out as
<folder>/<source>/<version>/<file>.
"""

from pathlib import Path
import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/archive-appimage-sources.py'
REPO = 'axpdev-lab/aeroftp'

HEADER = (
    'Libraries this AppImage bundles. Each line: the library, ...\n'
    'The terms of each package are in usr/share/doc/<package>/copyright.\n'
    'Written offer: for at least three years ...\n'
    '\n'
)

STUB_GH = r'''#!/usr/bin/env python3
import json, os, shutil, sys
state = os.environ['STUB_STATE']
log = os.path.join(state, 'calls.log')
with open(log, 'a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\n')
rel = os.path.join(state, 'release.json')
assets = os.path.join(state, 'assets')
fail = os.environ.get('STUB_FAIL', '')
args = sys.argv[1:]

def release():
    with open(rel) as f:
        r = json.load(f)
    r['assets'] = [{'name': n, 'size': os.path.getsize(os.path.join(assets, n))} for n in sorted(os.listdir(assets))]
    return r

tags = os.path.join(state, 'tags')
if args[:2] == ['api', '--include'] and not args[2].endswith('/releases/tags/appimage-sources'):
    # The AeroFTP release of the tag being published: state/tags/<tag>/ holds
    # its assets when it already exists.
    tag = args[2].rsplit('/', 1)[-1]
    if fail == 'tag-lookup':
        print('HTTP/2.0 502 Bad Gateway\nContent-Type: application/json\r\n\r\n{"message":"Bad Gateway"}')
        sys.exit(1)
    folder = os.path.join(tags, tag)
    if not os.path.isdir(folder):
        print('HTTP/2.0 404 Not Found\nContent-Type: application/json; charset=utf-8\r\n\r\n{"message":"Not Found","status":"404"}')
        sys.exit(1)
    r = {'tag_name': tag, 'body': '', 'assets': [{'name': n} for n in sorted(os.listdir(folder))]}
    print('HTTP/2.0 200 OK\nContent-Type: application/json; charset=utf-8\r\n\r\n' + json.dumps(r))
    sys.exit(0)
if args[:2] == ['release', 'download'] and args[2] != 'appimage-sources':
    if fail == 'tag-download':
        print('download refused', file=sys.stderr)
        sys.exit(1)
    name = args[args.index('--pattern') + 1]
    shutil.copyfile(os.path.join(tags, args[2], name), os.path.join(args[args.index('--dir') + 1], name))
    sys.exit(0)
if args[:2] == ['api', '--include']:
    if fail == 'lookup':
        print('HTTP/2.0 502 Bad Gateway\nContent-Type: application/json\r\n\r\n{"message":"Bad Gateway"}')
        sys.exit(1)
    if not os.path.exists(rel):
        print('HTTP/2.0 404 Not Found\nContent-Type: application/json; charset=utf-8\r\n\r\n{"message":"Not Found","status":"404"}')
        sys.exit(1)
    print('HTTP/2.0 200 OK\nContent-Type: application/json; charset=utf-8\r\nServer: github.com\r\n\r\n' + json.dumps(release()))
    sys.exit(0)
if args[:2] == ['release', 'create']:
    os.makedirs(assets, exist_ok=True)
    with open(args[args.index('--notes-file') + 1]) as f:
        body = f.read()
    with open(rel, 'w') as f:
        json.dump({'tag_name': args[2], 'prerelease': '--prerelease' in args, 'latest': '--latest=false' not in args, 'body': body}, f)
    sys.exit(0)
if args[:2] == ['release', 'edit']:
    if fail == 'edit':
        print('edit refused', file=sys.stderr)
        sys.exit(1)
    r = json.load(open(rel))
    with open(args[args.index('--notes-file') + 1]) as f:
        if fail != 'edit-lost':  # answers 0 and keeps the old body
            r['body'] = f.read()
    with open(rel, 'w') as f:
        json.dump(r, f)
    sys.exit(0)
if args[:2] == ['release', 'download']:
    name = args[args.index('--pattern') + 1]
    shutil.copyfile(os.path.join(assets, name), os.path.join(args[args.index('--dir') + 1], name))
    sys.exit(0)
if args[:2] == ['release', 'upload']:
    if fail == 'upload':
        print('upload refused', file=sys.stderr)
        sys.exit(1)
    if fail == 'manifest' and os.path.basename(args[3]) == 'SOURCES.json':
        # gh --clobber deletes the asset first, then the upload fails.
        if os.path.exists(os.path.join(assets, 'SOURCES.json')):
            os.remove(os.path.join(assets, 'SOURCES.json'))
        print('upload refused', file=sys.stderr)
        sys.exit(1)
    name = os.path.basename(args[3])
    if fail == 'manifest-lost' and name == 'SOURCES.json':
        sys.exit(0)  # answers 0 and stores nothing
    if fail == 'rename':
        name = name.replace('_', '.')  # as a host that rewrites asset names would
    shutil.copyfile(args[3], os.path.join(assets, name))
    sys.exit(0)
if args[:2] == ['release', 'delete-asset']:
    os.remove(os.path.join(assets, args[3]))
    sys.exit(0)
print('unexpected gh call: ' + ' '.join(args), file=sys.stderr)
sys.exit(2)
'''

STUB_FETCH = r'''#!/usr/bin/env bash
url="$1"; out="$2"; dir="$STUB_LAUNCHPAD"
echo "$url" >> "$dir/../fetched"
if [ -f "$dir/down" ] && [ "$(cat "$dir/down")" -gt 0 ]; then echo $(( $(cat "$dir/down") - 1 )) > "$dir/down"; echo 502; exit 0; fi
rest="${url#*+sourcefiles/}"
if [ -f "$dir/$rest" ]; then cp "$dir/$rest" "$out"; echo 200; else echo 404; fi
'''

STUB_APPIMAGE = r'''#!/usr/bin/env bash
[ "$1" = --appimage-extract ] || exit 2
mkdir -p "squashfs-root/$(dirname "$2")"
cp "$STUB_LIST" "squashfs-root/$2"
'''

# The AppImage a release already distributes: it answers with its own list.
STUB_PUBLISHED_APPIMAGE = STUB_APPIMAGE.replace('$STUB_LIST', '$STUB_PUBLISHED_LIST')


def sha(data):
    return hashlib.sha256(data).hexdigest()


class ArchiveAppImageSources(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.state = self.dir / 'state'
        self.state.mkdir()
        self.launchpad = self.dir / 'launchpad'
        self.launchpad.mkdir()
        self.bin = self.dir / 'bin'
        self.bin.mkdir()
        for name, body in (('gh', STUB_GH), ('fetch', STUB_FETCH)):
            (self.bin / name).write_text(body)
            (self.bin / name).chmod(0o755)
        self.release_dir = self.dir / 'release-assets' / 'appimage'
        self.release_dir.mkdir(parents=True)
        self.appimage = self.release_dir / 'AeroFTP_9.8.7_amd64.AppImage'
        self.appimage.write_text(STUB_APPIMAGE)  # left 0644: the script runs a copy
        self.list = self.dir / 'BUNDLED-LIBRARIES.txt'

    def tearDown(self):
        self.tmp.cleanup()

    def publish_source(self, source, version, files, dsc_source=None, dsc_version=None, signed=False):
        """A source version on the stub Launchpad: its .dsc lists files {name: bytes}."""
        folder = self.launchpad / source / version
        folder.mkdir(parents=True, exist_ok=True)
        for name, data in files.items():
            (folder / name).write_bytes(data)
        lines = [
            f'Format: 3.0 (quilt)',
            f'Source: {dsc_source or source}',
            f'Version: {dsc_version or version}',
            'Checksums-Sha256:',
            *[f' {sha(data)} {len(data)} {name}' for name, data in files.items()],
        ]
        body = '\n'.join(lines) + '\n'
        if signed:
            body = (
                '-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA512\n\n' + body
                + '\n-----BEGIN PGP SIGNATURE-----\nVersion: GnuPG v1\n\niQIcBAEBCgAGBQJ\n-----END PGP SIGNATURE-----\n'
            )
        (folder / f'{source}_{version.split(":", 1)[-1]}.dsc').write_text(body)

    def bundle(self, *lines):
        self.list.write_text(HEADER + ''.join(
            f'usr/lib/{lib} {pkg} 1.0 {src} {ver} https://launchpad.net/ubuntu/+source/{src}/{ver}\n'
            for lib, pkg, src, ver in lines
        ))

    def run_script(self, version='9.8.7', today='2026-11-20', **env):
        return subprocess.run(
            ['python3', str(SCRIPT), str(self.release_dir), version],
            env={
                **os.environ,
                'PATH': f'{self.bin}:{os.environ["PATH"]}',
                'GITHUB_REPOSITORY': REPO,
                'GITHUB_REF_NAME': f'v{version}',
                'STUB_PUBLISHED_LIST': str(self.dir / 'PUBLISHED-BUNDLED-LIBRARIES.txt'),
                'GITHUB_SHA': 'a' * 40,
                'STUB_STATE': str(self.state),
                'STUB_LAUNCHPAD': str(self.launchpad),
                'STUB_LIST': str(self.list),
                'AEROFTP_SRC_FETCH': str(self.bin / 'fetch'),
                'AEROFTP_SRC_BACKOFF': '0',
                'AEROFTP_TODAY': today,
                **env,
            },
            capture_output=True,
            text=True,
        )

    def assets(self):
        folder = self.state / 'assets'
        return sorted(p.name for p in folder.iterdir()) if folder.exists() else []

    def manifest(self):
        return json.loads((self.state / 'assets' / 'SOURCES.json').read_text())

    def record(self):
        body = json.loads((self.state / 'release.json').read_text())['body']
        start = body.index('<!-- appimage-sources-state') + len('<!-- appimage-sources-state')
        return json.loads(body[start:body.index('-->', start)])

    def calls(self):
        log = self.state / 'calls.log'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def refused(self, result, pattern):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertRegex(result.stderr, pattern)

    def two_sources(self):
        self.publish_source('gtk+3.0', '3.24.33-1ubuntu2', {
            'gtk+3.0_3.24.33.orig.tar.xz': b'gtk orig',
            'gtk+3.0_3.24.33-1ubuntu2.debian.tar.xz': b'gtk debian',
        })
        self.publish_source('libxi', '2:1.8-1build1', {'libxi_1.8.orig.tar.gz': b'xi orig'}, signed=True)
        self.bundle(
            ('fallback/libgtk-3.so.0', 'libgtk-3-0', 'gtk+3.0', '3.24.33-1ubuntu2'),
            ('fallback/libgdk-3.so.0', 'libgtk-3-0', 'gtk+3.0', '3.24.33-1ubuntu2'),
            ('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'),
        )

    def test_a_first_release_creates_the_archive_with_one_checked_tar_per_source_version(self):
        self.two_sources()
        self.ok(self.run_script())
        release = json.loads((self.state / 'release.json').read_text())
        self.assertEqual((release['tag_name'], release['prerelease'], release['latest']), ('appimage-sources', True, False))
        # Two libraries of one source are one asset; `+` and the epoch do not
        # reach the asset name, the record keeps the exact version.
        self.assertEqual(self.assets(), ['SOURCES.json', 'gtk_3.0_3.24.33-1ubuntu2.tar', 'libxi_1.8-1build1.tar'])
        record = self.record()
        self.assertEqual([(e['source'], e['version'], e['asset']) for e in record], [
            ('gtk+3.0', '3.24.33-1ubuntu2', 'gtk_3.0_3.24.33-1ubuntu2.tar'),
            ('libxi', '2:1.8-1build1', 'libxi_1.8-1build1.tar'),
        ])
        # The copy for people is the record.
        self.assertEqual(self.manifest()['sources'], record)
        for e in record:
            data = (self.state / 'assets' / e['asset']).read_bytes()
            self.assertEqual((e['sha256'], e['size']), (sha(data), len(data)))
            self.assertEqual((e['first_release'], e['last_release'], e['last_release_date']), ('9.8.7', '9.8.7', '2026-11-20'))
            with tarfile.open(fileobj=io.BytesIO(data)) as tar:
                members = {m.name: tar.extractfile(m).read() for m in tar.getmembers() if m.isfile()}
            # The .dsc and every file it lists, byte for byte as published.
            published = self.launchpad / e['source'] / e['version']
            folder = e['asset'][:-len('.tar')]
            self.assertEqual(members, {f'{folder}/{p.name}': p.read_bytes() for p in published.iterdir()})

    def test_the_tar_of_a_source_version_is_the_same_bytes_on_every_run(self):
        self.two_sources()
        self.ok(self.run_script())
        first = (self.state / 'assets' / 'gtk_3.0_3.24.33-1ubuntu2.tar').read_bytes()
        (self.state / 'assets' / 'gtk_3.0_3.24.33-1ubuntu2.tar').unlink()
        self.ok(self.run_script(version='9.8.8'))
        self.assertEqual((self.state / 'assets' / 'gtk_3.0_3.24.33-1ubuntu2.tar').read_bytes(), first)

    def test_a_later_release_uploads_only_new_versions_and_extends_the_ones_it_bundles_again(self):
        self.two_sources()
        self.ok(self.run_script())
        self.publish_source('gtk+3.0', '3.24.33-1ubuntu3', {'gtk+3.0_3.24.33.orig.tar.xz': b'gtk orig', 'gtk+3.0_3.24.33-1ubuntu3.debian.tar.xz': b'gtk debian 3'})
        self.bundle(
            ('fallback/libgtk-3.so.0', 'libgtk-3-0', 'gtk+3.0', '3.24.33-1ubuntu3'),
            ('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'),
        )
        before = len(self.calls())
        self.ok(self.run_script(version='9.8.8', today='2026-12-20'))
        uploads = [c[3].rsplit('/', 1)[-1] for c in self.calls()[before:] if c[:2] == ['release', 'upload']]
        self.assertEqual(uploads, ['gtk_3.0_3.24.33-1ubuntu3.tar', 'SOURCES.json'])
        entries = {(e['source'], e['version']): e for e in self.record()}
        self.assertEqual(len(entries), 3)
        old = entries[('gtk+3.0', '3.24.33-1ubuntu2')]
        self.assertEqual((old['last_release'], old['last_release_date']), ('9.8.7', '2026-11-20'))
        # Bundled again: its three years now count from the later release.
        xi = entries[('libxi', '2:1.8-1build1')]
        self.assertEqual((xi['first_release'], xi['last_release'], xi['last_release_date']), ('9.8.7', '9.8.8', '2026-12-20'))
        self.assertIn('gtk_3.0_3.24.33-1ubuntu2.tar', self.assets())

    def test_a_version_goes_three_years_and_30_days_after_the_last_release_that_bundles_it(self):
        self.two_sources()
        self.ok(self.run_script())
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        # Three years after 2026-11-20 is 2029-11-20, and 30 days more: still
        # kept that day, deleted the day after.
        self.ok(self.run_script(version='9.9.0', today='2029-12-20'))
        self.assertIn('gtk_3.0_3.24.33-1ubuntu2.tar', self.assets())
        self.ok(self.run_script(version='9.9.1', today='2029-12-21'))
        self.assertNotIn('gtk_3.0_3.24.33-1ubuntu2.tar', self.assets())
        self.assertEqual([e['source'] for e in self.record()], ['libxi'])
        # A version still bundled is never deleted, whatever its first release.
        self.assertIn('libxi_1.8-1build1.tar', self.assets())

    def test_a_file_that_does_not_match_its_dsc_stops_the_job_and_records_nothing(self):
        self.two_sources()
        (self.launchpad / 'gtk+3.0' / '3.24.33-1ubuntu2' / 'gtk+3.0_3.24.33.orig.tar.xz').write_bytes(b'tampered')
        self.refused(self.run_script(), r'gtk\+3\.0_3\.24\.33\.orig\.tar\.xz of gtk\+3\.0 3\.24\.33-1ubuntu2 is 8 bytes')
        self.assertEqual(self.record(), [])
        self.assertNotIn('SOURCES.json', self.assets())

    def test_a_dsc_of_another_source_or_version_stops_the_job(self):
        self.publish_source('libxi', '2:1.8-1build1', {'libxi_1.8.orig.tar.gz': b'xi'}, dsc_version='2:1.8-1')
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        self.refused(self.run_script(), r'the \.dsc of libxi 2:1\.8-1build1 is source libxi 2:1\.8-1')

    def test_a_version_launchpad_does_not_have_stops_the_job(self):
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        self.refused(self.run_script(), r'Launchpad has no .*/libxi/2:1\.8-1build1/libxi_1\.8-1build1\.dsc \(HTTP 404\)')

    def test_launchpad_is_tried_three_times_then_the_job_stops(self):
        self.two_sources()
        (self.launchpad / 'down').write_text('2')
        self.ok(self.run_script())
        (self.state / 'assets' / 'libxi_1.8-1build1.tar').unlink()
        (self.launchpad / 'down').write_text('3')
        self.refused(self.run_script(version='9.8.8'), r'Launchpad did not answer for .* after three tries')

    def test_a_release_it_cannot_read_or_write_stops_the_job(self):
        self.two_sources()
        self.refused(self.run_script(STUB_FAIL='lookup'), r'cannot look up the release appimage-sources: HTTP/2\.0 502')
        self.refused(self.run_script(STUB_FAIL='upload'), r'cannot upload gtk_3\.0_3\.24\.33-1ubuntu2\.tar')
        self.refused(self.run_script(STUB_FAIL='edit'), r'cannot write the record to the release appimage-sources')

    def test_the_release_is_read_back_after_the_upload(self):
        # An asset the release does not hold under the name the record gives
        # would make the offer name a file nobody can download.
        self.two_sources()
        self.refused(
            self.run_script(STUB_FAIL='rename'),
            r'the release appimage-sources does not hold gtk_3\.0_3\.24\.33-1ubuntu2\.tar at \d+ bytes after the upload \(it has no such asset\)',
        )

    def test_a_record_the_body_did_not_keep_stops_the_job(self):
        self.two_sources()
        self.refused(self.run_script(STUB_FAIL='edit-lost'), r'the body of the release appimage-sources does not hold the record just written')

    def test_a_copy_for_people_the_release_did_not_keep_stops_the_job(self):
        self.two_sources()
        self.refused(self.run_script(STUB_FAIL='manifest-lost'), r'the release appimage-sources has no SOURCES\.json after the upload')

    def test_a_first_run_that_stops_halfway_is_completed_by_the_next(self):
        self.two_sources()
        libxi = self.launchpad / 'libxi'
        libxi.rename(self.dir / 'libxi-later')
        self.refused(self.run_script(), r'Launchpad has no .*libxi_1\.8-1build1\.dsc')
        # gtk was uploaded before the stop, and the record does not name it yet.
        self.assertEqual(self.assets(), ['gtk_3.0_3.24.33-1ubuntu2.tar'])
        self.assertEqual(self.record(), [])
        (self.dir / 'libxi-later').rename(libxi)
        self.ok(self.run_script())
        self.assertEqual([e['source'] for e in self.record()], ['gtk+3.0', 'libxi'])
        self.assertEqual(self.assets(), ['SOURCES.json', 'gtk_3.0_3.24.33-1ubuntu2.tar', 'libxi_1.8-1build1.tar'])

    def test_the_record_survives_a_lost_or_failed_copy_for_people(self):
        self.two_sources()
        self.ok(self.run_script())
        written = self.record()
        # gh --clobber deletes SOURCES.json before uploading the new one; the
        # upload fails: the job stops, and the record in the body is whole.
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        self.refused(self.run_script(version='9.8.8', today='2026-12-01', STUB_FAIL='manifest'), r'cannot upload SOURCES\.json')
        self.assertNotIn('SOURCES.json', self.assets())
        after = {e['source']: e for e in self.record()}
        self.assertEqual(after['gtk+3.0'], written[0])
        self.ok(self.run_script(version='9.8.8', today='2026-12-01'))
        self.assertEqual(self.manifest()['sources'], self.record())
        self.assertEqual({e['source']: e['last_release_date'] for e in self.record()}, {'gtk+3.0': '2026-11-20', 'libxi': '2026-12-01'})

    def test_a_release_with_source_assets_but_no_record_is_not_guessed(self):
        self.two_sources()
        self.ok(self.run_script())
        release = json.loads((self.state / 'release.json').read_text())
        release['body'] = 'edited by hand'
        (self.state / 'release.json').write_text(json.dumps(release))
        self.refused(self.run_script(version='9.8.8'), r'has source assets but its body has no record')

    def test_a_kept_version_the_release_lost_is_archived_again_with_its_dates(self):
        self.two_sources()
        self.ok(self.run_script())
        # This AppImage no longer bundles gtk, whose three years still run.
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        for name in ('gtk_3.0_3.24.33-1ubuntu2.tar', 'libxi_1.8-1build1.tar'):
            (self.state / 'assets' / name).unlink()
        result = self.run_script(version='9.8.8', today='2027-01-10')
        self.ok(result)
        self.assertIn('does not hold gtk_3.0_3.24.33-1ubuntu2.tar', result.stdout)
        self.assertIn('gtk_3.0_3.24.33-1ubuntu2.tar', self.assets())
        self.assertIn('libxi_1.8-1build1.tar', self.assets())
        gtk = {e['source']: e for e in self.record()}['gtk+3.0']
        self.assertEqual((gtk['first_release'], gtk['last_release'], gtk['last_release_date']), ('9.8.7', '9.8.7', '2026-11-20'))

    def test_a_kept_version_it_cannot_archive_again_stops_the_job(self):
        self.two_sources()
        self.ok(self.run_script())
        self.bundle(('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        (self.state / 'assets' / 'gtk_3.0_3.24.33-1ubuntu2.tar').unlink()
        (self.launchpad / 'gtk+3.0' / '3.24.33-1ubuntu2' / 'gtk+3.0_3.24.33-1ubuntu2.dsc').unlink()
        self.refused(self.run_script(version='9.8.8'), r'Launchpad has no .*gtk\+3\.0_3\.24\.33-1ubuntu2\.dsc')
        self.assertIn('gtk+3.0', [e['source'] for e in self.record()])

    def test_a_tar_the_record_does_not_name_is_deleted_and_other_assets_stay(self):
        self.two_sources()
        self.ok(self.run_script())
        (self.state / 'assets' / 'leftover_1.0.tar').write_bytes(b'from a run that stopped')
        (self.state / 'assets' / 'README.txt').write_bytes(b'not a source')
        self.ok(self.run_script(version='9.8.8'))
        self.assertNotIn('leftover_1.0.tar', self.assets())
        self.assertIn('README.txt', self.assets())

    def test_a_list_it_cannot_read_stops_the_job(self):
        self.list.write_text(HEADER + 'usr/lib/libfoo.so.1 libfoo1 1.0 foo\n')
        self.refused(self.run_script(), r'cannot read this line of usr/share/doc/BUNDLED-LIBRARIES\.txt')
        self.list.write_text('no header\n')
        self.refused(self.run_script(), r'does not have its header and its list')

    def test_no_appimage_in_the_release_is_nothing_to_keep(self):
        self.appimage.unlink()
        result = self.run_script()
        self.ok(result)
        self.assertIn('nothing to keep', result.stdout)
        self.assertEqual(self.calls(), [])

    def test_the_publish_job_keeps_the_sources_before_it_publishes_one_tag_at_a_time(self):
        workflow = (ROOT / '.github/workflows/build.yml').read_text()
        job = workflow[workflow.index('\n  publish-github-release:'):workflow.index('\n  build-snap:')]
        keep = job.index('python3 scripts/archive-appimage-sources.py release-assets/appimage "${GITHUB_REF_NAME#v}"')
        self.assertLess(job.index('Pair AppImage .zsync with the published AppImage'), keep)
        self.assertLess(keep, job.index('Upload binaries to GitHub Release'))
        step = job[job.rindex('- name:', 0, keep):keep]
        self.assertIn('GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}', step)
        self.assertNotIn('continue-on-error', step)
        # Every tag's publish job shares one group, never cancelled: two runs
        # never write the record at once.
        # queue: max keeps every waiting tag: with the default single queue a
        # third tag would cancel the second one's pending publication.
        self.assertRegex(job, r'\n    concurrency:\n      group: appimage-sources-archive\n      cancel-in-progress: false\n      queue: max\n')

    def publish_appimage(self, tag, *lines):
        """The release `tag` already distributes an AppImage whose list is `lines`."""
        folder = self.state / 'tags' / tag
        folder.mkdir(parents=True)
        (folder / self.appimage.name).write_text(STUB_PUBLISHED_APPIMAGE)
        (self.dir / 'PUBLISHED-BUNDLED-LIBRARIES.txt').write_text(HEADER + ''.join(
            f'usr/lib/{lib} {pkg} 1.0 {src} {ver} https://launchpad.net/ubuntu/+source/{src}/{ver}\n'
            for lib, pkg, src, ver in lines
        ))

    def test_a_rerun_keeps_the_sources_of_the_published_appimage_not_of_the_rebuild(self):
        self.two_sources()
        self.publish_source('gtk+3.0', '3.24.33-1ubuntu3', {'gtk+3.0_3.24.33-1ubuntu3.debian.tar.xz': b'gtk debian 3'})
        # The release already has the AppImage built with ubuntu2; the rerun's
        # build bundles ubuntu3, which this release never distributes.
        self.publish_appimage('v9.8.7', ('fallback/libgtk-3.so.0', 'libgtk-3-0', 'gtk+3.0', '3.24.33-1ubuntu2'))
        self.bundle(('fallback/libgtk-3.so.0', 'libgtk-3-0', 'gtk+3.0', '3.24.33-1ubuntu3'))
        result = self.run_script()
        self.ok(result)
        self.assertIn('keeping the sources of the published one', result.stdout)
        self.assertEqual([(e['source'], e['version']) for e in self.record()], [('gtk+3.0', '3.24.33-1ubuntu2')])
        self.assertEqual(self.assets(), ['SOURCES.json', 'gtk_3.0_3.24.33-1ubuntu2.tar'])
        downloads = [c for c in self.calls() if c[:3] == ['release', 'download', 'v9.8.7']]
        self.assertEqual(len(downloads), 1)
        self.assertEqual(downloads[0][downloads[0].index('--pattern') + 1], self.appimage.name)

    def test_a_release_without_its_appimage_yet_keeps_the_sources_of_this_build(self):
        self.two_sources()
        # The release exists (another platform was uploaded) but not the AppImage.
        (self.state / 'tags' / 'v9.8.7').mkdir(parents=True)
        (self.state / 'tags' / 'v9.8.7' / 'AeroFTP_9.8.7_x64-setup.exe').write_text('exe')
        self.ok(self.run_script())
        self.assertEqual([e['source'] for e in self.record()], ['gtk+3.0', 'libxi'])
        self.assertFalse([c for c in self.calls() if c[:3] == ['release', 'download', 'v9.8.7']])

    def test_a_published_appimage_it_cannot_look_up_or_download_stops_the_job(self):
        self.two_sources()
        self.refused(self.run_script(STUB_FAIL='tag-lookup'), r'cannot look up the release v9\.8\.7: HTTP/2\.0 502')
        self.assertFalse((self.state / 'release.json').exists())
        self.publish_appimage('v9.8.7', ('fallback/libXi.so.6', 'libxi6', 'libxi', '2:1.8-1build1'))
        self.refused(self.run_script(STUB_FAIL='tag-download'), r'cannot download the published AeroFTP_9\.8\.7_amd64\.AppImage of v9\.8\.7')
        self.assertFalse((self.state / 'release.json').exists())


if __name__ == '__main__':
    unittest.main()
