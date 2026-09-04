// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    R2_JURISDICTIONS,
    getProviderById,
    jurisdictionSegment,
    parseS3EndpointParams,
    resolveS3Endpoint,
    s3TemplateParams,
} from './registry';

describe('Cloudflare R2 jurisdiction', () => {
    it('resolves the default endpoint when no jurisdiction is stored', () => {
        // Every R2 profile saved before this option existed has no such key. The
        // template gained a `{jurisdiction}` placeholder, and an unresolved
        // placeholder makes resolveS3Endpoint return null, so this is the case
        // that decides whether those profiles keep working.
        expect(resolveS3Endpoint('cloudflare-r2', 'auto', { accountId: 'a1b2c3d4e5f6' }))
            .toBe('a1b2c3d4e5f6.r2.cloudflarestorage.com');
    });

    it('puts each jurisdiction in its own endpoint host', () => {
        expect(resolveS3Endpoint('cloudflare-r2', 'auto', { accountId: 'acc', jurisdiction: 'eu' }))
            .toBe('acc.eu.r2.cloudflarestorage.com');
        expect(resolveS3Endpoint('cloudflare-r2', 'auto', { accountId: 'acc', jurisdiction: 'us' }))
            .toBe('acc.us.r2.cloudflarestorage.com');
    });

    it('treats an unknown or empty jurisdiction as the default placement', () => {
        // Not defensiveness for its own sake: a hand-edited profile or an import
        // can carry anything, and the wrong answer here is a profile with no
        // endpoint rather than a profile pointing somewhere odd.
        for (const value of ['', '   ', 'mars', undefined]) {
            expect(jurisdictionSegment(value)).toBe('');
        }
        expect(resolveS3Endpoint('cloudflare-r2', 'auto', { accountId: 'acc', jurisdiction: 'mars' }))
            .toBe('acc.r2.cloudflarestorage.com');
    });

    it('accepts the jurisdiction code in any case', () => {
        expect(jurisdictionSegment('EU')).toBe('.eu');
        expect(jurisdictionSegment(' Us ')).toBe('.us');
    });

    it('offers exactly the jurisdictions the preset lists, default first', () => {
        const field = getProviderById('cloudflare-r2')?.fields?.find(f => f.key === 'jurisdiction');
        expect(field?.type).toBe('select');
        expect(field?.options?.map(o => o.value)).toEqual(R2_JURISDICTIONS.map(j => j.value));
        // The default must stay first: it is what a profile with no stored value
        // shows, and what the select falls back to.
        expect(R2_JURISDICTIONS[0].value).toBe('');
        expect(R2_JURISDICTIONS[0].segment).toBe('');
    });

    it('carries every template parameter, not just the one being edited', () => {
        // The defect this guards: a caller that rebuilt the endpoint passing only
        // the field it had just changed dropped the other one, producing a host
        // for the wrong account or the wrong jurisdiction.
        expect(s3TemplateParams({ accountId: 'acc', jurisdiction: 'eu' }))
            .toEqual({ accountId: 'acc', jurisdiction: 'eu' });
        expect(s3TemplateParams({ accountId: 'acc' })).toEqual({ accountId: 'acc' });
        expect(s3TemplateParams({})).toBeUndefined();
        expect(s3TemplateParams(undefined)).toBeUndefined();
    });

    it('recovers account id and jurisdiction from an endpoint saved without them', () => {
        // A profile saved before a field existed carries the endpoint and not the
        // field. This is the path that fills the form back in, and the defect it
        // guards against is concrete: building the regex by substituting one
        // placeholder leaves the other one in the pattern as a literal, so the
        // host matches nothing and the account id of every old R2 profile is
        // silently lost.
        expect(parseS3EndpointParams('cloudflare-r2', 'https://a1b2c3.r2.cloudflarestorage.com'))
            .toEqual({ accountId: 'a1b2c3' });
        expect(parseS3EndpointParams('cloudflare-r2', 'https://a1b2c3.eu.r2.cloudflarestorage.com'))
            .toEqual({ accountId: 'a1b2c3', jurisdiction: 'eu' });
        expect(parseS3EndpointParams('cloudflare-r2', 'a1b2c3.us.r2.cloudflarestorage.com/bucket'))
            .toEqual({ accountId: 'a1b2c3', jurisdiction: 'us' });
    });

    it('recovers nothing from a host that is not this provider', () => {
        expect(parseS3EndpointParams('cloudflare-r2', 'https://s3.wasabisys.com')).toEqual({});
        expect(parseS3EndpointParams('cloudflare-r2', '')).toEqual({});
        expect(parseS3EndpointParams(undefined, 'https://a.r2.cloudflarestorage.com')).toEqual({});
        // A segment R2 does not publish is not a jurisdiction, and the account id
        // must not silently absorb it either.
        expect(parseS3EndpointParams('cloudflare-r2', 'https://a1b2c3.zz.r2.cloudflarestorage.com'))
            .toEqual({});
    });

    it('round-trips: what resolve builds, parse reads back', () => {
        for (const j of R2_JURISDICTIONS) {
            const endpoint = resolveS3Endpoint('cloudflare-r2', 'auto', s3TemplateParams({
                accountId: 'a1b2c3',
                jurisdiction: j.value,
            }));
            expect(endpoint).toBe(`a1b2c3${j.segment}.r2.cloudflarestorage.com`);
            const parsed = parseS3EndpointParams('cloudflare-r2', endpoint!);
            expect(parsed.accountId).toBe('a1b2c3');
            expect(parsed.jurisdiction ?? '').toBe(j.value);
        }
    });

    it('leaves presets without a jurisdiction placeholder untouched', () => {
        expect(resolveS3Endpoint('wasabi', 'eu-central-1'))
            .toBe('https://s3.eu-central-1.wasabisys.com');
    });
});
