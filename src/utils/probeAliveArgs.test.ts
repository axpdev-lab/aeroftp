// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';
import PROVIDER_COMMANDS from '../../src-tauri/src/provider_commands.rs?raw';
import PROBE_ALIVE_SRC from './probeAliveArgs.ts?raw';
import { probeAliveArgs, reusesOpenTab, sameQuickConnectTarget } from './probeAliveArgs';
import type { ConnectionParams } from '../types';

describe('probeAliveArgs', () => {
    it('names the saved profile and the endpoint, not only the user', () => {
        expect(probeAliveArgs({ server: '127.0.0.1', port: 2162, username: 'row163', password: '', protocol: 'ftp', savedServerId: 'srv_b' }, 'ftp'))
            .toEqual({ protocol: 'ftp', username: 'row163', profileId: 'srv_b', host: '127.0.0.1', port: 2162, bucket: null, endpoint: null, region: null, basePath: null });
    });

    it('sends nulls for what a Quick Connect tab does not have', () => {
        expect(probeAliveArgs({ server: 'ftp.example.com', username: '', password: '' }))
            .toEqual({ protocol: null, username: null, profileId: null, host: 'ftp.example.com', port: null, bucket: null, endpoint: null, region: null, basePath: null });
        expect(probeAliveArgs(undefined, 'mega')).toEqual({ protocol: 'mega', username: null, profileId: null, host: null, port: null, bucket: null, endpoint: null, region: null, basePath: null });
    });

    it('forwards WebDAV basePath distinctly for profile-less tabs on the same host', () => {
        const alice = probeAliveArgs({
            server: 'lifetimecloud.me',
            port: 443,
            username: 'alice',
            password: '',
            protocol: 'webdav',
            options: { basePath: '/dav/alice/' },
        }, 'webdav');
        const bob = probeAliveArgs({
            server: 'lifetimecloud.me',
            port: 443,
            username: 'alice',
            password: '',
            protocol: 'webdav',
            options: { basePath: '/dav/bob/' },
        }, 'webdav');
        const root = probeAliveArgs({
            server: 'lifetimecloud.me',
            port: 443,
            username: 'alice',
            password: '',
            protocol: 'webdav',
        }, 'webdav');
        expect(alice.basePath).toBe('/dav/alice/');
        expect(bob.basePath).toBe('/dav/bob/');
        expect(root.basePath).toBeNull();
        expect(alice).not.toEqual(bob);
        expect(alice).not.toEqual(root);
    });
});

describe('the liveness probes of App.tsx', () => {
    it('both send the identity of the tab they would reuse', () => {
        // A tab switch probes its target; a connect probes the open tab of the
        // same account it would reactivate instead of reconnecting.
        expect(APP.split("'provider_probe_alive'").length - 1).toBe(2);
        expect(APP).toContain("invoke<boolean>('provider_probe_alive', probeAliveArgs(targetSession.connectionParams, protocol))");
        expect(APP).toContain("invoke<boolean>('provider_probe_alive', probeAliveArgs(reusable.connectionParams, protocol))");
    });

    it('lets a Quick Connect tab be reused only on the same target', () => {
        const at = APP.indexOf('const reusable = sessions.find(s => {');
        const match = APP.slice(at, APP.indexOf('});', at));
        expect(at).toBeGreaterThan(-1);
        expect(match).toContain('return !!p && reusesOpenTab(p, effectiveParams, protocol);');
        expect(match).not.toContain('savedServerId');
    });
});

describe('reusesOpenTab', () => {
    const team = (basePath: string, savedServerId?: string): ConnectionParams => ({
        server: 'dav.example.test',
        port: 443,
        username: 'alice',
        password: '',
        protocol: 'webdav',
        savedServerId,
        options: { basePath },
    });

    it('does not reuse a saved profile tab for a request of another WebDAV collection', () => {
        // One saved profile on /team-a; a Quick Connect for /team-b that the
        // profile lookup matched on host and user carries the same id.
        const open = team('/team-a', 'srv_dav');
        expect(reusesOpenTab(open, team('/team-b', 'srv_dav'), 'webdav')).toBe(false);
        expect(reusesOpenTab(open, team('/team-a', 'srv_dav'), 'webdav')).toBe(true);
    });

    it('keeps the earlier rules for saved profiles and Quick Connect tabs', () => {
        const ftp: ConnectionParams = { server: 'ftp.example.test', username: 'u', password: '', protocol: 'ftp', savedServerId: 'srv_1' };
        expect(reusesOpenTab(ftp, { ...ftp, server: 'renamed.example.test' }, 'ftp')).toBe(true);
        expect(reusesOpenTab(ftp, { ...ftp, savedServerId: 'srv_2' }, 'ftp')).toBe(false);
        expect(reusesOpenTab(ftp, ftp, 'sftp')).toBe(false);
        const quick = { ...ftp, savedServerId: undefined };
        expect(reusesOpenTab(quick, quick, 'ftp')).toBe(true);
        expect(reusesOpenTab(quick, { ...quick, port: 2121 }, 'ftp')).toBe(false);
        expect(reusesOpenTab(team('/team-a'), team('/team-b'), 'webdav')).toBe(false);
    });
});

