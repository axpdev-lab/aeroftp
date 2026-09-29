// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { PROVIDERS } from './registry';

// The IBM Cloud Object Storage preset builds its host from the location code
// (`s3.{region}.cloud-object-storage.appdomain.cloud`), so a code that IBM
// does not serve on that template is a bucket the user can never reach. The
// 4.2.1 pre-release review (L19) asked whether `in-che`, `in-mum` and `ca-mon`
// exist as regional endpoints. They do: this is the public endpoint table of
// IBM's own documentation source, `endpoints.md` in
// github.com/ibm-cloud-docs/cloud-object-storage, read on 2026-09-29. The
// decommissioned locations of the same page (mel01, mex01, tor01, osl01,
// hkg02, seo01, mil01) are deliberately absent from the preset.
const REGIONAL = ['us-south', 'us-east', 'eu-gb', 'eu-de', 'au-syd', 'jp-tok', 'jp-osa', 'ca-tor', 'br-sao', 'eu-es', 'ca-mon', 'in-che', 'in-mum'];
const CROSS_REGION = ['us', 'eu', 'ap'];
const SINGLE_DATA_CENTER = ['ams03', 'che01', 'mon01', 'par01', 'sjc04', 'sng01'];
const DECOMMISSIONED = ['mel01', 'mex01', 'tor01', 'osl01', 'hkg02', 'seo01', 'mil01'];

function ibmLocationCodes(): string[] {
    const preset = PROVIDERS.find(p => p.id === 'ibm-cos');
    const region = preset?.fields.find(f => f.key === 'region');
    return (region?.options ?? []).map(o => o.value);
}

describe('IBM Cloud Object Storage locations', () => {
    it('offers every documented public location and nothing else', () => {
        const documented = [...REGIONAL, ...CROSS_REGION, ...SINGLE_DATA_CENTER].sort();
        expect([...ibmLocationCodes()].sort()).toEqual(documented);
    });

    it('does not offer a decommissioned location', () => {
        const offered = new Set(ibmLocationCodes());
        expect(DECOMMISSIONED.filter(code => offered.has(code))).toEqual([]);
    });

    it('builds the documented regional host from the default location', () => {
        const preset = PROVIDERS.find(p => p.id === 'ibm-cos')!;
        expect(REGIONAL).toContain(preset.defaults?.region);
        expect(preset.defaults?.endpointTemplate?.replace('{region}', preset.defaults.region!))
            .toBe(`https://s3.${preset.defaults?.region}.cloud-object-storage.appdomain.cloud`);
    });
});
