// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import hostMap from '../shared/s3-host-map.json';
import manifest from '../shared/connection-manifest.json';
import { deriveProviderId } from './serverListFilter';

/**
 * #1506 Requested 173: an S3 endpoint host is recognized on a domain boundary
 * from one table the frontend and the bridge mapper share, never by substring.
 * The substring rules took a bucket named after another vendor for that vendor
 * (`backblaze-backup.s3.amazonaws.com` read as Backblaze).
 */
type Entry = { providerId: string; preset: boolean; domains: string[]; hostPatterns?: string[] };
const entries = hostMap.providers as Entry[];
const presetIds = new Set(
    (manifest as { presets: { id: string; protocol: string }[] }).presets
        .filter(p => p.protocol === 's3')
        .map(p => p.id),
);

describe('S3 host map', () => {
    it('maps every listed host case, on the host and on a full endpoint URL', () => {
        for (const { host, providerId } of hostMap.cases) {
            const presetCase = providerId === null || entries.find(e => e.providerId === providerId)?.preset;
            const expected = presetCase ? (providerId ?? undefined) : undefined;
            expect(deriveProviderId({ name: 'x', host, username: '', protocol: 's3' }), host).toBe(expected);
            expect(
                deriveProviderId({ name: 'x', host: `https://${host}:443/bucket`, username: '', protocol: 's3' }),
                host,
            ).toBe(expected);
        }
    });

    it('names only real presets, and no domain twice', () => {
        const seen = new Set<string>();
        for (const entry of entries) {
            if (entry.preset) expect(presetIds.has(entry.providerId), entry.providerId).toBe(true);
            for (const domain of entry.domains) {
                expect(seen.has(domain), domain).toBe(false);
                seen.add(domain);
            }
        }
    });

    it('keeps every host pattern anchored and free of nested quantifiers', () => {
        // The patterns are compiled with `new RegExp` at module load. A group
        // that holds its own + or * and repeats without bound ((a+)+, (a*)*,
        // (a+){2,}) can backtrack exponentially; an optional group ((-[0-9]+)?)
        // runs at most once and stays linear. An unanchored pattern matches a
        // lookalike host.
        const nestedQuantifier = /\([^()]*[+*][^()]*\)[+*{]/;
        for (const entry of entries) {
            for (const pattern of entry.hostPatterns ?? []) {
                expect(() => new RegExp(pattern), pattern).not.toThrow();
                expect(pattern.endsWith('$'), pattern).toBe(true);
                expect(pattern.startsWith('^') || pattern.startsWith('(^|\\.)'), pattern).toBe(true);
                expect(nestedQuantifier.test(pattern), pattern).toBe(false);
            }
        }
    });

    it('covers every S3 preset whose endpoint has a fixed host', () => {
        const presets = (manifest as { presets: { id: string; protocol: string; defaults?: { endpoint?: string; endpointTemplate?: string } }[] }).presets;
        for (const preset of presets.filter(p => p.protocol === 's3')) {
            const endpoint = preset.defaults?.endpoint ?? preset.defaults?.endpointTemplate;
            if (!endpoint) continue;
            const host = endpoint
                .replace(/^https?:\/\//, '')
                .split(/[:/]/)[0]
                .replace(/\{[^}]+\}/g, 'x');
            expect(deriveProviderId({ name: 'x', host, username: '', protocol: 's3' }), preset.id).toBe(preset.id);
        }
    });
});
