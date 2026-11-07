// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { s3PresetOffersObjectTags, s3PresetOffersTrash } from './types';
import appSource from './App.tsx?raw';

// Fastly Object Storage answers 501 to the object tagging calls, so the file
// menu must not open a tag dialog whose first request can only fail.
describe('Object Tags in the S3 file menu', () => {
    it('is not offered for Fastly and still is for every other S3 provider', () => {
        expect(s3PresetOffersObjectTags('fastly')).toBe(false);
        expect(s3PresetOffersObjectTags('fastly-object-storage')).toBe(false);
        for (const id of ['amazon-s3', 'wasabi', 'cloudflare-r2', 'minio', 'tigris', 'custom-s3', '', null, undefined]) {
            expect(s3PresetOffersObjectTags(id), String(id)).toBe(true);
        }
    });

    it('is added to the menu only behind that check', () => {
        expect(appSource.match(/t\('s3\.objectTags'\)/g)?.length).toBe(1);
        expect(appSource).toMatch(
            /if \(s3PresetOffersObjectTags\(liveConnectionParams\.providerId\)\) \{\s*items\.push\(\{\s*label: t\('s3\.objectTags'\)/,
        );
    });

    it('hides the version-based trash view for both Fastly ids', () => {
        expect(s3PresetOffersTrash('fastly')).toBe(false);
        expect(s3PresetOffersTrash('fastly-object-storage')).toBe(false);
        expect(s3PresetOffersTrash('amazon-s3')).toBe(true);
        expect(appSource).toMatch(/if \(proto === 's3' && !s3PresetOffersTrash\(activeProviderId\)\) \{\s*handler = undefined;/);
    });
});
