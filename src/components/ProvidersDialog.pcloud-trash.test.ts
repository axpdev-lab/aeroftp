// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from 'vitest';

const sources = import.meta.glob('./ProvidersDialog.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * #397: pCloud OAuth cannot honour trash endpoints. The GUI hid the button
 * in #642; Help > Providers must not keep advertising the column. The same
 * refusal on `listrevisions` (measured 2026-11-16) shut the versions gate, so
 * the row lists no versioning either.
 */
describe('Help > Providers pCloud Drive trash and versions', () => {
    const src = Object.values(sources)[0] ?? '';

    it('lists neither trash nor versioning on the pCloud Drive row', () => {
        const start = src.indexOf("{ name: 'pCloud Drive'");
        expect(start).toBeGreaterThan(-1);
        const block = src.slice(start, src.indexOf('advanced:', start));
        expect(block).not.toMatch(/'trash'/);
        expect(block).not.toMatch(/'versioning'/);
        expect(block).toMatch(/'shareLink'/);
    });
});
