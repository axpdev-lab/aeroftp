// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for restoring the app preferences carried in a keystore backup: a
// preference that could not be restored must be reported to the caller, not
// swallowed, so the import message can say so.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const autostart = vi.hoisted(() => ({
    enable: vi.fn(async () => {}),
    disable: vi.fn(async () => {}),
}));
vi.mock('@tauri-apps/plugin-autostart', () => autostart);

import { applyLocalStorage } from './keystoreLocalStorage';

/** A `localStorage` that holds `capacity` keys and then throws, as a full quota does. */
const storageWithCapacity = (capacity: number) => {
    const items = new Map<string, string>();
    return {
        items,
        getItem: (key: string) => items.get(key) ?? null,
        setItem: (key: string, value: string) => {
            if (!items.has(key) && items.size >= capacity) {
                throw new DOMException('The quota has been exceeded.', 'QuotaExceededError');
            }
            items.set(key, value);
        },
        removeItem: (key: string) => { items.delete(key); },
    };
};

describe('applyLocalStorage', () => {
    beforeEach(() => {
        autostart.enable.mockReset().mockResolvedValue(undefined);
        autostart.disable.mockReset().mockResolvedValue(undefined);
    });

    afterEach(() => {
        vi.unstubAllGlobals();
    });

    it('restores every whitelisted preference and reports no error', async () => {
        const storage = storageWithCapacity(10);
        vi.stubGlobal('localStorage', storage);

        const result = await applyLocalStorage({ 'aeroftp-theme': 'dark', 'aeroftp-icon-theme': 'mono' });

        expect(result).toEqual({ applied: 2, error: null });
        expect(storage.items.get('aeroftp-theme')).toBe('dark');
    });

    it('reports a full quota instead of swallowing it', async () => {
        vi.stubGlobal('localStorage', storageWithCapacity(1));

        const result = await applyLocalStorage({
            'aeroftp-theme': 'dark',
            'aeroftp-custom-icons': 'x'.repeat(64),
            'aeroftp-icon-theme': 'mono',
        });

        expect(result).toEqual({ applied: 1, error: expect.stringContaining('QuotaExceededError') });
    });

    it('reports a launch-on-startup change that failed', async () => {
        vi.stubGlobal('localStorage', storageWithCapacity(10));
        autostart.enable.mockRejectedValue(new Error('autostart entry not writable'));

        const result = await applyLocalStorage({ _aeroftp_system_autostart: 'true', 'aeroftp-theme': 'dark' });

        expect(result).toEqual({ applied: 1, error: expect.stringContaining('autostart entry not writable') });
    });
});
