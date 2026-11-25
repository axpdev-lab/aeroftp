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
});
