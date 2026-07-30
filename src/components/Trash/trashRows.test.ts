// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    nextTrashSort,
    parseDeletedAt,
    selectionAfterRowClick,
    sortTrashRows,
    type TrashRow,
} from './trashRows';

const row = (over: Partial<TrashRow> & { id: string }): TrashRow => ({
    name: over.id,
    isDir: false,
    size: 0,
    deletedAt: null,
    ...over,
});

const names = (rows: TrashRow[]) => rows.map((r) => r.name);

describe('sortTrashRows', () => {
    const rows: TrashRow[] = [
        row({ id: 'b', name: 'beta.txt', size: 300, deletedAt: '2026-07-02T10:00:00Z' }),
        row({ id: 'd', name: 'docs', isDir: true, deletedAt: '2026-07-03T10:00:00Z' }),
        row({ id: 'a', name: 'alpha.txt', size: 100, deletedAt: '2026-07-01T10:00:00Z' }),
        row({ id: 'c', name: 'gamma.txt', size: 200, deletedAt: null }),
    ];

    it('leaves the provider order alone until a column is picked', () => {
        expect(names(sortTrashRows(rows, null))).toEqual(['beta.txt', 'docs', 'alpha.txt', 'gamma.txt']);
    });

    it('does not mutate the input', () => {
        const before = names(rows);
        sortTrashRows(rows, { key: 'name', direction: 'desc' });
        expect(names(rows)).toEqual(before);
    });

    it('sorts by name in both directions, numerically aware', () => {
        expect(names(sortTrashRows(rows, { key: 'name', direction: 'asc' })))
            .toEqual(['alpha.txt', 'beta.txt', 'docs', 'gamma.txt']);
        expect(names(sortTrashRows(rows, { key: 'name', direction: 'desc' })))
            .toEqual(['gamma.txt', 'docs', 'beta.txt', 'alpha.txt']);

        const numeric = [row({ id: '10', name: 'shot10.png' }), row({ id: '2', name: 'shot2.png' })];
        expect(names(sortTrashRows(numeric, { key: 'name', direction: 'asc' })))
            .toEqual(['shot2.png', 'shot10.png']);
    });

    it('groups folders and files by type, each group by name', () => {
        expect(names(sortTrashRows(rows, { key: 'type', direction: 'asc' })))
            .toEqual(['docs', 'alpha.txt', 'beta.txt', 'gamma.txt']);
        expect(names(sortTrashRows(rows, { key: 'type', direction: 'desc' })))
            .toEqual(['alpha.txt', 'beta.txt', 'gamma.txt', 'docs']);
    });

    it('sorts by size and keeps folders out of the ranking', () => {
        // A folder is not zero bytes, it has no size of its own: it lands last
        // whichever way the column points.
        expect(names(sortTrashRows(rows, { key: 'size', direction: 'asc' })))
            .toEqual(['alpha.txt', 'gamma.txt', 'beta.txt', 'docs']);
        expect(names(sortTrashRows(rows, { key: 'size', direction: 'desc' })))
            .toEqual(['beta.txt', 'gamma.txt', 'alpha.txt', 'docs']);
    });

    it('sorts by deletion date and puts unknown dates last both ways', () => {
        expect(names(sortTrashRows(rows, { key: 'deletedAt', direction: 'asc' })))
            .toEqual(['alpha.txt', 'beta.txt', 'docs', 'gamma.txt']);
        expect(names(sortTrashRows(rows, { key: 'deletedAt', direction: 'desc' })))
            .toEqual(['docs', 'beta.txt', 'alpha.txt', 'gamma.txt']);
    });

    it('breaks every tie on the name, so the order is stable', () => {
        const tied = [
            row({ id: '1', name: 'zulu.txt', size: 10, deletedAt: '2026-07-01T00:00:00Z' }),
            row({ id: '2', name: 'alpha.txt', size: 10, deletedAt: '2026-07-01T00:00:00Z' }),
        ];
        expect(names(sortTrashRows(tied, { key: 'size', direction: 'desc' })))
            .toEqual(['alpha.txt', 'zulu.txt']);
        expect(names(sortTrashRows(tied, { key: 'deletedAt', direction: 'desc' })))
            .toEqual(['alpha.txt', 'zulu.txt']);
    });
});

