// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { PROVIDERS, getProviderById, resolveProfileS3Location } from './registry';
import { connectionManifest } from './connectionManifest';

// Coritan publishes no region list: each service's dashboard shows its own
// endpoint, https://s3.<region>.coritan.com:7337. These six answered as S3
// servers on port 7337 on 2026-11-16 (an anonymous request refused with 403,
// a valid certificate); the wildcard DNS sends every other label to
// 127.0.0.1, and port 443 was closed on all six.
const REGIONS = ['fra', 'lhr', 'iad', 'pdx', 'sin', 'bom'];

function coritanRegionCodes(): string[] {
    const preset = PROVIDERS.find(p => p.id === 'coritan');
    const region = preset?.fields.find(f => f.key === 'region');
    return (region?.options ?? []).map(o => o.value);
}

describe('Coritan Object Storage regions', () => {
    it('offers the six regions that answered and nothing else', () => {
        expect([...coritanRegionCodes()].sort()).toEqual([...REGIONS].sort());
    });

    it('lists Frankfurt first, because it is the default', () => {
        expect(coritanRegionCodes()[0]).toBe('fra');
        expect(connectionManifest.presets.find(p => p.id === 'coritan')?.firstRegion).toBe('fra');
        expect(getProviderById('coritan')?.defaults?.region).toBe('fra');
    });

    it('uses proper-name labels that carry the region id, with no new translation key', () => {
        const region = PROVIDERS.find(p => p.id === 'coritan')!.fields.find(f => f.key === 'region')!;
        for (const option of region.options ?? []) {
            expect(option.label).toContain(`(${option.value})`);
        }
    });

    it('builds the endpoint on port 7337 from every region code, path-style', () => {
        const preset = getProviderById('coritan')!;
        expect(preset.defaults?.pathStyle).toBe(true);
        for (const code of REGIONS) {
            expect(resolveProfileS3Location('coritan', { region: code }, '')).toEqual({
                endpoint: `https://s3.${code}.coritan.com:7337`,
                region: code,
                signingRegion: code,
            });
        }
    });

    it('reads the region back from an endpoint that carries the port', () => {
        // The template names its port, and the region came back only when the
        // template was compared host to host: before, `:7337` stayed in the
        // template's suffix and no endpoint matched it.
        expect(resolveProfileS3Location('coritan', { endpoint: 'https://s3.sin.coritan.com:7337' }, ''))
            .toEqual({ endpoint: 'https://s3.sin.coritan.com:7337', region: 'sin', signingRegion: 'sin' });
        expect(resolveProfileS3Location('coritan', { endpoint: 'https://s3.lhr.coritan.com:7337/', region: 'fra' }, ''))
            .toEqual({ endpoint: 'https://s3.lhr.coritan.com:7337/', region: 'lhr', signingRegion: 'lhr' });
    });
});
