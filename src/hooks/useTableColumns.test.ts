// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Pure helpers extracted from useTableColumns. The React hook itself runs
// inside the components and is exercised live in the UI; these tests cover
// the data-shape contract: sanitization, ordering, width clamping, pinning.

import { describe, expect, it } from 'vitest';
import { __TEST_ONLY__, type TableColumnDef } from './useTableColumns';

const {
    sanitizeOrder,
    sanitizeWidths,
    sanitizeVisibility,
    sanitizeSort,
    sanitizeConfig,
    computeOrderedColumns,
    buildDefaults,
} = __TEST_ONLY__;

type ColId = 'a' | 'b' | 'c' | 'd';

const cols: TableColumnDef<ColId>[] = [
    { id: 'a', labelKey: 'a', sortable: true, defaultVisible: true, defaultWidth: 100, minWidth: 50, pinnedStart: true },
    { id: 'b', labelKey: 'b', sortable: true, defaultVisible: true, defaultWidth: 120, minWidth: 60 },
    { id: 'c', labelKey: 'c', sortable: false, defaultVisible: false, defaultWidth: 80, minWidth: 40 },
    { id: 'd', labelKey: 'd', sortable: true, defaultVisible: true, defaultWidth: 60, minWidth: 30, pinnedEnd: true },
];

const SORTABLE: ColId[] = ['a', 'b', 'd'];

describe('useTableColumns helpers', () => {
    it('buildDefaults respects defaultVisible/defaultWidth and registry order', () => {
        const def = buildDefaults<ColId>(cols, undefined);
        expect(def.visibility).toEqual({ a: true, b: true, c: false, d: true });
        expect(def.widths).toEqual({ a: 100, b: 120, c: 80, d: 60 });
        expect(def.order).toEqual(['a', 'b', 'c', 'd']);
        expect(def.sort).toBeNull();
    });

    it('sanitizeVisibility merges with fallback and ignores foreign keys', () => {
        const fallback = { a: true, b: true, c: false, d: true } as Record<ColId, boolean>;
        const result = sanitizeVisibility<ColId>(
            { a: false, c: true, foreign: true } as unknown,
            fallback,
        );
        expect(result).toEqual({ a: false, b: true, c: true, d: true });
    });

    it('sanitizeOrder filters unknown ids and appends missing knowns at the end', () => {
        const known: ColId[] = ['a', 'b', 'c', 'd'];
        const result = sanitizeOrder<ColId>(['c', 'evil', 'a', 'b'], known);
        // unknown stripped, missing 'd' appended
        expect(result).toEqual(['c', 'a', 'b', 'd']);
    });

    it('sanitizeOrder rejects duplicates within the persisted blob', () => {
        const known: ColId[] = ['a', 'b', 'c', 'd'];
        const result = sanitizeOrder<ColId>(['a', 'a', 'b'], known);
        expect(result).toEqual(['a', 'b', 'c', 'd']);
    });

    it('sanitizeWidths clamps to minWidth and floors decimals', () => {
        const fallback = { a: 100, b: 120, c: 80, d: 60 } as Record<ColId, number>;
        const result = sanitizeWidths<ColId>({ a: 10, b: 200.7, c: -5, d: 0 } as unknown, cols, fallback);
        // a clamped to min (50); b floored to 200; c invalid (negative) → fallback; d invalid (0) → fallback
        expect(result.a).toBe(50);
        expect(result.b).toBe(200);
        expect(result.c).toBe(80);
        expect(result.d).toBe(60);
    });

    it('sanitizeSort rejects unknown col ids and bad directions', () => {
        const sortable = new Set<ColId>(SORTABLE);
        expect(sanitizeSort({ colId: 'a', dir: 'asc' }, sortable)).toEqual({ colId: 'a', dir: 'asc' });
        expect(sanitizeSort({ colId: 'c', dir: 'asc' }, sortable)).toBeNull();   // 'c' not in sortable
        expect(sanitizeSort({ colId: 'a', dir: 'sideways' }, sortable)).toBeNull();
        expect(sanitizeSort({ colId: 'evil', dir: 'asc' }, sortable)).toBeNull();
        expect(sanitizeSort(null, sortable)).toBeNull();
    });

    it('sanitizeConfig produces a fully-populated, defensive config from a partial blob', () => {
        const blob = { visibility: { c: true }, order: ['evil', 'b'], widths: { b: 1000 }, sort: { colId: 'd', dir: 'desc' } };
        const config = sanitizeConfig<ColId>(blob, cols, undefined, SORTABLE);
        expect(config.visibility.c).toBe(true);
        expect(config.visibility.a).toBe(true);                         // fallback retained
        expect(config.order[0]).toBe('b');                              // 'evil' stripped
        expect(config.order).toContain('a');                            // a/c/d appended
        expect(config.widths.b).toBe(1000);                             // honoured
        expect(config.widths.a).toBe(100);                              // fallback
        expect(config.sort).toEqual({ colId: 'd', dir: 'desc' });
    });

    it('computeOrderedColumns puts pinnedStart first and pinnedEnd last regardless of order blob', () => {
        // user "moved" d before a — should be ignored: a stays start-pinned, d stays end-pinned
        const order: ColId[] = ['d', 'c', 'b', 'a'];
        const visibility: Record<ColId, boolean> = { a: true, b: true, c: true, d: true };
        const result = computeOrderedColumns(cols, order, visibility).map(c => c.id);
        expect(result).toEqual(['a', 'c', 'b', 'd']);
    });

    it('computeOrderedColumns filters by visibility but preserves the relative order', () => {
        const order: ColId[] = ['a', 'c', 'b', 'd'];
        const visibility: Record<ColId, boolean> = { a: true, b: false, c: true, d: true };
        const result = computeOrderedColumns(cols, order, visibility).map(c => c.id);
        expect(result).toEqual(['a', 'c', 'd']);
    });

    it('computeOrderedColumns handles a null visibility filter (manager popover use case)', () => {
        const order: ColId[] = ['a', 'b', 'c', 'd'];
        const result = computeOrderedColumns(cols, order, null).map(c => c.id);
        expect(result).toEqual(['a', 'b', 'c', 'd']);
    });

    it('overrideDefaultVisibility hook (compact ↔ detailed) flips defaults at build time', () => {
        const detailedOnly: TableColumnDef<ColId>[] = [
            { id: 'a', labelKey: 'a', sortable: true, defaultVisible: true, defaultWidth: 100, pinnedStart: true },
            { id: 'b', labelKey: 'b', sortable: true, defaultVisible: true, defaultWidth: 120 },
        ];
        const override = (id: ColId) => id === 'b' ? false : true;
        const def = buildDefaults<ColId>(detailedOnly as unknown as TableColumnDef<ColId>[], override);
        expect(def.visibility.a).toBe(true);
        expect(def.visibility.b).toBe(false);
    });
});
