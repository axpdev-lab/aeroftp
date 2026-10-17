"""Exercise scripts/snap-abi-check.sh (gate G1) with stub tools.

A stub `unsquashfs` "extracts" a prepared tree and a stub `objdump` prints,
for each file, what a test gives it; `--ceiling 2.35` replaces the base snap.
The `objdump -T` and `-p` text below follows the real output of binutils
(DYNAMIC SYMBOL TABLE rows with the version in parentheses, NEEDED rows in
the Dynamic Section).

The gate used to choose its ELFs by the MIME string `file` prints and to
treat an ELF objdump could not read as one that needs no glibc. An ELF named
in a new MIME string, and an objdump that failed, both printed OK; the cases
are below.
"""

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/snap-abi-check.sh'

ELF = b'\x7fELF\x02\x01\x01' + b'\0' * 57


def dynsym(*versions):
    rows = ''.join(
        f'0000000000000000      DF *UND*\t0000000000000000 ({v}) sym_{i}\n'
        for i, v in enumerate(versions)
    )
    return f'\nFILE:     file format elf64-x86-64\n\nDYNAMIC SYMBOL TABLE:\n{rows}'


def needed(*libs):
    rows = ''.join(f'  NEEDED               {lib}\n' for lib in libs)
    return f'\nFILE:     file format elf64-x86-64\n\nDynamic Section:\n{rows}'


def stub(path, body):
    path.write_text('#!/usr/bin/env bash\n' + body)
    path.chmod(0o755)


# name -> (file content, objdump -T stdout, -T stderr, -T exit, objdump -p stdout)
CLEAN = {
    'usr/bin/aeroftp': (ELF, dynsym('GLIBC_2.34', 'GLIBC_2.2.5'), '', 0, needed('libc.so.6')),
    'usr/lib/x86_64-linux-gnu/libfoo.so.1': (ELF, dynsym('GLIBC_2.14'), '', 0, needed('libc.so.6')),
    # Like libcairo-gobject in the 4.2.3 snap: no glibc symbol, no libc link.
    'usr/lib/x86_64-linux-gnu/libbar.so.2': (ELF, dynsym(), '', 0, needed('libglib-2.0.so.0')),
    'usr/share/doc/aeroftp/copyright': (b'GPL-3.0\n', '', '', 0, ''),
    'usr/lib/x86_64-linux-gnu/notelf.so': (b'INPUT(-lfoo)\n', '', '', 0, ''),
}


