// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { activeProviderId, isFilenDesktopBridge, withFilenBridgeCredentialDefaults } from './filenDesktopBridge';

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

describe('activeProviderId (#958 review)', () => {
    it('prefers the active session over a stale connection form', () => {
        // Reconnect to a saved Filen Desktop profile after a session whose
        // preset stayed in the form: the session decides.
        expect(activeProviderId('filen-desktop-s3', 'aws')).toBe('filen-desktop-s3');
        expect(isFilenDesktopBridge(activeProviderId('filen-desktop-webdav', 'koofr'))).toBe(true);
        expect(isFilenDesktopBridge(activeProviderId('aws', 'filen-desktop-s3'))).toBe(false);
    });

    it('falls back to the form only when the session carries no provider id', () => {
        expect(activeProviderId(undefined, 'filen-desktop-s3')).toBe('filen-desktop-s3');
        expect(activeProviderId(null, null)).toBeUndefined();
        expect(activeProviderId('', '')).toBeUndefined();
    });
});
