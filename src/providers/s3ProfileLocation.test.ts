// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { presetDefaultS3Region, resolveProfileS3Location } from './registry';

// One rule for every GUI path that turns a saved S3 profile into an address
// (connect, speed test, edit): stored endpoint, then an explicit host, then
// the preset. Mirrors apply_s3_profile_defaults in profile_loader.rs.
describe('resolveProfileS3Location', () => {
    it('keeps an endpoint carried only in the host (Cyberduck / restic import)', () => {
        // Before: the template built s3.us-east-1.cloud-object-storage... (no
        // such region) for an IBM bucket imported with its us-south host.
        expect(resolveProfileS3Location('ibm-cos', {}, 's3.us-south.cloud-object-storage.appdomain.cloud'))
            .toEqual({ endpoint: 's3.us-south.cloud-object-storage.appdomain.cloud', region: 'us-south', signingRegion: 'us-south' });
        expect(resolveProfileS3Location('wasabi', { region: 'eu-central-2' }, 's3.eu-central-2.wasabisys.com'))
            .toEqual({ endpoint: 's3.eu-central-2.wasabisys.com', region: 'eu-central-2', signingRegion: 'eu-central-2' });
    });

    it('prefers the stored endpoint option over the host', () => {
        expect(resolveProfileS3Location(
            'ibm-cos',
            { endpoint: 'https://s3.eu-gb.cloud-object-storage.appdomain.cloud' },
            's3.us-south.cloud-object-storage.appdomain.cloud',
        ).endpoint).toBe('https://s3.eu-gb.cloud-object-storage.appdomain.cloud');
    });

    it('expands the template with the stored region, else the preset default', () => {
        expect(resolveProfileS3Location('ibm-cos', { region: 'jp-tok' }, ''))
            .toEqual({ endpoint: 'https://s3.jp-tok.cloud-object-storage.appdomain.cloud', region: 'jp-tok', signingRegion: 'jp-tok' });
        // No region stored: the default is returned WITH the host built from
        // it, so region and endpoint cannot disagree.
        expect(resolveProfileS3Location('ibm-cos', {}, ''))
            .toEqual({ endpoint: 'https://s3.eu-de.cloud-object-storage.appdomain.cloud', region: 'eu-de', signingRegion: 'eu-de' });
        expect(resolveProfileS3Location('mega-s4', {}, ''))
            .toEqual({ endpoint: 's3.eu-central-1.s4.mega.io', region: 'eu-central-1', signingRegion: 'eu-central-1' });
    });

    it('does not treat an AWS host as an explicit endpoint', () => {
        expect(resolveProfileS3Location('amazon-s3', { region: 'eu-west-1' }, 's3.eu-west-1.amazonaws.com'))
            .toEqual({ endpoint: null, region: 'eu-west-1', signingRegion: 'eu-west-1' });
    });

    it('returns static preset endpoints and R2 account templates', () => {
        expect(resolveProfileS3Location('filelu-s3', {}, '')).toEqual({ endpoint: 's5lu.com', region: 'global', signingRegion: 'global' });
        expect(resolveProfileS3Location('cloudflare-r2', { accountId: 'abc', jurisdiction: 'eu' }, '').endpoint)
            .toBe('abc.eu.r2.cloudflarestorage.com');
    });
});

describe('resolveProfileS3Location signingRegion', () => {
    it('keeps the FileLu S5 global fallback for an explicit endpoint without a region', () => {
        // Every connect path (main, AeroCloud, speed test) sends signingRegion;
        // AeroCloud used to fall back to us-east-1 where the main path sent
        // global (CodeRabbit on PR #923).
        expect(resolveProfileS3Location('filelu-s3', { endpoint: 's5lu.com' }, '').signingRegion).toBe('global');
        expect(resolveProfileS3Location('filelu-s3', {}, 's5lu.com').signingRegion).toBe('global');
        expect(resolveProfileS3Location('custom-s3', {}, 'minio.lab:9000').signingRegion).toBe('us-east-1');
        // The template region is also the signing region, never the fallback.
        expect(resolveProfileS3Location('ibm-cos', {}, '').signingRegion).toBe('eu-de');
    });
});

describe('signing region read back from an explicit endpoint', () => {
    // Same case table as s3_region_is_read_back_from_a_regional_endpoint in
    // src-tauri/src/profile_loader.rs: the two sides must agree.
    const cases: Array<[string, string, string | undefined]> = [
        ['ibm-cos', 's3.us-south.cloud-object-storage.appdomain.cloud', 'us-south'],
        ['ibm-cos', 'https://s3.eu-gb.cloud-object-storage.appdomain.cloud/bucket', 'eu-gb'],
        ['ibm-cos', 'https://S3.JP-OSA.cloud-object-storage.appdomain.cloud:443', 'jp-osa'],
        ['wasabi', 'https://s3.eu-central-2.wasabisys.com', 'eu-central-2'],
        ['wasabi', 's3.wasabisys.com', undefined],
        ['mega-s4', 's3.eu-central-2.s4.mega.io', 'eu-central-2'],
        ['alibaba-oss', 'https://oss-eu-central-1.aliyuncs.com', 'eu-central-1'],
        ['tencent-cos', 'https://cos.eu-frankfurt.myqcloud.com', 'eu-frankfurt'],
        ['digitalocean-spaces', 'https://fra1.digitaloceanspaces.com', 'fra1'],
        ['digitalocean-spaces', 'https://bucket.fra1.digitaloceanspaces.com', undefined],
        ['cloudflare-r2', 'https://abc.r2.cloudflarestorage.com', undefined],
        ['backblaze', 's3.eu-central-003.backblazeb2.com', undefined],
    ];
    it.each(cases)('%s %s -> %s', (preset, endpoint, region) => {
        // Through both explicit forms: stored endpoint option and host.
        expect(resolveProfileS3Location(preset, { endpoint }, '').region).toBe(region);
        expect(resolveProfileS3Location(preset, {}, endpoint).region).toBe(region);
    });

    it('signs an IBM regional endpoint with its own region, not us-east-1', () => {
        expect(resolveProfileS3Location('ibm-cos', {}, 's3.us-south.cloud-object-storage.appdomain.cloud').signingRegion)
            .toBe('us-south');
        expect(resolveProfileS3Location('ibm-cos', { region: 'eu-gb' }, 's3.us-south.cloud-object-storage.appdomain.cloud').signingRegion)
            .toBe('eu-gb');
    });
});

describe('presetDefaultS3Region', () => {
    it('uses the preset default, else the first region option of a template preset', () => {
        expect(presetDefaultS3Region('ibm-cos')).toBe('eu-de');
        expect(presetDefaultS3Region('wasabi')).toBe('us-east-1');
        expect(presetDefaultS3Region('mega-s4')).toBe('eu-central-1');
        expect(presetDefaultS3Region('amazon-s3')).toBeUndefined();
        expect(presetDefaultS3Region(undefined)).toBeUndefined();
    });
});
