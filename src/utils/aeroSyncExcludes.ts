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
 * What `aeroftp-cli sync --exclude` needs to leave alone everything the
 * compare left out: the defaults, the user's patterns, then the backup
 * folder, which every compare drops whether versioned backup is on or not.
 * The CLI adds none of them itself, so a Mirror line or an exported script
 * without them would upload `.env` and delete the copies earlier runs kept.
 * The CLI matcher takes the folder as a pattern, which excludes that name at
 * any depth: more than the compare, never less.
 */
export function cliExcludePatterns(userPatterns: readonly string[], backupDir: string): string[] {
    const out = compareExcludePatterns(userPatterns);
    const dir = backupDir.trim().replace(/^\/+|\/+$/g, '');
    if (dir && !out.includes(dir)) out.push(dir);
    return out;
}

/**
 * The user's own patterns out of a full list: the inverse of
 * [`cliExcludePatterns`], for a list read back from an exported script. A
 * default dropped here still applies, because every compare adds it, and so
 * does the backup folder (`backupDir`, the Plan's default when not given),
 * which the compare and every export take from the Plan: kept as a user
 * pattern it would outlive a change of the folder.
 */
export function userExcludePatterns(
    patterns: readonly string[],
    backupDir: string = AEROSYNC_DEFAULT_BACKUP_DIR,
): string[] {
    const dir = backupDir.trim().replace(/^\/+|\/+$/g, '');
    return patterns.filter((pattern) => !AEROSYNC_DEFAULT_EXCLUDES.includes(pattern) && pattern !== dir);
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

/**
 * The patterns and backup folder a compare can say it applied, recorded on
 * the AeroSync context so the Plan can refuse to run on a stale compare. A
 * recursive backend scan applies both. The flat classify of the two panel
 * listings applies neither, so it reports nothing applied: the Plan then
 * blocks Execute until a rescan works, rather than let Mirror act on files
 * the compare never hid.
 */
export function appliedCompareFilters(
    scan: 'recursive' | 'flat',
    userPatterns: string[],
    backupDir: string,
): { compareExcludes: string[]; compareBackupDir: string | undefined } {
    return scan === 'recursive'
        ? { compareExcludes: userPatterns, compareBackupDir: backupDir }
        : { compareExcludes: [], compareBackupDir: undefined };
}
