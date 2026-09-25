// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import profileLoaderRs from '../../src-tauri/src/profile_loader.rs?raw';
import { PROVIDERS } from './registry';

// The default signing region of an S3 preset is declared twice: the registry
// `defaults.region` (GUI) and `s3_profile_default_region` in profile_loader.rs
// (CLI, MCP, bridge). They had drifted: Filen Desktop S3 declared `filen` in
// the registry and nothing in Rust, so a CLI profile without a stored region
// signed with us-east-1. This keeps the two tables identical.
function rustDefaultRegions(): Record<string, string> {
    const body = profileLoaderRs.split('fn s3_profile_default_region')[1]?.split('\n}\n')[0] ?? '';
    return Object.fromEntries([...body.matchAll(/"([a-z0-9-]+)" => Some\("([a-z0-9-]+)"\)/g)].map(m => [m[1], m[2]]));
}

describe('S3 preset default region parity (registry vs Rust loader)', () => {
    it('declares the same default region for every S3 preset', () => {
        const registry = Object.fromEntries(
            PROVIDERS.filter(p => p.protocol === 's3' && p.defaults?.region).map(p => [p.id, p.defaults!.region!]),
        );
        const rust = rustDefaultRegions();
        expect(Object.keys(rust).length).toBeGreaterThan(0);
        expect(rust).toEqual(registry);
    });
});
