// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { isProviderProtocol } from './SessionTabs';
import { PROVIDER_LOGOS } from './ProviderLogos';
import type { ProviderType } from '../types';

/** The `ProviderType` union, read from the source rather than restated here.
 *  Restating it is what let the original allowlist drift out of date.
 *  Read through Vite's raw glob, the same way the other source-scanning pins
 *  do, so the test needs no node type definitions. */
const TYPES_SOURCE = import.meta.glob('../types.ts', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

function providerTypeUnion(): string[] {
    const src = Object.values(TYPES_SOURCE)[0];
    if (!src) throw new Error('types.ts not found by the raw glob');
    const block = /export type ProviderType =((?:\s*\|\s*"[^"]+")+)/.exec(src);
    if (!block) throw new Error('ProviderType union not found in types.ts');
    return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

describe('session tab provider icon', () => {
    it('treats every protocol except plain FTP as a provider', () => {
        const union = providerTypeUnion();
        expect(union.length).toBeGreaterThan(20);

        for (const protocol of union) {
            const expected = protocol !== 'ftp' && protocol !== 'ftps';
            expect(
                isProviderProtocol(protocol as ProviderType),
                `${protocol} should ${expected ? '' : 'not '}take the provider icon path`,
            ).toBe(expected);
        }
    });

    it('covers the four protocols the old allowlist had missed', () => {
        // All four were in ProviderType and absent from the list. Only `swift`
        // was reachable, and Blomp is where it showed: the PNG, the BlompLogo
        // component and the PROVIDER_LOGOS entry all existed and none of them
        // was ever consulted. googlephotos, aerocloud and peer have no registry
        // entry, so no session can carry them: latent, not broken. They are
        // pinned here anyway, because "unreachable today" is not a reason to
        // leave the gap that made swift fail.
        for (const protocol of ['swift', 'googlephotos', 'aerocloud', 'peer']) {
            expect(isProviderProtocol(protocol as ProviderType), protocol).toBe(true);
        }
    });

    it('still excludes the two that are not providers', () => {
        expect(isProviderProtocol('ftp')).toBe(false);
        expect(isProviderProtocol('ftps')).toBe(false);
        expect(isProviderProtocol(undefined)).toBe(false);
    });

    it('has a logo registered for Blomp under the id its profiles carry', () => {
        // The tab resolves `PROVIDER_LOGOS[session.providerId]`, and saved Blomp
        // profiles carry providerId "blomp". A logo that exists under a
        // different key is a logo nobody renders.
        expect(PROVIDER_LOGOS['blomp']).toBeDefined();
    });
});
