// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Preset help text that names an AeroFTP control.
 *
 * A preset's help text, placeholder or setup step that tells the user to
 * press or fill a control of this form must not spell the control's name: the
 * name is translated, and it changes with the form. It writes a reference
 * instead, resolved where the text is rendered:
 *
 * - `{ui:<i18n key>}` is the translated label of a control of the form, for
 *   example `{ui:protocol.advanced}` for the Advanced toggle;
 * - `{field:<key>}` is the label this preset gives its own field, for example
 *   `{field:password}` for LifetimeCloud's "WebDAV token".
 *
 * Paths inside the provider's own console ("Profile > Enable WebDAV") are not
 * AeroFTP controls and stay literal. Any other brace text (`{username}` in a
 * URL) is left as written.
 */

import type { ProviderConfig } from './types';

const REFERENCE = /\{(ui|field):([\w.-]+)\}/g;

export interface PresetTextReference {
    kind: 'ui' | 'field';
    key: string;
}

/** Every reference a text makes, in order. */
export const presetTextReferences = (text: string): PresetTextReference[] =>
    [...text.matchAll(REFERENCE)].map(([, kind, key]) => ({ kind: kind as 'ui' | 'field', key }));

/**
 * The text with each reference replaced by the name the form shows. A
 * reference that does not resolve is left as written, so a broken one is
 * visible rather than silently dropped; the registry test fails on it.
 */
export const resolvePresetText = (
    text: string,
    provider: Pick<ProviderConfig, 'fields'> | null | undefined,
    t: (key: string) => string,
): string =>
    text.replace(REFERENCE, (reference, kind: string, key: string) => {
        if (kind === 'ui') return t(key);
        return provider?.fields?.find((field) => field.key === key)?.label || reference;
    });

/**
 * Every text of a preset that the connection form renders: each field's
 * placeholder and help text, then the setup steps. `where` names it for a
 * failing assertion.
 */
export const presetTexts = (provider: ProviderConfig): Array<{ where: string; text: string }> => [
    ...(provider.fields || []).flatMap((field) => [
        ...(field.placeholder ? [{ where: `${field.key}.placeholder`, text: field.placeholder }] : []),
        ...(field.helpText ? [{ where: `${field.key}.helpText`, text: field.helpText }] : []),
    ]),
    ...(provider.setupInstructions || []).map((text, index) => ({ where: `setup[${index}]`, text })),
];
