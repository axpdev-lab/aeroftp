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
//
// Entry-bounded across lines: ProtocolSelector puts `type` and `badge` on
// different lines in the live PROTOCOLS list, so a single-line `[^\n]*?` matcher
// only sees PROTOCOLS_FALLBACK and would stay green if the real catalogue
// drifted. Bound each entry by the next `type:` so an OAuth badge never steals
// a later E2E chip.
export function catalogueE2eBadges(source: string): Map<string, string> {
    const catalogued = new Map<string, string>();
    const typeRe = /\btype:\s*'([a-z0-9-]+)'(?:\s+as\s+const)?/g;
    const positions: { type: string; index: number }[] = [];
    for (const match of source.matchAll(typeRe)) {
        positions.push({ type: match[1], index: match.index ?? 0 });
    }
    for (let i = 0; i < positions.length; i++) {
        const start = positions[i].index;
        const end = i + 1 < positions.length ? positions[i + 1].index : source.length;
        const slice = source.slice(start, end);
        const badge = /\bbadge:\s*'(E2E [^']+)'/.exec(slice);
        if (badge) {
            catalogued.set(positions[i].type, badge[1]);
        }
    }
    return catalogued;
}

describe('catalogueE2eBadges (the matcher itself)', () => {
    it('reads a badge that sits on a later line of the same entry', () => {
        const snippet = `
    {
        type: 'mega',
        name: 'MEGA',
        badge: 'E2E 128-bit',
    },
`;
        expect(Object.fromEntries(catalogueE2eBadges(snippet))).toEqual({
            mega: 'E2E 128-bit',
        });
    });

    it('does not attribute a later entry E2E badge to an earlier non-E2E type', () => {
        const snippet = `
    {
        type: 'box',
        badge: 'OAuth',
    },
    {
        type: 'filen',
        badge: 'E2E 256-bit',
    },
`;
        const catalogued = catalogueE2eBadges(snippet);
        expect(catalogued.has('box')).toBe(false);
        expect(catalogued.get('filen')).toBe('E2E 256-bit');
    });

    it('falls empty when the matcher cannot reach any badge (the old single-line bug)', () => {
        // The defect CodeRabbit flagged: [^\n]*? never crosses the newline to
        // `badge`, so the map stays empty and a size>0 check was the only
        // guard. Keep that broken shape here so reintroducing it still fails.
        const multiline = `
    {
        type: 'mega',
        badge: 'E2E 128-bit',
    },
`;
        const broken = /\{\s*type:\s*'([a-z0-9-]+)'[^\n]*?badge:\s*'(E2E [^']+)'/g;
        expect([...multiline.matchAll(broken)]).toHaveLength(0);
        expect(catalogueE2eBadges(multiline).size).toBe(1);
    });
});

describe('the Add Service catalogue agrees with this list', () => {
    const catalogued = catalogueE2eBadges(protocolSelectorRaw);

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
