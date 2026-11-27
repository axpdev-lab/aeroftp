// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { getProviderById } from './registry';
import { isEndpointCustomized, presetEndpointDefault } from './presetEndpoint';

describe('presetEndpointDefault', () => {
    it('returns the WebDAV preset server and port', () => {
        const koofr = getProviderById('koofr');
        const def = presetEndpointDefault(koofr, { server: '', username: 'alice' });
        expect(def).toEqual({ server: 'https://app.koofr.net/dav/Koofr', port: 443 });
    });

    it('returns the Puter host template, so the URL follows the username again', () => {
        const puter = getProviderById('puter-webdav');
        const def = presetEndpointDefault(puter, { server: '', username: 'alice' });
        expect(def).toEqual({ server: 'https://dav.puter.com/{username}', port: 443 });
    });

    it('computes the S3 default from the current region, not a static string', () => {
        const wasabi = getProviderById('wasabi');
        const def = presetEndpointDefault(wasabi, { options: { region: 'eu-central-1' } });
        expect(def?.server).toBe('https://s3.eu-central-1.wasabisys.com');
        const other = presetEndpointDefault(wasabi, { options: { region: 'us-west-1' } });
        expect(other?.server).toBe('https://s3.us-west-1.wasabisys.com');
    });

    it('returns the Swift preset auth URL (Blomp)', () => {
        const blomp = getProviderById('blomp');
        expect(presetEndpointDefault(blomp, { server: '' })).toEqual({
            server: 'https://authenticate.blomp.com',
            port: 443,
        });
    });

    it('returns null for a generic provider and for Amazon S3 (no fixed endpoint)', () => {
        expect(presetEndpointDefault(getProviderById('custom-webdav'), { server: 'x' })).toBeNull();
        expect(presetEndpointDefault(getProviderById('amazon-s3'), { options: { region: 'us-east-1' } })).toBeNull();
        expect(presetEndpointDefault(undefined, { server: 'x' })).toBeNull();
    });
});

describe('isEndpointCustomized', () => {
    it('is false when the value equals the preset default', () => {
        const puter = getProviderById('puter-webdav');
        expect(isEndpointCustomized(puter, { server: 'https://dav.puter.com/{username}', port: 443, username: 'alice' }, 443)).toBe(false);
    });

    it('treats the {username} expansion and a trailing slash as the default', () => {
        const puter = getProviderById('puter-webdav');
        expect(isEndpointCustomized(puter, { server: 'https://dav.puter.com/alice', port: 443, username: 'alice' }, 443)).toBe(false);
        const koofr = getProviderById('koofr');
        const def = presetEndpointDefault(koofr, { username: 'alice' })!;
        expect(isEndpointCustomized(koofr, { server: def.server + '/', port: 443, username: 'alice' }, 443)).toBe(false);
    });

    it('is true when the server or the port differs', () => {
        const puter = getProviderById('puter-webdav');
        expect(isEndpointCustomized(puter, { server: 'https://dav.example.com/alice', port: 443, username: 'alice' }, 443)).toBe(true);
        expect(isEndpointCustomized(puter, { server: 'https://dav.puter.com/{username}', port: 8443, username: 'alice' }, 443)).toBe(true);
    });

    it('compares the S3 endpoint against the region-computed default', () => {
        const wasabi = getProviderById('wasabi');
        expect(isEndpointCustomized(wasabi, { options: { region: 'eu-central-1', endpoint: 'https://s3.eu-central-1.wasabisys.com' } }, 443)).toBe(false);
        expect(isEndpointCustomized(wasabi, { options: { region: 'eu-central-1', endpoint: 'https://s3.example.com' } }, 443)).toBe(true);
    });

    it('is false for generic providers (nothing to reset to)', () => {
        expect(isEndpointCustomized(getProviderById('custom-webdav'), { server: 'anything.example.com' }, 443)).toBe(false);
    });
});
