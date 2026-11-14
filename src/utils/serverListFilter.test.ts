// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ServerProfile } from '../types';
import {
    countSelectedOutside,
    deriveProviderId,
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

describe('deriveProviderId MEGA S4 host boundary', () => {
    const s3 = (host: string) => ({ name: 's', host, username: '', protocol: 's3' as const });

    it('classifies the current zone, the legacy zone, and mega.nz', () => {
        expect(deriveProviderId(s3('s3.eu-amsterdam-1.megas4.com'))).toBe('mega-s4');
        expect(deriveProviderId(s3('https://bucket.s3.ca-vancouver-1.megas4.com:443/bucket'))).toBe('mega-s4');
        expect(deriveProviderId(s3('s3.eu-central-1.s4.mega.io'))).toBe('mega-s4');
        expect(deriveProviderId(s3('https://mega.nz'))).toBe('mega-s4');
    });

    it('rejects lookalikes the old substring match accepted', () => {
        expect(deriveProviderId(s3('notmega.io'))).toBeUndefined();
        expect(deriveProviderId(s3('mega.io.example'))).toBeUndefined();
        expect(deriveProviderId(s3('s4.mega.io.example'))).toBeUndefined();
        expect(deriveProviderId(s3('notmegas4.com'))).toBeUndefined();
        expect(deriveProviderId(s3('megas4.com.evil.com'))).toBeUndefined();
        expect(deriveProviderId(s3('evilmega.nz'))).toBeUndefined();
    });
});

describe('deriveProviderId Tigris host boundary', () => {
    const s3 = (host: string) => ({ name: 's', host, username: '', protocol: 's3' as const });

    it('classifies the current host, the legacy host and bucket subdomains', () => {
        expect(deriveProviderId(s3('t3.storage.dev'))).toBe('tigris');
        expect(deriveProviderId(s3('https://t3.storage.dev:443/bucket'))).toBe('tigris');
        expect(deriveProviderId(s3('https://my-bucket.t3.storage.dev'))).toBe('tigris');
        expect(deriveProviderId(s3('fly.storage.tigris.dev'))).toBe('tigris');
        expect(deriveProviderId(s3('https://my-bucket.fly.storage.tigris.dev/key'))).toBe('tigris');
    });

    it('keeps a bucket named after another vendor on Tigris', () => {
        for (const bucket of ['backblaze-backups', 'wasabi-archive', 'idrive-copy', 'storj-mirror']) {
            expect(deriveProviderId(s3(`${bucket}.t3.storage.dev`))).toBe('tigris');
            expect(deriveProviderId(s3(`https://${bucket}.fly.storage.tigris.dev`))).toBe('tigris');
        }
        // Not a Backblaze host either: vendor words count only as a domain
        // (src/shared/s3-host-map.json), never as a substring (#1506 Requested 173).
        expect(deriveProviderId(s3('backblaze-backups.t3.storage.dev.example.com'))).toBeUndefined();
    });

    it('rejects lookalikes', () => {
        expect(deriveProviderId(s3('t3.storage.dev.example.com'))).toBeUndefined();
        expect(deriveProviderId(s3('not-t3.storage.dev'))).toBeUndefined();
        expect(deriveProviderId(s3('fly.storage.tigris.dev.evil.com'))).toBeUndefined();
        expect(deriveProviderId(s3('myfly.storage.tigris.dev'))).toBeUndefined();
        expect(deriveProviderId(s3('tigris.dev'))).toBeUndefined();
        expect(deriveProviderId(s3('storage.dev'))).toBeUndefined();
    });
});

describe('deriveProviderId Neon and Puter host boundaries', () => {
    const s3 = (host: string) => ({ name: 's', host, username: '', protocol: 's3' as const });
    const webdav = (host: string) => ({ name: 's', host, username: '', protocol: 'webdav' as const });

    it('classifies Neon per-branch endpoints', () => {
        expect(deriveProviderId(s3('br-misty-pine-bagvej1p.storage.c-8.eu-central-1.aws.neon.tech'))).toBe('neon-s3');
        expect(deriveProviderId(s3('https://br-x.storage.c-1.us-east-1.aws.neon.tech:443/bucket'))).toBe('neon-s3');
    });

    it('rejects Neon lookalikes', () => {
        expect(deriveProviderId(s3('aws.neon.tech.example.com'))).toBeUndefined();
        expect(deriveProviderId(s3('notaws.neon.tech'))).toBeUndefined();
        expect(deriveProviderId(s3('neon.tech'))).toBeUndefined();
    });

    it('classifies the Puter WebDAV host and rejects lookalikes', () => {
        expect(deriveProviderId(webdav('dav.puter.com'))).toBe('puter-webdav');
        expect(deriveProviderId(webdav('https://dav.puter.com/user'))).toBe('puter-webdav');
        expect(deriveProviderId(webdav('dav.puter.com.evil.com'))).not.toBe('puter-webdav');
        expect(deriveProviderId(webdav('notdav.puter.com'))).not.toBe('puter-webdav');
        expect(deriveProviderId(webdav('puter.com'))).not.toBe('puter-webdav');
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

describe('deriveProviderId LifetimeCloud host boundary', () => {
    const dav = (host: string) => ({ name: 'd', host, username: 'alice', protocol: 'webdav' as const });

    it('matches exact lifetimecloud.me and real subdomains before the cloud. Nextcloud rule', () => {
        expect(deriveProviderId(dav('https://lifetimecloud.me/dav/alice/'))).toBe('lifetimecloud');
        expect(deriveProviderId(dav('https://webdav.lifetimecloud.me/dav/{username}/'))).toBe('lifetimecloud');
        expect(deriveProviderId(dav('https://user@lifetimecloud.me:443/dav/alice?foo=bar'))).toBe('lifetimecloud');
    });

    it('rejects lookalike hosts and keeps Nextcloud hosts on nextcloud', () => {
        expect(deriveProviderId(dav('https://notlifetimecloud.me/dav/alice/'))).not.toBe('lifetimecloud');
        expect(deriveProviderId(dav('https://lifetimecloud.me.example/dav/alice/'))).not.toBe('lifetimecloud');
        expect(deriveProviderId(dav('https://evil-lifetimecloud.me/dav/alice/'))).not.toBe('lifetimecloud');
        expect(deriveProviderId(dav('https://lifetimecloud-host.example/dav/alice/'))).toBeUndefined();
        expect(deriveProviderId(dav('https://cloud.example.com/remote.php/dav/files/alice/'))).toBe('nextcloud');
        expect(deriveProviderId(dav('https://nextcloud.example.com/'))).toBe('nextcloud');
    });
});

describe('deriveProviderId preset host rules cannot shadow one another', () => {
    const dav = (host: string) => ({ name: 'd', host, username: 'alice', protocol: 'webdav' as const });
    // Every rule keyword, as it could appear in a user name or a folder of the
    // URL path. A host-bound rule claims its host whatever the path says.
    const KEYWORDS = ['koofr', 'mail.ru', 'dav.puter.com', 'lifetimecloud.me', 'nextcloud', 'cloud.', 'seafile',
        'jianguoyun', 'cloudme', 'drivehq', 'teracloud', 'filelu', 'felicloud', 'tab.digital'];
    const HOST_RULES: Array<[string, string]> = [
        ['app.koofr.net', 'koofr-webdav'],
        ['webdav.cloud.mail.ru', 'mailru-cloud'],
        ['dav.puter.com', 'puter-webdav'],
        ['lifetimecloud.me', 'lifetimecloud'],
    ];

    it('claims each preset host whatever its path names', () => {
        for (const [host, id] of HOST_RULES) {
            for (const kw of KEYWORDS) {
                expect(deriveProviderId(dav(`https://${host}/dav/${kw}/`)), `${host} /${kw}/`).toBe(id);
            }
        }
        expect(deriveProviderId(dav('https://app.koofr.net/dav/Koofr'))).toBe('koofr-webdav');
    });

    it('claims Koofr on its domain only', () => {
        expect(deriveProviderId(dav('https://app.koofr.net/dav/Koofr'))).toBe('koofr-webdav');
        expect(deriveProviderId(dav('https://koofr.net/dav/'))).toBe('koofr-webdav');
        expect(deriveProviderId(dav('https://koofr.evil.example/dav/'))).not.toBe('koofr-webdav');
        expect(deriveProviderId(dav('https://notkoofr.net/dav/'))).not.toBe('koofr-webdav');
        expect(deriveProviderId(dav('https://app.koofr.net.evil.example/dav/'))).not.toBe('koofr-webdav');
    });

    it('never gives a host-bound preset to a keyword found only in the path', () => {
        // The remaining substring rules still read the whole URL on purpose
        // (a self-hosted Nextcloud under a sub-path); the host-bound ones never do.
        const hostBound = HOST_RULES.map(([, id]) => id);
        for (const kw of KEYWORDS) {
            const got = deriveProviderId(dav(`https://nas.example/dav/${kw}/`));
            expect(hostBound, `/${kw}/ gave ${got}`).not.toContain(got);
        }
    });
});
