// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import largeIcons from './LargeIconsGrid.tsx?raw';
import providerThumb from './ProviderThumbnail.tsx?raw';
import imageThumb from './ImageThumbnail.tsx?raw';

/**
 * A file thumbnail shows the whole file, not the middle of it.
 *
 * `object-fit: cover` fills the square by cropping whatever does not fit, which
 * on a wide photo is both edges and on a screenshot is usually the part that
 * identifies it. Reported on discussion #347: Icons and Large Icons were hiding
 * the edges of every image, where the platform's own file manager does not.
 *
 * All three file-thumbnail components now state the fit themselves rather than
 * inheriting it from `.file-grid-thumbnail`. That is deliberate: a `?raw` import
 * of a stylesheet comes back empty under Vite, so a rule kept in `styles.css`
 * would be the one part of this decision no test could read. The class still
 * carries the box — size, radius, and the background behind the letterboxing.
 *
 * Avatars and the chat attachment chips are not in this set: those are
 * decorative crops of a known shape, and `cover` is right for them.
 */
describe('file thumbnails are shown whole (#347)', () => {
    const surfaces: Array<[string, string]> = [
        ['ImageThumbnail (Icons grid, and the remote panel)', imageThumb],
        ['LargeIconsGrid (Large Icons)', largeIcons],
        ['ProviderThumbnail (provider-supplied)', providerThumb],
    ];

    for (const [name, source] of surfaces) {
        it(`fits the image inside its box: ${name}`, () => {
            expect(source, name).toMatch(/object-contain/);
            expect(source, name).not.toMatch(/object-cover/);
        });
    }

    it('keeps the Large Icons tile at a fixed size, so the grid does not reflow', () => {
        // `contain` letterboxes inside the box instead of resizing it; the fixed
        // box is what keeps the change invisible to the layout.
        expect(largeIcons).toMatch(/w-24 h-24 object-contain/);
    });

    it('leaves the box class on ImageThumbnail so the fit is the only thing added', () => {
        expect(imageThumb).toMatch(/className \|\| "file-grid-thumbnail object-contain"/);
    });
});
