// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import panelRaw from './DiscoverPanel.tsx?raw';
import tableRaw from './CatalogTable.tsx?raw';
import hubRaw from './IntroHub.tsx?raw';

/**
 * Add Service kept two of everything: the grid and the table each rendered
 * their own search box, backed by their own state, and the tier filter existed
 * only inside the table. Typing a term in one view and switching lost it, the
 * box moved, and the filters vanished (Ehud, #347).
 *
 * The shape of the fix is "shared chrome, one body per view", and that is what
 * these check — the arrangement is the fix, so a future edit that re-adds a
 * second copy is what has to fail here.
 */
describe('Add Service: grid and table share their controls (#347)', () => {
    it('keeps a single search string in the parent', () => {
        // Two states was the actual defect: `discoverGridQuery` and
        // `discoverListQuery` could not agree by construction.
        expect(hubRaw).toContain('const [discoverQuery, setDiscoverQuery]');
        expect(hubRaw).not.toContain('discoverGridQuery');
        expect(hubRaw).not.toContain('discoverListQuery');
    });

    it('passes that one string down, with no per-view variant', () => {
        expect(panelRaw).toContain('query: string;');
        expect(panelRaw).not.toContain('gridQuery');
        expect(panelRaw).not.toContain('listQuery');
    });

    it('renders the search box once, outside the view switch', () => {
        // One <SearchBox> in the panel and none in the table: two would mean the
        // element differs between views again, which is what moved it.
        expect((panelRaw.match(/<SearchBox/g) ?? []).length).toBe(1);
        expect(tableRaw).not.toContain('<SearchBox');
        // It must sit before the branch, otherwise it is inside one arm of it.
        // Anchored on the JSX branch specifically: `viewMode === 'list' ?` also
        // appears earlier as a plain expression when counting the header.
        const branch = panelRaw.indexOf("{viewMode === 'list' ? (");
        expect(branch).toBeGreaterThan(-1);
        expect(panelRaw.indexOf('<SearchBox')).toBeLessThan(branch);
    });

    it('renders the tier filter once, in the panel rather than the table', () => {
        expect(panelRaw).toContain('TIER_FILTERS.map');
        expect(tableRaw).not.toContain('TIER_FILTERS.map');
        // The table still filters by it; it just no longer owns it.
        expect(tableRaw).toContain('tierFilter: TierFilter;');
        expect(tableRaw).not.toContain('useState<TierFilter>');
    });

    it('applies the tier filter to the grid too, not only to the table', () => {
        // The filter used to live in the table, so the grid ignored it entirely.
        expect(panelRaw).toContain('companyTierInCategory(company, activeCategory)');
    });

    it('shows the custom/generic servers and the category banner in both views', () => {
        // Both used to be inside one arm of the switch: custom servers under the
        // table only, the banner under the grid only.
        expect((panelRaw.match(/CUSTOM_PROFILES\.filter/g) ?? []).length).toBe(1);
        expect(panelRaw).not.toContain("viewMode === 'grid' && activeCategory !== 'all'");
        const customIdx = panelRaw.indexOf('CUSTOM_PROFILES.filter');
        const switchEnd = panelRaw.lastIndexOf('                )}');
        expect(customIdx).toBeGreaterThan(switchEnd);
    });
});
