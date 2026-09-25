// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * What AeroSync's compare leaves out, in one place.
 *
 * The compare always skips these names (matched per path segment by the Rust
 * compare), so they never reach a plan: build output, VCS metadata, OS litter,
 * secrets files and error-correction sidecars, which would otherwise show as
 * orphans after an earlier run with error correction on. The user's own
 * patterns from the Plan tab are added after them.
 */
export const AEROSYNC_DEFAULT_EXCLUDES: readonly string[] = [
    'node_modules', '.git', '.DS_Store', 'Thumbs.db',
    '__pycache__', '*.pyc', '.env', 'target',
    '*.aerocorrect',
];

/**
 * Where versioned backup keeps old copies, relative to the destination root,
 * unless the Plan tab says otherwise.
 */
export const AEROSYNC_DEFAULT_BACKUP_DIR = '.aeroftp-versions';

/** The full list a compare runs with: the defaults, then the user's patterns. */
export function compareExcludePatterns(userPatterns: readonly string[]): string[] {
    const out = [...AEROSYNC_DEFAULT_EXCLUDES];
    for (const pattern of userPatterns) {
        if (!out.includes(pattern)) out.push(pattern);
    }
    return out;
}

/**
 * True when two user pattern lists exclude the same things for the compare.
 * Order and repeats do not change what a pattern set matches, so they do not
 * count as a difference.
 */
export function sameExcludePatterns(a: readonly string[], b: readonly string[]): boolean {
    const left = new Set(a);
    const right = new Set(b);
    if (left.size !== right.size) return false;
    for (const pattern of left) if (!right.has(pattern)) return false;
    return true;
}

/**
 * The `options` every AeroSync compare sends to the backend, local or remote.
 * The Rust compare drops an excluded path from both sides before classifying
 * it (`classify_with_summary`), so an excluded file is never copied, and a
 * destination-only one is never deleted by Mirror. The backup folder is
 * dropped the same way whether versioned backup is on or not, so a later
 * Mirror never deletes the copies an earlier run kept.
 */
export function aeroSyncCompareOptions(userPatterns: readonly string[], backupDir: string) {
    return {
        compare_timestamp: true,
        compare_size: true,
        compare_checksum: false,
        exclude_patterns: compareExcludePatterns(userPatterns),
        direction: 'bidirectional' as const,
        backup_dir: backupDir,
    };
}
