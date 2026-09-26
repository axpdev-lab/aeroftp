// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import {
    AEROSYNC_DEFAULT_BACKUP_DIR,
    AEROSYNC_DEFAULT_EXCLUDES,
    aeroSyncCompareOptions,
    compareExcludePatterns,
    sameExcludePatterns,
    appliedCompareFilters,
} from './aeroSyncExcludes';

describe('AeroSync compare exclusions', () => {
    it('sends the Plan tab patterns to every compare, after the defaults', () => {
        // Before the Plan had an exclude field, a remote compare ran with the
        // defaults only, so a file matching the user's pattern was planned
        // (and a destination-only one deleted by Mirror).
        const options = aeroSyncCompareOptions(['*.log', 'cache/'], AEROSYNC_DEFAULT_BACKUP_DIR);
        expect(options.exclude_patterns).toEqual([...AEROSYNC_DEFAULT_EXCLUDES, '*.log', 'cache/']);
        expect(options.direction).toBe('bidirectional');
    });

    it('sends the backup folder to every compare, so neither side lists it', () => {
        expect(aeroSyncCompareOptions([], 'old/copies').backup_dir).toBe('old/copies');
        expect(aeroSyncCompareOptions([], AEROSYNC_DEFAULT_BACKUP_DIR).backup_dir).toBe('.aeroftp-versions');
    });

    it('keeps the defaults when the user adds nothing, and does not repeat one the user repeats', () => {
        expect(compareExcludePatterns([])).toEqual([...AEROSYNC_DEFAULT_EXCLUDES]);
        expect(compareExcludePatterns(['.git', 'x'])).toEqual([...AEROSYNC_DEFAULT_EXCLUDES, 'x']);
    });

    it('treats reordered or repeated patterns as the same set, and a new one as a change', () => {
        expect(sameExcludePatterns(['a', 'b'], ['b', 'a', 'a'])).toBe(true);
        expect(sameExcludePatterns([], [])).toBe(true);
        expect(sameExcludePatterns(['a'], [])).toBe(false);
        expect(sameExcludePatterns([], ['a'])).toBe(false);
        expect(sameExcludePatterns(['a', 'b'], ['a', 'c'])).toBe(false);
    });

    it('reports a flat classify as applying neither the patterns nor the backup folder', () => {
        // The flat classify of the two panel listings filters nothing. Reported
        // as filtered, the Plan saw its own fields matched and let Mirror act on
        // files, and on the backup folder, the compare never hid.
        const flat = appliedCompareFilters('flat', ['*.log'], AEROSYNC_DEFAULT_BACKUP_DIR);
        expect(flat).toEqual({ compareExcludes: [], compareBackupDir: undefined });
        expect(sameExcludePatterns(['*.log'], flat.compareExcludes)).toBe(false);
        expect(appliedCompareFilters('recursive', ['*.log'], 'old')).toEqual({
            compareExcludes: ['*.log'],
            compareBackupDir: 'old',
        });
    });
});