describe('sameQuickConnectTarget', () => {
    const tab = { server: 'ftp.example.com', username: 'u', password: '' };

    it('compares the effective port, so an unset port is the default', () => {
        expect(sameQuickConnectTarget(tab, { ...tab, port: 21 }, 'ftp')).toBe(true);
        expect(sameQuickConnectTarget(tab, { ...tab, port: 2121 }, 'ftp')).toBe(false);
        expect(sameQuickConnectTarget({ ...tab, port: 2121 }, { ...tab, port: 2121 }, 'ftp')).toBe(true);
    });

    it('tells two buckets, users or servers apart', () => {
        const s3 = { ...tab, server: 's3.example.com', options: { bucket: 'photos' } };
        expect(sameQuickConnectTarget(s3, { ...s3, options: { bucket: 'backups' } }, 's3')).toBe(false);
        expect(sameQuickConnectTarget(s3, { ...s3 }, 's3')).toBe(true);
        expect(sameQuickConnectTarget(s3, { ...s3, options: { bucket: 'photos', endpoint: 'https://minio.example:9000' } }, 's3')).toBe(false);
        expect(sameQuickConnectTarget(s3, { ...s3, options: { bucket: 'photos', region: 'us-east-1' } }, 's3')).toBe(false);
        expect(sameQuickConnectTarget(tab, { ...tab, username: 'v' }, 'ftp')).toBe(false);
        expect(sameQuickConnectTarget(tab, { ...tab, server: 'other.example.com' }, 'ftp')).toBe(false);
    });

    it('tells two WebDAV base paths on one server and user apart', () => {
        const dav = { ...tab, server: 'dav.example.com', options: { basePath: '/dav/alice' } };
        expect(sameQuickConnectTarget(dav, { ...dav, options: { basePath: '/dav/bob' } }, 'webdav')).toBe(false);
        expect(sameQuickConnectTarget(dav, { ...dav, options: {} }, 'webdav')).toBe(false);
        expect(sameQuickConnectTarget(dav, { ...dav, options: { basePath: '/dav/alice' } }, 'webdav')).toBe(true);
        // Another protocol never reads the WebDAV base path.
        expect(sameQuickConnectTarget(dav, { ...dav, options: { basePath: '/other' } }, 'ftp')).toBe(true);
    });

    it('sends the S3 bucket, endpoint and region to the probe', () => {
        const args = probeAliveArgs({ server: '', username: 'k', password: '', options: { bucket: 'photos', endpoint: 'https://minio.example:9000', region: 'eu-west-1' } }, 's3');
        expect([args.bucket, args.endpoint, args.region]).toEqual(['photos', 'https://minio.example:9000', 'eu-west-1']);
    });

    it('pins basePath forwarding across probeAliveArgs and backend provider_probe_alive', () => {
        expect(PROBE_ALIVE_SRC).toMatch(/basePath:\s*webdavConnectBasePath\(/);
        const cmdStart = PROVIDER_COMMANDS.indexOf('pub async fn provider_probe_alive(');
        const cmdEnd = PROVIDER_COMMANDS.indexOf(') -> Result<bool, String>', cmdStart);
        expect(cmdStart).toBeGreaterThan(-1);
        expect(cmdEnd).toBeGreaterThan(cmdStart);
        expect(PROVIDER_COMMANDS.slice(cmdStart, cmdEnd)).toMatch(/\bbase_path:\s*Option<String>/);
        // The tab's base path reaches the target the connect itself builds.
        expect(PROVIDER_COMMANDS).toContain('fn webdav_connect_target(');
        expect(PROVIDER_COMMANDS).toMatch(/webdav_connect_target\(\s*exp_host[^;]*expected\.base_path,/);
    });
});
