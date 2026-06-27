#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
"""
AeroFile Nautilus extension: an "AeroFile" submenu with "Extract here" /
"Extract to folder" verbs for archives and AeroFTP vaults (Deliverable G, Task 2).

Verbs only: the double-click "Open" of the general formats is untouched (decision
c). The actions are grouped under a single "AeroFile" submenu (like Dropbox) so
they do not clutter the top level next to the system's own extract entries. Labels
follow the desktop language, reusing the app's own `contextMenu.extractHere` /
`contextMenu.extractToFolder` translations (47 languages, kept in lockstep below).

  Extract here -> try the headless CLI `aeroftp extract <file> <dir>` (a clear
                  archive extracts in place with no UI), falling back to
                  `aeroftp --extract-here <file>` (the dedicated password window)
                  only when it is encrypted. Decision 2: a clear extract never
                  boots the app.
  Extract to folder -> extracts into a never-clobbering subfolder named after the
                  archive, in the archive's own directory, automatically (no folder
                  picker, like standard extractors). Same clear/encrypted split.

The work runs detached so Nautilus never blocks.
"""

import os
import subprocess

import gi

# Nautilus 4.0 (GNOME 43+) first, fall back to 3.0 for older desktops.
try:
    gi.require_version("Nautilus", "4.0")
except ValueError:
    gi.require_version("Nautilus", "3.0")

from gi.repository import Nautilus, GObject  # noqa: E402

# Localized (here, to_folder) labels, mirrored from the app's contextMenu.* keys
# so the file-manager verbs read in the same language as AeroFTP itself.
LABELS = {
    "bg": ("Разархивиране тук", "Разархивиране в папка"),
    "bn": ("এখানে এক্সট্রাক্ট করুন", "ফোল্ডারে এক্সট্রাক্ট করুন"),
    "ca": ("Extreure aquí", "Extreure a carpeta"),
    "cs": ("Rozbalit zde", "Rozbalit do slozky"),
    "cy": ("Echdynnu Yma", "Echdynnu i Ffolder"),
    "da": ("Udpak her", "Udpak til mappe"),
    "de": ("Hier entpacken", "In Ordner entpacken"),
    "el": ("Εξαγωγή Εδώ", "Εξαγωγή σε Φάκελο"),
    "en": ("Extract Here", "Extract to Folder"),
    "es": ("Extraer aquí", "Extraer a carpeta"),
    "et": ("Paki siia lahti", "Paki kausta lahti"),
    "eu": ("Erauzi hemen", "Erauzi karpetara"),
    "fi": ("Pura tähän", "Pura kansioon"),
    "fr": ("Extraire ici", "Extraire dans un dossier"),
    "gl": ("Extraer aquí", "Extraer nun cartafol"),
    "hi": ("यहाँ निकालें", "फ़ोल्डर में निकालें"),
    "hr": ("Izdvoji ovdje", "Izdvoji u mapu"),
    "hu": ("Kibontás ide", "Kibontás mappába"),
    "hy": ("Արտահանել այստեղ", "Արտահանել պանակում"),
    "id": ("Ekstrak Di Sini", "Ekstrak ke Folder"),
    "is": ("Afþjappa hér", "Afþjappa í möppu"),
    "it": ("Estrai qui", "Estrai nella cartella"),
    "ja": ("ここに展開", "フォルダに展開"),
    "ka": ("აქ ამოღება", "საქაღალდეში ამოღება"),
    "km": ("ស្រង់ចេញនៅទីនេះ", "ស្រង់ចេញទៅថត"),
    "ko": ("여기에 압축 해제", "폴더에 압축 해제"),
    "lt": ("Išskleisti čia", "Išskleisti į aplanką"),
    "lv": ("Izvilkt šeit", "Izvilkt mapē"),
    "mk": ("Извлечи тука", "Извлечи во папка"),
    "ms": ("Ekstrak Di Sini", "Ekstrak ke Folder"),
    "nl": ("Hier uitpakken", "Uitpakken naar map"),
    "no": ("Pakk ut her", "Pakk ut til mappe"),
    "pl": ("Wypakuj tutaj", "Wypakuj do folderu"),
    "pt": ("Extrair Aqui", "Extrair para Pasta"),
    "ro": ("Extrage aici", "Extrage în dosar"),
    "ru": ("Извлечь сюда", "Извлечь в папку"),
    "sk": ("Rozbalit tu", "Rozbalit do priecinka"),
    "sl": ("Razširi sem", "Razširi v mapo"),
    "sr": ("Raspakuj ovde", "Raspakuj u fasciklu"),
    "sv": ("Extrahera här", "Extrahera till mapp"),
    "sw": ("Fungua Hapa", "Fungua kwenye Folda"),
    "th": ("แตกไฟล์ที่นี่", "แตกไฟล์ไปยังโฟลเดอร์"),
    "tl": ("I-extract Dito", "I-extract sa Folder"),
    "tr": ("Buraya Çıkar", "Klasöre Çıkar"),
    "uk": ("Видобути тут", "Видобути в теку"),
    "vi": ("Giải nén tại đây", "Giải nén vào thư mục"),
    "zh": ("解压到此处", "解压到文件夹"),
}

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

