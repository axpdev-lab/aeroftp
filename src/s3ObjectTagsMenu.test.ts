// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
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

    it('refreshes provider capabilities when switching between two connected S3 sessions and discards stale responses', async () => {
        const switchStart = appSource.indexOf('  const switchSession = async');
        const switchBody = appSource.slice(switchStart, appSource.indexOf('\n  const ', switchStart + 1));
        const resetIdx = switchBody.indexOf('setStorageQuota(null);');
        const connectedIdx = switchBody.indexOf('setIsConnected(true);');
        expect(resetIdx).toBeGreaterThan(-1);
        expect(connectedIdx).toBeGreaterThan(resetIdx);

        // Leaving the previous session clears cached capabilities before reconnect,
        // and completing the reconnect refreshes capabilities even when isConnected
        // and protocol ('s3') are unchanged across both tabs.
        const beforeReconnect = switchBody.slice(resetIdx, connectedIdx);
        const afterReconnect = switchBody.slice(connectedIdx);
        expect(beforeReconnect).toContain('providerCapsVersionRef.current++;');
        expect(beforeReconnect).toContain('setProviderCaps({ versions: false, thumbnails: false, permissions: false, locking: false });');
        expect(afterReconnect).toContain('refreshProviderCaps(protocol);');

        // Execute the production refreshProviderCaps implementation across
        // AWS S3 -> Fastly S3 and Fastly S3 -> AWS S3 switches, plus an
        // out-of-order stale response from the previous session.
        const helperStart = appSource.indexOf('  const refreshProviderCaps = useLatestCallback(');
        const helperEnd = appSource.indexOf('\n  useEffect(() => {', helperStart);
        expect(helperStart).toBeGreaterThan(-1);
        expect(helperEnd).toBeGreaterThan(helperStart);
        const helperSource = appSource.slice(helperStart, helperEnd);
        const { code } = await transformWithOxc(helperSource, 'refresh-provider-caps.ts');

        let caps = { versions: false, thumbnails: false, permissions: false, locking: false };
        const providerCapsVersionRef = { current: 0 };
        const liveConnectionParams = { protocol: 's3' };
        let activeBackend: 'aws' | 'fastly' = 'aws';
        let resolveStaleAwsVersions: ((v: boolean) => void) | null = null;

        const invoke = vi.fn(async (cmd: string) => {
            if (cmd === 'provider_supports_versions') {
                if (resolveStaleAwsVersions) {
                    const hook = resolveStaleAwsVersions;
                    resolveStaleAwsVersions = null;
                    return await new Promise<boolean>((resolve) => {
                        hook(true);
                        // Wait until resolved by test
                        resolveStaleAwsVersions = resolve;
                    });
                }
                return activeBackend === 'aws';
            }
            return false;
        });

        const refreshProviderCaps = new Function(
            'useLatestCallback',
            'providerCapsVersionRef',
            'liveConnectionParams',
            'isSshProtocol',
            'setProviderCaps',
            'invoke',
            `${code}\nreturn refreshProviderCaps;`,
        )(
            (fn: unknown) => fn,
            providerCapsVersionRef,
            liveConnectionParams,
            (p: string) => p === 'sftp' || p === 'scp',
            (next: typeof caps) => { caps = next; },
            invoke,
        ) as (protocolOverride?: string) => Promise<void>;

        // 1. Connected to AWS S3: versions supported
        activeBackend = 'aws';
        await refreshProviderCaps('s3');
        expect(caps.versions).toBe(true);

        // 2. Switch AWS S3 -> Fastly S3: versions must become false
        activeBackend = 'fastly';
        await refreshProviderCaps('s3');
        expect(caps.versions).toBe(false);

        // 3. Switch Fastly S3 -> AWS S3: versions must become true again
        activeBackend = 'aws';
        await refreshProviderCaps('s3');
        expect(caps.versions).toBe(true);

        // 4. Stale in-flight AWS S3 response arriving after switching to Fastly S3 is ignored
        let releaseStale!: (v: boolean) => void;
        resolveStaleAwsVersions = () => {};
        const stalePromise = refreshProviderCaps('s3');
        releaseStale = resolveStaleAwsVersions!;
        resolveStaleAwsVersions = null;

        activeBackend = 'fastly';
        await refreshProviderCaps('s3');
        expect(caps.versions).toBe(false);

        releaseStale(true);
        await stalePromise;
        expect(caps.versions).toBe(false);
    });
});
