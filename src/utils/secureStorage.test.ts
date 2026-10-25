// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { secureGetWithFallback } from './secureStorage';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    localStorage.clear();
});

afterEach(() => vi.useRealTimers());

describe('secureGetWithFallback', () => {
    it('prefers the vault value when the vault is ready', async () => {
        invoke.mockResolvedValue(JSON.stringify({ compactMode: true }));
        localStorage.setItem('aeroftp_settings', JSON.stringify({ compactMode: false }));

        const result = secureGetWithFallback('app_settings', 'aeroftp_settings');
        await vi.advanceTimersByTimeAsync(0);

        await expect(result).resolves.toEqual({ compactMode: true });
        expect(invoke).toHaveBeenCalledTimes(1);
    });

    it('returns the legacy fallback immediately when the vault is not ready', async () => {
        invoke.mockRejectedValue(new Error('STORE_NOT_READY'));
        localStorage.setItem('aeroftp_settings', JSON.stringify({ compactMode: true }));

        let settled = false;
        const result = secureGetWithFallback('app_settings', 'aeroftp_settings').then(value => {
            settled = true;
            return value;
        });
        await vi.advanceTimersByTimeAsync(0);

        expect(settled).toBe(true);
        await expect(result).resolves.toEqual({ compactMode: true });
        expect(invoke).toHaveBeenCalledTimes(1);
    });
});
