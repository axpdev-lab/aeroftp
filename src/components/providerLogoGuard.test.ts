// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { existsSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { connectionManifest } from '../providers/connectionManifest';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { PROVIDER_CATALOG, PROVIDER_GRID } from './providerCatalog';

// PROVIDER_LOGOS is the only lookup table behind every logo surface (Discover,
// Add Service, Quick Connect, My Servers, session tabs, rails, pickers). A
// missing entry makes the provider fall back to a protocol glyph on all of
// them, and nothing else notices.
describe('provider logo coverage', () => {
    it('gives every named S3 preset in the manifest a logo', () => {
        const missing = connectionManifest.presets
            .filter(p => p.protocol === 's3' && !p.generic)
            .map(p => p.id)
            .filter(id => !(id in PROVIDER_LOGOS));
        expect(missing).toEqual([]);
    });

    it('gives every named WebDAV preset in the manifest a logo', () => {
        const missing = connectionManifest.presets
            .filter(p => p.protocol === 'webdav' && !p.generic)
            .map(p => p.id)
            .filter(id => !(id in PROVIDER_LOGOS));
        expect(missing).toEqual([]);
    });

    it('gives every catalog company a logo', () => {
        const missing = PROVIDER_CATALOG
            .map(c => c.logoId)
            .filter(id => !(id in PROVIDER_LOGOS));
        expect(missing).toEqual([]);
    });

    it('ships the file of every README grid tile', () => {
        const missing = PROVIDER_GRID
            .map(tile => tile.icon)
            .filter(icon => !existsSync(`public/icons/providers/grid/${icon}`));
        expect(missing).toEqual([]);
    });
});
