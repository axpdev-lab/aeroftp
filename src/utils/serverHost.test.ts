// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { serverHost } from './serverHost';
import APP from '../App.tsx?raw';

describe('the host of a server field', () => {
    it('drops a port, bracketed or not', () => {
        expect(serverHost('example.com:2222')).toBe('example.com');
        expect(serverHost('192.0.2.1:22')).toBe('192.0.2.1');
        expect(serverHost('[2001:db8::1]:2222')).toBe('2001:db8::1');
        expect(serverHost('[::1]')).toBe('::1');
    });

    it('keeps a bare IPv6 address and a host with no port whole', () => {
        expect(serverHost('::1')).toBe('::1');
        expect(serverHost('2001:db8::1')).toBe('2001:db8::1');
        expect(serverHost('example.com')).toBe('example.com');
        expect(serverHost('')).toBe('');
    });

    it('is what App uses wherever it takes the host of a server', () => {
        expect(APP).not.toMatch(/\.split\(':'\)\[0\]/);
        expect(APP).toContain('host: serverHost(liveConnectionParams.server),');
    });
});
