// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import grid from './LargeIconsGrid.tsx?raw';
import appRaw from '../App.tsx?raw';
import providerThumb from './ProviderThumbnail.tsx?raw';
import imageThumb from './ImageThumbnail.tsx?raw';

/**
 * Large Icons over a cloud drive could not show a picture at all.
 *
 * `LargeIconsGrid` renders both panels, but passed `isRemote={false}` to every
 * thumbnail regardless — the value was written into the component, not taken
 * from a prop, and the component had no such prop to take. On the remote panel
 * each preview therefore asked the *local* filesystem for a remote path, failed,
 * and fell back to a generic icon. Reported on discussion #347.
 */
describe('remote panel thumbnails (#347)', () => {
    /** Source with comments removed: a doc comment naming the old bug is not the
     *  old bug, and an assertion that cannot tell them apart is worthless. */
    const code = (source: string): string =>
        source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');

    it('takes isRemote from a prop instead of deciding it', () => {
        expect(code(grid)).not.toMatch(/isRemote=\{false\}/);
        expect(grid).toMatch(/isRemote\?: boolean/);
        expect(grid).toMatch(/isRemote=\{isRemote\}/);
    });

    it('tells the grid it is the remote panel where it is', () => {
        const mount = appRaw.slice(appRaw.indexOf('<LargeIconsGrid'));
        const remoteMount = mount.slice(0, mount.indexOf('files={sortedRemoteFiles') + 200);
        expect(remoteMount, 'the remote Large Icons grid').toMatch(/\bisRemote\b/);
    });

    it('uses the path the backend listed rather than rebuilding one', () => {
        // Rejoining `currentPath` with a forward slash was merely usually right,
        // and on Windows produced a mixed-separator path.
        expect(grid).toMatch(/const imagePath = file\.path\s*\n?\s*\|\|/);
    });

    it('falls back to reading the file when a provider thumbnail fails', () => {
        // A provider that advertises thumbnail support can still refuse one file.
        // Ending at a document icon made a whole Icons view look preview-less.
        expect(providerThumb).toMatch(/fallback\?: React\.ReactNode/);
        expect(providerThumb).toMatch(/if \(fallback\) return/);
        const mount = appRaw.slice(appRaw.indexOf('<ProviderThumbnail'));
        expect(mount.slice(0, 900)).toMatch(/fallback=\{[\s\S]*<ImageThumbnail/);
    });

    it('caches through the shared cache, and only with a signature', () => {
        for (const [name, source] of [['ImageThumbnail', imageThumb], ['ProviderThumbnail', providerThumb]] as const) {
            expect(source, name).toMatch(/from '\.\.\/utils\/thumbnailCache'/);
            expect(source, name).toMatch(/keyFor\(/);
            expect(source, name).toMatch(/putThumbnail\(/);
        }
        // The old private Map is gone, so there is one budget rather than two.
        expect(providerThumb).not.toMatch(/new Map<string, string>\(\)/);
    });

    it('passes a signature at every call site that renders a file thumbnail', () => {
        // A call site that forgets it silently opts out of the cache, which is
        // the bug this replaces in a quieter form.
        const sites = [...appRaw.matchAll(/<(?:Image|Provider)Thumbnail[\s\S]{0,700}?\/>/g)].map((m) => m[0]);
        expect(sites.length, 'thumbnail mounts in App.tsx').toBeGreaterThanOrEqual(2);
        for (const site of sites) {
            expect(site, site.slice(0, 60)).toMatch(/signature=\{signatureOf\(/);
        }
    });
});
