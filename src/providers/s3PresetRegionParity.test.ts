// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import profileLoaderRs from '../../src-tauri/src/profile_loader.rs?raw';
import { PROVIDERS } from './registry';
import { connectionManifest } from './connectionManifest';

// Both consumers use the shared manifest; selectable region fields stay in
// the presentation registry and must retain the manifest's first selection.
function rustDefaultRegions(): Record<string, string> {
    expect(profileLoaderRs).toContain('crate::connection_manifest::preset(provider_id)');
    return Object.fromEntries(connectionManifest.presets
        .filter(p => p.protocol === 's3' && p.defaults.region)
        .map(p => [p.id, p.defaults.region!]));
}

describe('S3 preset default region parity (registry vs Rust loader)', () => {
    it('declares the same default region for every S3 preset', () => {
        const registry = Object.fromEntries(
            PROVIDERS.filter(p => p.protocol === 's3' && p.defaults?.region).map(p => [p.id, p.defaults!.region!]),
        );
        const rust = rustDefaultRegions();
        expect(Object.keys(rust).length).toBeGreaterThan(0);
        expect(rust).toEqual(registry);
        for (const provider of PROVIDERS.filter(p => p.protocol === 's3' && p.defaults?.endpointTemplate && !p.defaults.region)) {
            const field = provider.fields.find(f => f.key === 'region' && f.type === 'select');
            expect(connectionManifest.presets.find(p => p.id === provider.id)?.firstRegion ?? undefined,
                `${provider.id} first selectable region`).toEqual(field?.options?.[0]?.value);
        }
    });
});
