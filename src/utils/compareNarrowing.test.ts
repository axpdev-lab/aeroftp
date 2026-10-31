// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import { narrowsComparison } from './aeroSyncExcludes';
import { adaptFileComparisons } from './recursiveCompare';
import { withoutEntries } from './compareEndpoints';
import type { FileComparison } from '../types';

const info = (path: string, size: number) => ({ name: path, path, size, modified: null, is_dir: false, checksum: null });
const row = (relative_path: string, status: string, size: number): FileComparison => ({
    relative_path,
    status,
    is_dir: false,
    local_info: status === 'local_newer' ? info(relative_path, size) : null,
    remote_info: status === 'remote_only' ? info(relative_path, size) : null,
}) as unknown as FileComparison;

describe('narrowsComparison', () => {
    it('accepts a template that only adds exclusions (Ehud, .stfolder)', () => {
        expect(narrowsComparison([], '.aeroftp-versions', ['.stfolder'], '.aeroftp-versions')).toBe(true);
        expect(narrowsComparison(['*.tmp'], 'b', ['*.tmp', '.stfolder'], 'b')).toBe(true);
    });

    it('asks for a scan when an exclusion goes away: those files were never read', () => {
        expect(narrowsComparison(['*.tmp'], 'b', ['.stfolder'], 'b')).toBe(false);
    });

    it('asks for a scan when the old backup folder would come back', () => {
        expect(narrowsComparison([], 'old', [], 'new')).toBe(false);
        expect(narrowsComparison([], 'old', ['old'], 'new')).toBe(true);
    });

    it('never narrows a compare of the two panel listings, which applied nothing', () => {
        expect(narrowsComparison([], undefined, ['.stfolder'], '.aeroftp-versions')).toBe(false);
    });

    it('reads a default exclusion as still applied', () => {
        expect(narrowsComparison(['node_modules'], 'b', [], 'b')).toBe(true);
    });
});

describe('withoutEntries', () => {
    const summary = { examined_count: 10, identical_count: 7, examined_bytes: 1000, identical_bytes: 700 };
    const result = adaptFileComparisons([
        row('keep.txt', 'local_newer', 100),
        row('.stfolder/marker', 'local_newer', 50),
        row('.stfolder/other', 'remote_only', 150),
    ], true, summary);

    it('drops the rows and their counts and leaves the identical share alone', () => {
        const leftOut = result.entries.map((entry) => entry.relativePath!.startsWith('.stfolder'));
        const narrowed = withoutEntries(result, leftOut);
        expect(narrowed.entries.map((entry) => entry.relativePath)).toEqual(['keep.txt']);
        expect(narrowed.buckets['only-right']).toEqual([]);
        expect(narrowed.stats['only-right']).toEqual({ count: 0, bytes: 0 });
        expect(narrowed.stats.same).toEqual(result.stats.same);
        expect(narrowed.totals).toEqual({ count: 8, bytes: 800 });
        expect(narrowed.totals.count - narrowed.stats.same.count).toBe(narrowed.entries.length);
    });

    it('returns the same result when nothing is left out', () => {
        expect(withoutEntries(result, result.entries.map(() => false))).toBe(result);
    });
});
