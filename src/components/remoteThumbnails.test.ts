// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import grid from './LargeIconsGrid.tsx?raw';
import appRaw from '../App.tsx?raw';
import providerThumb from './ProviderThumbnail.tsx?raw';
import imageThumb from './ImageThumbnail.tsx?raw';
import { jsxTagContaining, jsxTags, withoutComments } from '../utils/jsxTag';

/**
 * Large Icons over a cloud drive could not show a picture at all.
 *
 * `LargeIconsGrid` renders both panels, but passed `isRemote={false}` to every
 * thumbnail regardless: the value was written into the component, not taken
 * from a prop, and the component had no such prop to take. On the remote panel
 * each preview therefore asked the *local* filesystem for a remote path, failed,
 * and fell back to a generic icon. Reported on discussion #347.
 *
 * The first version of this file was reviewed after it had already merged, and
 * four of its assertions were shown to check less than their names claimed: they
 * anchored on a character window rather than on an element, asserted that a
 * declaration existed rather than that it was used, and scanned one file while
 * saying "every call site". Each is now anchored on the element it is about, and
 * each was re-verified by breaking the property and watching it fail.
 */
describe('remote panel thumbnails (#347)', () => {
    const appCode = withoutComments(appRaw);
    const gridCode = withoutComments(grid);

    it('takes isRemote from a prop instead of deciding it', () => {
        expect(gridCode).not.toMatch(/isRemote=\{false\}/);
        expect(grid).toMatch(/isRemote\?: boolean/);
        expect(grid).toMatch(/isRemote=\{isRemote\}/);
    });

    it('puts the flag on the grid element that renders the remote panel', () => {
        // Anchored on the element carrying `files={sortedRemoteFiles}`, not on a
        // window around it. The window version passed on the *prose* of the
        // comment above the tag, and would have passed on a sibling grid's
        // `isRemote={false}` had the two mounts ever been reordered.
        const tag = jsxTagContaining(appCode, 'LargeIconsGrid', 'files={sortedRemoteFiles');
        expect(tag, 'the grid element that renders sortedRemoteFiles').toBeTruthy();
        expect(tag!, 'the remote grid must claim to be remote').toMatch(
            /\bisRemote(?:\s*=\s*\{true\})?[\s/>]/,
        );
        expect(tag!, 'and must not claim the opposite').not.toMatch(/isRemote\s*=\s*\{false\}/);
    });

    it('hands the thumbnail the path the backend listed, not one it rebuilt', () => {
        // Asserting the declaration alone let the component keep passing a
        // rebuilt path to the thumbnail while the variable sat unused.
        expect(gridCode).toMatch(/const imagePath = file\.path\s*\n?\s*\|\|/);
        const tag = jsxTagContaining(gridCode, 'ImageThumbnail', 'fallbackIcon');
        expect(tag, 'the thumbnail element in LargeIconsGrid').toBeTruthy();
        expect(tag!, 'it must be given imagePath').toMatch(/path=\{imagePath\}/);
    });

    it('falls back to reading the file when a provider thumbnail fails', () => {
        // A provider that advertises thumbnail support can still refuse one file.
        // Ending at a document icon made a whole Icons view look preview-less.
        expect(providerThumb).toMatch(/fallback\?: React\.ReactNode/);
        expect(providerThumb).toMatch(/if \(fallback\) return/);
        const tag = jsxTagContaining(appCode, 'ProviderThumbnail', 'fallback=');
        expect(tag, 'the provider thumbnail element').toBeTruthy();
        expect(tag!).toMatch(/fallback=\{[\s\S]*<ImageThumbnail/);
    });

    it('routes both components through the shared cache, keyed with the signature', () => {
        // What this can check is the wiring: that neither component builds a key
        // of its own and so bypasses the gate. That the gate itself holds, no
        // signature meaning no cache entry, is a behaviour and is pinned as one
        // in `thumbnailCache.test.ts` ("refuses to cache anything it cannot tell
        // apart in time"), against the real module rather than against its text.
        for (const [name, source] of [['ImageThumbnail', imageThumb], ['ProviderThumbnail', providerThumb]] as const) {
            expect(source, name).toMatch(/from '\.\.\/utils\/thumbnailCache'/);
            expect(withoutComments(source), `${name} must key through keyFor, with the signature`)
                .toMatch(/keyFor\([^;]*\bsignature\b[^;]*\)/);
            expect(withoutComments(source), `${name} must not assemble a key itself`)
                .not.toMatch(/const cacheKey\s*=\s*[`'"]/);
        }
        // The old private Map is gone, so there is one budget rather than two.
        expect(providerThumb).not.toMatch(/new Map<string, string>\(\)/);
    });

    it('loads only near the viewport and bounds the first-read burst', () => {
        expect(imageThumb).toContain('MAX_CONCURRENT_THUMBNAIL_READS = 4');
        expect(imageThumb).toContain('new IntersectionObserver');
        expect(imageThumb).toContain('scheduleThumbnailRead(loadImage)');
        expect(imageThumb).not.toMatch(/^\s*loadImage\(\);\s*$/m);
    });

    it('passes a signature at every production thumbnail mount, in every file', () => {
        // The first version scanned `App.tsx` alone while its name said "every
        // call site". Three of the five mounts live elsewhere, one of them in
        // `LargeIconsGrid.tsx`, which is the file this very change edited: a
        // signature dropped there would have been invisible to it.
        const modules = import.meta.glob('../**/*.tsx', {
            query: '?raw',
            import: 'default',
            eager: true,
        }) as Record<string, string>;

        const mounts: { where: string; tag: string }[] = [];
        for (const [file, source] of Object.entries(modules)) {
            if (file.includes('.test.')) continue;
            const code = withoutComments(source);
            for (const name of ['ImageThumbnail', 'ProviderThumbnail']) {
                for (const tag of jsxTags(code, name)) {
                    mounts.push({ where: `${file} <${name}>`, tag });
                }
            }
        }
        // A glob that matched nothing would make the loop below vacuously true.
        expect(mounts.length, 'production thumbnail mounts found').toBeGreaterThanOrEqual(5);
        for (const { where, tag } of mounts) {
            expect(tag, `${where} silently opts out of the cache`).toMatch(/signature=\{/);
        }
    });
});
