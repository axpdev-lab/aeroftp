// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import README from '../README.md?raw';
import { PROVIDER_GRID } from './components/providerCatalog';

/** Every provider guide the README links to, as `providers/<slug>`. */
const readmeGuides = (): string[] =>
    [...README.matchAll(/https:\/\/docs\.aeroftp\.app\/(providers\/[a-z0-9-]+)/g)].map(match => match[1]);

describe('the README provider links', () => {
    // A README link is a page the app itself links to from its provider grid,
    // so the two can never drift apart: a guide the docs site publishes for
    // the app is the guide the README opens.
    it('open the same guide the app opens for that provider', () => {
        const grid = new Set(PROVIDER_GRID.map(tile => tile.docsPath));
        const guides = readmeGuides();
        expect(guides.length).toBeGreaterThan(0);
        expect(guides.filter(guide => !grid.has(guide))).toEqual([]);
    });

    it('link the Neon and Puter guides', () => {
        const guides = readmeGuides();
        for (const [logoId, guide] of [['neon', 'providers/neon'], ['puter', 'providers/puter']] as const) {
            expect(guides, `README links ${guide}`).toContain(guide);
            expect(PROVIDER_GRID.find(tile => tile.logoId === logoId)?.docsPath).toBe(guide);
        }
    });
});
