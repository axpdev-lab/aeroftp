// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * No component or hook writes a ref while it renders.
 *
 * React can render and then discard the result, and a ref written during that
 * render keeps a value from a tree that never committed: a callback, the
 * options or the rows of a render that was thrown away. A latest-value ref is
 * written in a layout effect, once the render commits and before an event can
 * read it. A write guarded by a null check (`if (ref.current === null)
 * ref.current = ...`) is lazy initialisation, which React allows.
 *
 * The test parses every source file and lists each `x.current = ...` whose
 * nearest enclosing function is a component or a hook (`Name`, `useName`),
 * not a callback or an effect inside it.
 */

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.ts', '/src/**/*.tsx', '!/src/**/*.test.ts', '!/src/**/*.test.tsx', '!/src/**/*.d.ts'],
    { query: '?raw', import: 'default', eager: true },
);

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';
const FUNCTION = /^(FunctionDeclaration|FunctionExpression|ArrowFunctionExpression)$/;

/** The name a function is declared or bound under, through one call (`memo(() => ...)`). */
function functionName(fn: Node, ancestors: Node[]): string | null {
    const id = fn.id as Node | null | undefined;
    if (id && id.type === 'Identifier') return id.name as string;
    let parent = ancestors[ancestors.length - 1];
    if (parent?.type === 'CallExpression') parent = ancestors[ancestors.length - 2];
    const bound = parent?.type === 'VariableDeclarator' ? parent.id as Node : null;
    return bound?.type === 'Identifier' ? bound.name as string : null;
}

const LAZY_INIT = /\.current\s*(===?|==)\s*(null|undefined)|!\s*[\w.]+\.current\b/;

function renderTimeRefWrites(file: string, source: string): string[] {
    const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
    const found: string[] = [];
    const visit = (node: Node, ancestors: Node[]) => {
        const left = node.left as Node | undefined;
        if (node.type === 'AssignmentExpression' && node.operator === '=' && left?.type === 'MemberExpression'
            && !left.computed && (left.property as Node).name === 'current') {
            let i = ancestors.length - 1;
            while (i >= 0 && !FUNCTION.test(ancestors[i].type)) i--;
            const name = i >= 0 ? functionName(ancestors[i], ancestors.slice(0, i)) : null;
            const lazy = ancestors.slice(i).some(a => a.type === 'IfStatement'
                && LAZY_INIT.test(source.slice((a.test as Node).start, (a.test as Node).end)));
            if (name && /^([A-Z]|use[A-Z])/.test(name) && !lazy) {
                const line = source.slice(0, node.start).split('\n').length;
                found.push(`${file}:${line} ${name}: ${source.slice(node.start, node.end).split('\n')[0]}`);
            }
        }
        for (const value of Object.values(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, [...ancestors, node]));
            else if (isNode(value)) visit(value, [...ancestors, node]);
        }
    };
    visit(program, []);
    return found;
}

describe('no ref is written during render', () => {
    it('reads the sources', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(Object.keys(SOURCES)).toContain('/src/App.tsx');
    });

    it('writes every latest-value ref after the commit', () => {
        const writes = Object.entries(SOURCES).flatMap(([file, source]) => renderTimeRefWrites(file, source));
        expect(writes).toEqual([]);
    });

    it('finds a write during render, and leaves callbacks, effects and lazy initialisation alone', () => {
        const source = [
            'function Panel({ onClose }) {',
            '  const closeRef = useRef(onClose);',
            '  closeRef.current = onClose;',
            '  const lazy = useRef(null);',
            '  if (lazy.current === null) lazy.current = new Map();',
            '  useLayoutEffect(() => { closeRef.current = onClose; });',
            '  const onClick = () => { closeRef.current = null; };',
            '  return null;',
            '}',
            'const useLatest = (value) => { const r = useRef(value); r.current = value; return r; };',
            'const View = memo(() => { const r = useRef(0); r.current = 1; return null; });',
        ].join('\n');
        expect(renderTimeRefWrites('/src/x.tsx', source)).toEqual([
            '/src/x.tsx:3 Panel: closeRef.current = onClose',
            '/src/x.tsx:10 useLatest: r.current = value',
            '/src/x.tsx:11 View: r.current = 1',
        ]);
    });
});
