// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { LocalFile } from '../types';
import { selectedLocalRows } from './localSelection';

const row = (name: string, is_dir = true): LocalFile => ({
    name, path: `/home/u/${name}`, size: is_dir ? null : 1, is_dir, modified: null,
});

describe('the rows a local selection names', () => {
    it('finds a nested row of a flattened search, which the listing does not hold', () => {
        const listing = [row('sub'), row('a.txt', false)];
        const shown = [row('sub'), row('sub/inner'), row('a.txt', false)];
        expect(selectedLocalRows(shown, listing, new Set(['sub/inner'])).map(f => f.path))
            .toEqual(['/home/u/sub/inner']);
    });

    it('keeps a selected entry the rows no longer show', () => {
        const listing = [row('docs'), row('music')];
        const shown = [row('docs')];
        expect(selectedLocalRows(shown, listing, new Set(['docs', 'music'])).map(f => f.name))
            .toEqual(['docs', 'music']);
    });

    it('gives each name once, in the order the panel shows it', () => {
        const listing = [row('a'), row('b'), row('c')];
        const shown = [row('c'), row('a'), row('b')];
        expect(selectedLocalRows(shown, listing, new Set(['a', 'c'])).map(f => f.name))
            .toEqual(['c', 'a']);
    });

    it('gives nothing for a name neither holds', () => {
        expect(selectedLocalRows([row('a')], [row('a')], new Set(['gone']))).toEqual([]);
    });
});
