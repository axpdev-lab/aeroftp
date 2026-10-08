// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { isFilenDesktopBridge, withFilenBridgeCredentialDefaults } from './filenDesktopBridge';

describe('Filen Desktop bridge credentials (#215, #958)', () => {
    it('recognises only the two local bridge presets', () => {
        expect(isFilenDesktopBridge('filen-desktop-s3')).toBe(true);
        expect(isFilenDesktopBridge('filen-desktop-webdav')).toBe(true);
        expect(isFilenDesktopBridge('filen')).toBe(false);
        expect(isFilenDesktopBridge(undefined)).toBe(false);
    });

    it('falls back to admin only for empty keys on a bridge preset', () => {
        expect(withFilenBridgeCredentialDefaults({ providerId: 'filen-desktop-s3', username: '', password: '' }))
            .toEqual({ providerId: 'filen-desktop-s3', username: 'admin', password: 'admin' });
        expect(withFilenBridgeCredentialDefaults({ providerId: 'filen-desktop-webdav', username: 'me', password: 'pw' }))
            .toEqual({ providerId: 'filen-desktop-webdav', username: 'me', password: 'pw' });
        expect(withFilenBridgeCredentialDefaults({ providerId: 'aws', username: '', password: '' }))
            .toEqual({ providerId: 'aws', username: '', password: '' });
    });
});
