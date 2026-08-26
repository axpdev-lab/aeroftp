// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { BLOMP_AUTH_URL, isBlompAuthUrl } from './swiftAuthUrl';

/** Source read through Vite's raw glob, the same way the other source-scanning
 *  pins do, so the test needs no node type definitions. */
const SOURCES = import.meta.glob(['./ProtocolSelector.tsx', './ConnectionScreen.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

function source(name: string): string {
    const key = Object.keys(SOURCES).find((k) => k.endsWith(name));
    const src = key ? SOURCES[key] : undefined;
    if (!src) throw new Error(`${name} not found by the raw glob`);
    return src;
}

describe('swift auth URL identity', () => {
    it('recognises the preset regardless of trailing slash or case', () => {
        expect(isBlompAuthUrl(BLOMP_AUTH_URL)).toBe(true);
        expect(isBlompAuthUrl(`${BLOMP_AUTH_URL}/`)).toBe(true);
        expect(isBlompAuthUrl(` ${BLOMP_AUTH_URL.toUpperCase()} `)).toBe(true);
    });

    it('does not claim a private OpenStack is the preset', () => {
        expect(isBlompAuthUrl('https://keystone.internal.example/v3')).toBe(false);
        expect(isBlompAuthUrl('')).toBe(false);
        expect(isBlompAuthUrl(undefined)).toBe(false);
        // A hostname that merely contains the preset's is a different service.
        expect(isBlompAuthUrl('https://authenticate.blomp.com.evil.test')).toBe(false);
    });

    /** The tile carried no id, so picking Blomp from the protocol grid and
     *  connecting without saving sent `provider_id: null`. The cleartext
     *  exemption is keyed on that id, so the connection was refused. */
    it('declares the preset id on the Blomp tile', () => {
        const src = source('ProtocolSelector.tsx');
        const tile = /\{\s*type:\s*'swift' as const,[\s\S]*?\n    \},/.exec(src);
        expect(tile, 'swift tile not found in ProtocolSelector').toBeTruthy();
        expect(tile![0]).toContain("providerId: 'blomp'");
    });

    /** A blanket `protocol === 'swift' ? 'blomp'` would hand Blomp's exemption
     *  to any Keystone configured through the same form. */
    it('never assigns the preset id to swift unconditionally', () => {
        const src = source('ConnectionScreen.tsx');
        expect(src).not.toMatch(/protocol === 'swift' \? 'blomp'/);
        const guarded = src.match(
            /protocol === 'swift'\s*\n?\s*\?\s*\(isBlompAuthUrl\(connectionParams\.server\) \? 'blomp' : undefined\)/g,
        );
        expect(guarded?.length, 'both save paths must key the id on the auth URL').toBe(2);
    });
});
