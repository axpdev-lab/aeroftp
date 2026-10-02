// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ServerProfile } from '../types';
import {
    countSelectedOutside,
    filterServersByQuery,
    getServerSearchText,
    matchesServerQuery,
    selectShownLabel,
    toggleVisibleSelection,
} from './serverListFilter';

const profile = (p: Partial<ServerProfile> & Pick<ServerProfile, 'id' | 'name' | 'host'>): ServerProfile => ({
    port: 22,
    username: '',
    protocol: 'sftp',
    ...p,
} as ServerProfile);

// A slice of a real lab: several profiles of one host family on Hetzner,
// plus unrelated ones that share a protocol or a user with them.
const LAB: ServerProfile[] = [
    profile({ id: 'a', name: 'axpbuntu SFTP', host: 'lab1.hetzner.example', username: 'root' }),
    profile({ id: 'b', name: 'Lab FTP', host: 'AXPBUNTU.hetzner.example', protocol: 'ftp', port: 21, username: 'ftplab' }),
    profile({ id: 'c', name: 'Lab WebDAV', host: 'dav.hetzner.example', protocol: 'webdav', port: 443, username: 'axpbuntu-dav' }),
    profile({ id: 'd', name: 'Office NAS', host: 'nas.local', protocol: 'sftp', username: 'admin' }),
    profile({ id: 'e', name: 'Website', host: 'www.example.org', protocol: 'ftps', port: 21, username: 'deploy' }),
];

const ids = (servers: { id: string }[]) => servers.map(s => s.id);

describe('server list filter predicate', () => {
    it('matches name, host and username case-insensitively', () => {
        expect(ids(filterServersByQuery(LAB, 'axpbuntu'))).toEqual(['a', 'b', 'c']);
        expect(ids(filterServersByQuery(LAB, 'AXPBUNTU'))).toEqual(['a', 'b', 'c']);
    });

    it('matches the protocol', () => {
        expect(ids(filterServersByQuery(LAB, 'ftps'))).toEqual(['e']);
        expect(ids(filterServersByQuery(LAB, 'webdav'))).toEqual(['c']);
    });

    it('ANDs whitespace-separated terms across fields', () => {
        // "axpbuntu" comes from name/host/user, "webdav" from the protocol.
        expect(ids(filterServersByQuery(LAB, 'axpbuntu webdav'))).toEqual(['c']);
        // Terms are substrings: "ftp" also matches "sftp".
        expect(ids(filterServersByQuery(LAB, 'axpbuntu ftp'))).toEqual(['a', 'b']);
        expect(ids(filterServersByQuery(LAB, '  hetzner   root '))).toEqual(['a']);
        expect(filterServersByQuery(LAB, 'axpbuntu nas')).toEqual([]);
    });

    it('returns every server, in order, for a blank query', () => {
        expect(ids(filterServersByQuery(LAB, ''))).toEqual(ids(LAB));
        expect(ids(filterServersByQuery(LAB, '   '))).toEqual(ids(LAB));
    });

    it('does not match a term that occurs in no field', () => {
        expect(filterServersByQuery(LAB, 'zzz-nothing')).toEqual([]);
        expect(matchesServerQuery('axpbuntu sftp', 'axpbuntu dav')).toBe(false);
    });

    it('uses the cached text a caller passes instead of rebuilding it', () => {
        const cached = new Map(LAB.map(s => [s.id, s.id === 'd' ? 'only-in-cache' : '']));
        expect(ids(filterServersByQuery(LAB, 'only-in-cache', s => cached.get(s.id) ?? ''))).toEqual(['d']);
    });

    it('searches bridge import previews, which carry only the basic fields', () => {
        const preview = [
            { id: 'x', name: 'axpbuntu', host: '10.0.0.1', username: 'u', protocol: 'sftp' },
            { id: 'y', name: 'other', host: '10.0.0.2', username: 'u' },
        ];
        expect(ids(filterServersByQuery(preview, 'AXPBUNTU'))).toEqual(['x']);
    });

    it('keeps a stale providerId out of the search text (issue #318)', () => {
        const text = getServerSearchText(profile({
            id: 'o', name: 'Drive', host: '', protocol: 'opendrive', providerId: 'opendrive-webdav',
        }));
        expect(text).not.toContain('webdav');
    });
});

describe('select all on a filtered checklist', () => {
    const all = ids(LAB);

    it('selects only the rows on screen and keeps hidden selections', () => {
        // "Office NAS" was selected before the user typed the filter.
        const before = new Set(['d']);
        const shown = ids(filterServersByQuery(LAB, 'axpbuntu'));
        const after = toggleVisibleSelection(before, shown);
        expect([...after].sort()).toEqual(['a', 'b', 'c', 'd']);
        expect(after.has('e')).toBe(false);
    });

    it('deselects only the rows on screen once they are all selected', () => {
        const before = new Set(all);
        const shown = ids(filterServersByQuery(LAB, 'axpbuntu'));
        const after = toggleVisibleSelection(before, shown);
        expect([...after].sort()).toEqual(['d', 'e']);
    });

    it('completes a partial selection of the shown rows instead of clearing it', () => {
        const shown = ['a', 'b', 'c'];
        expect([...toggleVisibleSelection(new Set(['a']), shown)].sort()).toEqual(['a', 'b', 'c']);
    });

    it('is a no-op when the filter shows nothing', () => {
        const before = new Set(['d']);
        expect([...toggleVisibleSelection(before, [])]).toEqual(['d']);
    });

    it('does not mutate the selection it was given', () => {
        const before = new Set(['d']);
        toggleVisibleSelection(before, ['a']);
        expect([...before]).toEqual(['d']);
    });

    it('counts the selected rows the filter hides', () => {
        const shown = ids(filterServersByQuery(LAB, 'axpbuntu'));
        expect(countSelectedOutside(new Set(['a', 'd', 'e']), all, shown)).toBe(2);
        expect(countSelectedOutside(new Set(['a', 'b']), all, shown)).toBe(0);
        // An id no longer in the list is not counted as hidden.
        expect(countSelectedOutside(new Set(['gone']), all, shown)).toBe(0);
    });

    it('names the filtered count on the button while a filter is active', () => {
        const shown = ['a', 'b', 'c'];
        expect(selectShownLabel(new Set(), shown, true)).toEqual({ key: 'settings.selectShown', params: { count: 3 } });
        expect(selectShownLabel(new Set(shown), shown, true)).toEqual({ key: 'settings.deselectShown', params: { count: 3 } });
        expect(selectShownLabel(new Set(), all, false)).toEqual({ key: 'settings.selectAll' });
        expect(selectShownLabel(new Set(all), all, false)).toEqual({ key: 'settings.deselectAll' });
    });
});
