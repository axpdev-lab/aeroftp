// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// A var() naming a custom property that nothing defines resolves to the initial
// value of the property: a transparent background, a border in the text color, an
// inherited text color. The browser reports nothing, so a theme drops out silently;
// VaultSyncDialog and CloudPanel.css rendered that way with legacy --bg-*,
// --border-* and --text-* names that no stylesheet defines.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseAst } from 'vite';
import { describe, expect, it } from 'vitest';

interface Scan {
    defined: Set<string>;
    referenced: Map<string, Set<string>>;
}

interface AstNode {
    type: string;
    [field: string]: unknown;
}

const NAME = '--[A-Za-z0-9_-]+';
// Tailwind writes its own --tw-* properties into the generated stylesheet.
const isTailwindInternal = (name: string) => name.startsWith('--tw-');
// Expression wrappers between a style key and its string: ['--x' as string].
const WRAPPERS = new Set(['TSAsExpression', 'TSSatisfiesExpression', 'TSTypeAssertion', 'TSNonNullExpression', 'ParenthesizedExpression']);

function addReferences(scan: Scan, file: string, text: string): void {
    for (const match of text.matchAll(new RegExp(`var\\(\\s*(${NAME})`, 'g'))) {
        const users = scan.referenced.get(match[1]) ?? new Set<string>();
        users.add(file);
        scan.referenced.set(match[1], users);
    }
}

function scanStylesheet(scan: Scan, file: string, text: string): void {
    const source = text.replace(/\/\*[\s\S]*?\*\//g, '');
    // A declaration, never the name inside var( or after a comma.
    for (const match of source.matchAll(new RegExp(`(?<![\\w(,-])(${NAME})\\s*:`, 'g'))) scan.defined.add(match[1]);
    addReferences(scan, file, source);
}

const isAstNode = (value: unknown): value is AstNode =>
    typeof value === 'object' && value !== null && typeof (value as AstNode).type === 'string';

// A string literal defines a property when it is a style key ({ '--x': v },
// { ['--x' as string]: v }) or the name passed to element.style.setProperty.
function isDefinitionSite(node: AstNode, ancestors: AstNode[]): boolean {
    let child = node;
    let index = ancestors.length - 1;
    while (index >= 0 && WRAPPERS.has(ancestors[index].type)) child = ancestors[index--];
    const parent = ancestors[index];
    if (parent?.type === 'Property') return parent.key === child;
    if (parent?.type !== 'CallExpression' || (parent.arguments as unknown[])[0] !== child) return false;
    const callee = parent.callee as AstNode;
    const method = callee.type === 'MemberExpression' ? callee.property as AstNode : undefined;
    return method?.type === 'Identifier' && method.name === 'setProperty';
}

// Components are parsed, not pattern matched: comments, JSX text and the other
// branch of a ternary never reach the scan, only string and template contents do.
function scanScript(scan: Scan, file: string, text: string): void {
    const ancestors: AstNode[] = [];
    const visit = (node: AstNode): void => {
        const literal = node.type === 'Literal' && typeof node.value === 'string' ? node.value
            : node.type === 'TemplateLiteral' && (node.expressions as unknown[]).length === 0
                ? ((node.quasis as AstNode[])[0].value as { cooked: string | null }).cooked ?? undefined
                : undefined;
        if (literal !== undefined && new RegExp(`^${NAME}$`).test(literal) && isDefinitionSite(node, ancestors)) {
            scan.defined.add(literal);
        }
        const content = node.type === 'Literal' && typeof node.value === 'string' ? node.value
            : node.type === 'TemplateElement' ? (node.value as { raw: string }).raw : undefined;
        if (content !== undefined) {
            addReferences(scan, file, content);
            // Tailwind arbitrary property in a class string: [--x:value].
            for (const match of content.matchAll(new RegExp(`\\[(${NAME}):`, 'g'))) scan.defined.add(match[1]);
        }
        ancestors.push(node);
        for (const field of Object.values(node)) {
            for (const child of Array.isArray(field) ? field : [field]) if (isAstNode(child)) visit(child);
        }
        ancestors.pop();
    };
    visit(parseAst(text, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }) as unknown as AstNode);
}

function scanText(scan: Scan, file: string, text: string): void {
    if (file.endsWith('.css') || file.endsWith('.html')) scanStylesheet(scan, file, text);
    else scanScript(scan, file, text);
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

    it('ignores comments, JSX text and plain string values in components', () => {
        const scan = scanSnippet('c.tsx', `
            // const legacy = { '--c': 1 };
            /* className="bg-[var(--d)]" */
            const tone = dark ? '--e' : '--f';
            const bar = { [\`--y\`]: 1 };
            <a className="bg-[var(--u)]">Don't open http://example.com {\`p \${q} var(--m)\`}</a>;
            <p>var(--g)</p>;`);
        expect([...scan.defined]).toEqual(['--y']);
        expect(undefinedReferences(scan)).toEqual({ '--m': ['c.tsx'], '--u': ['c.tsx'] });
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
