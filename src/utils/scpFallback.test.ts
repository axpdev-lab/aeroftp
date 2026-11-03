// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { ProviderOptions } from '../types';
import { connectionProtocolLabel, scpFallbackAllowed, scpFallbackConnectParam, wentOnOverScpAfter, withScpFallback } from './scpFallback';
import app from '../App.tsx?raw';

describe('SFTP with SCP fallback', () => {
    it('is on for a profile that never touched the option', () => {
        expect(scpFallbackAllowed({})).toBe(true);
        expect(scpFallbackAllowed(undefined)).toBe(true);
        expect(scpFallbackConnectParam('sftp', {})).toEqual({});
    });

    it('reads a WinSCP "SFTP only" import as off and tells the backend', () => {
        const imported = { allowScpFallback: false, private_key_path: '/k' };
        expect(scpFallbackAllowed(imported)).toBe(false);
        expect(scpFallbackConnectParam('sftp', imported)).toEqual({ allow_scp_fallback: false });
    });

    it('sends nothing when the fallback is on: absent means allowed', () => {
        expect(scpFallbackConnectParam('sftp', { allowScpFallback: true })).toEqual({});
        expect('allow_scp_fallback' in scpFallbackConnectParam('sftp', { allowScpFallback: true })).toBe(false);
    });

    it('never sends the field for a protocol other than SFTP', () => {
        for (const protocol of ['scp', 'ftp', 'ftps', 'webdav', 's3', undefined]) {
            expect(scpFallbackConnectParam(protocol, { allowScpFallback: false }), String(protocol)).toEqual({});
        }
    });

    it('stores only the exception, and turning it back on leaves no key behind', () => {
        const saved: ProviderOptions = { timeout: 30 };
        const off = withScpFallback(saved, false);
        expect(off).toEqual({ timeout: 30, allowScpFallback: false });
        const on = withScpFallback(off, true);
        expect(on).toEqual({ timeout: 30 });
        expect('allowScpFallback' in on).toBe(false);
        // A saved profile goes through JSON: what is stored reads back the same.
        expect(scpFallbackAllowed(JSON.parse(JSON.stringify(off)))).toBe(false);
        expect(scpFallbackAllowed(JSON.parse(JSON.stringify(on)))).toBe(true);
    });

    it('names SCP only for an SFTP connection that went on over SCP', () => {
        expect(connectionProtocolLabel('sftp', true)).toBe('scp');
        expect(connectionProtocolLabel('sftp', false)).toBe('sftp');
        // No other protocol has the fallback, whatever the flag says.
        for (const protocol of ['ftp', 'ftps', 'scp', 'webdav', 's3']) {
            expect(connectionProtocolLabel(protocol, true)).toBe(protocol);
        }
        expect(connectionProtocolLabel(undefined, true)).toBeUndefined();
    });
});

describe('the SCP label across a silent reconnect', () => {
    it('follows a reconnect that lands on SCP, and one that lands back on SFTP', () => {
        expect(wentOnOverScpAfter('ssh-transport', 'scp', false)).toBe(true);
        expect(wentOnOverScpAfter('ssh-transport', 'sftp', true)).toBe(false);
        expect(wentOnOverScpAfter('scp-mode', 'router.lan', false)).toBe(true);
    });

    it('keeps what it was for every other session event', () => {
        for (const kind of ['lost', 'reconnecting', 'reconnected', 'reconnect-failed', undefined]) {
            expect(wentOnOverScpAfter(kind, '', true), String(kind)).toBe(true);
            expect(wentOnOverScpAfter(kind, '', false), String(kind)).toBe(false);
        }
    });

    it('is what the App session listener applies to every event', () => {
        expect(app).toContain('wentOnOverScpRef.current = wentOnOverScpAfter(kind, event.payload?.detail, wentOnOverScpRef.current);');
    });
});
