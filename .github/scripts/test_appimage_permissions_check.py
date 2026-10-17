"""Exercise scripts/appimage-permissions-check.sh with stub tools.

The check lists the repackaged AppImage with `unsquashfs -ll` and fails on an
entry that only its owner can run or a folder other than 755 (Fix 4 of the
"Fix AppImage" step in build.yml). A stub `unsquashfs` on PATH prints a
listing, and a stub AppImage answers `--appimage-offset`, so no image is
built. The listings follow the real long format, measured on unsquashfs 4.5,
4.6.1 and 4.7.5: permissions, owner/group, size, date, time, path, no header.

The check used to be an awk one-liner in the workflow step, which passed every
listing it could not read: a moved permission column, an unsquashfs that
failed (the step has no pipefail), an empty listing. Those cases are below.
"""

from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/appimage-permissions-check.sh'

# What Fix 4 leaves behind: folders and executables 755, data 644.
FIXED = """drwxr-xr-x root/root               171 2026-10-17 01:20 squashfs-root
lrwxrwxrwx root/root                11 2026-10-17 01:20 squashfs-root/.DirIcon -> aeroftp.png
-rwxr-xr-x root/root              3120 2026-10-17 01:20 squashfs-root/AppRun
-rwxr-xr-x root/root          25123456 2026-10-17 01:20 squashfs-root/AppRun.wrapped
-rw-r--r-- root/root             28648 2026-10-17 01:20 squashfs-root/aeroftp.png
drwxr-xr-x root/root                40 2026-10-17 01:20 squashfs-root/usr
drwxr-xr-x root/root                40 2026-10-17 01:20 squashfs-root/usr/share
drwxr-xr-x root/root                40 2026-10-17 01:20 squashfs-root/usr/share/glib-2.0/schemas
-rw-r--r-- root/root              3070 2026-10-17 01:20 squashfs-root/usr/share/glib-2.0/schemas/gschemas.compiled
-rw-r--r-- root/root               512 2026-10-17 01:20 squashfs-root/usr/share/doc/aero ftp/copyright
"""

# What linuxdeploy leaves behind before Fix 4 (seen on the 2.7.1 AppImage).
WRAPPED_0750 = FIXED.replace(
    '-rwxr-xr-x root/root          25123456', '-rwxr-x--- root/root          25123456')
SCHEMAS_0777 = FIXED.replace(
    'drwxr-xr-x root/root                40 2026-10-17 01:20 squashfs-root/usr/share/glib-2.0/schemas\n',
    'drwxrwxrwx root/root                40 2026-10-17 01:20 squashfs-root/usr/share/glib-2.0/schemas\n')


def stub(path, body):
    path.write_text('#!/usr/bin/env bash\n' + body)
    path.chmod(0o755)


def swap_first_columns(listing):
    return ''.join(
        ' '.join([line.split()[1], line.split()[0]] + line.split()[2:]) + '\n'
        for line in listing.splitlines()
    )


