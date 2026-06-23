// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Frontend pre-filter for AeroFile bridge-config recognition.
//
// The authoritative classifier is the Rust `bridge_identify` command
// (src-tauri/src/bridge_commands.rs -> identify_sources), which reads a
// bounded content head and returns the matching bridge-source ids. Calling it
// on EVERY file in a directory would be wasteful (a Tauri round-trip + a 64 KB
// read per file), so this pure, I/O-free matcher decides which files are even
// plausibly a third-party client config before we probe them. It mirrors the
// file-name / extension signals used by identify_sources; content-only formats
// still need the backend to confirm, which is exactly what the probe does.
//
// Tuning rationale: the broad extensions (.json/.xml/.sh) are intentionally
// NOT probed on their own (a data folder full of JSON should not trigger a
// scan); they only qualify via a name hint. The curated exact names + hints
// below cover the real-world config filenames each supported tool ships
// (rclone.conf, WinSCP.ini, sitemanager.xml, ~/.aws/credentials, ~/.ssh/config,
// .s3cfg, ~/.mc/config.json, kopia repository.config, duplicacy preferences,
// restic-env.sh, ...). The caller still caps the number of probes per
// directory as a backstop for pathological cases.

// Extensions that, on their own, make a file worth probing.
const CANDIDATE_EXTS = ['.conf', '.ini', '.cfg', '.rc', '.duck', '.ste', '.reg', '.config'];

// Whole (lowercased) file names that are config files for a supported tool
// even without a telltale extension.
const CANDIDATE_EXACT_NAMES = ['config', 'config.json', 'credentials', 'preferences', 'repository'];

// Substrings that mark a file as a likely client config regardless of its
// extension (covers FileZilla XML exports, MobaXterm .ini, restic .sh, ...).
const CANDIDATE_NAME_HINTS = [
    'rclone', 'winscp', 'mobaxterm', 's3cfg', 's3cmd', 'filezilla', 'sitemanager',
    'kopia', 'duplicacy', 'minio', 'restic', 'lftp', 'cyberduck', 'bookmark',
];

/**
 * Cheap, synchronous, no-I/O guard: could this file name plausibly be a
 * third-party client profile/config worth sending to `bridge_identify`?
 * False positives are fine (the backend rejects them); the goal is only to
 * avoid probing obvious non-configs (images, archives, media, source code).
 */
export function isBridgeConfigCandidate(fileName: string): boolean {
    if (!fileName || fileName === '..') return false;
    const n = fileName.toLowerCase();
    if (CANDIDATE_EXACT_NAMES.includes(n)) return true;
    if (CANDIDATE_NAME_HINTS.some(h => n.includes(h))) return true;
    return CANDIDATE_EXTS.some(ext => n.endsWith(ext));
}
