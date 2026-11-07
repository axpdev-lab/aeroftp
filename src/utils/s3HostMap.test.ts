// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import hostMap from '../shared/s3-host-map.json';
import manifest from '../shared/connection-manifest.json';
import { deriveProviderId } from './serverListFilter';

/**
 * #1506 Requested 173: an S3 endpoint host is recognized on a domain boundary
 * from one table the frontend and the bridge mapper share, never by substring.
 * The substring rules took a bucket named after another vendor for that vendor
 * (`backblaze-backup.s3.amazonaws.com` read as Backblaze).
 */
type Entry = { providerId: string; preset: boolean; domains: string[]; hostPatterns?: string[] };
const entries = hostMap.providers as Entry[];
const presetIds = new Set(
    (manifest as { presets: { id: string; protocol: string }[] }).presets
        .filter(p => p.protocol === 's3')
        .map(p => p.id),
);

/**
 * Whether a pattern has a group that repeats without bound (`+`, `*`,
 * `{n,}`) and holds, at any depth, something else that repeats without
 * bound: the shape that backtracks exponentially (`(a+)+`, `((a+))+`,
 * `(x(a*)y)*`). A bounded repeat (`?`, `{n}`, `{n,m}`) runs a fixed number of
 * times, so `(-[0-9]+)?` stays allowed. Escapes and character classes are
 * literal.
 */
function hasNestedUnboundedQuantifier(pattern: string): boolean {
    // One frame per open group: whether anything inside repeats without bound.
    const stack: boolean[] = [false];
    let i = 0;
    // Reads the quantifier at i, if any; returns whether it is unbounded and moves i past it.
    const quantifier = (): boolean | undefined => {
        const c = pattern[i];
        let unbounded: boolean | undefined;
        if (c === '+' || c === '*') {
            unbounded = true;
            i += 1;
        } else if (c === '?') {
            unbounded = false;
            i += 1;
        } else if (c === '{') {
            const m = /^\{(\d+)(,(\d*))?\}/.exec(pattern.slice(i));
            if (!m) return undefined;
            unbounded = m[2] !== undefined && m[3] === '';
            i += m[0].length;
        } else {
            return undefined;
        }
        if (pattern[i] === '?') i += 1; // lazy modifier
        return unbounded;
    };
    while (i < pattern.length) {
        const c = pattern[i];
        if (c === '(') {
            stack.push(false);
            i += 1;
            // Skip a group prefix: (?:, (?=, (?!, (?<=, (?<! or (?<name>.
            if (pattern[i] === '?') {
                const named = pattern[i + 1] === '<' && pattern[i + 2] !== '=' && pattern[i + 2] !== '!';
                i = named ? pattern.indexOf('>', i) + 1 : i + (pattern[i + 1] === '<' ? 3 : 2);
            }
            continue;
        }
        if (c === ')') {
            const inner = stack.pop() ?? false;
            i += 1;
            const unbounded = quantifier();
            if (unbounded && inner) return true;
            stack[stack.length - 1] ||= inner || unbounded === true;
            continue;
        }
        // One atom: an escape, a character class or a single character.
        if (c === '\\') {
            i += 2;
        } else if (c === '[') {
            i += 1;
            if (pattern[i] === '^') i += 1;
            if (pattern[i] === ']') i += 1;
            while (i < pattern.length && pattern[i] !== ']') i += pattern[i] === '\\' ? 2 : 1;
            i += 1;
        } else {
            i += 1;
        }
        if (quantifier()) stack[stack.length - 1] = true;
    }
    return false;
}

describe('S3 host map', () => {
    it('maps every listed host case, on the host and on a full endpoint URL', () => {
        for (const { host, providerId } of hostMap.cases) {
            const presetCase = providerId === null || entries.find(e => e.providerId === providerId)?.preset;
            // The frontend maps only presets: a case for a provider without a
            // preset expects no match here, and its own mapping is checked by
            // the bridge mapper's tests, not by this one.
            const expected = presetCase ? (providerId ?? undefined) : undefined;
            expect(deriveProviderId({ name: 'x', host, username: '', protocol: 's3' }), host).toBe(expected);
            expect(
                deriveProviderId({ name: 'x', host: `https://${host}:443/bucket`, username: '', protocol: 's3' }),
                host,
            ).toBe(expected);
        }
    });

    it('names only real presets, and no domain twice', () => {
        const seen = new Set<string>();
        for (const entry of entries) {
            if (entry.preset) expect(presetIds.has(entry.providerId), entry.providerId).toBe(true);
            for (const domain of entry.domains) {
                expect(seen.has(domain), domain).toBe(false);
                seen.add(domain);
            }
        }
    });

    it('keeps every host pattern anchored and free of nested quantifiers', () => {
        // The patterns are compiled with `new RegExp` at module load. A group
        // that repeats without bound and holds, at any depth, something else
        // that repeats without bound can backtrack exponentially. An
        // unanchored pattern matches a lookalike host.
        for (const entry of entries) {
            for (const pattern of entry.hostPatterns ?? []) {
                expect(() => new RegExp(pattern), pattern).not.toThrow();
                expect(pattern.endsWith('$'), pattern).toBe(true);
                expect(pattern.startsWith('^') || pattern.startsWith('(^|\\.)'), pattern).toBe(true);
                expect(hasNestedUnboundedQuantifier(pattern), pattern).toBe(false);
            }
        }
    });

    it('finds a nested unbounded quantifier at any depth', () => {
        for (const bad of ['^(a+)+\\.com$', '^((a+))+\\.com$', '^(x(a*)y)*\\.com$', '^(a*)*$', '^(a+){2,}$', '^((ab)+c)*$', '^(a{1,})+$', '^(?:a+)+$', '^(?<n>a+)+$', '^(?:x(?<n>a*)y)*$']) {
            expect(hasNestedUnboundedQuantifier(bad), bad).toBe(true);
        }
        for (const good of ['(^|\\.)idrivee2(-[0-9]+)?\\.com$', '^(a+)?$', '^(a+){2}$', '^(a+){1,3}$', '^a+b*$', '^([+*]a)+$', '^(\\+a)+$', '^(a)+$', '^(?:a)+$', '^(?<n>ab)+$', '^(?=a)(a+)$']) {
            expect(hasNestedUnboundedQuantifier(good), good).toBe(false);
        }
    });

    it('covers every S3 preset whose endpoint has a fixed host', () => {
        const presets = (manifest as { presets: { id: string; protocol: string; defaults?: { endpoint?: string; endpointTemplate?: string } }[] }).presets;
        for (const preset of presets.filter(p => p.protocol === 's3')) {
            const endpoint = preset.defaults?.endpoint ?? preset.defaults?.endpointTemplate;
            if (!endpoint) continue;
            const host = endpoint
                .replace(/^https?:\/\//, '')
                .split(/[:/]/)[0]
                .replace(/\{[^}]+\}/g, 'x');
            expect(deriveProviderId({ name: 'x', host, username: '', protocol: 's3' }), preset.id).toBe(preset.id);
        }
    });
});
