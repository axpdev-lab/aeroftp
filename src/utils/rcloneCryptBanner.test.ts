// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { bannerOverlayParams } from './rcloneCryptBanner';

describe('bannerOverlayParams', () => {
    it('opens the overlay with the revealed options read as clear', () => {
        const params = bannerOverlayParams({
            password: 'crypt-pass-954',
            salt: 'hD1lB5uyIChoDFqhaHOsUg',
            filenameEncryption: 'standard',
            directoryNameEncryption: true,
            initialPath: '/enc',
        });
        expect(params.passwordForm).toBe('clear');
        expect(params.saltForm).toBe('clear');
        expect(params.salt).toBe('hD1lB5uyIChoDFqhaHOsUg');
        expect(params.remoteScope).toBe('/enc');
    });
});
