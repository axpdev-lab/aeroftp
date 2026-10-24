// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * A number is not rendered in front of a word translated on its own. The vault
 * creation dialog showed `{stagedCount} {stagedCount === 1 ? t('vault.itemSingular')
 * : t('vault.itemPlural')}`, and the status bar `{localFileCount}
 * {t('browser.items')}`: a translator sees "items" with no number, so every
 * language with more than one plural form (Czech "2 položky" but "5 položek",
 * Russian "2 файла" but "5 файлов", Lithuanian, Polish, Romanian "20 de
 * fișiere") reads wrong for most counts, and a language that puts the number
 * after the noun cannot do it at all. The number belongs in the translation as
 * a `{count}` placeholder, in a form that reads right for every number.
 *
 * This test finds, in JSX, an expression followed by a space and then a
 * translated word (`t()`, a choice between two `t()` calls, `t('x') ||
 * 'fallback'`, `t('x').toLowerCase()`), and the same pair in a template
 * literal (`${n} ${t('browser.files')}`). An expression that renders an
 * element (an icon), a formatted size (`formatBytes(size)`: the unit is the
 * noun), a version string or a name or label is not a count and passes.
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

const isT = (node: Node) =>
    node.type === 'CallExpression' && (node.callee as Node).type === 'Identifier' && (node.callee as { name: string }).name === 't';

const contains = (node: Node, test: (node: Node) => boolean): boolean => test(node) || children(node).some((child) => contains(child, test));

/** A word that comes out of a translation: `t()`, a choice of two, a fallback, a case change. */
function translatedWord(node: Node): boolean {
    if (isT(node)) return true;
    if (node.type === 'ConditionalExpression') return translatedWord(node.consequent as Node) && translatedWord(node.alternate as Node);
    if (node.type === 'LogicalExpression') return node.operator !== '&&' && translatedWord(node.left as Node);
    if (node.type === 'ParenthesizedExpression') return translatedWord(node.expression as Node);
    if (node.type === 'CallExpression' && (node.callee as Node).type === 'MemberExpression') {
        return translatedWord((node.callee as { object: Node }).object);
    }
    return false;
}

/** The last name of an identifier, a member (`tab.icon`) or a computed member's object (`ICONS[key]`). */
function lastName(node: Node): string | null {
    if (node.type === 'Identifier') return node.name as string;
    if (node.type === 'MemberExpression') {
        return node.computed ? lastName(node.object as Node) : lastName(node.property as Node);
    }
    return null;
}

const NOT_A_COUNT = /(?:icon|label|version|name)s?$/i;

function count(node: Node): boolean {
    if (contains(node, isT) || contains(node, (n) => n.type === 'JSXElement' || n.type === 'JSXFragment')) return false;
    if (node.type === 'Literal' || node.type === 'TemplateLiteral' || node.type === 'JSXEmptyExpression') return false;
    if (node.type === 'BinaryExpression' && node.operator === '+') {
        if ([node.left, node.right].some((side) => (side as Node).type === 'Literal' && typeof (side as Node).value === 'string')) return false;
    }
    if (node.type === 'CallExpression' && /^format/.test(lastName(node.callee as Node) ?? '')) return false;
    return !NOT_A_COUNT.test(lastName(node) ?? '');
}

const blank = (node: Node) =>
    (node.type === 'JSXText' && /^\s+$/.test(node.value as string)) ||
    (node.type === 'JSXExpressionContainer' && (node.expression as Node).type === 'Literal' && (node.expression as Node).value === ' ');

function scan(sources: Record<string, string>): string[] {
    const found: string[] = [];
    for (const [file, source] of Object.entries(sources)) {
        const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
        const report = (start: number, end: number) => {
            const line = source.slice(0, start).split('\n').length;
            found.push(`${file.replace(/^\/src\//, '')}:${line} ${source.slice(start, end).replace(/\s+/g, ' ')}`);
        };
        const visit = (node: Node) => {
            if (node.type === 'JSXElement' || node.type === 'JSXFragment') {
                const list = node.children as Node[];
                list.forEach((child, index) => {
                    if (child.type !== 'JSXExpressionContainer' || !count(child.expression as Node)) return;
                    let next = index + 1;
                    while (next < list.length && blank(list[next])) next += 1;
                    const word = list[next];
                    if (next > index + 1 && word?.type === 'JSXExpressionContainer' && translatedWord(word.expression as Node)) {
                        report(child.start, word.end);
                    }
                });
            }
            if (node.type === 'TemplateLiteral') {
                const expressions = node.expressions as Node[];
                const quasis = node.quasis as Array<{ value: { cooked: string } }>;
                for (let index = 0; index + 1 < expressions.length; index += 1) {
                    if (/^\s+$/.test(quasis[index + 1].value.cooked) && count(expressions[index]) && translatedWord(expressions[index + 1])) {
                        // Include the `${` and `}` around the pair.
                        report(expressions[index].start - 2, expressions[index + 1].end + 1);
                    }
                }
            }
            children(node).forEach(visit);
        };
        visit(program);
    }
    return found;
}

describe('a number is not rendered in front of a word translated on its own', () => {
    it('finds the shapes it is meant to find, and passes the neutral ones', () => {
        const sample = [
            "const a = <span>{count} {t('browser.items')}</span>;",
            "const b = <span>{n} {n === 1 ? t('vault.itemSingular') : t('vault.itemPlural')}</span>;",
            "const c = <span>{list.length}{' '}{t('aerosync.sync.errorDetails') || 'errors'}</span>;",
            "const d = `${files.length} ${t('browser.files')}`;",
            "const e = `(${skipped} ${t('settings.bridgeSkipped').toLowerCase()})`;",
            "const f = <span>{total.toLocaleString()} {t('diskUsage.items')}</span>;",
            "const g = <span>{t('browser.items', { count })}</span>;",
            "const h = <span>{t('diskUsage.total')} {count}</span>;",
            "const i = <button>{busy ? <Spinner /> : <Icon />} {t('common.save')}</button>;",
            "const j = <span>{tab.icon} {t('debug.tabs.logs')}</span>;",
            "const k = <span>{formatBytes(size)} {t('duplicates.wasted')}</span>;",
            "const l = <span>{'v' + plugin.version} {t('ai.settings.pluginBy')}</span>;",
            "const m = `${update.latest_version} ${t('statusbar.updateAvailable')}`;",
            "const n = <span>{count}{t('common.items')}</span>;",
            "const o = `${label} ${t('statusBar.usedNoCap')}`;",
            "const p = <p>{/* a comment */} {t('duplicates.deleteConfirm', { count })}</p>;",
        ].join('\n');
        expect(scan({ 'sample.tsx': sample })).toEqual([
            "sample.tsx:1 {count} {t('browser.items')}",
            "sample.tsx:2 {n} {n === 1 ? t('vault.itemSingular') : t('vault.itemPlural')}",
            "sample.tsx:3 {list.length}{' '}{t('aerosync.sync.errorDetails') || 'errors'}",
            "sample.tsx:4 ${files.length} ${t('browser.files')}",
            "sample.tsx:5 ${skipped} ${t('settings.bridgeSkipped').toLowerCase()}",
            "sample.tsx:6 {total.toLocaleString()} {t('diskUsage.items')}",
        ]);
    });

    // Parsing every source file takes a few seconds, more under load, past
    // vitest's default 5 s.
    it('no source file of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(scan(SOURCES)).toEqual([]);
    }, 30_000);
});
