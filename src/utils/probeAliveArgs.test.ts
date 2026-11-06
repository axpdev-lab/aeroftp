// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';
import { probeAliveArgs, sameQuickConnectTarget } from './probeAliveArgs';

describe('probeAliveArgs', () => {
    it('names the saved profile and the endpoint, not only the user', () => {
        expect(probeAliveArgs({ server: '127.0.0.1', port: 2162, username: 'row163', password: '', protocol: 'ftp', savedServerId: 'srv_b' }, 'ftp'))
            .toEqual({ protocol: 'ftp', username: 'row163', profileId: 'srv_b', host: '127.0.0.1', port: 2162, bucket: null });
    });

    it('sends nulls for what a Quick Connect tab does not have', () => {
        expect(probeAliveArgs({ server: 'ftp.example.com', username: '', password: '' }))
            .toEqual({ protocol: null, username: null, profileId: null, host: 'ftp.example.com', port: null, bucket: null });
        expect(probeAliveArgs(undefined, 'mega')).toEqual({ protocol: 'mega', username: null, profileId: null, host: null, port: null, bucket: null });
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
        expect(match).toContain('sameQuickConnectTarget(p, effectiveParams, protocol)');
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
        expect(sameQuickConnectTarget(tab, { ...tab, username: 'v' }, 'ftp')).toBe(false);
        expect(sameQuickConnectTarget(tab, { ...tab, server: 'other.example.com' }, 'ftp')).toBe(false);
    });

    it('sends the bucket to the probe', () => {
        expect(probeAliveArgs({ server: 's3.example.com', username: 'k', password: '', options: { bucket: 'photos' } }, 's3').bucket).toBe('photos');
    });
});
