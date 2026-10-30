// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ConnectionParams } from '../types';
import { connectionNumberOf, connectionTargetKey, isSameConnection, resolveLiveConnectionParams, savedProfileIdOf } from './liveConnection';

const EMPTY_FORM: ConnectionParams = { server: '', username: '', password: '' };

const webdav: ConnectionParams = {
    server: 'https://dav.example.test/remote.php/dav',
    username: 'alice',
    password: 'secret',
    protocol: 'webdav',
    savedServerId: 'profile-webdav',
    providerId: 'nextcloud',
};

const sftp: ConnectionParams = {
    server: 'sftp.example.test',
    username: 'bob',
    password: 'other',
    protocol: 'sftp',
    port: 22,
    savedServerId: 'profile-sftp',
};

describe('resolveLiveConnectionParams', () => {
    it('gives the session params after a saved-profile connect reset the form', () => {
        // The Debug Panel case: the connect flow empties the form for the next
        // "Add New Server", the session tab keeps the connection's facts.
        const sessions = [{ id: 's1', connectionParams: webdav }];
        expect(resolveLiveConnectionParams(sessions, 's1', EMPTY_FORM)).toBe(webdav);
    });

    it('gives the form when no session is active', () => {
        const typed: ConnectionParams = { server: 'ftp.example.test', username: 'u', password: 'p', protocol: 'ftp' };
        expect(resolveLiveConnectionParams([], null, typed)).toBe(typed);
        // A cached tab is not the active one: the servers screen and the
        // AeroCloud tab deselect it, and the form is what is being connected.
        expect(resolveLiveConnectionParams([{ id: 's1', connectionParams: webdav }], null, typed)).toBe(typed);
    });

    it('lets the session win over a stale form of another protocol', () => {
        // Servers screen opened over a live WebDAV tab, an SFTP profile opened in
        // the form, then back to the tab: the form says SFTP, the tab is WebDAV.
        const sessions = [{ id: 's1', connectionParams: sftp }, { id: 's2', connectionParams: webdav }];
        const live = resolveLiveConnectionParams(sessions, 's2', sftp);
        expect(live.protocol).toBe('webdav');
        expect(live.server).toBe(webdav.server);
        expect(live.savedServerId).toBe('profile-webdav');
    });

    it('never fills a field the session lacks from the form', () => {
        // An OAuth tab has no server and a preset-less tab no provider id: those
        // are facts of that connection, the form's values belong to another one.
        const oauth: ConnectionParams = { server: '', username: '', password: '', protocol: 'googledrive' };
        const staleForm: ConnectionParams = { ...sftp, providerId: 'filen-desktop-s3' };
        const live = resolveLiveConnectionParams([{ id: 's1', connectionParams: oauth }], 's1', staleForm);
        expect(live).toBe(oauth);
        expect(live.server).toBe('');
        expect(live.providerId).toBeUndefined();
        expect(live.savedServerId).toBeUndefined();
    });

    it('falls back to the form when the active id names no session', () => {
        // An id that names no tab is no active session: it must not erase
        // what the app is connected with.
        expect(resolveLiveConnectionParams([{ id: 's1', connectionParams: webdav }], 'gone', sftp)).toBe(sftp);
    });
});

describe('savedProfileIdOf', () => {
    const quick: ConnectionParams = { server: 'files.example.com:22', username: 'u', password: '', protocol: 'sftp' };

    it('gives the saved profile of the connection or of its session', () => {
        expect(savedProfileIdOf({ ...quick, savedServerId: 'srv_1' }, null)).toBe('srv_1');
        expect(savedProfileIdOf(quick, { savedServerId: 'srv_2' })).toBe('srv_2');
    });

    it('is null for a connection opened without a saved profile, never its host', () => {
        expect(savedProfileIdOf(quick, null)).toBeNull();
        expect(savedProfileIdOf(quick, { savedServerId: null })).toBeNull();
        expect(savedProfileIdOf({ ...quick, savedServerId: '' }, undefined)).toBeNull();
    });
});

