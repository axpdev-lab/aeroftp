"""Exercise scripts/pair-appimage-zsync.sh with a stub gh.

The publish job pairs the AppImage it uploads with a .zsync from the same
bytes. It reads the published asset list from `gh api --include`, which
prints the status line, the headers, a blank line and the JSON body (gh 2.46
measured: the status line ends in LF, the headers and the blank line in CRLF).
A stub gh on PATH prints that shape, a stub sudo installs nothing, and a stub
zsyncmake writes a marker, so nothing is downloaded or published.

The check used to be inline in the workflow step and split the output with
`sed '1,/^[[:space:]]*$/d' | jq -r '.assets[].name'`: without the blank line
sed deleted the body, jq exited 0 on the empty input, and the step read an
empty asset list, which is what a fresh release looks like. Both checks were
then skipped. Those cases are below.
"""

from pathlib import Path
import json
import os
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/pair-appimage-zsync.sh'

TAG = 'v4.2.3'
APPIMAGE = 'AeroFTP_4.2.3_amd64.AppImage'

HEADERS = (
    'HTTP/2.0 {status}\n'
    'Content-Type: application/json; charset=utf-8\r\n'
    'Date: Sat, 17 Oct 2026 07:43:59 GMT\r\n'
    'Server: github.com\r\n'
    'X-Github-Api-Version-Selected: 2022-11-28\r\n'
    'X-Github-Media-Type: github.v3; format=json\r\n'
    'X-Ratelimit-Resource: core\r\n'
)


def release(*assets, tag=TAG):
    return json.dumps({'tag_name': tag, 'assets': [{'name': a} for a in assets]})


def include(body, status='200 OK', separator='\r\n'):
    return HEADERS.format(status=status) + separator + body


NOT_FOUND = include(
    '{"message":"Not Found","documentation_url":"https://docs.github.com/rest/releases/releases'
    '#get-a-release-by-tag-name","status":"404"}',
    status='404 Not Found',
)


def stub(path, body):
    path.write_text('#!/usr/bin/env bash\n' + body)
    path.chmod(0o755)


class PairAppImageZsyncTests(unittest.TestCase):
    def run_pair(self, output, *, api_rc=0, apt_update_rc=0):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bin_dir = tmp / 'bin'
            bin_dir.mkdir()
            (tmp / 'api.txt').write_text(output)
            trace = tmp / 'trace.txt'
            stub(bin_dir / 'gh', (
                f'printf "%s\\n" "$*" >> "{trace}"\n'
                'if [ "$1 $2" = "api --include" ]; then\n'
                f'  cat "{tmp / "api.txt"}"; echo "gh: stub" >&2; exit {api_rc}\n'
                'fi\n'
                'if [ "$1 $2" = "release download" ]; then\n'
                '  while [ "$#" -gt 0 ]; do\n'
                '    case "$1" in --pattern) p="$2" ;; --dir) d="$2" ;; esac; shift\n'
                '  done\n'
                '  printf "published bytes" > "$d/$p"; exit 0\n'
                'fi\n'
                'exit 99\n'
            ))
            stub(bin_dir / 'sudo', (
                f'printf "sudo %s\\n" "$*" >> "{trace}"\n'
                f'[ "$*" = "apt-get update" ] && exit {apt_update_rc}\n'
                'exit 0\n'
            ))
            stub(bin_dir / 'zsyncmake', 'printf "zsync of the published bytes" > "$1.zsync"\n')
            assets = tmp / 'release-assets/appimage'
            assets.mkdir(parents=True)
            (assets / APPIMAGE).write_text('this build')
            (assets / f'{APPIMAGE}.zsync').write_text('zsync of this build')
            env = dict(os.environ,
                       PATH=f'{bin_dir}:{os.environ["PATH"]}',
                       GITHUB_REPOSITORY='axpdev-lab/aeroftp', GITHUB_REF_NAME=TAG)
            result = subprocess.run(
                ['bash', str(SCRIPT), str(assets)],
                capture_output=True, text=True, env=env, cwd=tmp, check=False,
            )
            zsync = (assets / f'{APPIMAGE}.zsync').read_text()
            calls = trace.read_text() if trace.exists() else ''
        return result, zsync, calls

    def test_fresh_release_keeps_this_build(self):
        result, zsync, _ = self.run_pair(NOT_FOUND, api_rc=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('does not exist yet', result.stdout)
        self.assertEqual(zsync, 'zsync of this build')

    def test_complete_release_keeps_this_build(self):
        result, zsync, calls = self.run_pair(include(release(APPIMAGE, f'{APPIMAGE}.zsync')))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(zsync, 'zsync of this build')
        self.assertNotIn('release download', calls)

    def test_published_appimage_without_zsync_is_repaired_from_its_bytes(self):
        result, zsync, calls = self.run_pair(include(release(APPIMAGE, 'AeroFTP_4.2.3_amd64.deb')))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Repair case', result.stdout)
        self.assertEqual(zsync, 'zsync of the published bytes')
        self.assertIn(f'release download {TAG} --repo axpdev-lab/aeroftp --pattern {APPIMAGE}', calls)

    # `apt-get update && apt-get install` is an && list, and set -e ignores a
    # failure in it unless it is the last command: a failed update skipped
    # the install and the repair went on to download the AppImage anyway.
    def test_a_failed_package_index_update_stops_the_repair(self):
        result, zsync, calls = self.run_pair(include(release(APPIMAGE)), apt_update_rc=100)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn('sudo apt-get install', calls)
        self.assertNotIn('release download', calls)
        self.assertEqual(zsync, 'zsync of this build')

    def test_published_zsync_without_appimage_refuses(self):
        result, _, _ = self.run_pair(include(release(f'{APPIMAGE}.zsync')))
        self.assertEqual(result.returncode, 1)
        self.assertIn('without its AppImage', result.stderr)

    def test_blank_line_without_cr_still_separates_the_body(self):
        result, zsync, _ = self.run_pair(include(release(APPIMAGE), separator='\n'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(zsync, 'zsync of the published bytes')

    def test_failed_lookup_refuses(self):
        result, _, _ = self.run_pair(include('{"message":"Bad credentials"}', status='401 Unauthorized'), api_rc=1)
        self.assertEqual(result.returncode, 1)
        self.assertIn('Release lookup failed', result.stderr)

    # Each of these hides a published AppImage without its .zsync: read as an
    # empty asset list, the repair was skipped and this build's .zsync shipped
    # next to bytes it does not describe.
    def test_unreadable_release_refuses(self):
        body = release(APPIMAGE)
        cases = {
            'no blank line after the headers': include(body, separator=''),
            'body before the headers': body + '\n' + HEADERS.format(status='200 OK') + '\r\n',
            'headers only': HEADERS.format(status='200 OK') + '\r\n',
            'another release': include(release(APPIMAGE, tag='v4.2.2')),
            'assets not a list': include(json.dumps({'tag_name': TAG, 'assets': {APPIMAGE: {}}})),
            'asset without a name': include(json.dumps({'tag_name': TAG, 'assets': [{'label': APPIMAGE}]})),
            'two releases': include(body + '\n' + body),
            'not JSON': include(f'tag_name: {TAG}\nassets: {APPIMAGE}\n'),
            'empty output': '',
        }
        for name, output in cases.items():
            with self.subTest(name):
                result, zsync, _ = self.run_pair(output)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn('::error::cannot read release', result.stderr)
                self.assertEqual(zsync, 'zsync of this build')


if __name__ == '__main__':
    unittest.main()
