// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Cross-checks the TS dedup logic against the same six scenarios the Rust
// twin (src-tauri/src/storage_dedup.rs) covers under cargo test. The two
// implementations need not produce byte-identical dedup keys (TS uses a fast
// non-crypto hash, Rust uses SHA-256/12): only the aggregation result must
// agree on the same input data.

import { describe, expect, it } from 'vitest';
import { ServerProfile } from '../types';
import {
    aggregateByDedupKey,
    getStorageDedupKey,
    normalizeHost,
    normalizeUser,
} from './storageDedup';

interface MakeProfileOpts {
    id: string;
    name?: string;
    protocol: ServerProfile['protocol'];
    providerId?: string;
    host?: string;
    port?: number;
    username?: string;
    used?: number;
    total?: number;
}

const make = (opts: MakeProfileOpts): ServerProfile => ({
    id: opts.id,
    name: opts.name ?? opts.id,
    host: opts.host ?? '',
    port: opts.port ?? 0,
    username: opts.username ?? '',
    protocol: opts.protocol,
    providerId: opts.providerId,
    lastQuota:
        typeof opts.used === 'number' && typeof opts.total === 'number'
            ? { used: opts.used, total: opts.total, fetched_at: '2026-05-03T00:00:00Z' }
            : undefined,
});

