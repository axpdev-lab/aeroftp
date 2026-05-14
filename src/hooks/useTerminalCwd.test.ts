// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Pure resolver tests for the terminal cwd binding (Z.3.10).
// The React hook wrapper is exercised live in App.tsx; here we cover the
// resolution matrix that decides which panel the next PTY tab should
// follow.

import { describe, expect, it } from 'vitest';
import { resolveTerminalCwd, type TerminalCwdInputs } from './useTerminalCwd';

const base: TerminalCwdInputs = {
    activePanel: 'local',
    activeLocalPanelId: 'local',
    currentLocalPath: '/home/user',
    currentLocalPath2: '/mnt/scratch',
    isDualLocalAeroFileMode: false,
    isConnectionPresent: false,
    mountpoint: null,
};

describe('resolveTerminalCwd', () => {
    it('returns the primary local path in single-pane mode', () => {
        expect(resolveTerminalCwd(base)).toEqual({
            cwd: '/home/user',
            source: 'local',
        });
    });

    it('returns the secondary local path when dual mode is on and panel2 is focused', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                isDualLocalAeroFileMode: true,
                activeLocalPanelId: 'local2',
            }),
        ).toEqual({ cwd: '/mnt/scratch', source: 'local2' });
    });

    it('keeps the primary local path when dual mode is on but panel1 stays focused', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                isDualLocalAeroFileMode: true,
                activeLocalPanelId: 'local',
            }),
        ).toEqual({ cwd: '/home/user', source: 'local' });
    });

    it('returns the mountpoint when the remote panel is focused and a mount is live', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                activePanel: 'remote',
                isConnectionPresent: true,
                mountpoint: '/mnt/aero/srv_abc',
            }),
        ).toEqual({ cwd: '/mnt/aero/srv_abc', source: 'remote-mount' });
    });

    it('returns an undefined cwd (open home) when remote is focused but unmounted', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                activePanel: 'remote',
                isConnectionPresent: true,
                mountpoint: null,
            }),
        ).toEqual({ cwd: undefined, source: 'remote-unmounted' });
    });

    it('falls back to the local path when remote is focused but we are disconnected', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                activePanel: 'remote',
                isConnectionPresent: false,
                mountpoint: null,
            }),
        ).toEqual({ cwd: '/home/user', source: 'idle' });
    });

    it('collapses empty currentLocalPath to undefined so SSHTerminal opens the home dir', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                currentLocalPath: '',
            }),
        ).toEqual({ cwd: undefined, source: 'local' });
    });

    it('collapses empty currentLocalPath2 to undefined in dual mode panel2', () => {
        expect(
            resolveTerminalCwd({
                ...base,
                isDualLocalAeroFileMode: true,
                activeLocalPanelId: 'local2',
                currentLocalPath2: '',
            }),
        ).toEqual({ cwd: undefined, source: 'local2' });
    });
});
