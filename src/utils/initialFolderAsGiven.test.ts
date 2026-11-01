// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';

/**
 * A profile's initial folder is used as given: `/reports ` is its own folder,
 * and the providers, the CLI and the TUI open it by that name. A frontend that
 * trims it opens, shows, mounts or compares another folder.
 *
 * This is a source guard, not a behaviour test: it reads the frontend through
 * Vite's raw glob and looks for a trim of a value named after the initial
 * folder (`initialPath`, `localInitialPath`, `serverInitialPath`,
 * `initialSourcePath`, ...), written on one line or across several.
 *
 * Outside its reach on purpose: the AeroCrypt overlay anchor
 * (`remoteScope?.trim() || initialPath?.trim()`). Its normalization trims on
 * both sides, `normCryptScope` here and `norm_anchor` in the backend, and the
 * write guard decides inside or outside the vault with it, so it changes on
 * both sides together or not at all.
 */
const SOURCES = import.meta.glob(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}'], {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

const NAME = String.raw`\w*[iI]nitial\w*Path`;
const TRIM = String.raw`trim(?:Start|End)?\(`;
const PATTERNS = [
    // initialPath.trim(), profile?.initialPath?.trim(), initialSourcePath?.trim()
    new RegExp(String.raw`\b${NAME}\s*\??\.\s*${TRIM}`, 'g'),
    // (initialPath ?? '').trim(), (initialPath || "").trimEnd()
    new RegExp(String.raw`\(\s*[\w.?]*\b${NAME}\s*(?:\?\?|\|\|)\s*(?:''|""|\`\`)\s*\)\s*\.\s*${TRIM}`, 'g'),
    // initialPath: form.path.trim(), initial_path: value.trim()
    new RegExp(String.raw`\b(?:${NAME}|initial_path)\s*:\s*[\w.?]+\s*\??\.\s*${TRIM}`, 'g'),
];
const ANCHOR = /Scope\s*\??\.\s*trim\(\)\s*\|\|\s*$/;

function trimsOfTheInitialFolder(source: string): string[] {
    // One line per statement is not how this code is written: read it as one.
    const text = source.replace(/\/\/[^\n]*/g, ' ').replace(/\s+/g, ' ');
    const found: string[] = [];
    for (const pattern of PATTERNS) {
        for (const match of text.matchAll(pattern)) {
            const before = text.slice(Math.max(0, match.index - 80), match.index);
            if (ANCHOR.test(before.replace(/[\w.?]*$/, ''))) continue;
            found.push(text.slice(Math.max(0, match.index - 40), match.index + match[0].length));
        }
    }
    return found;
}

describe('the initial folder is used as given', () => {
    it('reads the frontend sources', () => {
        const names = Object.keys(SOURCES);
        expect(names.some(name => name.endsWith('/App.tsx'))).toBe(true);
        expect(names.some(name => name.endsWith('/aeroSyncCliCommand.ts'))).toBe(true);
        expect(names.some(name => name.endsWith('.test.ts'))).toBe(false);
    });

    it.each([
        "const wantedRemote = profile?.initialPath?.trim();",
        "if (prof.initialPath?.trim()) {",
        "{profile.initialPath.trim()}",
        "const base = (initialPath ?? '').trim();",
        "setSourcePath(initialSourcePath?.trim() || fallback);",
        "const wantedLocal = profile?.localInitialPath\n    ?.trimEnd();",
        "const draft = { initialPath: form.remotePath.trim() };",
        "invoke('x', { initial_path: value.trimStart() });",
    ])('finds a trim of the initial folder: %s', (sample) => {
        expect(trimsOfTheInitialFolder(sample)).toHaveLength(1);
    });

    it.each([
        "const wantedRemote = profile?.initialPath;",
        "const base = initialPath.replace(/\\/+$/, '');",
        "const label = initialLabel.trim();",
        "const name = form.name.trim();",
        "const scope = binding.remoteScope?.trim() || profile.initialPath?.trim() || '';",
        "const overlayScope = remoteScope?.trim() || initialPath?.trim() || '';",
    ])('leaves alone what is not one: %s', (sample) => {
        expect(trimsOfTheInitialFolder(sample)).toEqual([]);
    });

    it('nothing in the frontend trims the initial folder', () => {
        const found = Object.entries(SOURCES).flatMap(([name, source]) =>
            trimsOfTheInitialFolder(source).map(snippet => `${name}: ${snippet}`));
        expect(found).toEqual([]);
    });
});
