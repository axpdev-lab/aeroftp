// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// A preset's help text that sends the user to a control of the connection
// form names it by reference ({ui:...}, {field:...}), so the name is the one
// the form shows, in every language. Puter's help said "set Server" to a field
// the WebDAV form labels "Endpoint URL", behind Advanced; two setup steps sent
// the user to a "Protocol field" that the form does not have.

import { describe, expect, it } from 'vitest';
import { PROVIDERS, presetNeedsUserEndpoint } from './registry';
import { presetTextReferences, presetTexts, resolvePresetText } from './presetText';
import type { ProviderConfig } from './types';

const LOCALES = import.meta.glob<{ translations: Record<string, unknown> }>(
    '../i18n/locales/*.json',
    { eager: true, import: 'default' },
);

const lookup = (translations: Record<string, unknown>, key: string): unknown =>
    key.split('.').reduce<unknown>(
        (node, part) => (node && typeof node === 'object' ? (node as Record<string, unknown>)[part] : undefined),
        translations,
    );

const everyText = PROVIDERS.flatMap((provider) =>
    presetTexts(provider).map(({ where, text }) => ({ provider, where: `${provider.id} ${where}`, text })),
);

/**
 * Where a text names a control of this form in plain words instead of by
 * reference. A path inside the provider's own console ("Console → Access Keys
 * → Access Key ID") is the provider's, not ours, and is not a hit.
 */
