// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { refusalFor } from './overlayRefusal';
import APP from '../App.tsx?raw';

describe('refusalFor', () => {
    const refusal = { savedServerId: 'p1', kind: 'rclone-crypt', reason: 'the crypt salt reads two ways' };

    it('gives the rclone-crypt refusal of the same profile', () => {
        expect(refusalFor(refusal, 'p1')).toBe('the crypt salt reads two ways');
    });

    it('gives nothing for another profile, for AeroCrypt, or when nothing was refused', () => {
        expect(refusalFor(refusal, 'p2')).toBeUndefined();
        expect(refusalFor({ ...refusal, kind: 'aerocrypt' }, 'p1')).toBeUndefined();
        expect(refusalFor(null, 'p1')).toBeUndefined();
    });

    it('reaches the locked banner from both failure paths', () => {
        // The auto-unlock records the refusal, both places that arm the locked
        // banner read it, and the banner renders it: a toast closed in seconds.
        expect(APP).toContain('overlayRefusalRef.current = {');
        expect(APP.match(/reason: refusalFor\(overlayRefusalRef\.current, /g) ?? []).toHaveLength(2);
        expect(APP).toContain('{lockedOverlayProfile.reason && (');
    });
});
