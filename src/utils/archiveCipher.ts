// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Shared archive-cipher helpers, used by both the unlock dialog badge and the
 * list-view Encryption column so they format a detected cipher identically.
 */

/** Backend archive kind accepted by the detect_archive_cipher / detect_archive_meta
 *  commands. Only these formats support proactive encryption detection. */
export type ArchiveKind = 'zip' | 'sevenz' | 'rar';

/** Map a file name to the backend archive kind, or null when it is not an
 *  archive we can inspect. Case-insensitive on the extension. */
export function archiveKindForName(name: string): ArchiveKind | null {
    const lower = name.toLowerCase();
    if (lower.endsWith('.zip')) return 'zip';
    if (lower.endsWith('.7z')) return 'sevenz';
    if (lower.endsWith('.rar')) return 'rar';
    return null;
}

/** Format a detected cipher token for a badge, adding "-bit" where it applies
 *  (AES variants) and flagging legacy ZipCrypto as weak so it reads amber, not
 *  green. Keeps the house "256-bit" wording consistent with the provider badges. */
export function formatArchiveCipher(cipher: string): { label: string; strong: boolean } {
    const aes = /^AES-(\d+)$/.exec(cipher);
    if (aes) return { label: `AES ${aes[1]}-bit`, strong: true };
    if (cipher === 'AES') return { label: 'AES', strong: true };
    if (cipher.toLowerCase() === 'zipcrypto') return { label: 'ZipCrypto', strong: false };
    return { label: cipher, strong: true };
}
