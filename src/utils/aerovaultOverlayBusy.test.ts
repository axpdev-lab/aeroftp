// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the AeroVault overlay busy-lock wrapper (Z.3.6).
// Verifies acquire/release pairing including the error-path release
// guarantee.

import { describe, expect, it, vi, beforeEach } from 'vitest';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import { withOverlayBusyLock } from './aerovaultOverlayBusy';

describe('withOverlayBusyLock', () => {
    beforeEach(() => {
        mockInvoke.mockReset();
    });

    it('acquires before fn, releases after, returns fn result', async () => {
        mockInvoke
            .mockResolvedValueOnce(1) // acquire
            .mockResolvedValueOnce(0); // release
        const fn = vi.fn().mockResolvedValue('payload');

        const result = await withOverlayBusyLock('avol_test', fn);

        expect(result).toBe('payload');
        expect(mockInvoke).toHaveBeenNthCalledWith(1, 'aerovault_overlay_busy_acquire', { sessionId: 'avol_test' });
        expect(mockInvoke).toHaveBeenNthCalledWith(2, 'aerovault_overlay_busy_release', { sessionId: 'avol_test' });
        expect(fn).toHaveBeenCalledOnce();
    });

    it('releases the lock even when fn throws', async () => {
        mockInvoke
            .mockResolvedValueOnce(1) // acquire
            .mockResolvedValueOnce(0); // release
        const boom = new Error('transfer failed');
        const fn = vi.fn().mockRejectedValue(boom);

        await expect(withOverlayBusyLock('avol_test', fn)).rejects.toBe(boom);

        // Both acquire and release must have run.
        expect(mockInvoke).toHaveBeenCalledTimes(2);
        expect(mockInvoke).toHaveBeenNthCalledWith(2, 'aerovault_overlay_busy_release', { sessionId: 'avol_test' });
    });

    it('swallows release errors so transfer success is not masked', async () => {
        mockInvoke
            .mockResolvedValueOnce(1) // acquire ok
            .mockRejectedValueOnce(new Error('release blew up'));
        const fn = vi.fn().mockResolvedValue(42);

        const result = await withOverlayBusyLock('avol_test', fn);

        expect(result).toBe(42);
    });

    it('propagates acquire failures and never calls fn or release', async () => {
        mockInvoke.mockRejectedValueOnce(new Error('session evicted'));
        const fn = vi.fn();

        await expect(withOverlayBusyLock('missing', fn)).rejects.toThrow('session evicted');
        expect(fn).not.toHaveBeenCalled();
        // Only the acquire call landed; release was never attempted.
        expect(mockInvoke).toHaveBeenCalledTimes(1);
    });
});
