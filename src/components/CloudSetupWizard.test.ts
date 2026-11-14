// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { aeroCloudBase, aeroCloudRemoteFolder, profileRoot } from './CloudSetupWizard';
import { profileMatchesQuery } from './common/ProfileRail';
import { profileOverlayScope, resolveUsernameTemplate } from '../utils/cryptScope';
import type { ServerProfile } from '../types';
import usernameCases from '../shared/username-template-cases.json';

const profile = (fields: Partial<ServerProfile>): ServerProfile =>
    ({ id: 'srv_1', name: 'Profile', host: 'example.com', port: 21, username: '', ...fields }) as ServerProfile;

describe('the {username} placeholder', () => {
    // The same table the backend's `profile_root` and `profile_overlay_scope`
    // pass: both sides insert the user name into the same folder name. A null
    // path is a blank user name, which leaves the template as written.
    const template = usernameCases.pathTemplate;
    it.each(usernameCases.cases)('$user', ({ user, path }) => {
        const expected = path ?? template;
        const folder = expected.replace(/\/+$/, '');
        expect(resolveUsernameTemplate(template, user)).toBe(expected);
        const webdav = { protocol: 'webdav', username: user } as Partial<ServerProfile>;
        expect(profileRoot(profile({ ...webdav, initialPath: template }))).toBe(folder);
        expect(profileOverlayScope({ ...webdav, initialPath: template } as ServerProfile)).toBe(expected);
        const bound = profile({
            ...webdav,
            initialPath: '/',
            aeroCryptOverlay: { enabled: true, kind: 'aerocrypt', remoteScope: template },
        } as Partial<ServerProfile>);
        expect(profileOverlayScope(bound)).toBe(expected);
        expect(aeroCloudBase(bound)).toBe(folder);
        expect(aeroCloudRemoteFolder(bound, 'cloud')).toBe(`${folder}/cloud/`);
        const sftp = profile({ protocol: 'sftp', username: user, initialPath: template.slice(1) });
        expect(aeroCloudBase(sftp)).toBe(`~/${folder.slice(1)}`);
    });
});

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
        expect(profileRoot(profile({ initialPath: '/{username}/files/{username}', username: 'ale' }))).toBe('/ale/files/ale');
    });

    it('keeps the spaces of the starting folder, as the backend does', () => {
        expect(profileRoot(profile({ initialPath: '/var/www ' }))).toBe('/var/www ');
        expect(profileRoot(profile({ initialPath: '  /var/www' }))).toBe('/  /var/www');
    });
});

describe('aeroCloudBase', () => {
    it('keeps a login-relative profile base in ~/ form for the backend to place', () => {
        for (const protocol of ['sftp', 'scp', 'ftp', 'ftps'] as const) {
            expect(aeroCloudBase(profile({ protocol, initialPath: 'docs' }))).toBe('~/docs');
            expect(aeroCloudBase(profile({ protocol, initialPath: '/srv' }))).toBe('/srv');
        }
        expect(aeroCloudBase(profile({ protocol: 'webdav', initialPath: '/var/www' }))).toBe('/var/www');
        expect(aeroCloudBase(profile({ initialPath: '/var/www' }))).toBe('/var/www');
    });

    it('prefers the overlay folder, in ~/ form when it is relative', () => {
        const overlay = (remoteScope?: string) =>
            ({ enabled: true, kind: 'aerocrypt', remoteScope }) as ServerProfile['aeroCryptOverlay'];
        expect(aeroCloudBase(profile({ protocol: 'ftp', initialPath: 'docs', aeroCryptOverlay: overlay('~/vault') })))
            .toBe('~/vault');
        expect(aeroCloudBase(profile({ protocol: 'ftp', initialPath: 'docs', aeroCryptOverlay: overlay('vault') })))
            .toBe('~/vault');
        expect(aeroCloudBase(profile({ protocol: 'ftp', initialPath: 'docs', aeroCryptOverlay: overlay('/abs/vault') })))
            .toBe('/abs/vault');
        expect(aeroCloudRemoteFolder(profile({ protocol: 'ftp', initialPath: 'docs' }), 'cloud'))
            .toBe('~/docs/cloud/');
    });

    it('reads an omitted protocol as FTP, the ServerProfile default', () => {
        expect(aeroCloudBase(profile({ initialPath: 'docs' }))).toBe('~/docs');
        expect(aeroCloudRemoteFolder(profile({ initialPath: 'docs' }), 'sub')).toBe('~/docs/sub/');
    });

    it('treats an all-blank starting folder as the server root on login-relative protocols', () => {
        for (const protocol of ['sftp', 'ftp'] as const) {
            expect(aeroCloudBase(profile({ protocol, initialPath: '  ' }))).toBe('');
            expect(aeroCloudRemoteFolder(profile({ protocol, initialPath: '  ' }), 'sub'))
                .toBe('/sub/');
        }
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
        expect(aeroCloudRemoteFolder(profile({ protocol: 'webdav', initialPath: '/data', aeroCryptOverlay: overlay('enc') }), 'cloud'))
            .toBe('/enc/cloud/');
        // On FTP (the omitted-protocol default) a relative scope opens under
        // the login folder, the same rule `initialPath` follows.
        expect(aeroCloudRemoteFolder(profile({ initialPath: '/data', aeroCryptOverlay: overlay('enc') }), 'cloud'))
            .toBe('~/enc/cloud/');
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
