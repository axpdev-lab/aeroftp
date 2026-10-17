// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * An element that aligns its content to the center or the right does not move
 * a bare icon. Tailwind's preflight makes every `svg` `display: block`, and
 * `text-align` places inline content only, so a lucide icon alone in a
 * `text-center` cell sits at the left edge under a centered header. The
 * Dependencies panel's Status column showed it (#1073 had moved the table
 * columns, the icons stayed left).
 *
 * The icon has to be centered by itself (`mx-auto`, or `inline-block` so the
 * text alignment applies), or the container has to lay it out (`flex` with
 * `justify-center`). This test finds every element whose class says
 * `text-center`, `text-right` or `text-end`, is not a flex or grid container,
 * and holds directly, or through a condition, a bare icon: a `lucide-react`
 * import, a raw `<svg>`, a component of the app whose every return is such an
 * icon, or a call to a function that returns one.
 */

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.tsx', '!/src/**/*.test.tsx'],
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

const ALIGNED = /(^|\s)(text-center|text-right|text-end)(\s|$)/;
const SELF_PLACED = /(^|\s)(inline|inline-block|inline-flex|mx-auto|ml-auto|m-auto)(\s|$)/;
const LAYOUT = /(^|\s)(flex|inline-flex|grid|inline-grid)(\s|$)/;

interface Parsed {
    file: string;
    source: string;
    program: Node;
    icons: Set<string>;
}

function parse(file: string, source: string): Parsed {
    const program = parseAst(source, { lang: 'tsx' }, file) as unknown as Node;
    const icons = new Set<string>();
    for (const statement of program.body as Node[]) {
        if (statement.type !== 'ImportDeclaration' || (statement.source as { value: string }).value !== 'lucide-react') continue;
        for (const specifier of statement.specifiers as Node[]) icons.add((specifier.local as { name: string }).name);
    }
    return { file, source, program, icons };
}

