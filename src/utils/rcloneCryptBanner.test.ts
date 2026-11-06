// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { bannerOverlayParams } from './rcloneCryptBanner';
import app from '../App.tsx?raw';

describe('bannerOverlayParams', () => {
    it('opens the overlay with the revealed options read as clear', () => {
        const params = bannerOverlayParams({
            password: 'crypt-pass-954',
            salt: 'hD1lB5uyIChoDFqhaHOsUg',
            filenameEncryption: 'standard',
            directoryNameEncryption: true,
        }, '/enc');
        expect(params.passwordForm).toBe('clear');
        expect(params.saltForm).toBe('clear');
        expect(params.salt).toBe('hD1lB5uyIChoDFqhaHOsUg');
        expect(params.remoteScope).toBe('/enc');
    });
    it('anchors at the folder it is given, never at the rclone remote string', () => {
        const params = bannerOverlayParams({ password: 'p' }, '/home/u/docs');
        expect(params.remoteScope).toBe('/home/u/docs');
        expect(params.remoteScope).not.toContain(':');
    });
    it('is opened on the folder the panel shows', () => {
        expect(app).toContain('bannerOverlayParams(banner, currentRemotePath)');
        expect(app).not.toMatch(/initialPath: typeof opts\.rcloneCryptRemote/);
    });
});
