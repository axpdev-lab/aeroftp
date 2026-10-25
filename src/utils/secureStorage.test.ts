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
    it('waits for the vault instead of treating STORE_NOT_READY as an absent preference', async () => {
        invoke
            .mockRejectedValueOnce(new Error('STORE_NOT_READY'))
            .mockResolvedValueOnce(JSON.stringify({ compactMode: true }));

        const result = secureGetWithFallback('app_settings', 'aeroftp_settings');
        await vi.advanceTimersByTimeAsync(200);

        await expect(result).resolves.toEqual({ compactMode: true });
        expect(invoke).toHaveBeenCalledTimes(2);
    });

    it('uses the legacy fallback only after the readiness retries are exhausted', async () => {
        invoke.mockRejectedValue(new Error('STORE_NOT_READY'));
        localStorage.setItem('aeroftp_settings', JSON.stringify({ compactMode: true }));

        const result = secureGetWithFallback('app_settings', 'aeroftp_settings');
        await vi.advanceTimersByTimeAsync(3000);

        await expect(result).resolves.toEqual({ compactMode: true });
        expect(invoke).toHaveBeenCalledTimes(6);
    });
});
