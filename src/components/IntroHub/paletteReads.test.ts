// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ accountChanges: 0 }));
vi.mock('../../utils/userPartitions', () => ({
    accountChangeGeneration: () => state.accountChanges,
}));

import { PaletteReadGuard } from './paletteReads';

describe('PaletteReadGuard', () => {
    beforeEach(() => {
        state.accountChanges = 0;
    });

    it('only the latest read is current', () => {
        const guard = new PaletteReadGuard();
        const first = guard.begin();
        const second = guard.begin();
        expect(guard.isCurrent(first)).toBe(false);
        expect(guard.isCurrent(second)).toBe(true);
    });

    it('an account switch while a read is pending makes that read stale', () => {
        const guard = new PaletteReadGuard();
        const token = guard.begin();
        expect(guard.isCurrent(token)).toBe(true);
        state.accountChanges += 1; // a switch/lock started and settled
        expect(guard.isCurrent(token)).toBe(false);
    });

    it('an import (writer) invalidates a read that is still pending', () => {
        const guard = new PaletteReadGuard();
        const token = guard.begin();
        guard.invalidate();
        expect(guard.isCurrent(token)).toBe(false);
        const afterImport = guard.begin();
        expect(guard.isCurrent(afterImport)).toBe(true);
    });

    it('a read pending across an import and a switch commits nothing stale', () => {
        const guard = new PaletteReadGuard();
        const pending = guard.begin();
        guard.invalidate(); // the import writer landed first
        state.accountChanges += 2; // then an account switch landed
        expect(guard.isCurrent(pending)).toBe(false);
        const fresh = guard.begin();
        expect(guard.isCurrent(fresh)).toBe(true);
    });

    it('discards a stale palette read when the account switches mid-read (same profile id in both accounts)', () => {
        // Two accounts that share the same profile id, like a duplicated lab
        // server saved under both.
        const listsByAccount: Record<number, { profiles: { id: string; name: string }[]; userId: number }> = {
            1: { profiles: [{ id: 'srv-shared', name: 'alice-lab' }], userId: 1 },
            2: { profiles: [{ id: 'srv-shared', name: 'bob-lab' }], userId: 2 },
        };
        let activeAccount = 1;
        const loadSavedServerProfilesWithAccount = () =>
            Promise.resolve(listsByAccount[activeAccount]);

        // The reloadPalette commit pattern: begin, await the read, commit only
        // if the token is still current.
        const guard = new PaletteReadGuard();
        let committed: { profiles: unknown; accountId: number | null } | null = null;
        const reload = async () => {
            const token = guard.begin();
            const read = await loadSavedServerProfilesWithAccount();
            if (!guard.isCurrent(token)) return; // stale: never reaches the export dialog
            committed = { profiles: read.profiles, accountId: read.userId ?? null };
        };

        // A read starts under account 1, the account switches to 2 mid-flight,
        // and only then the stale read resolves.
        const pendingRead = reload();
        activeAccount = 2;
        state.accountChanges += 2; // the switch started and settled
        return pendingRead.then(async () => {
            expect(committed).toBeNull();
            // The re-read after the switch commits the NEW account's list+id.
            await reload();
            expect(committed).toEqual({
                profiles: [{ id: 'srv-shared', name: 'bob-lab' }],
                accountId: 2,
            });
        });
    });
});