class SnapAbiCheckTests(unittest.TestCase):
    def run_gate(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            bin_dir = tmp / 'bin'
            bin_dir.mkdir()
            for tool in ('bash', 'cat', 'mktemp', 'rm', 'find', 'grep', 'sort',
                         'tail', 'sed', 'head', 'awk', 'paste', 'cp', 'mkdir', 'yes'):
                (bin_dir / tool).symlink_to(shutil.which(tool))
            tree = tmp / 'tree'
            answers = tmp / 'answers'
            answers.mkdir()
            for i, (name, (content, t_out, t_err, t_rc, p_out, *p_extra)) in enumerate(files.items()):
                path = tree / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
                key = name.replace('/', '__')
                (answers / f'{key}.T').write_text(t_out)
                (answers / f'{key}.err').write_text(t_err)
                (answers / f'{key}.rc').write_text(str(t_rc))
                (answers / f'{key}.p').write_text(p_out)
                # Optional: the exit status of `objdump -p`, and whether more
                # output follows its NEEDED lines than a pipe buffer holds.
                p_rc, p_flood = (p_extra + [0, False])[:2]
                (answers / f'{key}.prc').write_text(str(p_rc))
                if p_flood:
                    (answers / f'{key}.flood').write_text('')
            # unsquashfs -q -n -f -no-xattrs -d DIR SNAP
            stub(bin_dir / 'unsquashfs', (
                'while [ "$1" != -d ]; do shift; done\n'
                f'mkdir -p "$2" && cp -a "{tree}/." "$2/"\n'
            ))
            # objdump -T|-p FILE: the answer for FILE, with FILE's own path in
            # the header and in an error, as the real tool prints them.
            stub(bin_dir / 'objdump', (
                'flag=$1 file=$2\n'
                'rel=${file#*/app/}\n'
                f'key="{answers}/${{rel//\\//__}}"\n'
                'case $flag in\n'
                '  -T) sed "s#FILE#$file#" "$key.T"; sed "s#FILE#$file#" "$key.err" >&2; exit "$(cat "$key.rc")" ;;\n'
                '  -p) sed "s#FILE#$file#" "$key.p"\n'
                '      if [ -e "$key.flood" ]; then yes "  filler" | head -n 200000 || exit 141; fi\n'
                '      exit "$(cat "$key.prc")" ;;\n'
                'esac\n'
            ))
            stub(bin_dir / 'strings', 'exit 0\n')
            snap = tmp / 'aeroftp_4.2.3_amd64.snap'
            snap.write_bytes(b'not read by the stub')
            result = subprocess.run(
                ['bash', str(SCRIPT), str(snap), '--ceiling', '2.35'],
                capture_output=True, text=True, env={'PATH': str(bin_dir), 'HOME': str(tmp)},
                check=False, cwd=tmp,
            )
            return result.returncode, result.stdout + result.stderr

    def test_a_snap_within_the_ceiling_passes(self):
        rc, out = self.run_gate(CLEAN)
        self.assertEqual(rc, 0, out)
        self.assertIn('Scanned:  3 ELF files (2 with a glibc floor, 0 static)', out)
        self.assertIn('OK: every ELF fits within GLIBC_2.35.', out)

    def test_an_elf_above_the_ceiling_fails(self):
        files = dict(CLEAN)
        files['usr/bin/aeroftp'] = (ELF, dynsym('GLIBC_2.39', 'GLIBC_2.34'), '', 0, needed('libc.so.6'))
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 1, out)
        self.assertIn('::error::usr/bin/aeroftp requires GLIBC_2.39 but base (override) provides at most GLIBC_2.35', out)

    def test_an_elf_is_found_by_its_bytes_whatever_its_name(self):
        # No `file` on PATH at all: nothing depends on how it names an ELF.
        files = dict(CLEAN)
        files['usr/share/aeroftp/helper.bin'] = (ELF, dynsym('GLIBC_2.39'), '', 0, needed('libc.so.6'))
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 1, out)
        self.assertIn('usr/share/aeroftp/helper.bin requires GLIBC_2.39', out)

    def test_a_static_elf_has_no_floor(self):
        files = dict(CLEAN)
        files['usr/bin/static-helper'] = (ELF, '', 'objdump: FILE: not a dynamic object\n', 1, '')
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 0, out)
        self.assertIn('Scanned:  4 ELF files (2 with a glibc floor, 1 static)', out)

    # Only an ELF that shows no GLIBC_x.y version reaches the `objdump -p`
    # probe, which is the ELF this probe exists to catch. A failed probe, or
    # a piped `grep -q` that stops reading at the NEEDED line and cuts
    # objdump off with SIGPIPE (141 under pipefail), used to read as "does
    # not link libc" and skip it.
    def test_a_libc_probe_that_fails_is_unverified(self):
        files = dict(CLEAN)
        files['usr/bin/aeroftp'] = (ELF, dynsym(), '', 0, '', 1)
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::objdump -p cannot read usr/bin/aeroftp, so whether it links libc.so.6 is unverified', out)

    def test_a_libc_probe_cut_off_after_the_needed_line_still_finds_libc(self):
        files = dict(CLEAN)
        files['usr/bin/aeroftp'] = (ELF, dynsym(), '', 0, needed('libc.so.6'), 0, True)
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::usr/bin/aeroftp links libc.so.6 but objdump shows no GLIBC_x.y version', out)

    def test_an_elf_objdump_cannot_read_is_unverified(self):
        for name, err in {
            'unreadable': 'objdump: FILE: file format not recognized\n',
            'static, in another wording': 'objdump: FILE: no dynamic symbols\n',
            'static, translated': 'objdump: FILE: non è un oggetto dinamico\n',
            'no message': '',
        }.items():
            with self.subTest(name):
                files = dict(CLEAN)
                files['usr/bin/aeroftp'] = (ELF, '', err, 1, needed('libc.so.6'))
                rc, out = self.run_gate(files)
                self.assertEqual(rc, 2, out)
                self.assertIn('::error::objdump cannot read usr/bin/aeroftp, so its glibc floor is unverified', out)
                self.assertNotIn('OK:', out)

    def test_an_elf_that_links_libc_but_shows_no_version_is_unverified(self):
        # What every ELF looks like if objdump changes how it prints versions.
        files = dict(CLEAN)
        files['usr/bin/aeroftp'] = (
            ELF, dynsym('GLIBC_2.39').replace('(GLIBC_2.39)', 'glibc 2.39'), '', 0, needed('libc.so.6'))
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::usr/bin/aeroftp links libc.so.6 but objdump shows no GLIBC_x.y version', out)

    def test_a_snap_without_any_glibc_version_is_unverified(self):
        files = {name: (c, dynsym() if c == ELF else t, e, r, needed('libglib-2.0.so.0') if c == ELF else p)
                 for name, (c, t, e, r, p) in CLEAN.items()}
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::no ELF inside', out)
        self.assertNotIn('OK:', out)

    def test_a_snap_without_elves_is_an_error(self):
        files = {k: v for k, v in CLEAN.items() if v[0] != ELF}
        rc, out = self.run_gate(files)
        self.assertEqual(rc, 2, out)
        self.assertIn('::error::no ELF files found', out)


if __name__ == '__main__':
    unittest.main()
