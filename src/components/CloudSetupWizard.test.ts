// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { aeroCloudRemoteFolder, profileRoot } from './CloudSetupWizard';
import { profileMatchesQuery } from './common/ProfileRail';
import type { ServerProfile } from '../types';

const profile = (fields: Partial<ServerProfile>): ServerProfile =>
    ({ id: 'srv_1', name: 'Profile', host: 'example.com', port: 21, username: '', ...fields }) as ServerProfile;

describe('profileRoot', () => {
    it('is the server root when the profile has no starting folder', () => {
        expect(profileRoot(profile({}))).toBe('');
        expect(profileRoot(profile({ initialPath: '/' }))).toBe('');
        expect(profileRoot(profile({ initialPath: '  ' }))).toBe('');
    });

    it('is the starting folder, absolute and without a trailing slash', () => {
        expect(profileRoot(profile({ initialPath: '/var/www/' }))).toBe('/var/www');
        expect(profileRoot(profile({ initialPath: 'public_html' }))).toBe('/public_html');
    });

    it('resolves the {username} template some WebDAV presets keep in the start path', () => {
        expect(profileRoot(profile({ initialPath: '/files/{username}/', username: 'ale' }))).toBe('/files/ale');
    });
});

describe('aeroCloudRemoteFolder', () => {
    it('nests the AeroCloud folder under the root of a plain profile, server-absolute', () => {
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/var/www' }), 'cloud')).toBe('/var/www/cloud/');
        expect(aeroCloudRemoteFolder(profile({}), 'cloud')).toBe('/cloud/');
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/var/www' }), ' /nested/dir/ ')).toBe('/var/www/nested/dir/');
    });

    it('syncs the root itself when no sub-folder is given', () => {
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/var/www' }), '')).toBe('/var/www/');
        expect(aeroCloudRemoteFolder(profile({}), '  ')).toBe('/');
    });

    it('nests a crypt-bound profile under its overlay folder, or its starting folder without one', () => {
        const overlay = (remoteScope?: string) =>
            ({ enabled: true, kind: 'aerocrypt', remoteScope }) as ServerProfile['aeroCryptOverlay'];
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/Vault', aeroCryptOverlay: overlay() }), 'cloud'))
            .toBe('/Vault/cloud/');
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/data', aeroCryptOverlay: overlay('/data/enc/') }), 'cloud'))
            .toBe('/data/enc/cloud/');
        // A binding's folder is plaintext-absolute, as the app normalizes it.
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/data', aeroCryptOverlay: overlay('enc') }), 'cloud'))
            .toBe('/enc/cloud/');
        expect(aeroCloudRemoteFolder(profile({ aeroCryptOverlay: overlay() }), 'cloud')).toBe('/cloud/');
    });

    it('ignores the folder of a disabled overlay', () => {
        const disabled = { enabled: false, kind: 'aerocrypt', remoteScope: '/enc' } as ServerProfile['aeroCryptOverlay'];
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/data', aeroCryptOverlay: disabled }), 'cloud'))
            .toBe('/data/cloud/');
    });
});

describe('profileMatchesQuery', () => {
    const p = profile({ name: 'Work NAS', host: 'nas.example.net', username: 'backup', protocol: 'sftp', providerId: 'synology' });

    it('matches everything on an empty query', () => {
        expect(profileMatchesQuery(p, '')).toBe(true);
        expect(profileMatchesQuery(p, '   ')).toBe(true);
    });

    it('matches name, host, user, protocol and provider, case-insensitively', () => {
        expect(profileMatchesQuery(p, 'work')).toBe(true);
        expect(profileMatchesQuery(p, 'NAS.EXAMPLE')).toBe(true);
        expect(profileMatchesQuery(p, 'backup')).toBe(true);
        expect(profileMatchesQuery(p, 'SFTP')).toBe(true);
        expect(profileMatchesQuery(p, 'synology')).toBe(true);
    });

    it('rejects a query no field contains', () => {
        expect(profileMatchesQuery(p, 'dropbox')).toBe(false);
    });
});
