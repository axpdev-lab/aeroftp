// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { PROVIDERS } from './registry';
import { getProviderById } from './registry';
import { connectionManifest } from './connectionManifest';

// Fastly Object Storage serves each region at
// https://<region>.object.fastlystorage.app and signs SigV4 with the same
// label, so a code that is not in this list is a bucket nobody can reach, and
// a wrong code is a host that does not resolve. The list comes from the Fastly
// docs and rclone's Fastly provider (12 regions) and was checked by DNS on
// 2026-11-05. us-east, us-west and eu-central (no -1) are older regions that
// live next to us-east-1, us-west-1 and eu-central-1; they are not aliases.
// eu-west and us-central (no -1) do not exist.
const REGIONS = [
    'eu-central-1', 'us-east-1', 'us-central-1', 'us-west-1', 'us-east', 'us-west',
    'uk-east-1', 'eu-west-1', 'eu-south-1', 'eu-central', 'jp-central-1', 'au-east-1',
];
const NOT_REGIONS = ['eu-west', 'us-central'];

function fastlyRegionCodes(): string[] {
    const preset = PROVIDERS.find(p => p.id === 'fastly');
    const region = preset?.fields.find(f => f.key === 'region');
    return (region?.options ?? []).map(o => o.value);
}

describe('Fastly Object Storage regions', () => {
    it('offers the twelve documented regions and nothing else', () => {
        expect([...fastlyRegionCodes()].sort()).toEqual([...REGIONS].sort());
    });

    it('lists Frankfurt first, because it is the default', () => {
        expect(fastlyRegionCodes()[0]).toBe('eu-central-1');
        expect(connectionManifest.presets.find(p => p.id === 'fastly')?.firstRegion).toBe('eu-central-1');
    });

    it('does not offer a region that has no host', () => {
        const offered = new Set(fastlyRegionCodes());
        expect(NOT_REGIONS.filter(code => offered.has(code))).toEqual([]);
    });

    it('uses proper-name labels that carry the region id, with no new translation key', () => {
        const preset = PROVIDERS.find(p => p.id === 'fastly')!;
        const region = preset.fields.find(f => f.key === 'region')!;
        for (const option of region.options ?? []) {
            expect(option.label).toContain(`(${option.value})`);
        }
    });

    it('builds the documented host from every region code', () => {
        const preset = getProviderById('fastly')!;
        expect(preset.defaults?.endpointTemplate).toBe('https://{region}.object.fastlystorage.app');
        for (const code of REGIONS) {
            expect(preset.defaults!.endpointTemplate!.replace('{region}', code))
                .toBe(`https://${code}.object.fastlystorage.app`);
        }
    });

    it('is path-style, as the host has no bucket subdomains', () => {
        expect(getProviderById('fastly')?.defaults?.pathStyle).toBe(true);
    });
});