class AppImagePermissionsTests(unittest.TestCase):
    def run_check(self, listing, *, list_rc=0, offset='188392', offset_rc=0):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bin_dir = tmp / 'bin'
            bin_dir.mkdir()
            for tool in ('bash', 'cat', 'mktemp', 'rm', 'sed'):
                (bin_dir / tool).symlink_to(shutil.which(tool))
            (tmp / 'listing.txt').write_text(listing)
            trace = tmp / 'trace.txt'
            stub(bin_dir / 'unsquashfs', (
                f'printf "%s\\n" "$*" >> "{trace}"\n'
                f'cat "{tmp / "listing.txt"}"\nexit {list_rc}\n'
            ))
            image = tmp / 'Aero FTP_4.2.3_amd64.AppImage'
            stub(image, f'[ "$1" = --appimage-offset ] || exit 99\necho "{offset}"\nexit {offset_rc}\n')
            env = {'PATH': str(bin_dir), 'HOME': str(tmp)}
            result = subprocess.run(
                ['bash', str(SCRIPT), str(image)],
                capture_output=True, text=True, env=env, check=False,
            )
            calls = trace.read_text() if trace.exists() else ''
            return result.returncode, result.stdout + result.stderr, calls

    def test_a_fixed_image_passes(self):
        rc, out, calls = self.run_check(FIXED)
        self.assertEqual(rc, 0, out)
        self.assertIn('OK: 10 AppImage entries', out)
        self.assertRegex(calls, r'^-ll -o 188392 /\S.*/Aero FTP_4\.2\.3_amd64\.AppImage\n$')

    def test_an_executable_only_its_owner_can_run_fails(self):
        rc, out, _ = self.run_check(WRAPPED_0750)
        self.assertEqual(rc, 1, out)
        self.assertIn('::error::AppImage entries not runnable or readable by every user:', out)
        self.assertIn('-rwxr-x--- squashfs-root/AppRun.wrapped', out)

    # AppRun is the entry point: a regular AppRun nobody can run is an
    # AppImage that does not start, whatever the other files look like.
    def test_an_apprun_that_is_not_runnable_fails(self):
        for mode in ('-rw-r--r--', '-rw-r--r-x'):
            with self.subTest(mode):
                listing = FIXED.replace('-rwxr-xr-x root/root              3120', f'{mode} root/root              3120')
                rc, out, _ = self.run_check(listing)
                self.assertEqual(rc, 1, out)
                self.assertIn(f'{mode} squashfs-root/AppRun\n', out)

    def test_a_folder_other_than_755_fails(self):
        rc, out, _ = self.run_check(SCHEMAS_0777)
        self.assertEqual(rc, 1, out)
        self.assertIn('drwxrwxrwx squashfs-root/usr/share/glib-2.0/schemas', out)

    def test_an_unterminated_last_line_is_still_read(self):
        listing = FIXED + '-rwxr-x--- root/root 10 2026-10-17 01:20 squashfs-root/usr/bin/aeroftp-cli'
        rc, out, _ = self.run_check(listing)
        self.assertEqual(rc, 1, out)
        self.assertIn('-rwxr-x--- squashfs-root/usr/bin/aeroftp-cli', out)

    def test_a_device_node_is_read(self):
        listing = FIXED + 'crw-rw-rw- root/root           1,  3 2026-10-17 01:20 squashfs-root/dev-null\n'
        rc, out, _ = self.run_check(listing)
        self.assertEqual(rc, 0, out)

    def test_a_listing_the_check_cannot_read_is_never_a_pass(self):
        for name, listing in {
            # Each one hides the 0750 AppRun.wrapped from a column reader.
            'permission column moved': swap_first_columns(WRAPPED_0750),
            'a header line': 'Parallel unsquashfs: Using 4 processors\n' + WRAPPED_0750,
            'relative paths': WRAPPED_0750.replace(' squashfs-root/', ' ./').replace(' squashfs-root\n', ' .\n'),
            'a date in another format': WRAPPED_0750.replace('2026-10-17 01:20', 'Oct 17 01:20 2026'),
            'no root': FIXED.split('\n', 1)[1],
            'two roots': FIXED + FIXED.split('\n', 1)[0] + '\n',
            'no AppRun': FIXED.replace('squashfs-root/AppRun\n', 'squashfs-root/AppRun.sh\n'),
            'empty': '',
        }.items():
            with self.subTest(name):
                rc, out, _ = self.run_check(listing)
                self.assertEqual(rc, 2, out)
                self.assertIn('::error::cannot read the listing of', out)
                self.assertNotIn('OK:', out)

    def test_a_failed_listing_is_an_error(self):
        rc, out, _ = self.run_check(FIXED, list_rc=1)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::could not list', out)

    def test_an_offset_that_is_not_one_is_an_error(self):
        for name, kwargs in {
            'runtime fails': {'offset_rc': 1},
            'no number': {'offset': ''},
            'not a number': {'offset': 'unknown option'},
            'zero': {'offset': '0'},
        }.items():
            with self.subTest(name):
                rc, out, calls = self.run_check(FIXED, **kwargs)
                self.assertEqual(rc, 2, out)
                self.assertIn('--appimage-offset', out)
                self.assertEqual(calls, '')


if __name__ == '__main__':
    unittest.main()
