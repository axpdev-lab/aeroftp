// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { overlayWarningTitleKey } from './overlayWarningTitle';

describe('overlayWarningTitleKey', () => {
    it('titles an rclone-crypt warning as rclone crypt, never as an AeroCrypt restore', () => {
        expect(overlayWarningTitleKey('rclone-crypt', false)).toBe('aerocrypt.title');
        expect(overlayWarningTitleKey('rclone-crypt', undefined)).toBe('aerocrypt.title');
    });

    it('says "restored" for AeroCrypt only when a marker was restored', () => {
        expect(overlayWarningTitleKey('aerocrypt', true)).toBe('aerocryptNative.markerMissingRestoredTitle');
        expect(overlayWarningTitleKey('aerocrypt', false)).toBe('aerocryptNative.title');
        expect(overlayWarningTitleKey('aerocrypt', undefined)).toBe('aerocryptNative.title');
    });
});