const literalControlNames = (text: string): string[] => {
    // A reference, and the position word that may follow it ("{ui:...} below"),
    // is the sanctioned way to name a control.
    const prose = text.replace(/\{(?:ui|field):[\w.-]+\}(?:\s+(?:above|below))?/g, '{ref}');
    const consolePath = /→|->|\s>\s/.test(prose);
    const cues: Array<[RegExp, string]> = [
        [/\b(?:field|fields|button)\b/i, 'a field or button of this form'],
        [/\b(?:above|below)\b/i, 'a position in this form'],
        [/\bpress\s+(?!\{ref\})/i, 'a button pressed by its name'],
        [/\bopen\s+"?(?:Edit|Advanced)\b/, 'a toggle opened by its name'],
        [/\bset\s+(?:Server|Endpoint|Protocol|Port|Region)\b/, 'a field set by its name'],
        [/\bleave\s+the\s+[\w ]+?\s+blank\b/i, 'fields left blank by their names'],
    ];
    if (!consolePath) cues.push([/\b(?:Endpoint|Region)\b/, 'an S3 field by its name']);
    return cues.filter(([cue]) => cue.test(prose)).map(([, what]) => what);
};

describe('preset text references', () => {
    it('resolves a reference to the label the form shows and leaves other braces alone', () => {
        const t = (key: string) => `<${key}>`;
        const provider = { fields: [{ key: 'password', label: 'WebDAV token', type: 'password' as const, required: true }] };
        expect(resolvePresetText('set {ui:connection.endpointUrl} to https://x/{username}', provider, t))
            .toBe('set <connection.endpointUrl> to https://x/{username}');
        expect(resolvePresetText('Enter it in {field:password}', provider, t)).toBe('Enter it in WebDAV token');
        // A broken reference stays visible instead of vanishing.
        expect(resolvePresetText('in {field:nope}', provider, t)).toBe('in {field:nope}');
        // So does a control whose key has no translation: `t` hands back the key.
        const missing = (key: string) => (key === 'connection.missing' ? key : `<${key}>`);
        expect(resolvePresetText('press {ui:connection.missing}', provider, missing)).toBe('press {ui:connection.missing}');
        expect(resolvePresetText('press {ui:connection.missing}', provider, () => '')).toBe('press {ui:connection.missing}');
    });

    it('resolves every reference of the registry in every language', () => {
        const references = everyText.flatMap(({ provider, where, text }) =>
            presetTextReferences(text).map((reference) => ({ provider, where, ...reference })));
        // The five presets that name a control of the form, at least once each.
        expect(new Set(references.map(({ provider }) => provider.id))).toEqual(new Set([
            'lifetimecloud', 'megacmd-webdav', 'filen-desktop-webdav', 'filen-desktop-s3', 's3drive',
        ]));
        expect(Object.keys(LOCALES).length).toBeGreaterThan(40);
        for (const { provider, where, kind, key } of references) {
            if (kind === 'field') {
                expect(provider.fields?.find((field) => field.key === key)?.label, `${where}: {field:${key}}`).toBeTruthy();
                continue;
            }
            for (const [path, locale] of Object.entries(LOCALES)) {
                const value = lookup(locale.translations, key);
                expect(typeof value === 'string' && value.length > 0, `${where}: {ui:${key}} in ${path}`).toBe(true);
            }
        }
    });

    it('names no control of the form in plain words', () => {
        const hits = everyText
            .map(({ where, text }) => ({ where, hits: literalControlNames(text) }))
            .filter(({ hits }) => hits.length > 0);
        expect(hits).toEqual([]);
        // The provider-console paths are most of the registry and stay silent.
        expect(everyText.filter(({ text }) => /→|->|\s>\s/.test(text)).length).toBeGreaterThan(50);
    });

    // The texts as they were before the references, one per string this
    // change rewrote: the check above has to catch each of them.
    it.each([
        ['puter-webdav password', 'Token auth: the auth token as password with -token as username, and set Server to https://dav.puter.com/<username> (the {username} path resolves to -token otherwise)'],
        ['filen-desktop-s3 bucket placeholder', 'Press Fetch to list your folders'],
        ['filen-desktop-s3 setup[3]', 'Leave the Access key and Secret key blank to use Filen Desktop\'s default "admin"/"admin"; AeroFTP fills them automatically (it does not store them silently)'],
        ['filen-desktop-s3 setup[5]', 'Default in Filen Desktop is HTTPS with a self-signed certificate; this preset matches that. If you switched Filen Desktop to plain HTTP, open Edit and change the endpoint scheme to http://'],
        ['filen-desktop-s3 setup[6]', 'Region stays "filen" (handled automatically)'],
        ['filen-desktop-s3 setup[7]', 'Enter the keys, then press Fetch next to Bucket: Filen Desktop lists every top-level folder of your Filen drive as a bucket.'],
        ['s3drive setup[3]', 'Copy access_key_id, secret_access_key and the bucket name into the fields here'],
        ['s3drive setup[4]', 'Endpoint and Region come from the same rclone snippet: open "Advanced" below if your values differ from the defaults (storage.kapsa.io / us-east-1)'],
        ['lifetimecloud setup[2]', 'Enter that WebDAV token in the WebDAV token field above, not your LifetimeCloud account password'],
        ['megacmd-webdav setup[2]', 'Just connect: AeroFTP runs "mega-webdav /" itself to start the bridge and read its address (no manual step). Fetch URL below is an optional manual refresh.'],
        ['megacmd-webdav setup[3]', 'If you enabled TLS in MEGAcmd (webdav -tls /), switch the Protocol field below to HTTPS'],
        ['filen-desktop-webdav setup[3]', 'Leave the WebDAV username and password blank to use Filen Desktop\'s default "admin"/"admin"; AeroFTP fills them automatically (it does not store them silently)'],
        ['filen-desktop-webdav setup[5]', 'Note the HTTP Protocol setting (HTTP or HTTPS) and pick the same value in the Protocol field below'],
    ])('catches the former %s', (_where, text) => {
        expect(literalControlNames(text)).not.toEqual([]);
    });
});

describe('a preset endpoint the user must type is not hidden under Advanced', () => {
    const s3 = PROVIDERS.filter((provider) => provider.protocol === 's3' && !provider.isGeneric);
    const needs = (provider: ProviderConfig) => presetNeedsUserEndpoint(provider);

    it('is the set of presets with a required endpoint field and no endpoint of their own', () => {
        const visible = s3.filter(needs).map(({ id }) => id).sort();
        expect(visible).toEqual(['backblaze', 'idrive-e2', 'minio', 'neon-s3', 'oracle-cloud', 'storj']);
        // The hand list it replaces, kept here to show what changed: three
        // presets enter, none leaves.
        const before = ['minio', 'oracle-cloud', 'storj'];
        expect(visible.filter((id) => !before.includes(id))).toEqual(['backblaze', 'idrive-e2', 'neon-s3']);
        expect(before.filter((id) => !visible.includes(id))).toEqual([]);
    });

    it('leaves every preset with a fixed or template endpoint out', () => {
        for (const provider of s3.filter((p) => p.defaults?.endpoint || p.defaults?.endpointTemplate)) {
            expect(needs(provider), provider.id).toBe(false);
        }
    });
});
