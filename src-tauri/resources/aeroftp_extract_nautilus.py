#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
"""
AeroFTP Nautilus extension: "Extract here" / "Extract to folder..." context-menu
verbs for archives and AeroFTP vaults (Deliverable G, Task 2).

These are verbs ONLY: they do not change the double-click "Open" of any format
(decision c). They attach to the general archive MIME types (zip, 7z, tar*, rar)
and to the aero* containers (.aerovault, .aerozip), and shell the Task 1 intent:

  Extract here  -> try the headless CLI `aeroftp extract <file> <dir>` (a clear
                   archive extracts in place with no UI), and if that fails
                   (encrypted, needs a password) fall back to `aeroftp
                   --extract-here <file>`, which opens the dedicated password
                   window. Decision 2: a clear "Extract here" never boots the app.
  Extract to folder... -> always `aeroftp --extract-to <file>`, which opens the
                   dedicated window, shows the native folder picker, and extracts
                   into a never-clobbering stem subfolder (decision 4).

The work runs in a detached process so Nautilus never blocks.
"""

import os
import shlex
import subprocess

import gi

# Nautilus 4.0 (GNOME 43+) first, fall back to 3.0 for older desktops.
try:
    gi.require_version("Nautilus", "4.0")
except ValueError:
    gi.require_version("Nautilus", "3.0")

from gi.repository import Nautilus, GObject  # noqa: E402

# MIME types AeroFTP can extract. The general archive types live in the shared
# MIME database already; the aero* types are registered by the AeroFTP package.
SUPPORTED_MIME = {
    "application/zip",
    "application/x-7z-compressed",
    "application/x-rar",
    "application/x-rar-compressed",
    "application/vnd.rar",
    "application/x-tar",
    "application/gzip",
    "application/x-gzip",
    "application/x-compressed-tar",
    "application/x-xz",
    "application/x-xz-compressed-tar",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-bzip-compressed-tar",
    "application/x-aerovault",
    "application/x-aerozip",
}

# Extension fallback for servers/filesystems that report a generic MIME type.
SUPPORTED_SUFFIXES = (
    ".zip",
    ".7z",
    ".rar",
    ".tar",
    ".tar.gz",
    ".tgz",
    ".tar.xz",
    ".txz",
    ".tar.bz2",
    ".tbz2",
    ".aerovault",
    ".aerozip",
)


def _is_supported(nautilus_file) -> bool:
    if nautilus_file.is_directory():
        return False
    try:
        mime = nautilus_file.get_mime_type()
    except Exception:
        mime = None
    if mime and mime in SUPPORTED_MIME:
        return True
    name = nautilus_file.get_name().lower()
    return name.endswith(SUPPORTED_SUFFIXES)


def _local_path(nautilus_file):
    """Local filesystem path of a Nautilus file, or None for a remote URI."""
    location = nautilus_file.get_location()
    return location.get_path() if location is not None else None


def _spawn(args) -> None:
    """Run a command fully detached so Nautilus is never blocked."""
    try:
        subprocess.Popen(
            args,
            start_new_session=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        # A missing `aeroftp` on PATH (partial install) must not crash Nautilus.
        pass


class AeroFTPExtractMenuProvider(GObject.GObject, Nautilus.MenuProvider):
    """Adds the two extract verbs to archives / vaults in the file manager."""

    def _extract_here(self, _menu, path: str) -> None:
        # Clear archive: pure CLI into the archive's own directory (decision 2),
        # falling back to the dedicated password window when it is encrypted.
        parent = os.path.dirname(path) or "."
        script = (
            'aeroftp extract "$1" "$2" >/dev/null 2>&1 '
            '|| aeroftp --extract-here "$1"'
        )
        _spawn(["sh", "-c", script, "sh", path, parent])

    def _extract_to(self, _menu, path: str) -> None:
        # Always the dedicated window: native folder picker, stem subfolder.
        _spawn(["aeroftp", "--extract-to", path])

    def _items_for(self, files):
        if len(files) != 1:
            return []
        nfile = files[0]
        if not _is_supported(nfile):
            return []
        path = _local_path(nfile)
        if not path:
            # Remote URIs (smb://, sftp://, ...) cannot be extracted in place.
            return []

        here = Nautilus.MenuItem(
            name="AeroFTP::ExtractHere",
            label="Extract here",
            tip="Extract this archive next to itself with AeroFTP",
        )
        here.connect("activate", self._extract_here, path)

        to_folder = Nautilus.MenuItem(
            name="AeroFTP::ExtractToFolder",
            label="Extract to folder...",
            tip="Extract this archive into a chosen folder with AeroFTP",
        )
        to_folder.connect("activate", self._extract_to, path)

        return [here, to_folder]

    # Nautilus 4.0 calls get_file_items(self, files); 3.0 passes (self, window,
    # files). Accept both by taking the last positional argument as the file list.
    def get_file_items(self, *args):
        files = args[-1] if args else []
        return self._items_for(files)
