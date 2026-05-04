// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Cross-checks the TS dedup logic against the same six scenarios the Rust
// twin (src-tauri/src/storage_dedup.rs) covers under cargo test. The two
// implementations need not produce byte-identical dedup keys (TS uses a fast
// non-crypto hash, Rust uses SHA-256/12) — only the aggregation result must
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
