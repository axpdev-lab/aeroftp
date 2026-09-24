// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import profileLoaderRs from '../../src-tauri/src/profile_loader.rs?raw';
import { PROVIDERS } from './registry';

// The backend expands a `{region}` endpoint template with the region the GUI
// form preselects (the first option of the preset's region select) when a
// caller sends neither a region nor an endpoint. That table lives in Rust
// (`s3_profile_template_default_region` in profile_loader.rs); this test keeps
// it in step with the registry, so a reordered or new region select cannot
// silently build a different host on the backend than the form shows.
function rustTemplateDefaultRegions(): Map<string, string> {
    const body = profileLoaderRs.split('fn s3_profile_template_default_region')[1]?.split('\n}\n')[0] ?? '';
    const arms = new Map<string, string>();
    for (const match of body.matchAll(/"([a-z0-9-]+)" => Some\("([a-z0-9-]+)"\)/g)) {
        arms.set(match[1], match[2]);
    }
    return arms;
}

describe('S3 template default region parity (Rust vs registry)', () => {
    const arms = rustTemplateDefaultRegions();

    it('covers every {region} template preset that has no default region', () => {
        const expected = PROVIDERS
            .filter(p => p.defaults?.endpointTemplate?.includes('{region}') && !p.defaults?.region)
            .map(p => p.id)
            .sort();
        expect(expected.length).toBeGreaterThan(0);
        expect([...arms.keys()].sort()).toEqual(expected);
    });

    it('uses the first option of each preset region select', () => {
        for (const [id, region] of arms) {
            const field = PROVIDERS.find(p => p.id === id)?.fields?.find(f => f.key === 'region');
            expect(field?.options?.[0]?.value, id).toBe(region);
        }
    });
});
