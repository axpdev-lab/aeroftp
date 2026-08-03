// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { isImageAvatar } from './UserAvatar';
import avatarRaw from './UserAvatar.tsx?raw';
import pickerRaw from './IconPickerDialog.tsx?raw';

/**
 * Issue #550: picking a provider logo for a user avatar left some of them
 * looking blank, and the ones that did draw were cropped at the edges.
 *
 * Two independent causes in the same feature, so two groups below.
 */
describe('avatar images (#550)', () => {
    describe('which values count as an image', () => {
        it('accepts an app icon path, which is what a PNG-backed logo stores', () => {
            // Providers whose logo is a PNG (Hetzner, FileLu, AWS, MinIO, Koofr,
            // Blomp, OpenDrive...) render as <img>, so the picker cannot
            // serialize them to a data URL and stores the asset path instead.
            // Only data URLs passed, so those avatars fell through to the text
            // branch and painted the path as a string inside the circle.
            expect(isImageAvatar('/icons/providers/filelu.png')).toBe(true);
            expect(isImageAvatar('/icons/providers/hetzner-storage-box.png')).toBe(true);
            expect(isImageAvatar('/icons/aeroftp.svg')).toBe(true);
        });

        it('accepts a data URL, which is what an inline-SVG logo stores', () => {
            // Azure and the other inline-<svg> providers already worked.
            expect(isImageAvatar('data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=')).toBe(true);
            expect(isImageAvatar('data:image/png;base64,iVBORw0KGgo=')).toBe(true);
            // A data URL is not required to carry `;base64`.
            expect(isImageAvatar('data:image/svg+xml,%3Csvg%3E%3C/svg%3E')).toBe(true);
        });

        it('refuses anything that would fetch from a host we did not ship', () => {
            // The value arrives from stored user data, so the test is an
            // allowlist rather than "does it look like a URL". An avatar must
            // never be able to turn a user record into a tracking pixel.
            expect(isImageAvatar('https://evil.example/pixel.png')).toBe(false);
            expect(isImageAvatar('http://evil.example/pixel.png')).toBe(false);
            expect(isImageAvatar('//evil.example/pixel.png')).toBe(false);
            expect(isImageAvatar('/icons/../../../etc/passwd.png')).toBe(false);
            expect(isImageAvatar('javascript:alert(1)')).toBe(false);
            expect(isImageAvatar('/uploads/anything.png')).toBe(false);
        });

        it('treats an emoji or an empty value as not an image', () => {
            expect(isImageAvatar('🚀')).toBe(false);
            expect(isImageAvatar('')).toBe(false);
            expect(isImageAvatar(null)).toBe(false);
            expect(isImageAvatar(undefined)).toBe(false);
        });
    });

    describe('how the image is fitted', () => {
        it('shows the whole logo instead of cropping it to the circle', () => {
            // `cover` fills the square by cutting whatever overflows. On a logo
            // with content at the edge that removes part of the mark rather than
            // some background: AeroFTP's own rocket lost its red tip.
            expect(avatarRaw).toContain('object-contain');
            expect(avatarRaw).not.toContain('object-cover');
        });
    });

    describe('what the picker stores', () => {
        it('stores the path the logo declares, not the URL it resolved to', () => {
            // `img.src` resolves against the document, so it yields an absolute
            // URL whose origin is an accident of how the app was loaded
            // (`http://tauri.localhost/...`). Persisting that into a user record
            // ties the avatar to an origin that can change between builds.
            expect(pickerRaw).toContain("img?.getAttribute('src')");
            expect(pickerRaw).not.toMatch(/if \(img && img\.src\) \{\s*return img\.src;/);
        });
    });
});
