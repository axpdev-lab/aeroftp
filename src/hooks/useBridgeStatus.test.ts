// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { deriveBridgeUiState, type BridgeStatus } from './useBridgeStatus';

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
