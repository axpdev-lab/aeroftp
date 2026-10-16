// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ConnectionParams } from '../types';
import { resolveLiveConnectionParams } from './liveConnection';

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