# Archive extensions stripped to derive the "Extract to folder" subfolder name.
# Mirrors the Rust archive_extract_stem / the TS archiveStem (kept in lockstep).
_MULTI_EXT = (".tar.gz", ".tar.xz", ".tar.bz2")
_SINGLE_EXT = (".tgz", ".txz", ".tbz2", ".tar", ".zip", ".7z", ".rar", ".aerovault", ".aerozip")


def _desktop_lang() -> str:
    """Two-letter language code from the desktop locale env, default 'en'."""
    for var in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"):
        val = os.environ.get(var)
        if val:
            # LANGUAGE may be "it:en"; locale may be "it_IT.UTF-8".
            code = val.split(":")[0].split(".")[0].split("_")[0].strip().lower()
            if code:
                return code
    return "en"


def _labels():
    return LABELS.get(_desktop_lang(), LABELS["en"])


def _archive_stem(name: str) -> str:
    lower = name.lower()
    for ext in _MULTI_EXT:
        if lower.endswith(ext):
            return name[: -len(ext)]
    for ext in _SINGLE_EXT:
        if lower.endswith(ext):
            return name[: -len(ext)]
    dot = name.rfind(".")
    return name[:dot] if dot > 0 else name


def _unique_subfolder(parent: str, name: str) -> str:
    """parent/stem, or parent/stem (2), (3)... if earlier candidates exist."""
    stem = _archive_stem(name) or "extracted"
    candidate = os.path.join(parent, stem)
    if not os.path.exists(candidate):
        return candidate
    n = 2
    while n < 10000:
        candidate = os.path.join(parent, "%s (%d)" % (stem, n))
        if not os.path.exists(candidate):
            return candidate
        n += 1
    return os.path.join(parent, stem)


def _is_supported(nautilus_file) -> bool:
    if nautilus_file.is_directory():
        return False
    try:
        mime = nautilus_file.get_mime_type()
    except Exception:
        mime = None
    if mime and mime in SUPPORTED_MIME:
        return True
    return nautilus_file.get_name().lower().endswith(SUPPORTED_SUFFIXES)


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
    """Adds the AeroFile submenu (extract verbs) to archives / vaults."""

    def _extract_here(self, _menu, path: str) -> None:
        # Clear archive: pure CLI into the archive's own directory (decision 2),
        # falling back to the dedicated password window when it is encrypted.
        parent = os.path.dirname(path) or "."
        script = 'aeroftp extract "$1" "$2" >/dev/null 2>&1 || aeroftp --extract-here "$1"'
        _spawn(["sh", "-c", script, "sh", path, parent])

    def _extract_to_folder(self, _menu, path: str) -> None:
        # Like standard extractors: a subfolder named after the archive, in the
        # archive's own directory, no folder picker. Never clobbers an existing
        # folder. Clear archive stays pure CLI; encrypted falls back to the window.
        parent = os.path.dirname(path) or "."
        dest = _unique_subfolder(parent, os.path.basename(path))
        script = 'aeroftp extract "$1" "$2" >/dev/null 2>&1 || aeroftp --extract-to "$1"'
        _spawn(["sh", "-c", script, "sh", path, dest])

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

        here_label, to_label = _labels()

        here = Nautilus.MenuItem(
            name="AeroFile::ExtractHere",
            label=here_label,
            tip="Extract this archive here with AeroFTP",
        )
        here.connect("activate", self._extract_here, path)

        to_folder = Nautilus.MenuItem(
            name="AeroFile::ExtractToFolder",
            label=to_label,
            tip="Extract this archive into a subfolder with AeroFTP",
        )
        to_folder.connect("activate", self._extract_to_folder, path)

        # Group both under one "AeroFile" submenu (brand, not translated).
        top = Nautilus.MenuItem(name="AeroFile::Menu", label="AeroFile")
        submenu = Nautilus.Menu()
        submenu.append_item(here)
        submenu.append_item(to_folder)
        top.set_submenu(submenu)
        return [top]

    # Nautilus 4.0 calls get_file_items(self, files); 3.0 passes (self, window,
    # files). Accept both by taking the last positional argument as the file list.
    def get_file_items(self, *args):
        files = args[-1] if args else []
        return self._items_for(files)
