// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { BLOMP_AUTH_URL, isBlompAuthUrl, swiftOptionsForAuthUrl } from './swiftAuthUrl';

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

    /** The exemption follows the preset identity, which is the whole point of
     *  keying it on the auth URL. A private OpenStack has no exemption to
     *  inherit, so leaving Blomp must drop it. */
    it('drops the preset exemption on leaving the preset, and restores it on returning', () => {
        expect(
            swiftOptionsForAuthUrl(BLOMP_AUTH_URL, 'https://keystone.internal.example/v3', {
                allowCleartextStorage: true,
            }),
        ).toEqual({ allowCleartextStorage: undefined });

        expect(
            swiftOptionsForAuthUrl('https://keystone.internal.example/v3', BLOMP_AUTH_URL, {}),
        ).toEqual({ allowCleartextStorage: true });
    });

    /** The defect this pins: the opt-in was recomputed from the URL on every
     *  keystroke, so a user who ticked the box for their own OpenStack and then
     *  corrected one character of the URL lost the tick without being told.
     *  An edit that does not cross the preset boundary is not a decision about
     *  cleartext, so it must not touch the flag, in either direction. */
    it('leaves the user opt-in alone while the auth URL is merely being edited', () => {
        const opted = { allowCleartextStorage: true };
        expect(
            swiftOptionsForAuthUrl(
                'https://keystone.internal.example/v3',
                'https://keystone.internal.example/v3/',
                opted,
            ),
        ).toBe(opted);
        expect(
            swiftOptionsForAuthUrl('https://keystone.internal.exampl', 'https://keystone.internal.example', opted),
        ).toBe(opted);

        // The same holds for the untouched default: editing must not invent an
        // opt-in either.
        const untouched = {};
        expect(swiftOptionsForAuthUrl('https://a.example', 'https://b.example', untouched)).toBe(untouched);
    });

    /** Other options travel with the profile and have nothing to do with the
     *  auth URL: a boundary crossing must not drop them. */
    it('keeps the rest of the options across a boundary crossing', () => {
        expect(
            swiftOptionsForAuthUrl(BLOMP_AUTH_URL, 'https://keystone.internal.example/v3', {
                allowCleartextStorage: true,
                region: 'eu-central',
            }),
        ).toEqual({ allowCleartextStorage: undefined, region: 'eu-central' });
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
