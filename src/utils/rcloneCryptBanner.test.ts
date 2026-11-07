// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { bannerAnchorFolder, bannerOverlayParams } from './rcloneCryptBanner';
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
    // #1523 CodeRabbit: the banner can be clicked after the user moved to a
    // subfolder; the crypt root is the profile's start folder, not the panel's.
    it('anchors at the start folder, whatever folder the panel shows', () => {
        expect(bannerAnchorFolder('/docs', '/docs/sub')).toBe('/docs');
        expect(bannerAnchorFolder('/', '/docs/sub')).toBe('/');
        expect(bannerAnchorFolder(undefined, '/home/u/docs')).toBe('/home/u/docs');
        expect(bannerAnchorFolder('docs', '/home/u/docs')).toBe('/home/u/docs');
        expect(app).toContain('bannerOverlayParams(banner, bannerAnchorFolder(sessions.find(s => s.id === activeSessionId)?.serverInitialPath, currentRemotePath))');
        expect(app).not.toMatch(/initialPath: typeof opts\.rcloneCryptRemote/);
    });
});
