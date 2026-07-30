// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import protocolSelectorRaw from '../components/ProtocolSelector.tsx?raw';
import { getNativeE2eBits, nativeE2eBadge, NATIVE_E2E_BY_PROTOCOL } from './nativeE2e';

describe('getNativeE2eBits', () => {
    it('answers for the providers that encrypt on the client', () => {
        expect(getNativeE2eBits('mega')).toBe(128);
        expect(getNativeE2eBits('filen')).toBe(256);
        expect(getNativeE2eBits('internxt')).toBe(256);
    });

    it('says no for a transport that is merely encrypted in flight', () => {
        expect(getNativeE2eBits('sftp')).toBeNull();
        expect(getNativeE2eBits('ftps')).toBeNull();
        expect(getNativeE2eBits('webdav')).toBeNull();
    });

    it('says no for MEGA S4, which is the same company over S3', () => {
        // Ehud asked for the badge on "all the protocols of MEGA". S4 is MEGA's
        // paid S3-compatible object storage: a profile pointed at it has
        // protocol 's3' and the server holds the keys, so the badge would be a
        // false claim. This is the case the badge exists to distinguish.
        expect(getNativeE2eBits('s3')).toBeNull();
    });

    it('is defensive about missing input', () => {
        expect(getNativeE2eBits(undefined)).toBeNull();
        expect(getNativeE2eBits(null)).toBeNull();
        expect(getNativeE2eBits('')).toBeNull();
        expect(getNativeE2eBits('constructor')).toBeNull();
    });
});

// Add Service reads its chip from a hand-written catalogue in ProtocolSelector,
// My Servers reads this list. They said the same thing by coincidence until now;
// this holds them to it. Read as source text rather than imported, because the
// catalogue is a .tsx module full of SVG logo components (?raw, the same way
// providerCatalog.test.ts reads README.md).
describe('the Add Service catalogue agrees with this list', () => {
    const source = protocolSelectorRaw;
    const entryPattern = /\{\s*type:\s*'([a-z0-9-]+)'[^\n]*?badge:\s*'(E2E [^']+)'/g;

    const catalogued = new Map<string, string>();
    for (const match of source.matchAll(entryPattern)) {
        catalogued.set(match[1], match[2]);
    }

    it('finds the E2E entries at all (the pattern still matches the file)', () => {
        expect(catalogued.size).toBeGreaterThan(0);
    });

    it('badges exactly the protocols on this list, at the same strength', () => {
        const expected = new Map(
            Object.entries(NATIVE_E2E_BY_PROTOCOL).map(([p, bits]) => [p, nativeE2eBadge(bits)]),
        );
        expect(Object.fromEntries([...catalogued].sort()))
            .toEqual(Object.fromEntries([...expected].sort()));
    });
});
