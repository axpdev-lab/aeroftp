// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the vault-aware sidebar filter rule (#311 D3).

import { describe, expect, it } from 'vitest';
import { visibleQuickFilters, visibleProtocolFilters } from './sidebarFilters';

describe('visibleQuickFilters', () => {
    it('always pins `all`, even on an empty vault', () => {
        expect(visibleQuickFilters({})).toEqual(['all']);
    });

    it('shows favorites/encrypted only when their count is non-zero', () => {
        expect(visibleQuickFilters({ favorites: 0, encrypted: 3 })).toEqual(['all', 'encrypted']);
        expect(visibleQuickFilters({ favorites: 2, encrypted: 1 })).toEqual(['all', 'favorites', 'encrypted']);
    });
});

describe('visibleProtocolFilters', () => {
    it('hides every empty protocol chip', () => {
        expect(visibleProtocolFilters({})).toEqual([]);
    });

    it('keeps only the non-empty protocol chips in canonical order', () => {
        expect(visibleProtocolFilters({ ftp: 2, webdav: 0, cloud: 5 })).toEqual(['ftp', 'cloud']);
    });
});
