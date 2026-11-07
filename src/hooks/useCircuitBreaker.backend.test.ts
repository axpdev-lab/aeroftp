// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// #1506 row 164 (CodeRabbit, #1520): the breaker acts on the backend's
// verdict, not on the English copy, and decides failures in the order they
// were recorded.

import { describe, expect, it, vi } from 'vitest';

vi.mock('react', async importOriginal => ({
    ...(await importOriginal<typeof import('react')>()),
    useRef: <T,>(value: T) => ({ current: value }),
    useCallback: <T,>(fn: T) => fn,
}));

const verdicts: Array<{ kind: string; retryable: boolean; delayMs: number }> = [];
vi.mock('@tauri-apps/api/core', () => ({
    invoke: vi.fn(async (cmd: string) => {
        if (cmd !== 'classify_transfer_error') throw new Error(`unexpected ${cmd}`);
        const next = verdicts.shift()!;
        await new Promise(resolve => setTimeout(resolve, next.delayMs));
        return { kind: next.kind, message: 'x', retryable: next.retryable, file_path: null };
    }),
}));

import { useCircuitBreaker, DEFAULT_CONFIG } from './useCircuitBreaker';

describe('useCircuitBreaker with the backend classifier', () => {
    it('trips on a fatal backend verdict the English copy would miss', async () => {
        verdicts.push({ kind: 'disk_error', retryable: false, delayMs: 0 });
        const breaker = useCircuitBreaker(DEFAULT_CONFIG);
        const result = await breaker.recordFailure('Spazio su disco insufficiente. (os error 112)');
        expect(result).toMatchObject({ isFatal: true, shouldPause: true, errorKind: 'disk_error' });
        expect(breaker.stateRef.current).toBe('open');
    });

    it('applies two failures in the order they were recorded, a slow first verdict included', async () => {
        verdicts.push({ kind: 'disk_error', retryable: false, delayMs: 30 });
        verdicts.push({ kind: 'auth', retryable: false, delayMs: 0 });
        const breaker = useCircuitBreaker(DEFAULT_CONFIG);
        const first = breaker.recordFailure('full disk');
        const second = breaker.recordFailure('expired login');
        const [a, b] = await Promise.all([first, second]);
        expect(a.errorKind).toBe('disk_error');
        expect(b.errorKind).toBe('auth');
        // The failure recorded last is the trip the breaker shows. Decided
        // out of order, the slow first verdict landed last and overwrote it.
        expect(breaker.tripErrorKindRef.current).toBe('auth');
    });
});
