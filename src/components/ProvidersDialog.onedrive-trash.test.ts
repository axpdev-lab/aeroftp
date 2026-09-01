// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';

const sources = import.meta.glob('./ProvidersDialog.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * #397: Microsoft Graph does not expose the recycle bin of a OneDrive
 * Personal drive, and the app reports that as a limit. Help > Providers
 * keeps the trash tick for OneDrive, because business drives do expose it,
 * but the tick must carry the caveat and the caveat must reach the tooltip.
 */
describe('Help > Providers OneDrive trash', () => {
    const src = Object.values(sources)[0] ?? '';

    it('keeps trash on the OneDrive row and qualifies it for Personal drives', () => {
        const start = src.indexOf("{ name: 'OneDrive'");
        expect(start).toBeGreaterThan(-1);
        const block = src.slice(start, src.indexOf('advanced:', start));
        expect(block).toMatch(/'trash'/);

        const details = src.indexOf('const FEATURE_DETAILS');
        expect(details).toBeGreaterThan(-1);
        const trash = src.slice(src.indexOf('trash: {', details), src.indexOf('},', src.indexOf('trash: {', details)));
        expect(trash).toMatch(/onedrive:/);
        expect(trash).toMatch(/Personal/);
    });

    it('renders per-feature details on the capability tick, not only on share links', () => {
        expect(src).toMatch(/FEATURE_DETAILS\[f\]\?\.\[provider\.logoId\]/);
    });
});
