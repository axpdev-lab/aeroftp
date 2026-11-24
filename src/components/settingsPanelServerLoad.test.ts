// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Review round 6: the mount-time server-list read in SettingsPanel must not
 * abort the whole settings init when the vault is locked (STORE_NOT_READY /
 * USER_LOCKED). The panel is 4000 lines over some 60 modules, too many to
 * mock for one load, so like settingsPanelEscape.test.ts this test pins the
 * wiring from SettingsPanel.tsx's source: the server load is wrapped in its
 * own try/catch that leaves servers empty, and the vault/account status load
 * still runs after it in the same init block.
 */
const source = (): string => readFileSync(resolve(process.cwd(), 'src/components/SettingsPanel.tsx'), 'utf8');

const serverLoadBlock = (): string => {
    const src = source();
    const start = src.indexOf('const vaultRead = await loadSavedServerProfilesWithAccount();');
    expect(start).toBeGreaterThan(-1);
    // Walk back to the opening of the try that wraps the server load.
    const tryAt = src.lastIndexOf('try {', start);
    expect(tryAt).toBeGreaterThan(-1);
    const end = src.indexOf('get_credential_store_status', start);
    expect(end).toBeGreaterThan(start);
    return src.slice(tryAt, end);
};

describe('the Settings panel mount-time server load', () => {
    it('is wrapped in its own try/catch that leaves servers empty', () => {
        const block = serverLoadBlock();
        expect(block).toMatch(/try \{\s*const vaultRead = await loadSavedServerProfilesWithAccount\(\);/);
        expect(block).toMatch(/catch \{[^}]*setServers\(\[\]\)[^}]*setServersAccountId\(null\)/s);
    });

    it('does not guard the vault/account status load, which still runs after it', () => {
        const src = source();
        const loadAt = src.indexOf('const vaultRead = await loadSavedServerProfilesWithAccount();');
        expect(loadAt).toBeGreaterThan(-1);
        const catchAt = src.indexOf('} catch {', loadAt);
        const statusAt = src.indexOf("'get_credential_store_status'", loadAt);
        // The status invoke comes after the server load's own catch in the same
        // init, so a locked vault still reports its status instead of aborting.
        expect(catchAt).toBeGreaterThan(loadAt);
        expect(statusAt).toBeGreaterThan(catchAt);
    });
});
