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
    // Read to the semicolon, not as a run of `| "member"`: a run stops at the
    // first thing that is not one, and a doc comment on a member is exactly
    // that. Comments are stripped instead, so a documented member still counts.
    const block = /export type ProviderType =([\s\S]*?);/.exec(src);
    if (!block) throw new Error('ProviderType union not found in types.ts');
    const body = block[1].replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/\/\/[^\n]*/g, ' ');
    const members = [...body.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    // One member per separator, or this function is reading less than the union
    // declares and every caller below would silently check a shorter list. An
    // extraction that cannot read the file has to say so, not return a subset.
    const separators = (body.match(/\|/g) ?? []).length;
    if (members.length !== separators) {
        throw new Error(
            `ProviderType parse read ${members.length} member(s) for ${separators} separator(s): ` +
            'the union uses a syntax this reader does not understand',
        );
    }
    return members;
}

describe('session tab provider icon', () => {
    /** The loop below is only as good as what it reads. A doc comment landing
     *  between two members used to end the match early, so the union came back
     *  short and the newest protocol was never checked: the test still passed,
     *  reporting coverage it did not have. This asserts the parse reaches the
     *  end of the declaration rather than naming any protocol, so it keeps
     *  holding when the last member changes. */
    it('reads the union to its last member', () => {
        const src = Object.values(TYPES_SOURCE)[0]!;
        const declared = [
            ...(/export type ProviderType =([\s\S]*?);/.exec(src)![1]).matchAll(/"([^"]+)"/g),
        ].map((m) => m[1]);
        expect(providerTypeUnion()).toEqual(declared);
    });

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