/** The class text an element declares, with quotes and braces read as spaces. */
function classOf(parsed: Parsed, element: Node): string {
    const opening = element.openingElement as Node;
    for (const attribute of opening.attributes as Node[]) {
        const name = attribute.name as Node | undefined;
        if (attribute.type === 'JSXAttribute' && name && (name as { name?: string }).name === 'className' && attribute.value) {
            const value = attribute.value as Node;
            return parsed.source.slice(value.start, value.end).replace(/[`'"{}$]/g, ' ');
        }
    }
    return '';
}

function tagOf(parsed: Parsed, node: Node): string | null {
    if (node.type !== 'JSXElement') return null;
    const name = (node.openingElement as Node).name as Node;
    return parsed.source.slice(name.start, name.end);
}

function unwrap(node: Node | null | undefined): Node | null {
    let current = node ?? null;
    while (current && (current.type === 'ParenthesizedExpression' || current.type === 'TSAsExpression')) current = current.expression as Node;
    return current;
}

/** The bare icons `node` renders as it stands: through conditions, not into other elements. */
function bareIcons(parsed: Parsed, node: Node | null, iconMakers: Set<string>): string[] {
    const expression = unwrap(node);
    if (!expression) return [];
    if (expression.type === 'ConditionalExpression') {
        return [
            ...bareIcons(parsed, expression.consequent as Node, iconMakers),
            ...bareIcons(parsed, expression.alternate as Node, iconMakers),
        ];
    }
    if (expression.type === 'LogicalExpression') return bareIcons(parsed, expression.right as Node, iconMakers);
    if (expression.type === 'CallExpression') {
        const callee = expression.callee as Node;
        const name = callee.type === 'Identifier' ? (callee as { name?: string }).name : undefined;
        return name && iconMakers.has(name) ? [`${name}()`] : [];
    }
    const tag = tagOf(parsed, expression);
    if (!tag || SELF_PLACED.test(classOf(parsed, expression))) return [];
    return parsed.icons.has(tag) || tag === 'svg' || iconMakers.has(tag) ? [tag] : [];
}

/** Names of functions and components of `parsed` whose every return is a bare icon. */
function iconMakersOf(parsed: Parsed, known: Set<string>): string[] {
    const found: string[] = [];
    const visit = (node: Node) => {
        let fn: Node | null = null;
        let name: string | undefined;
        if (node.type === 'VariableDeclarator' && isNode(node.init) && /Function/.test(node.init.type)) {
            fn = node.init;
            name = (node.id as { name?: string }).name;
        } else if (node.type === 'FunctionDeclaration' && isNode(node.id)) {
            fn = node;
            name = (node.id as { name?: string }).name;
        }
        if (fn && name) {
            const returns: Node[] = [];
            const body = fn.body as Node;
            if (body.type !== 'BlockStatement') returns.push(body);
            else {
                const collect = (inner: Node) => {
                    if (inner !== fn && /Function/.test(inner.type)) return;
                    if (inner.type === 'ReturnStatement' && isNode(inner.argument)) returns.push(inner.argument);
                    children(inner).forEach(collect);
                };
                collect(fn);
            }
            if (returns.length > 0 && returns.every(r => bareIcons(parsed, r, known).length > 0)) found.push(name);
        }
        children(node).forEach(visit);
    };
    visit(parsed.program);
    return found;
}

function offenders(files: Record<string, string>): string[] {
    const parsed = Object.entries(files).map(([file, source]) => parse(file, source));
    // A component that wraps a component that returns an icon: settle in a few rounds.
    const iconMakers = new Set<string>();
    for (let round = 0; round < 3; round++) {
        for (const p of parsed) iconMakersOf(p, iconMakers).forEach(name => iconMakers.add(name));
    }
    const found: string[] = [];
    for (const p of parsed) {
        const visit = (node: Node) => {
            if (node.type === 'JSXElement') {
                const cls = classOf(p, node);
                if (ALIGNED.test(cls) && !LAYOUT.test(cls)) {
                    const icons = (node.children as Node[]).flatMap(child =>
                        child.type === 'JSXExpressionContainer'
                            ? bareIcons(p, child.expression as Node, iconMakers)
                            : bareIcons(p, child, iconMakers),
                    );
                    if (icons.length > 0) {
                        const line = p.source.slice(0, node.start).split('\n').length;
                        found.push(`${p.file}:${line} <${tagOf(p, node)}> holds ${[...new Set(icons)].join(', ')}`);
                    }
                }
            }
            children(node).forEach(visit);
        };
        visit(p.program);
    }
    return found;
}

describe('an aligned element does not hold a bare icon', () => {
    it('the scan finds the shapes it is meant to find', () => {
        const sample = [
            "import { Check, X } from 'lucide-react';",
            'const Badge = ({ ok }: { ok: boolean }) => { if (ok) return <Check size={14} />; return <X size={14} />; };',
            'const iconFor = (ok: boolean) => (ok ? <Check /> : <X />);',
            'export const T = ({ ok }: { ok: boolean }) => (<table><tbody><tr>',
            '  <td className="text-center"><Badge ok={ok} /></td>',
            '  <td className="px-2 text-right">{ok ? <Check /> : <X className="text-red-500" />}</td>',
            '  <td className={`py-1 ${ok ? "text-center" : ""}`}>{iconFor(ok)}</td>',
            '  <td className="text-center"><svg /></td>',
            '  <td className="text-center"><Check className="mx-auto" /></td>',
            '  <td className="text-center"><span><Check /></span></td>',
            '  <td className="flex justify-center text-center"><Check /></td>',
            '</tr></tbody></table>);',
        ].join('\n');
        expect(offenders({ 'sample.tsx': sample })).toEqual([
            'sample.tsx:5 <td> holds Badge',
            'sample.tsx:6 <td> holds Check, X',
            'sample.tsx:7 <td> holds iconFor()',
            'sample.tsx:8 <td> holds svg',
        ]);
    });

    // Parsing every component takes a few seconds on an idle machine and timed out
    // at vitest's default 5 s under a load average of 17 to 20.
    it('no component of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(offenders(SOURCES)).toEqual([]);
    }, 30_000);
});
