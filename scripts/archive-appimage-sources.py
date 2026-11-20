#!/usr/bin/env python3
"""Keep the source of every library the AppImage bundles, for the written offer.

The AppImage bundles system libraries from Ubuntu packages (WebKitGTK, GTK,
GStreamer, ...). usr/share/doc/BUNDLED-LIBRARIES.txt in it lists the Ubuntu
source package and version of each one, with a written offer: for at least
three years we give the corresponding source to anyone who asks. Launchpad and
the Ubuntu snapshot archive are not ours to keep serving an old version for
that long, so the publish job keeps its own copy, in the GitHub release
`appimage-sources` of this repository:

- one asset per source version, `<source>_<version>.tar`, holding the `.dsc`
  and every file it lists, as the Ubuntu archive publishes them (each file
  checked against the SHA-256 and size in the `.dsc`);
- the record of what is kept, in an HTML comment of the release body: for each
  source version its asset, the SHA-256 and size of the asset, the first and
  the last AeroFTP release whose AppImage bundles it, and the date of that last
  release. The body is replaced in one request, so a run that stops halfway
  leaves the previous record whole; no state lives in an asset that a
  replacement deletes first;
- `SOURCES.json`, a copy of that record for people, written from it on every
  run.

A version already kept is not uploaded again: only its last release and date
move. A version that no AppImage released in the last three years (plus 30
days) bundles is deleted. A kept version whose asset is missing is archived
again, bundled now or not. A `.tar` asset the record does not name is a
leftover of a run that stopped before recording it, which therefore published
nothing that names it: it is deleted. So a version stays downloadable for at
least three years after the last release that ships it, which is what the
offer promises.

Usage: archive-appimage-sources.py <folder with this release's AppImage> <AeroFTP version>
Reads GITHUB_REPOSITORY, GITHUB_REF_NAME and GITHUB_SHA; gh reads GH_TOKEN.

The sources kept are those of the AppImage the release distributes. On a
re-run of a tag whose release already has its AppImage, the publish job keeps
the published one (it uploads with overwrite_files: false), and a rebuild can
bundle newer Ubuntu libraries: the list is then read from the published
AppImage, downloaded from the release, never from the rebuild. The publish job runs
it under a concurrency group shared by every tag, so two runs never write the
record at once.

The list is read without running the AppImage: a type 2 AppImage is an ELF
runtime with a squashfs image appended at the end of the ELF (the end of its
section header table), and unsquashfs reads that image at that offset. The
published AppImage is a download, and this job holds a token that can write
the releases: no code of it runs here, and unsquashfs runs with an
environment that holds no token. unsquashfs is squashfs-tools, which the
publish job installs.

Any step that cannot be done or checked stops with an error: the publish job
then does not publish, because the AppImage's offer names this release.
No AppImage in the folder (the Linux build failed) is not an error: nothing
of this release names the archive.

For the tests only: AEROFTP_SRC_FETCH (a command run as `<command> <url>
<file>` that writes the body to <file> and prints the HTTP status),
AEROFTP_SRC_BACKOFF (seconds between tries, 5 by default) and AEROFTP_TODAY
(YYYY-MM-DD). Tests: .github/scripts/test_archive_appimage_sources.py
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

TAG = 'appimage-sources'
MANIFEST = 'SOURCES.json'
LIST_IN_APPIMAGE = 'usr/share/doc/BUNDLED-LIBRARIES.txt'
SOURCEFILES = 'https://launchpad.net/ubuntu/+archive/primary/+sourcefiles'
# Three years, and 30 days of margin, after the last release that bundles it.
KEEP_AFTER_LAST_RELEASE = dt.timedelta(days=3 * 365 + 1 + 30)
STATE_OPEN = '<!-- appimage-sources-state'
STATE_CLOSE = '-->'
# GitHub refuses a release body over 125000 characters.
BODY_LIMIT = 120000
ENTRY_KEYS = ('source', 'version', 'asset', 'sha256', 'size', 'first_release', 'last_release', 'last_release_date')
NOTES = (
    'The source of every library the AeroFTP AppImage bundles, kept for the written offer in '
    '`usr/share/doc/BUNDLED-LIBRARIES.txt` of each AppImage. Each `<source>_<version>.tar` holds the '
    '`.dsc` of that Ubuntu source version and every file it lists, as the Ubuntu archive publishes '
    'them. `SOURCES.json` names the asset of each version, its SHA-256, and the AeroFTP releases that '
    'bundle it. A version is kept for at least three years after the last AeroFTP release whose '
    'AppImage bundles it. This is not an AeroFTP release: the AeroFTP downloads are in the releases '
    'named after a version.'
)


class Refused(Exception):
    pass


def gh(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(['gh', *args], capture_output=True, text=True)


def today() -> dt.date:
    forced = os.environ.get('AEROFTP_TODAY')
    return dt.date.fromisoformat(forced) if forced else dt.datetime.now(dt.timezone.utc).date()


def bundled_sources(list_text: str) -> list[tuple[str, str]]:
    """The distinct (source, version) pairs of BUNDLED-LIBRARIES.txt."""
    head, sep, body = list_text.partition('\n\n')
    if not sep or 'Written offer:' not in head:
        raise Refused(f'{LIST_IN_APPIMAGE} does not have its header and its list')
    pairs = set()
    for line in body.splitlines():
        if not line.strip():
            continue
        fields = line.split(' ')
        # <library> <package> <version> <source> <source version> <Launchpad page>
        if len(fields) != 6 or not fields[5].startswith('https://launchpad.net/ubuntu/+source/'):
            raise Refused(f'cannot read this line of {LIST_IN_APPIMAGE}: {line!r}')
        pairs.add((fields[3], fields[4]))
    if not pairs:
        raise Refused(f'{LIST_IN_APPIMAGE} lists no library')
    return sorted(pairs)


def asset_name(source: str, version: str) -> str:
    # The file name of a Debian source drops the epoch. Characters GitHub may
    # rewrite in an asset name become `_`; the record keeps the exact pair.
    base = f'{source}_{version.split(":", 1)[-1]}'
    return re.sub(r'[^A-Za-z0-9._-]', '_', base) + '.tar'


def fetch(url: str, out: Path) -> None:
    """The body of url into out, three tries; a 404/410 or no answer refuses."""
    backoff = float(os.environ.get('AEROFTP_SRC_BACKOFF', '5'))
    command = os.environ.get('AEROFTP_SRC_FETCH')
    for attempt in range(1, 4):
        if command:
            done = subprocess.run([command, url, str(out)], capture_output=True, text=True)
            code = done.stdout.strip() if done.returncode == 0 else '000'
        else:
            try:
                with urllib.request.urlopen(url, timeout=120) as response, open(out, 'wb') as f:
                    shutil.copyfileobj(response, f)
                code = str(response.status)
            except urllib.error.HTTPError as e:
                code = str(e.code)
            except (urllib.error.URLError, OSError):
                code = '000'
        if code == '200':
            return
        if code in ('404', '410'):
            raise Refused(f'Launchpad has no {url} (HTTP {code})')
        if attempt < 3:
            time.sleep(attempt * backoff)
    raise Refused(f'Launchpad did not answer for {url} after three tries')


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def dsc_files(dsc: str, source: str, version: str) -> list[tuple[str, str, int]]:
    """(name, sha256, size) of every file the .dsc lists; its Source and Version checked."""
    fields = {}
    current = None
    for line in dsc.splitlines():
        # The armor of a signed .dsc can carry its own `Version:` header.
        if line.startswith('-----BEGIN PGP SIGNATURE-----'):
            break
        if line.startswith((' ', '\t')) and current:
            fields[current].append(line.strip())
        elif ':' in line and not line.startswith('-----'):
            key, _, value = line.partition(':')
            current = key.strip().lower()
            fields[current] = [value.strip()] if value.strip() else []
        else:
            current = None
    got = (' '.join(fields.get('source', [])), ' '.join(fields.get('version', [])))
    if got != (source, version):
        raise Refused(f'the .dsc of {source} {version} is source {got[0] or "?"} {got[1] or "?"}')
    files = []
    for entry in fields.get('checksums-sha256', []):
        parts = entry.split()
        if len(parts) != 3 or not re.fullmatch(r'[0-9a-f]{64}', parts[0]) or not parts[1].isdigit():
            raise Refused(f'cannot read this Checksums-Sha256 line of the .dsc of {source} {version}: {entry!r}')
        if '/' in parts[2] or parts[2] in ('.', '..'):
            raise Refused(f'the .dsc of {source} {version} lists a file outside its folder: {parts[2]!r}')
        files.append((parts[2], parts[0], int(parts[1])))
    if not files:
        raise Refused(f'the .dsc of {source} {version} lists no file under Checksums-Sha256')
    return files


def archive(source: str, version: str, work: Path) -> Path:
    """The tar of one source version: its .dsc and every file it lists, checked."""
    name = asset_name(source, version)
    folder = work / name[: -len('.tar')]
    folder.mkdir()
    base = f'{SOURCEFILES}/{source}/{version}'
    dsc_name = f'{source}_{version.split(":", 1)[-1]}.dsc'
    fetch(f'{base}/{dsc_name}', folder / dsc_name)
    listed = dsc_files((folder / dsc_name).read_text(encoding='utf-8', errors='replace'), source, version)
    names = [dsc_name]
    for file_name, want_sha, want_size in listed:
        fetch(f'{base}/{file_name}', folder / file_name)
        got_sha, got_size = sha256(folder / file_name), (folder / file_name).stat().st_size
        if (got_sha, got_size) != (want_sha, want_size):
            raise Refused(
                f'{file_name} of {source} {version} is {got_size} bytes with SHA-256 {got_sha}, '
                f'the .dsc says {want_size} bytes with {want_sha}'
            )
        names.append(file_name)
    tar_path = work / name

    def plain(info: tarfile.TarInfo) -> tarfile.TarInfo:
        info.uid = info.gid = 0
        info.uname = info.gname = ''
        info.mtime = 0
        info.mode = 0o755 if info.isdir() else 0o644
        return info

    with tarfile.open(tar_path, 'w', format=tarfile.PAX_FORMAT) as tar:
        tar.add(folder, arcname=folder.name, recursive=False, filter=plain)
        for file_name in sorted(names):
            tar.add(folder / file_name, arcname=f'{folder.name}/{file_name}', filter=plain)
    shutil.rmtree(folder)
    return tar_path


def release_by_tag(repo: str, tag: str) -> dict | None:
    """The release `tag` with its body and assets, or None when it does not exist."""
    done = gh('api', '--include', f'repos/{repo}/releases/tags/{tag}')
    status = done.stdout.split('\n', 1)[0]
    if done.returncode != 0:
        if re.match(r'^HTTP/[0-9.]+ 404(\s|$)', status):
            return None
        raise Refused(f'cannot look up the release {tag}: {status or done.stderr.strip()}')
    body = re.split(r'\r?\n\r?\n', done.stdout, maxsplit=1)
    try:
        release = json.loads(body[1]) if len(body) == 2 else None
    except json.JSONDecodeError:
        release = None
    if (
        not isinstance(release, dict)
        or release.get('tag_name') != tag
        or not isinstance(release.get('body') or '', str)
        or not isinstance(release.get('assets'), list)
        or not all(isinstance(a, dict) and isinstance(a.get('name'), str) for a in release['assets'])
    ):
        raise Refused(f'cannot read the release {tag}, its body and its assets from gh api --include')
    return release


def lookup(repo: str) -> dict | None:
    """The archive release with its body and assets, or None when it does not exist."""
    return release_by_tag(repo, TAG)


def distributed_appimage(repo: str, tag: str, built: Path, work: Path) -> Path:
    """The AppImage the release `tag` distributes: the one already published
    under the same name, downloaded, or this build's when there is none yet."""
    release = release_by_tag(repo, tag)
    if release is None or built.name not in {a['name'] for a in release['assets']}:
        return built
    folder = work / 'published'
    folder.mkdir()
    done = gh('release', 'download', tag, '--repo', repo, '--pattern', built.name, '--dir', str(folder), '--clobber')
    published = folder / built.name
    if done.returncode != 0 or not published.is_file():
        raise Refused(f'cannot download the published {built.name} of {tag}: {done.stderr.strip()}')
    print(f'{tag} already distributes {built.name}: keeping the sources of the published one, not of this rebuild')
    return published


