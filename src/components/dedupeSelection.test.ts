// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { keeperOf, selectionForPolicy, shortHash, SHORT_HASH_HEX } from './DuplicateFinderDialog';
import dialogRaw from './DuplicateFinderDialog.tsx?raw';
import type { DuplicateGroup } from '../types/aerofile';

const group = (files: string[], sizes?: number[]): DuplicateGroup => ({
    hash: files[0],
    size: sizes ? Math.max(...sizes) : 1000,
    files,
    file_sizes: sizes,
});

describe('which copy is kept (#347)', () => {
    it('keeps the shortest name by default, which is the one without " (copy)"', () => {
        // The reported case exactly: the dialog insisted the " (Copy)" file was
        // the one that could not be deleted, because it happened to be first.
        const g = group([
            '/photos/holiday (Copy).jpg',
            '/photos/holiday.jpg',
            '/backup/holiday (1).jpg',
        ]);
        expect(keeperOf(g, 'shortestName')).toBe('/photos/holiday.jpg');
        expect(selectionForPolicy([g], 'shortestName')).toEqual(
            new Set(['/photos/holiday (Copy).jpg', '/backup/holiday (1).jpg']),
        );
    });

    it('compares the file name, not the whole path', () => {
        // A short name deep in a tree must still win: the length of the folders
        // above it says nothing about which copy is the original.
        const g = group(['/a/b/c/d/e/f/g/photo.jpg', '/x/photo (2).jpg']);
        expect(keeperOf(g, 'shortestName')).toBe('/a/b/c/d/e/f/g/photo.jpg');
    });

    it('keeps the smallest or the largest when asked', () => {
        const g = group(['/a.jpg', '/b.jpg', '/c.jpg'], [3000, 1000, 2000]);
        expect(keeperOf(g, 'smallest')).toBe('/b.jpg');
        expect(keeperOf(g, 'largest')).toBe('/a.jpg');
    });

    it('falls back to scan order when there are no sizes to compare', () => {
        const g = group(['/a.jpg', '/b.jpg']);
        expect(keeperOf(g, 'smallest')).toBe('/a.jpg');
        expect(keeperOf(g, 'largest')).toBe('/a.jpg');
        expect(keeperOf(g, 'firstFound')).toBe('/a.jpg');
    });

    it('never selects a group entirely', () => {
        // Whatever the policy, the default selection always leaves one copy: the
        // starting point may not be a state that deletes everything.
        const groups = [
            group(['/a.jpg', '/a (1).jpg']),
            group(['/x.txt', '/y.txt', '/z.txt'], [10, 20, 30]),
        ];
        for (const policy of ['shortestName', 'smallest', 'largest', 'firstFound'] as const) {
            const selected = selectionForPolicy(groups, policy);
            for (const g of groups) {
                expect(g.files.some((f) => !selected.has(f)), `${policy} keeps one of ${g.files}`).toBe(true);
            }
        }
    });

    it('handles a group of one, and a group of none, without inventing a keeper', () => {
        expect(keeperOf(group([]), 'shortestName')).toBeUndefined();
        expect(selectionForPolicy([group(['/only.jpg'])], 'shortestName')).toEqual(new Set());
    });
});

describe('the hash is shortened for reading, not for comparing (#347)', () => {
    const full = 'a'.repeat(64);

    it('shows 128 bits and marks the rest as elided', () => {
        expect(SHORT_HASH_HEX).toBe(32);
        expect(shortHash(full)).toBe(`${'a'.repeat(32)}…`);
    });

    it('leaves anything that is not a long hex digest alone', () => {
        // TLSH signatures and the fuzzy hashes are not all plain hex of that
        // length, and truncating one to a fixed width would misrepresent it.
        expect(shortHash('deadbeef')).toBe('deadbeef');
        expect(shortHash('T1A2B3-not-hex')).toBe('T1A2B3-not-hex');
    });

    it('keeps the full value reachable, and keeps it as the grouping key', () => {
        // Displayed short, stored whole: there is no reason to weaken the key of
        // an operation that deletes files, and the digest is computed in full
        // either way.
        expect(dialogRaw).toMatch(/title=\{rowHash\}/);
        expect(dialogRaw).toMatch(/\{shortHash\(rowHash\)\}/);
        expect(dialogRaw).toMatch(/<RowCopyButton value=\{rowHash\}/);
    });
});