describe('parseDeletedAt', () => {
    it('accepts what providers actually send, and refuses the rest', () => {
        expect(parseDeletedAt('2026-07-30T12:00:00Z')).toBe(Date.parse('2026-07-30T12:00:00Z'));
        expect(parseDeletedAt(null)).toBeNull();
        expect(parseDeletedAt('')).toBeNull();
        expect(parseDeletedAt('yesterday')).toBeNull();
    });
});

describe('nextTrashSort', () => {
    it('opens name ascending and size/date descending', () => {
        expect(nextTrashSort(null, 'name')).toEqual({ key: 'name', direction: 'asc' });
        expect(nextTrashSort(null, 'type')).toEqual({ key: 'type', direction: 'asc' });
        expect(nextTrashSort(null, 'size')).toEqual({ key: 'size', direction: 'desc' });
        expect(nextTrashSort(null, 'deletedAt')).toEqual({ key: 'deletedAt', direction: 'desc' });
    });

    it('flips the direction of the column that is already active', () => {
        expect(nextTrashSort({ key: 'name', direction: 'asc' }, 'name'))
            .toEqual({ key: 'name', direction: 'desc' });
        expect(nextTrashSort({ key: 'name', direction: 'desc' }, 'name'))
            .toEqual({ key: 'name', direction: 'asc' });
    });

    it('starts fresh when a different column is picked', () => {
        expect(nextTrashSort({ key: 'name', direction: 'desc' }, 'size'))
            .toEqual({ key: 'size', direction: 'desc' });
    });
});

describe('selectionAfterRowClick', () => {
    const rows = ['a', 'b', 'c', 'd', 'e'].map((id) => row({ id }));

    it('toggles one row and keeps the others, which is what Ctrl is for', () => {
        const first = selectionAfterRowClick(rows, 1, new Set(), null);
        expect([...first.selected]).toEqual(['b']);
        const second = selectionAfterRowClick(rows, 3, first.selected, first.anchor);
        expect([...second.selected].sort()).toEqual(['b', 'd']);
        const untick = selectionAfterRowClick(rows, 1, second.selected, second.anchor);
        expect([...untick.selected]).toEqual(['d']);
    });

    it('extends a range with shift, without dropping earlier picks', () => {
        const anchored = selectionAfterRowClick(rows, 1, new Set(), null);
        const ranged = selectionAfterRowClick(rows, 3, anchored.selected, anchored.anchor, { shift: true });
        expect([...ranged.selected].sort()).toEqual(['b', 'c', 'd']);
    });

    it('extends backwards too', () => {
        const anchored = selectionAfterRowClick(rows, 3, new Set(), null);
        const ranged = selectionAfterRowClick(rows, 1, anchored.selected, anchored.anchor, { shift: true });
        expect([...ranged.selected].sort()).toEqual(['b', 'c', 'd']);
    });

    it('keeps the anchor so a second shift-click re-extends from the same origin', () => {
        const anchored = selectionAfterRowClick(rows, 1, new Set(), null);
        const wide = selectionAfterRowClick(rows, 4, anchored.selected, anchored.anchor, { shift: true });
        expect(wide.anchor).toBe(1);
        const narrowed = selectionAfterRowClick(rows, 2, wide.selected, wide.anchor, { shift: true });
        // Still anchored at b: the range is b..c, added to what was there.
        expect(narrowed.anchor).toBe(1);
        expect([...narrowed.selected].sort()).toEqual(['b', 'c', 'd', 'e']);
    });

    it('lets several disjoint ranges be picked in turn', () => {
        let state = selectionAfterRowClick(rows, 0, new Set(), null);
        state = selectionAfterRowClick(rows, 1, state.selected, state.anchor, { shift: true });
        state = selectionAfterRowClick(rows, 3, state.selected, state.anchor);
        state = selectionAfterRowClick(rows, 4, state.selected, state.anchor, { shift: true });
        expect([...state.selected].sort()).toEqual(['a', 'b', 'd', 'e']);
    });

    it('falls back to a plain toggle when shift is held with no anchor', () => {
        const state = selectionAfterRowClick(rows, 2, new Set(), null, { shift: true });
        expect([...state.selected]).toEqual(['c']);
        expect(state.anchor).toBe(2);
    });

    it('ignores a click on an index that is not there', () => {
        const state = selectionAfterRowClick(rows, 99, new Set(['a']), 0);
        expect([...state.selected]).toEqual(['a']);
    });
});