def squashfs_offset(image: Path) -> int:
    """Where the squashfs image of a type 2 AppImage starts: the end of its
    ELF runtime, as the runtime itself computes it (appimage_get_elf_size in
    type2-runtime's runtime.c): the later of the end of the section header
    table (e_shoff + e_shentsize * e_shnum) and the end of the last section
    it lists (sh_offset + sh_size), since an ELF ends with either. Read from
    the file, never by running it."""
    with image.open('rb') as f:
        head = f.read(64)
        if len(head) < 52 or head[:4] != b'\x7fELF' or head[4] not in (1, 2) or head[5] not in (1, 2):
            raise Refused(f'{image.name} is not an ELF AppImage: cannot find its squashfs image without running it')
        order = 'little' if head[5] == 1 else 'big'
        if head[4] == 2:
            if len(head) < 64:
                raise Refused(f'{image.name} has a truncated ELF header')
            shoff, shentsize, shnum = (int.from_bytes(head[a:b], order) for a, b in ((0x28, 0x30), (0x3A, 0x3C), (0x3C, 0x3E)))
            # sh_offset and sh_size of an Elf64_Shdr.
            fields = ((0x18, 0x20), (0x20, 0x28))
        else:
            shoff, shentsize, shnum = (int.from_bytes(head[a:b], order) for a, b in ((0x20, 0x24), (0x2E, 0x30), (0x30, 0x32)))
            # sh_offset and sh_size of an Elf32_Shdr.
            fields = ((0x10, 0x14), (0x14, 0x18))
        offset = shoff + shentsize * shnum
        if shnum > 0:
            f.seek(shoff + shentsize * (shnum - 1))
            last = f.read(fields[1][1])
            if len(last) < fields[1][1]:
                raise Refused(f'{image.name} has no squashfs image at the end of its ELF runtime: cannot read its last section header')
            sh_offset, sh_size = (int.from_bytes(last[a:b], order) for a, b in fields)
            offset = max(offset, sh_offset + sh_size)
        f.seek(offset)
        if offset <= 0 or f.read(4) != b'hsqs':
            raise Refused(f'{image.name} has no squashfs image at the end of its ELF runtime (offset {offset})')
    return offset


