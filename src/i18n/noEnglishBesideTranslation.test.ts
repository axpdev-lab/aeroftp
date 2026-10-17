// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * A translated string is not glued to English written in the code. The vault
 * toolbar built its tooltips as `t('vault.scrubErrorCorrection') + ' (verify
 * cipher hashes)'`, so every locale showed a translated label followed by an
 * English sentence, and the overlay button prefixed its translated tooltip with
 * `AeroVault ON: `. No locale gate can see this: the English is in the source,
 * not in a locale file.
 *
 * This test finds a `t()` call joined with `+` to a string literal, or placed
 * in a template literal, where the literal text holds a word. Shortcut key
 * names (`(Ctrl+S)`), Aero product names and a version prefix are not prose
 * and pass. The text belongs in the translation, with the variable part as a
 * `{placeholder}`. Text next to `{t()}` in JSX is not covered.
 */

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.{ts,tsx}', '!/src/**/*.test.{ts,tsx}', '!/src/**/*.d.ts'],
    { query: '?raw', import: 'default', eager: true },
);

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';

function children(node: Node): Node[] {
    const out: Node[] = [];
    for (const value of Object.values(node)) {
        if (Array.isArray(value)) out.push(...value.filter(isNode));
        else if (isNode(value)) out.push(value);
    }
    return out;
}

const NEUTRAL = /\b(?:Ctrl|Shift|Alt|Cmd|Meta|Esc|Enter|Tab|Aero[A-Z][A-Za-z]*)\b/g;
const PROSE = /[A-Za-z]{2,}/;

const isT = (node: Node) =>
    node.type === 'CallExpression' && (node.callee as Node).type === 'Identifier' && (node.callee as { name: string }).name === 't';

const containsT = (node: Node): boolean => isT(node) || children(node).some(containsT);

function literalText(node: Node): string | null {
    if (node.type === 'Literal' && typeof node.value === 'string') return node.value;
    if (node.type === 'TemplateLiteral' && (node.expressions as Node[]).length === 0) {
        return (node.quasis as Array<{ value: { cooked: string } }>)[0].value.cooked;
    }
    return null;
}

const isProse = (text: string) => PROSE.test(text.replace(NEUTRAL, ''));

function offenders(sources: Record<string, string>): string[] {
    const out: string[] = [];
    for (const [file, source] of Object.entries(sources)) {
        const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
        const report = (node: Node) => {
            const line = source.slice(0, node.start).split('\n').length;
            out.push(`${file.replace(/^\//, '')}:${line} ${source.slice(node.start, node.end).replace(/\s+/g, ' ').slice(0, 90)}`);
        };
        const visit = (node: Node) => {
            if (node.type === 'BinaryExpression' && node.operator === '+') {
                const left = node.left as Node;
                const right = node.right as Node;
                const text = containsT(left) ? literalText(right) : containsT(right) ? literalText(left) : null;
                if (text !== null && isProse(text)) report(node);
            }
            if (node.type === 'TemplateLiteral' && (node.expressions as Node[]).some(containsT)) {
                const quasis = node.quasis as Array<{ value: { cooked: string } }>;
                if (quasis.some((quasi) => isProse(quasi.value.cooked))) report(node);
            }
            children(node).forEach(visit);
        };
        visit(program);
    }
    return out;
}

describe('a translated string is not glued to English in the code', () => {
    it('finds the shapes it is meant to find, and passes the neutral ones', () => {
        const sample = [
            "const a = t('vault.scrubErrorCorrection') + ' (verify cipher hashes)';",
            "const b = t('vault.remote.open') + ': .aerovault';",
            "const c = `AeroVault ON: ${t('toolbar.aerovaultOverlayActive') || 'fallback'}`;",
            "const d = 'Error: ' + t('common.failed');",
            "const e = `${t('devtools.editorPanel.save')} (Ctrl+S)`;",
            "const f = `${t('statusBar.updateAvailable')}: AeroFTP v${version}`;",
            "const g = t('vault.repairErrorCorrectionTooltip', { label: t('vault.repairErrorCorrection') });",
            "const h = t('a') + ' ' + t('b');",
            "const i = `${t('guiController.banner')}: ${ok ? t('common.open') : t('common.close')}`;",
        ].join('\n');
        expect(offenders({ 'sample.ts': sample })).toEqual([
            "sample.ts:1 t('vault.scrubErrorCorrection') + ' (verify cipher hashes)'",
            "sample.ts:2 t('vault.remote.open') + ': .aerovault'",
            "sample.ts:3 `AeroVault ON: ${t('toolbar.aerovaultOverlayActive') || 'fallback'}`",
            "sample.ts:4 'Error: ' + t('common.failed')",
        ]);
    });

    it('no source file of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(offenders(SOURCES)).toEqual([]);
    });
});
