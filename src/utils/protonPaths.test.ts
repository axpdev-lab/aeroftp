// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { usedScanRoot } from './protonPaths';

describe('usedScanRoot', () => {
    it('scans the sections on connect for Proton on the CLI, which now starts in My Files', () => {
        expect(usedScanRoot({ currentRemotePath: '/my-files', automatic: true, protocol: 'proton' })).toBe('/');
    });

    it('keeps the current folder for a scan asked from the status bar', () => {
        expect(usedScanRoot({ currentRemotePath: '/my-files/docs', automatic: false, protocol: 'proton' })).toBe('/my-files/docs');
    });

    it('keeps the current folder for the native client and other providers', () => {
        expect(usedScanRoot({
            currentRemotePath: '/',
            automatic: true,
            protocol: 'proton',
            options: { proton_mode: 'native' },
        })).toBe('/');
        expect(usedScanRoot({ currentRemotePath: '/data', automatic: true, protocol: 'sftp' })).toBe('/data');
        expect(usedScanRoot({ currentRemotePath: '', automatic: false, protocol: 'ftp' })).toBe('/');
    });
});