def extract_list(image: Path, work: Path) -> Path:
    """LIST_IN_APPIMAGE from the squashfs image of `image`, extracted with
    unsquashfs: the AppImage is not run. unsquashfs gets only PATH and a
    locale, so no token of this job reaches it, and the list must be a
    regular file of the image at its own path: a symbolic link, the list's or
    a folder's above it, would make this job read a file of the runner
    instead."""
    offset = squashfs_offset(image)
    # unsquashfs runs in `work`: a path relative to this job's folder (the
    # workflow passes release-assets/appimage) would name nothing there.
    image, work = image.absolute(), work.absolute()
    unsquashfs = shutil.which('unsquashfs')
    if unsquashfs is None:
        raise Refused('unsquashfs (squashfs-tools) is not installed: the list is read from the AppImage without running it')
    root = work / 'squashfs-root'
    extracted = subprocess.run(
        [unsquashfs, '-no-xattrs', '-o', str(offset), '-d', str(root), str(image), LIST_IN_APPIMAGE],
        cwd=work, capture_output=True, text=True,
        env={'PATH': os.environ.get('PATH', os.defpath), 'LC_ALL': 'C'},
    )
    list_path = root / LIST_IN_APPIMAGE
    try:
        inside = list_path.resolve() == root.resolve() / LIST_IN_APPIMAGE
    except (OSError, RuntimeError):  # a loop of symbolic links
        inside = False
    if extracted.returncode != 0 or not inside or not list_path.is_file():
        detail = extracted.stderr.strip() if extracted.returncode != 0 else 'it is not a regular file of the image'
        raise Refused(f'cannot extract {LIST_IN_APPIMAGE} from {image.name}: {detail}')
    return list_path