describe('every copy can be ticked (#347)', () => {
    it('no longer disables a checkbox by position', () => {
        const code = dialogRaw.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
        expect(code).not.toMatch(/disabled=\{isFirst\}/);
        expect(code).not.toMatch(/const isFirst = fileIdx === 0/);
    });

    it('confirms the state where a group would lose every copy, rather than blocking it', () => {
        // The guard used to be a hard `disabled` on the Delete button. Combined
        // with Select All, which ticks every copy of every group on purpose, that
        // made the button offer an action the user could never complete: the only
        // feedback was a tooltip on a dead control. The invariant is still
        // watched, but it now costs a confirmation rather than the action.
        expect(dialogRaw).toMatch(/fullyTickedGroups/);
        expect(dialogRaw).toMatch(/disabled=\{selectedCount === 0 \|\| isDeleting\}/);
        expect(dialogRaw).not.toMatch(/disabled=\{[^}]*fullyTickedGroups[^}]*\}/);
    });

    it('names the groups that would lose every copy, instead of only counting them', () => {
        // "3 groups" is not something a user can check against what they ticked.
        const confirm = dialogRaw.slice(dialogRaw.indexOf('pendingDeleteConfirm &&'));
        expect(confirm).toMatch(/fullyTickedGroups\.slice\(0, 5\)\.map/);
        expect(confirm).toMatch(/getFileName\(g\.files\[0\]\)/);
    });

    it('keeps the keep policy out of the scan callback dependencies', () => {
        // The effect that runs `scan` keys off the callback identity so that a
        // mode or threshold change re-scans. Listing `keepPolicy` there would turn
        // a dropdown into a full filesystem walk; a ref gives Retry the current
        // policy without it.
        expect(dialogRaw).toMatch(/keepPolicyRef\.current/);
        const scan = dialogRaw.slice(dialogRaw.indexOf('const scan = useCallback'));
        const deps = scan.slice(0, scan.indexOf('useEffect'));
        expect(deps).not.toMatch(/\[scanPath, mode, appliedThreshold, keepPolicy\]/);
    });

    it('names the size policies after what they order by', () => {
        // They were `oldest`/`newest` while ordering by size, which is what the
        // labels say and what `keeperOf` does. A review of this change read the
        // option values and proposed rewriting four locales to say oldest and
        // newest, which would have made the label disagree with the file kept.
        expect(dialogRaw).toMatch(/<option value="smallest">\{t\('duplicates\.keepSmallest'\)\}/);
        expect(dialogRaw).toMatch(/<option value="largest">\{t\('duplicates\.keepLargest'\)\}/);
        expect(dialogRaw).not.toMatch(/value="oldest"|value="newest"/);
    });

    it('labels the old button for what it does, and adds the one it claimed to be', () => {
        expect(dialogRaw).toMatch(/onClick=\{selectAllButOne\}/);
        expect(dialogRaw).toMatch(/duplicates\.selectAllButOne/);
        expect(dialogRaw).toMatch(/onClick=\{selectAll\}/);
    });
});

describe('rows carry their own size (#347)', () => {
    it('reads the per-file size the backend now sends', () => {
        expect(dialogRaw).toMatch(/group\.file_sizes\?\.\[fileIdx\]/);
        expect(dialogRaw).toMatch(/sortBy === 'sizeSpread'/);
    });

    it('keeps the parallel arrays parallel after a delete', () => {
        // `file_hashes` and `file_sizes` are indexed by position in `files`.
        // Filtering one and not the others shifts every row's hash and size by
        // the number of copies removed above it.
        const after = dialogRaw.slice(dialogRaw.indexOf('const updatedGroups'));
        expect(after.slice(0, 900)).toMatch(/file_hashes: group\.file_hashes \? kept\.map/);
        expect(after.slice(0, 900)).toMatch(/file_sizes: group\.file_sizes \? kept\.map/);
    });
});
