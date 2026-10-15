// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { quickLookStartIndex, quickLookView } from './quickLookPanel';

const entry = (name: string, is_dir = false) => ({ name, is_dir });
const files = [entry('src', true), entry('a.txt'), entry('b.txt')];

describe('quickLookStartIndex', () => {
    it('opens on the single selected file, including the first row', () => {
        expect(quickLookStartIndex(files, new Set(['b.txt']))).toBe(2);
        expect(quickLookStartIndex([entry('only.txt')], new Set(['only.txt']))).toBe(0);
    });

    it('does not open on a folder, a missing name, or zero or several selected', () => {
        expect(quickLookStartIndex(files, new Set(['src']))).toBeNull();
        expect(quickLookStartIndex(files, new Set(['gone.txt']))).toBeNull();
        expect(quickLookStartIndex(files, new Set(['a.txt', 'b.txt']))).toBeNull();
        expect(quickLookStartIndex(files, new Set())).toBeNull();
    });
});

describe('quickLookView', () => {
    it('reads the file, its siblings and the folder from the panel it is given', () => {
        const view = quickLookView({ sortedFiles: files, currentPath: '/right', setSelection: vi.fn() }, 1);
        expect(view?.file.name).toBe('a.txt');
        expect(view?.allFiles).toBe(files);
        expect(view?.currentPath).toBe('/right');
    });

    it('shows nothing once the index falls outside that panel list', () => {
        expect(quickLookView({ sortedFiles: files, currentPath: '/right', setSelection: vi.fn() }, 3)).toBeNull();
    });

    it('moves the selection of that panel when navigating', () => {
        const setSelection = vi.fn();
        quickLookView({ sortedFiles: files, currentPath: '/right', setSelection }, 1)!.select(2);
        expect(setSelection).toHaveBeenCalledWith(new Set(['b.txt']));
    });
});