def read_state(body: str) -> list[dict] | None:
    """The record in the release body, or None when the body has none."""
    start = body.find(STATE_OPEN)
    if start < 0:
        return None
    end = body.find(STATE_CLOSE, start)
    try:
        if end < 0:
            raise ValueError('the record has no end')
        entries = json.loads(body[start + len(STATE_OPEN):end])
        if not isinstance(entries, list):
            raise ValueError('the record is not a list')
        for e in entries:
            if not isinstance(e, dict) or set(e) != set(ENTRY_KEYS):
                raise ValueError(f'an entry does not have exactly {ENTRY_KEYS}: {e!r}')
            if not all(isinstance(e[k], str) for k in ENTRY_KEYS if k != 'size') or not isinstance(e['size'], int):
                raise ValueError(f'an entry has a field of the wrong type: {e!r}')
            dt.date.fromisoformat(e['last_release_date'])
    except ValueError as e:
        raise Refused(f'cannot read the record in the body of the release {TAG}: {e}') from e
    return entries


def render_body(entries: list[dict]) -> str:
    state = json.dumps(entries, ensure_ascii=False, separators=(',', ':'), sort_keys=True)
    body = f'{NOTES}\n\n{STATE_OPEN}\n{state}\n{STATE_CLOSE}\n'
    if len(body) > BODY_LIMIT:
        raise Refused(f'the record of the release {TAG} is {len(body)} characters, over the {BODY_LIMIT} a release body takes')
    return body


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print('usage: archive-appimage-sources.py <folder with the AppImage> <AeroFTP version>', file=sys.stderr)
        return 2
    folder, version = Path(argv[1]), argv[2]
    repo = os.environ['GITHUB_REPOSITORY']
    appimages = sorted(folder.glob('*.AppImage')) if folder.is_dir() else []
    if not appimages:
        print(f'No AppImage in {folder}: this release names no source archive, nothing to keep')
        return 0
    if len(appimages) > 1:
        raise Refused(f'more than one AppImage in {folder}: {[a.name for a in appimages]}')
    if shutil.which('unsquashfs') is None:
        raise Refused('unsquashfs (squashfs-tools) is not installed: the list is read from the AppImage without running it')
    when = today()
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        source = distributed_appimage(repo, os.environ['GITHUB_REF_NAME'], appimages[0], work)
        list_path = extract_list(source, work)
        needed = bundled_sources(list_path.read_text(encoding='utf-8'))

        release = lookup(repo)
        if release is None:
            notes = work / 'notes.md'
            notes.write_text(render_body([]), encoding='utf-8')
            created = gh(
                'release', 'create', TAG, '--repo', repo, '--target', os.environ['GITHUB_SHA'],
                '--title', 'Source of the libraries the AppImage bundles', '--notes-file', str(notes),
                '--prerelease', '--latest=false',
            )
            if created.returncode != 0:
                raise Refused(f'cannot create the release {TAG}: {created.stderr.strip()}')
            release = lookup(repo)
            if release is None:
                raise Refused(f'the release {TAG} is not there after its creation')
        state = read_state(release.get('body') or '')
        sizes = {a['name']: a.get('size') for a in release['assets']}
        if state is None:
            if any(name.endswith('.tar') for name in sizes):
                raise Refused(f'the release {TAG} has source assets but its body has no record: cannot tell which source each one is')
            state = []
        entries = {(e['source'], e['version']): e for e in state}

        taken = {e['asset']: key for key, e in entries.items()}
        for key in needed:
            name = asset_name(*key)
            if taken.get(name, key) != key:
                raise Refused(f'{key[0]} {key[1]} and {taken[name][0]} {taken[name][1]} would share the asset name {name}')
            taken[name] = key

        # A kept version the release lost goes back, whether this AppImage
        # bundles it or only an earlier one still within its three years.
        missing = set()
        for key, e in entries.items():
            expiring = key not in needed and dt.date.fromisoformat(e['last_release_date']) + KEEP_AFTER_LAST_RELEASE < when
            if not expiring and sizes.get(e['asset']) != e['size']:
                print(f'::warning::the release {TAG} does not hold {e["asset"]} at {e["size"]} bytes; archiving {key[0]} {key[1]} again')
                missing.add(key)

        uploaded = 0
        for key in sorted((set(needed) - set(entries)) | missing):
            tar_path = archive(key[0], key[1], work)
            done = gh('release', 'upload', TAG, str(tar_path), '--repo', repo, '--clobber')
            if done.returncode != 0:
                raise Refused(f'cannot upload {tar_path.name} to the release {TAG}: {done.stderr.strip()}')
            entry = entries.get(key) or {'first_release': version, 'last_release': version, 'last_release_date': when.isoformat()}
            entry.update(source=key[0], version=key[1], asset=tar_path.name, sha256=sha256(tar_path), size=tar_path.stat().st_size)
            entries[key] = entry
            tar_path.unlink()
            uploaded += 1
        for key in needed:
            entries[key].update(last_release=version, last_release_date=when.isoformat())

        expired = sorted(
            key for key, e in entries.items()
            if dt.date.fromisoformat(e['last_release_date']) + KEEP_AFTER_LAST_RELEASE < when
        )
        for key in expired:
            del entries[key]
        record = [{k: entries[key][k] for k in ENTRY_KEYS} for key in sorted(entries)]

        # The record first, in one request: from here on it names only assets
        # that are there. Then the assets it no longer names go.
        notes = work / 'notes.md'
        notes.write_text(render_body(record), encoding='utf-8')
        done = gh('release', 'edit', TAG, '--repo', repo, '--notes-file', str(notes))
        if done.returncode != 0:
            raise Refused(f'cannot write the record to the release {TAG}: {done.stderr.strip()}')
        named = {e['asset'] for e in record}
        stale = sorted(name for name in sizes if name.endswith('.tar') and name not in named)
        for name in stale:
            done = gh('release', 'delete-asset', TAG, name, '--repo', repo, '--yes')
            if done.returncode != 0:
                raise Refused(f'cannot delete {name}, which the record no longer names, from the release {TAG}: {done.stderr.strip()}')
        (work / MANIFEST).write_text(
            json.dumps({'about': NOTES, 'sources': record}, indent=2, ensure_ascii=False) + '\n', encoding='utf-8'
        )
        done = gh('release', 'upload', TAG, str(work / MANIFEST), '--repo', repo, '--clobber')
        if done.returncode != 0:
            raise Refused(f'cannot upload {MANIFEST} to the release {TAG}: {done.stderr.strip()}')

        # What the release holds now, read back: the record as written, every
        # asset it names at its size, and its copy for people.
        final = lookup(repo)
        if final is None or read_state(final.get('body') or '') != record:
            raise Refused(f'the body of the release {TAG} does not hold the record just written')
        held = {a['name']: a.get('size') for a in final['assets']}
        for e in record:
            if held.get(e['asset']) != e['size']:
                raise Refused(
                    f'the release {TAG} does not hold {e["asset"]} at {e["size"]} bytes after the upload '
                    f'(it has {held.get(e["asset"], "no such asset")})'
                )
        if MANIFEST not in held:
            raise Refused(f'the release {TAG} has no {MANIFEST} after the upload')
    print(
        f'Source archive {TAG}: {len(needed)} source versions bundled by AeroFTP {version}, '
        f'{uploaded} uploaded, {len(record)} kept, {len(stale)} deleted'
    )
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main(sys.argv))
    except Refused as e:
        print(f'::error::{e}', file=sys.stderr)
        sys.exit(1)