describe('isSameConnection', () => {
    const s3: ConnectionParams = {
        server: 's3.example.test',
        username: 'AKIAEXAMPLE',
        password: 'secret',
        protocol: 's3',
        options: { bucket: 'photos', region: 'eu-west-1', endpoint: 'https://s3.example.test' },
        savedServerId: 'profile-photos',
    };

    it('is false for another bucket on the same host and account', () => {
        // The AeroCloud case: a host, protocol and username check took the
        // live session for the cloud profile and synced on the wrong bucket.
        const otherBucket = { ...s3, options: { ...s3.options, bucket: 'backups' }, savedServerId: 'profile-backups' };
        expect(isSameConnection(s3, otherBucket)).toBe(false);
    });

    it('is false for every option that chooses the storage', () => {
        const differing: Array<Record<string, unknown>> = [
            { region: 'us-east-1' },
            { endpoint: 'https://other.example.test' },
            { container: 'archive' },
            { githubBranch: 'release' },
            { drive_id: '42' },
            { roleArn: 'arn:aws:iam::123456789012:role/other' },
            { pathStyle: true },
        ];
        for (const change of differing) {
            const target = { ...s3, options: { ...s3.options, ...change } };
            expect(isSameConnection(s3, target), JSON.stringify(change)).toBe(false);
        }
    });

    it('is false for another protocol, host, port or account', () => {
        expect(isSameConnection(s3, { ...s3, protocol: 'webdav' })).toBe(false);
        expect(isSameConnection(s3, { ...s3, server: 's3.other.test' })).toBe(false);
        expect(isSameConnection(sftp, { ...sftp, port: 2222 })).toBe(false);
        expect(isSameConnection(s3, { ...s3, username: 'AKIAOTHER' })).toBe(false);
    });

    it('is true for the same target opened with another password, profile or one-shot code', () => {
        const live = {
            ...s3,
            password: 'rotated',
            savedServerId: 'profile-duplicate',
            options: { ...s3.options, roleMfaTokenCode: '123456', sessionToken: 'tok', session_expires_at: 1 },
        };
        expect(isSameConnection(live, s3)).toBe(true);
    });

    it('reads a server spelled host:port with that port as the host', () => {
        // My Servers keeps host and port apart; AeroCloud builds host:port.
        const live: ConnectionParams = { ...sftp, server: 'sftp.example.test', port: 2222 };
        const cloud: ConnectionParams = { ...sftp, server: 'sftp.example.test:2222', port: 2222 };
        expect(isSameConnection(live, cloud)).toBe(true);
    });

    it('reads absent, null and empty option values as the same absence', () => {
        const live = { ...webdav, options: { webdavScheme: undefined, accountName: '' } } as unknown as ConnectionParams;
        const target = { ...webdav, options: { accountName: null } } as unknown as ConnectionParams;
        expect(isSameConnection(live, target)).toBe(true);
    });
});

describe('connectionTargetKey', () => {
    const s3 = (options: Record<string, unknown>, extra: Partial<ConnectionParams> = {}): ConnectionParams =>
        ({ protocol: 's3', server: 's3.example.com', username: 'AKIA', password: '', options, ...extra } as ConnectionParams);
    const pairs: [ConnectionParams, ConnectionParams][] = [
        [s3({ bucket: 'photos' }), s3({ bucket: 'backups' })],
        [s3({ bucket: 'photos', region: 'eu' }), s3({ bucket: 'photos', region: 'us' })],
        [s3({ bucket: 'photos' }), s3({ bucket: 'photos', sessionToken: 'x', timeout: 30 })],
        [s3({ bucket: 'photos' }, { port: 9000 }), s3({ bucket: 'photos' }, { port: 9001 })],
        [s3({ githubBranch: 'main' }, { protocol: 'github' }), s3({ githubBranch: 'dev' }, { protocol: 'github' })],
        [s3({ bucket: 'photos', endpoint: '' }), s3({ bucket: 'photos' })],
    ];

    it('is equal for two connections exactly when they are the same connection', () => {
        for (const [a, b] of pairs) {
            expect(connectionTargetKey(a) === connectionTargetKey(b), JSON.stringify([a.options, b.options, a.port, b.port]))
                .toBe(isSameConnection(a, b));
        }
    });

    it('tells buckets, regions, ports and branches apart, not credentials', () => {
        expect(connectionTargetKey(pairs[0][0])).not.toBe(connectionTargetKey(pairs[0][1]));
        expect(connectionTargetKey(pairs[3][0])).not.toBe(connectionTargetKey(pairs[3][1]));
        expect(connectionTargetKey(pairs[4][0])).not.toBe(connectionTargetKey(pairs[4][1]));
        expect(connectionTargetKey(pairs[2][0])).toBe(connectionTargetKey(pairs[2][1]));
    });
});

describe('connectionNumberOf', () => {
    const filelu = (apiKey: string): ConnectionParams =>
        ({ protocol: 'filelu', server: 'filelu.com', username: '', password: apiKey } as ConnectionParams);

    it('tells apart two accounts that differ only in a credential', () => {
        // Same target, as far as connectionTargetKey can see.
        const [one, two] = [filelu('key-one'), filelu('key-two')];
        expect(connectionTargetKey(one)).toBe(connectionTargetKey(two));
        expect(connectionNumberOf(one)).not.toBe(connectionNumberOf(two));
        // And two connects of one profile: each copies its own params.
        expect(connectionNumberOf(filelu('key-one'))).not.toBe(connectionNumberOf(one));
    });

    it('stays the same for as long as a connection lasts', () => {
        // App resolves the live params anew on every change of its sessions;
        // a session update spreads the session and keeps its params object.
        const params = filelu('key-one');
        const session = { id: 's1', connectionParams: params };
        const first = connectionNumberOf(resolveLiveConnectionParams([session], 's1', EMPTY_FORM));
        const updated = [{ ...session, remotePath: '/docs' }, { id: 's2', connectionParams: filelu('key-two') }];
        expect(connectionNumberOf(resolveLiveConnectionParams(updated, 's1', EMPTY_FORM))).toBe(first);
        expect(connectionNumberOf(params)).toBe(first);
    });
});
