// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { deriveBridgeUiState, singleFlight, type BridgeStatus } from './useBridgeStatus';

// #215 follow-up: the 🔴/🟠/🟢 mapping that drives the dot, the app-aware
// message, and the Save gate. Owner decision: hard-block Save only on 🔴.
describe('deriveBridgeUiState', () => {
    const mk = (p: Partial<BridgeStatus>): BridgeStatus => ({
        installed: false,
        active: false,
        port: 1900,
        install_known: true,
        ...p,
    });

    it('active bridge -> green (regardless of install detection)', () => {
        expect(deriveBridgeUiState(mk({ active: true, installed: false, install_known: false }))).toBe('green');
        expect(deriveBridgeUiState(mk({ active: true, installed: true }))).toBe('green');
    });

    it('confidently not installed -> red (the only Save-blocking state)', () => {
        expect(deriveBridgeUiState(mk({ active: false, installed: false, install_known: true }))).toBe('red');
    });

    it('installed but not active -> amber', () => {
        expect(deriveBridgeUiState(mk({ active: false, installed: true, install_known: true }))).toBe('amber');
    });

    it('install unknown and not active -> amber (never red, so Save is not blocked)', () => {
        // The Filen-Desktop-not-running case: we cannot tell if it is installed.
        expect(deriveBridgeUiState(mk({ active: false, installed: true, install_known: false }))).toBe('amber');
        expect(deriveBridgeUiState(mk({ active: false, installed: false, install_known: false }))).toBe('amber');
    });

    it('null probe (in flight / failed) -> amber, never blocks on doubt', () => {
        expect(deriveBridgeUiState(null)).toBe('amber');
    });
});

// The Proton CLI probe runs a process that can outlast the 4 s poll: a poll or a
// manual refresh arriving meanwhile must join it, not start a second CLI run.
describe('singleFlight', () => {
    it('joins a call made while the previous one is running, and runs again after', async () => {
        let runs = 0;
        let release: () => void = () => {};
        const probe = singleFlight(
            () =>
                new Promise<void>((resolve) => {
                    runs += 1;
                    release = resolve;
                }),
        );
        const first = probe();
        const second = probe();
        expect(runs).toBe(1);
        expect(second).toBe(first);
        release();
        await first;
        const third = probe();
        expect(runs).toBe(2);
        release();
        await third;
    });

    it('starts a new run once the previous one has finished, also after a failure', async () => {
        let runs = 0;
        const probe = singleFlight(async () => {
            runs += 1;
            if (runs === 1) throw new Error('cli timed out');
        });
        await expect(probe()).rejects.toThrow('cli timed out');
        await probe();
        expect(runs).toBe(2);
    });
});
