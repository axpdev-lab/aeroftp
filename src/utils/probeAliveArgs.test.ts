// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';
import { probeAliveArgs } from './probeAliveArgs';

describe('probeAliveArgs', () => {
    it('names the saved profile and the endpoint, not only the user', () => {
        expect(probeAliveArgs({ server: '127.0.0.1', port: 2162, username: 'row163', password: '', protocol: 'ftp', savedServerId: 'srv_b' }, 'ftp'))
            .toEqual({ protocol: 'ftp', username: 'row163', profileId: 'srv_b', host: '127.0.0.1', port: 2162 });
    });

    it('sends nulls for what a Quick Connect tab does not have', () => {
        expect(probeAliveArgs({ server: 'ftp.example.com', username: '', password: '' }))
            .toEqual({ protocol: null, username: null, profileId: null, host: 'ftp.example.com', port: null });
        expect(probeAliveArgs(undefined, 'mega')).toEqual({ protocol: 'mega', username: null, profileId: null, host: null, port: null });
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

    it('lets a Quick Connect tab be reused only on the same port', () => {
        const at = APP.indexOf('const reusable = sessions.find(s => {');
        const match = APP.slice(at, APP.indexOf('});', at));
        expect(at).toBeGreaterThan(-1);
        expect(match).toContain("(p.port || null) === (effectiveParams.port || null)");
    });
});
