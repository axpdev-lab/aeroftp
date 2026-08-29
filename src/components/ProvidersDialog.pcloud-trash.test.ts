// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

/**
 * #397: pCloud OAuth cannot honour trash endpoints. The GUI hid the button
 * in #642; Help > Providers must not keep advertising the column.
 */
describe('Help > Providers pCloud Drive trash', () => {
    const src = readFileSync(
        join(dirname(fileURLToPath(import.meta.url)), 'ProvidersDialog.tsx'),
        'utf8',
    );

    it('does not list trash on the pCloud Drive row', () => {
        const start = src.indexOf("{ name: 'pCloud Drive'");
        expect(start).toBeGreaterThan(-1);
        const block = src.slice(start, src.indexOf('advanced:', start));
        expect(block).not.toMatch(/'trash'/);
        expect(block).toMatch(/'versioning'/);
    });
});