describe('storageDedup', () => {
    it('case 1: Koofr WebDAV + REST collapse to a single drive', () => {
        // Two Koofr surfaces of the same account land in the same family bucket
        // (`family:koofr:<user>`), so the deduped quota counts only once.
        const profiles = [
            make({
                id: 'k1',
                protocol: 'webdav',
                providerId: 'koofr-webdav',
                host: 'https://app.koofr.net/dav/Koofr',
                port: 443,
                username: 'user@example.com',
                used: 2_000_000_000,
                total: 10_000_000_000,
            }),
            make({
                id: 'k2',
                protocol: 'koofr',
                providerId: 'koofr',
                username: 'user@example.com',
                used: 2_000_000_000,
                total: 10_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.profiles).toBe(2);
        expect(summary.uniqueCount).toBe(1);
        expect(summary.totalUsed).toBe(2_000_000_000);
        expect(summary.totalTotal).toBe(10_000_000_000);
        expect(summary.dedupedQuotaCount).toBe(1);
        expect(getStorageDedupKey(profiles[0])).toBe('family:koofr:user@example.com');
        expect(getStorageDedupKey(profiles[0])).toBe(getStorageDedupKey(profiles[1]));
    });

    it('Proton profiles of one account are one drive, in either mode', () => {
        const cli = make({ id: 'c', protocol: 'proton', username: 'Someone@Proton.me' });
        const native = make({ id: 'n', protocol: 'proton', username: 'someone@proton.me' });
        const other = make({ id: 'o', protocol: 'proton', username: 'other@proton.me' });
        expect(getStorageDedupKey(native)).toBe('api:proton:someone@proton.me');
        expect(getStorageDedupKey(cli)).toBe(getStorageDedupKey(native));
        expect(getStorageDedupKey(other)).not.toBe(getStorageDedupKey(native));
    });

    it('case 2: Filen with different usernames keeps two keys', () => {
        const profiles = [
            make({
                id: 'f1', protocol: 'filen', providerId: 'filen',
                username: 'alice@proton.me',
                used: 1_000_000, total: 1_000_000_000,
            }),
            make({
                id: 'f2', protocol: 'filen', providerId: 'filen',
                username: 'bob@proton.me',
                used: 2_000_000, total: 1_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(2);
        expect(summary.totalUsed).toBe(3_000_000);
        expect(summary.totalTotal).toBe(2_000_000_000);
    });

    it('case 3: SFTP same host different ports stays separate', () => {
        const profiles = [
            make({ id: 's1', protocol: 'sftp', host: 'nas.local', port: 22, username: 'axp' }),
            make({ id: 's2', protocol: 'sftp', host: 'nas.local', port: 2222, username: 'axp' }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(2);
    });

    it('SFTP and SCP to the same SSH server are one storage, counted under SFTP', () => {
        const profiles = [
            make({ id: 's1', protocol: 'sftp', host: 'router.lan', port: 22, username: 'root', used: 10, total: 100 }),
            make({ id: 'c1', protocol: 'scp', host: 'router.lan', username: 'root', used: 10, total: 100 }),
            make({ id: 'c2', protocol: 'scp', host: 'router.lan', port: 2222, username: 'root' }),
        ];
        const summary = aggregateByDedupKey(profiles);
        // Port 0 means the default, 22, so c1 is s1's server; c2 is another port.
        expect(summary.uniqueCount).toBe(2);
        expect(summary.totalTotal).toBe(100);
        expect(summary.byProtocolClass.map(r => r.protocolClass)).toEqual(['SFTP']);
        expect(summary.byProtocolClass[0].profiles).toBe(3);
    });

    it('case 4: Wasabi with the same access key dedups to one', () => {
        const profiles = [
            make({
                id: 'w1', protocol: 's3', providerId: 'wasabi',
                host: 's3.wasabisys.com', port: 443,
                username: 'AKIAEXAMPLE12345',
                used: 50_000_000_000, total: 1_000_000_000_000,
            }),
            make({
                id: 'w2', protocol: 's3', providerId: 'wasabi',
                host: 's3.eu-west-1.wasabisys.com', port: 443,
                username: 'AKIAEXAMPLE12345',
                used: 50_000_000_000, total: 1_000_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(1);
        expect(summary.totalUsed).toBe(50_000_000_000);
        expect(summary.totalTotal).toBe(1_000_000_000_000);
        expect(summary.dedupedQuotaCount).toBe(1);
    });

    it('case 4b: Filebase with the same access key dedups to one (S3 preset)', () => {
        const profiles = [
            make({
                id: 'fb1', protocol: 's3', providerId: 'filebase',
                host: 's3.filebase.io', port: 443,
                username: 'FBACCESSKEY12345',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
            make({
                id: 'fb2', protocol: 's3', providerId: 'filebase',
                host: 's3.filebase.io', port: 443,
                username: 'FBACCESSKEY12345',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(1);
        expect(summary.totalUsed).toBe(1_000_000_000);
        expect(summary.totalTotal).toBe(5_000_000_000);
        expect(summary.dedupedQuotaCount).toBe(1);
    });

    it('case 4b2: Tigris keys by preset id, so the current and the legacy host of one key are one account', () => {
        const current = make({
            id: 'tg1', protocol: 's3', providerId: 'tigris',
            host: 't3.storage.dev', port: 443, username: 'tid_example_key',
        });
        const legacy = make({
            id: 'tg2', protocol: 's3', providerId: 'tigris',
            host: 'fly.storage.tigris.dev', port: 443, username: 'tid_example_key',
        });
        expect(getStorageDedupKey(current)).toMatch(/^s3:tigris:/);
        expect(getStorageDedupKey(legacy)).toBe(getStorageDedupKey(current));
    });

    it('case 4c: Mail.ru Cloud keys by preset id, matching the Rust dedup_key', () => {
        // Regression: the frontend mirror missed 'mailru-cloud', so the GUI
        // logged webdav-host:... while Rust computed webdav:mailru-cloud:....
        const profile = make({
            id: 'm1', protocol: 'webdav', providerId: 'mailru-cloud',
            host: 'https://webdav.cloud.mail.ru', port: 443,
            username: 'aeroftp@mail.ru',
            used: 1_000_000, total: 8_000_000_000,
        });
        expect(getStorageDedupKey(profile)).toBe('webdav:mailru-cloud:aeroftp@mail.ru');
    });

    it('case 4c2: Puter token profiles of two accounts stay two drives', () => {
        // Token auth uses the literal username `-token` for every account;
        // the account lives in the server URL path, so the key must carry it
        // or alice and bob collapse into one drive.
        const alice = make({
            id: 'p1', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com/alice', port: 443,
            username: '-token',
            used: 1_000_000, total: 25_000_000,
        });
        const bob = make({
            id: 'p2', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'dav.puter.com/bob', port: 443,
            username: '-token',
            used: 2_000_000, total: 25_000_000,
        });
        expect(getStorageDedupKey(alice)).toBe('webdav:puter-webdav:alice');
        expect(getStorageDedupKey(bob)).toBe('webdav:puter-webdav:bob');
        // Password auth of the same account produces the same key: the
        // account path is the identity, not the auth username.
        const alicePassword = make({
            id: 'p1b', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com/alice', port: 443,
            username: 'alice',
        });
        expect(getStorageDedupKey(alicePassword)).toBe(getStorageDedupKey(alice));
        const summary = aggregateByDedupKey([alice, bob]);
        expect(summary.uniqueCount).toBe(2);
        expect(summary.totalUsed).toBe(3_000_000);
    });

    it('case 4c5: the unchanged Puter default URL keys by the user name', () => {
        // The form saves `https://dav.puter.com/{username}` unchanged, so
        // the placeholder must not become the account of every profile.
        const template = 'https://dav.puter.com/{username}';
        const puter = (id: string, host: string, username: string, used = 0) => make({
            id, protocol: 'webdav', providerId: 'puter-webdav',
            host, port: 443, username, used, total: 25_000_000,
        });
        const alice = puter('t1', template, 'alice', 1_000_000);
        const bob = puter('t2', template, 'bob', 2_000_000);
        expect(getStorageDedupKey(alice)).toBe('webdav:puter-webdav:alice');
        expect(getStorageDedupKey(bob)).toBe('webdav:puter-webdav:bob');
        const summary = aggregateByDedupKey([alice, bob]);
        expect(summary.uniqueCount).toBe(2);
        expect(summary.totalUsed).toBe(3_000_000);
        // The same account written with the expanded URL is the same drive.
        expect(getStorageDedupKey(puter('t3', 'https://dav.puter.com/alice', 'alice')))
            .toBe(getStorageDedupKey(alice));
        // The percent-encoded spelling of the placeholder is the same case.
        expect(getStorageDedupKey(puter('t4', 'https://dav.puter.com/%7Busername%7D', 'alice')))
            .toBe(getStorageDedupKey(alice));
        // Under token auth the placeholder resolves to `-token`, which names
        // no account: each profile keeps its own id.
        expect(getStorageDedupKey(puter('t5', template, '-token'))).toBe('id:t5');
    });

    it('case 4c3: a Puter token profile with no account path never dedups', () => {
        // Without the account in the URL two `-token` profiles are
        // indistinguishable, so each falls back to its own id.
        const a = make({
            id: 'p3', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'dav.puter.com', port: 443, username: '-token',
        });
        const b = make({
            id: 'p4', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com', port: 443, username: '-token',
        });
        expect(getStorageDedupKey(a)).toBe('id:p3');
        expect(getStorageDedupKey(b)).toBe('id:p4');
        // Password auth still keys by the real username even without a path.
        const c = make({
            id: 'p5', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'dav.puter.com', port: 443, username: 'carol',
        });
        expect(getStorageDedupKey(c)).toBe('webdav:puter-webdav:carol');
        // Side by side, the same account with no account path: the password
        // profile keys by its user name, the token profile cannot name the
        // account at all (`-token` is shared by every account), so it keys by
        // its own id. Merging them would need the account path; without it
        // they stay two entries.
        const carolTokenNoPath = make({
            id: 'p9', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com', port: 443, username: '-token',
        });
        expect(getStorageDedupKey(carolTokenNoPath)).toBe('id:p9');
        expect(getStorageDedupKey(carolTokenNoPath)).not.toBe(getStorageDedupKey(c));
        expect(aggregateByDedupKey([c, carolTokenNoPath]).uniqueCount).toBe(2);
        // The password profile with no path and the token profile with the
        // account path name the same drive, so they share one key.
        const carolToken = make({
            id: 'p6', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com/carol', port: 443, username: '-token',
        });
        expect(getStorageDedupKey(carolToken)).toBe(getStorageDedupKey(c));
        // An account written only in the query or fragment is no account
        // path: the key falls back as if there were none.
        const queryOnly = make({
            id: 'p7', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com?next=/carol', port: 443, username: '-token',
        });
        expect(getStorageDedupKey(queryOnly)).toBe('id:p7');
        const fragmentOnly = make({
            id: 'p8', protocol: 'webdav', providerId: 'puter-webdav',
            host: 'https://dav.puter.com#/carol', port: 443, username: 'dave',
        });
        expect(getStorageDedupKey(fragmentOnly)).toBe('webdav:puter-webdav:dave');
    });

    it('case 4c4: user-hosted WebDAV presets key by host too', () => {
        // nextcloud, seafile and infinicloud take the server URL from the
        // user: the same user name on two servers is two drives, while case
        // and the default port do not split one.
        const a = make({
            id: 'n1', protocol: 'webdav', providerId: 'nextcloud',
            host: 'https://Cloud-A.example.com', port: 443, username: 'alice',
        });
        const b = make({
            id: 'n2', protocol: 'webdav', providerId: 'nextcloud',
            host: 'cloud-b.example.com', port: 443, username: 'alice',
        });
        expect(getStorageDedupKey(a)).toBe('webdav:nextcloud:cloud-a.example.com:alice');
        expect(getStorageDedupKey(b)).toBe('webdav:nextcloud:cloud-b.example.com:alice');
        const summary = aggregateByDedupKey([a, b]);
        expect(summary.uniqueCount).toBe(2);
        // Same host spelled differently, and the implicit vs explicit 443,
        // are the same drive.
        const c = make({
            id: 'n3', protocol: 'webdav', providerId: 'nextcloud',
            host: 'cloud-a.example.com', port: 0, username: 'alice',
        });
        expect(getStorageDedupKey(c)).toBe(getStorageDedupKey(a));
        // A non-default port is a different endpoint.
        const d = make({
            id: 'n4', protocol: 'webdav', providerId: 'nextcloud',
            host: 'cloud-a.example.com', port: 8443, username: 'alice',
        });
        expect(getStorageDedupKey(d)).toBe('webdav:nextcloud:cloud-a.example.com:8443:alice');
        // seafile and infinicloud follow the same rule.
        const s = make({
            id: 's1', protocol: 'webdav', providerId: 'seafile',
            host: 'https://seafile.example.com', port: 443, username: 'bob',
        });
        expect(getStorageDedupKey(s)).toBe('webdav:seafile:seafile.example.com:bob');
        const i = make({
            id: 'i1', protocol: 'webdav', providerId: 'infinicloud',
            host: 'https://alice-node.teracloud.jp', port: 443, username: 'alice@example.com',
        });
        expect(getStorageDedupKey(i)).toBe('webdav:infinicloud:alice-node.teracloud.jp:alice@example.com');
        // An embedded `:port` in the host and the port field name the same
        // endpoint: both spellings, and the explicit-443 forms, key alike.
        const embedded = make({
            id: 'n5', protocol: 'webdav', providerId: 'nextcloud',
            host: 'cloud-a.example.com:8443', port: 8443, username: 'alice',
        });
        expect(getStorageDedupKey(embedded)).toBe(getStorageDedupKey(d));
        const embedded443 = make({
            id: 'n6', protocol: 'webdav', providerId: 'nextcloud',
            host: 'https://cloud-a.example.com:443', port: 0, username: 'alice',
        });
        expect(getStorageDedupKey(embedded443)).toBe(getStorageDedupKey(a));
    });

    it('normalizeHost strips URL userinfo so secrets never reach a key', () => {
        expect(normalizeHost('https://user:pass@nas.example.com:8080/dav')).toBe('nas.example.com:8080');
        expect(normalizeHost('user:pass@nas.example.com/dav')).toBe('nas.example.com');
    });

    it('normalizeHost strips the query and fragment so secrets never reach a key', () => {
        expect(normalizeHost('https://cloud.example.com?access_token=SECRET')).toBe('cloud.example.com');
        expect(normalizeHost('cloud.example.com:8443#SECRET')).toBe('cloud.example.com:8443');
        expect(normalizeHost('https://cloud.example.com/dav?access_token=SECRET')).toBe('cloud.example.com');
        // An `@` inside the query is not userinfo: the host stays.
        expect(normalizeHost('https://cloud.example.com?next=a@b.example')).toBe('cloud.example.com');
        const withQuery = make({
            id: 'q1', protocol: 'webdav', providerId: 'nextcloud',
            host: 'https://cloud.example.com?access_token=SECRET', port: 443, username: 'alice',
        });
        const plain = make({
            id: 'q2', protocol: 'webdav', providerId: 'nextcloud',
            host: 'https://cloud.example.com', port: 443, username: 'alice',
        });
        expect(getStorageDedupKey(withQuery)).not.toContain('secret');
        expect(getStorageDedupKey(withQuery)).toBe(getStorageDedupKey(plain));
        // Generic WebDAV: the `webdav-host:` key.
        const dav = make({
            id: 'q5', protocol: 'webdav', host: 'https://dav.example.com/remote?token=SECRET',
            port: 443, username: 'alice',
        });
        const davPlain = make({
            id: 'q6', protocol: 'webdav', host: 'https://dav.example.com', port: 443, username: 'alice',
        });
        expect(getStorageDedupKey(dav)).toBe('webdav-host:dav.example.com:alice');
        expect(getStorageDedupKey(dav)).toBe(getStorageDedupKey(davPlain));
        // Custom S3: the `s3-host:` key.
        const s3 = make({
            id: 'q3', protocol: 's3', host: 'https://s3.example.com?X-Amz-Signature=SECRET',
            port: 443, username: 'AKIAEXAMPLE',
        });
        const s3Plain = make({
            id: 'q7', protocol: 's3', host: 'https://s3.example.com', port: 443, username: 'AKIAEXAMPLE',
        });
        expect(getStorageDedupKey(s3)).toMatch(/^s3-host:s3\.example\.com:/);
        expect(getStorageDedupKey(s3)).not.toContain('secret');
        expect(getStorageDedupKey(s3)).toBe(getStorageDedupKey(s3Plain));
        // FTP: the `host:` key.
        const ftp = make({
            id: 'q4', protocol: 'ftp', host: 'ftp.example.com#SECRET', port: 21, username: 'bob',
        });
        const ftpPlain = make({
            id: 'q8', protocol: 'ftp', host: 'ftp.example.com', port: 21, username: 'bob',
        });
        expect(getStorageDedupKey(ftp)).toMatch(/^host:[^:]+:ftp\.example\.com:21:bob$/);
        expect(getStorageDedupKey(ftp)).not.toContain('secret');
        expect(getStorageDedupKey(ftp)).toBe(getStorageDedupKey(ftpPlain));
    });

    it('case 4d: IBM COS same access key in two regions counts one drive', () => {
        // The frontend S3 mirror missed 'ibm-cos', so same-key profiles in
        // different regions keyed by host (two drives) while Rust keyed by
        // provider id (one drive). Parity with is_s3_preset.
        const profiles = [
            make({
                id: 'i1', protocol: 's3', providerId: 'ibm-cos',
                host: 's3.eu-de.cloud-object-storage.appdomain.cloud', port: 443,
                username: 'IBMACCESSKEY12345',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
            make({
                id: 'i2', protocol: 's3', providerId: 'ibm-cos',
                host: 's3.us-south.cloud-object-storage.appdomain.cloud', port: 443,
                username: 'IBMACCESSKEY12345',
                used: 2_000_000_000, total: 5_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(1);
        expect(getStorageDedupKey(profiles[0]).startsWith('s3:ibm-cos:')).toBe(true);
        expect(getStorageDedupKey(profiles[0])).toBe(getStorageDedupKey(profiles[1]));
    });

    it('case 4e: Fastly same access key in two regions counts one drive', () => {
        // A Fastly access key is account-wide; the region is only the host label.
        const profiles = [
            make({
                id: 'f1', protocol: 's3', providerId: 'fastly',
                host: 'eu-central-1.object.fastlystorage.app', port: 443,
                username: 'FASTLYEXAMPLEKEY1',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
            make({
                id: 'f2', protocol: 's3', providerId: 'fastly',
                host: 'us-east.object.fastlystorage.app', port: 443,
                username: 'FASTLYEXAMPLEKEY1',
                used: 2_000_000_000, total: 5_000_000_000,
            }),
        ];
        expect(aggregateByDedupKey(profiles).uniqueCount).toBe(1);
        expect(getStorageDedupKey(profiles[0]).startsWith('s3:fastly:')).toBe(true);
        expect(getStorageDedupKey(profiles[0])).toBe(getStorageDedupKey(profiles[1]));
    });

    it('case 4f: a stored preset alias keys like its preset (counted once)', () => {
        // fastly-object-storage, backblaze-b2 and azure-blob are manifest
        // aliases: an old or hand-edited profile may carry them.
        const pair = (canonical: string, alias: string, host: string) => [
            make({
                id: `${canonical}-1`, protocol: 's3', providerId: canonical,
                host, port: 443, username: 'ALIASEXAMPLEKEY01',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
            make({
                id: `${canonical}-2`, protocol: 's3', providerId: alias,
                host, port: 443, username: 'ALIASEXAMPLEKEY01',
                used: 1_000_000_000, total: 5_000_000_000,
            }),
        ];
        for (const [canonical, alias, host] of [
            ['fastly', 'fastly-object-storage', 'eu-central-1.object.fastlystorage.app'],
            ['backblaze', 'backblaze-b2', 's3.us-west-004.backblazeb2.com'],
        ]) {
            const profiles = pair(canonical, alias, host);
            expect(getStorageDedupKey(profiles[1]), alias).toBe(getStorageDedupKey(profiles[0]));
            const summary = aggregateByDedupKey(profiles);
            expect(summary.uniqueCount, alias).toBe(1);
            // The cached quota counts once, not twice.
            expect(summary.totalUsed, alias).toBe(1_000_000_000);
        }
    });

    it('case 5: OAuth keyed by email, not by display name', () => {
        const profiles = [
            make({
                id: 'o1', protocol: 'googledrive', providerId: 'googledrive',
                username: 'user@gmail.com',
                used: 1_000_000, total: 15_000_000_000,
            }),
            make({
                id: 'o2', protocol: 'googledrive', providerId: 'googledrive',
                username: 'user@gmail.com',
                used: 1_000_000, total: 15_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(1);
        const key = getStorageDedupKey(profiles[0]);
        expect(key.startsWith('oauth:googledrive:')).toBe(true);
        expect(key).toContain('user@gmail.com');
    });

    it('case 6: quota summed once per dedup key (not 3x)', () => {
        const profiles = ['a', 'b', 'c'].map(id => make({
            id, protocol: 's3', providerId: 'wasabi',
            host: 's3.wasabisys.com', port: 443,
            username: 'AKIA_SAME_KEY_HERE_X',
            used: 1_000_000_000, total: 10_000_000_000,
        }));
        const summary = aggregateByDedupKey(profiles);
        expect(summary.profiles).toBe(3);
        expect(summary.uniqueCount).toBe(1);
        expect(summary.totalUsed).toBe(1_000_000_000); // 1 GB, not 3 GB
        expect(summary.totalTotal).toBe(10_000_000_000);
    });

    it('opaque token usernames fall back to id:<profileId>', () => {
        // Two Drime profiles with very long opaque tokens as username -> two
        // separate dedup keys (no false dedup).
        const opaque = 'thisisaverylongopaquetokenstringwithoutemailorspacesabc';
        const profiles = [
            make({ id: 'd1', protocol: 'drime', providerId: 'drime', username: opaque, used: 0, total: 20_000_000_000 }),
            make({ id: 'd2', protocol: 'drime', providerId: 'drime', username: opaque, used: 0, total: 20_000_000_000 }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(2);
        expect(getStorageDedupKey(profiles[0])).not.toBe(getStorageDedupKey(profiles[1]));
    });

    it('divergent quotas use Math.max, never sum', () => {
        const profiles = [
            make({
                id: 'p1', protocol: 's3', providerId: 'wasabi',
                host: 's3.wasabisys.com', port: 443,
                username: 'AKIA_SAME',
                used: 40_000_000_000, total: 1_000_000_000_000,
            }),
            make({
                id: 'p2', protocol: 's3', providerId: 'wasabi',
                host: 's3.wasabisys.com', port: 443,
                username: 'AKIA_SAME',
                used: 50_000_000_000, total: 1_000_000_000_000,
            }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.uniqueCount).toBe(1);
        expect(summary.totalUsed).toBe(50_000_000_000); // max, not 90 GB
    });

    it('breakdown groups by protocol class, sorted by total desc', () => {
        const profiles = [
            make({ id: '1', protocol: 'sftp', host: 'a', port: 22, username: 'u', used: 100, total: 1000 }),
            make({ id: '2', protocol: 'sftp', host: 'b', port: 22, username: 'u', used: 50, total: 500 }),
            make({ id: '3', protocol: 'googledrive', providerId: 'googledrive', username: 'x@y.com', used: 9000, total: 10000 }),
            make({ id: '4', protocol: 'ftp', host: 'c', port: 21, username: 'u' }),
        ];
        const summary = aggregateByDedupKey(profiles);
        expect(summary.byProtocolClass.length).toBeGreaterThanOrEqual(3);
        // First row should be the class with the largest deduped total. With
        // these inputs it's OAuth (Google Drive, 10k > 1.5k SFTP).
        expect(summary.byProtocolClass[0].protocolClass).toBe('OAuth');
        expect(summary.byProtocolClass[0].profiles).toBe(1);
        // FTP rows have no quota -> they go to the bottom of the breakdown.
        const ftpRow = summary.byProtocolClass.find(r => r.protocolClass === 'FTP');
        expect(ftpRow?.total).toBe(0);
    });

    it('normalizeHost strips scheme, www, trailing slash and path', () => {
        expect(normalizeHost('https://Www.Example.com/dav/')).toBe('example.com');
        expect(normalizeHost('webdavs://nas.local:8080/share')).toBe('nas.local:8080');
        expect(normalizeHost('http://host/')).toBe('host');
        expect(normalizeHost('  HOST  ')).toBe('host');
    });

    it('normalizeUser returns undefined for opaque tokens', () => {
        expect(normalizeUser('user@example.com')).toBe('user@example.com');
        expect(normalizeUser('  Alice  ')).toBe('alice');
        expect(normalizeUser('a'.repeat(50))).toBeUndefined();
        expect(normalizeUser('deadbeef'.repeat(8))).toBeUndefined(); // hex 64 char
        expect(normalizeUser('')).toBeUndefined();
        expect(normalizeUser(undefined)).toBeUndefined();
    });
});
