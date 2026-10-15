// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// A var() naming a custom property that nothing defines resolves to the initial
// value of the property: a transparent background, a border in the text color, an
// inherited text color. The browser reports nothing, so a theme drops out silently;
// VaultSyncDialog and CloudPanel.css rendered that way with legacy --bg-*,
// --border-* and --text-* names that no stylesheet defines.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

interface Scan {
    defined: Set<string>;
    referenced: Map<string, Set<string>>;
}

const NAME = '--[A-Za-z0-9_-]+';
// Tailwind writes its own --tw-* properties into the generated stylesheet.
const isTailwindInternal = (name: string) => name.startsWith('--tw-');

function scanText(scan: Scan, file: string, text: string): void {
    const isCss = file.endsWith('.css') || file.endsWith('.html');
    const source = isCss ? text.replace(/\/\*[\s\S]*?\*\//g, '') : text;
    const definitions = isCss
        // A declaration, never the name inside var( or after a comma.
        ? [new RegExp(`(?<![\\w(,-])(${NAME})\\s*:`, 'g')]
        : [
            // Style object keys, computed ones included: { '--x': v, ['--y' as string]: v }.
            new RegExp(`['"\`](${NAME})['"\`]\\s*(?:as\\s+\\w+\\s*)?\\]?\\s*:`, 'g'),
            new RegExp(`setProperty\\(\\s*['"\`](${NAME})`, 'g'),
            // Tailwind arbitrary property: [--x:value].
            new RegExp(`\\[(${NAME}):`, 'g'),
        ];
    for (const pattern of definitions) {
        for (const match of source.matchAll(pattern)) scan.defined.add(match[1]);
    }
    for (const match of source.matchAll(new RegExp(`var\\(\\s*(${NAME})`, 'g'))) {
        const users = scan.referenced.get(match[1]) ?? new Set<string>();
        users.add(file);
        scan.referenced.set(match[1], users);
    }
}

function scanFiles(files: string[]): Scan {
    const scan: Scan = { defined: new Set(), referenced: new Map() };
    for (const file of files) scanText(scan, file, readFileSync(file, 'utf8'));
    return scan;
}

function shippedSources(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) return shippedSources(path);
        // Tests set placeholder properties of their own and never ship.
        return /\.(css|ts|tsx)$/.test(entry.name) && !entry.name.endsWith('.test.ts') ? [path] : [];
    });
}

function undefinedReferences(scan: Scan): Record<string, string[]> {
    return Object.fromEntries([...scan.referenced]
        .filter(([name]) => !scan.defined.has(name) && !isTailwindInternal(name))
        .map(([name, users]) => [name, [...users].sort()]));
}

describe('custom property scanner', () => {
    const scanSnippet = (file: string, text: string) => {
        const scan: Scan = { defined: new Set(), referenced: new Map() };
        scanText(scan, file, text);
        return scan;
    };

    it('reads CSS declarations as definitions and every var() as a reference', () => {
        const scan = scanSnippet('a.css', '.a { --x: 1px; color: var(--y, var(--z)); }\n/* --c: 1; var(--d) */');
        expect([...scan.defined]).toEqual(['--x']);
        expect([...scan.referenced.keys()].sort()).toEqual(['--y', '--z']);
        expect(undefinedReferences(scan)).toEqual({ '--y': ['a.css'], '--z': ['a.css'] });
    });

    it('reads style keys, setProperty and Tailwind arbitrary properties in components', () => {
        const scan = scanSnippet('b.tsx', `
            <div style={{ ['--k' as string]: v, '--q': 1 }} className="[--t:2px] bg-[var(--u)]" />;
            el.style.setProperty('--s', value);`);
        expect([...scan.defined].sort()).toEqual(['--k', '--q', '--s', '--t']);
        expect(undefinedReferences(scan)).toEqual({ '--u': ['b.tsx'] });
    });
});

describe('shipped styles', () => {
    const files = ['index.html', ...shippedSources('src')];
    const scan = scanFiles(files);

    it('scans the real tree, not an empty one', () => {
        expect(files.length).toBeGreaterThan(500);
        // One definition of each kind, and references through both a stylesheet and a component.
        for (const name of ['--color-bg-secondary', '--tpb-toggle-bg', '--app-font-size']) {
            expect(scan.defined.has(name), name).toBe(true);
        }
        expect(scan.referenced.get('--color-bg-secondary')?.has(join('src', 'styles.css'))).toBe(true);
        expect(scan.referenced.has('--tpb-toggle-bg')).toBe(true);
    });

    it('defines every custom property a var() names', () => {
        // Each key is a property nothing defines, with the files that read it:
        // map it onto a --color-* theme token from src/styles.css, or define it.
        expect(undefinedReferences(scan)).toEqual({});
    });
});
