// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';
import appRaw from '../App.tsx?raw';

/**
 * Every callback, memo and effect of App lists the values of App it reads.
 *
 * A callback built with `useCallback(fn, deps)` keeps the values of the render
 * that last built it: one it reads but does not list is the value of an older
 * render (the remote folder, the files, the vault of then). App's `notify` was
 * rebuilt on every render, and so was everything that listed it, which hid
 * those gaps: `executeUnifiedTransferPlan` read `currentRemotePath` and
 * `remoteFiles` without listing them, the native-menu Refresh kept the
 * hidden-files setting of the first render, the keep-alive kept the toast
 * setting of the connect. The same rebuild made about 50 hooks run again on
 * every render, effects that subscribe to events among them.
 *
 * Values that keep their identity need not be listed: refs, state setters,
 * constants, and functions from `useLatestCallback` (or a memo with no
 * dependencies). A function declared in App counts as the values it reads. An
 * effect that is meant to run only on some of its reads (it reads the rest
 * when it runs) says so with the `react-hooks/exhaustive-deps` comment on its
 * dependency list.
 */

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';
const children = (node: Node): Node[] => Object.entries(node).flatMap(([key, value]) =>
    key === 'parent' ? [] : Array.isArray(value) ? value.filter(isNode) : isNode(value) ? [value] : []);
const FUNCTION = /^(FunctionDeclaration|FunctionExpression|ArrowFunctionExpression)$/;
const HOOKS = new Set(['useCallback', 'useMemo', 'useEffect', 'useLayoutEffect']);

const ast = parseAst(appRaw, { lang: 'tsx' }) as unknown as Node;
const lineOf = (offset: number) => appRaw.slice(0, offset).split('\n').length;
const text = (node: Node) => appRaw.slice(node.start, node.end);
const calleeName = (callee: Node): string | null => callee.type === 'Identifier' ? callee.name as string
    : callee.type === 'MemberExpression' && !callee.computed ? (callee.property as Node).name as string : null;

function patternNames(pattern: Node | null | undefined, out: string[] = []): string[] {
    if (!pattern) return out;
    if (pattern.type === 'Identifier') out.push(pattern.name as string);
    else if (pattern.type === 'ObjectPattern') for (const p of pattern.properties as Node[]) patternNames(p.type === 'RestElement' ? p.argument as Node : p.value as Node, out);
    else if (pattern.type === 'ArrayPattern') for (const e of pattern.elements as Node[]) patternNames(e, out);
    else if (pattern.type === 'AssignmentPattern') patternNames(pattern.left as Node, out);
    else if (pattern.type === 'RestElement') patternNames(pattern.argument as Node, out);
    return out;
}

/** App's component function. */
const app = (() => {
    for (const statement of (ast.body as Node[])) {
        const declarations = statement.type === 'VariableDeclaration' ? statement.declarations as Node[] : [];
        for (const d of declarations) {
            if ((d.id as Node).name !== 'App') continue;
            const init = d.init as Node;
            return init.type === 'CallExpression' ? (init.arguments as Node[])[0] : init;
        }
    }
    throw new Error('App not found');
})();

type Kind = 'stable' | 'value' | 'function';
/** What each binding declared in App's body is. */
const kinds = new Map<string, Kind>();
const functions = new Map<string, Node>();
for (const statement of (app.body as Node).body as Node[]) {
    if (statement.type === 'FunctionDeclaration') {
        kinds.set((statement.id as Node).name as string, 'function');
        functions.set((statement.id as Node).name as string, statement);
        continue;
    }
    if (statement.type !== 'VariableDeclaration') continue;
    for (const d of statement.declarations as Node[]) {
        const id = d.id as Node;
        const init = d.init as Node | null;
        const hook = init?.type === 'CallExpression' ? calleeName(init.callee as Node) : null;
        const args = (init?.arguments ?? []) as Node[];
        if (hook === 'useRef' || hook === 'useLatestCallback'
            || ((hook === 'useCallback' || hook === 'useMemo') && args[1]?.type === 'ArrayExpression' && (args[1].elements as Node[]).length === 0)) {
            patternNames(id).forEach(name => kinds.set(name, 'stable'));
        } else if ((hook === 'useState' || hook === 'useReducer') && id.type === 'ArrayPattern') {
            const [value, setter] = id.elements as Node[];
            patternNames(value).forEach(name => kinds.set(name, 'value'));
            patternNames(setter).forEach(name => kinds.set(name, 'stable'));
        } else if (id.type === 'ObjectPattern') {
            // `const { x, setX, SOME_KEY } = useSomething()`: setters and constants keep their identity.
            for (const p of id.properties as Node[]) {
                const key = p.type === 'Property' ? (p.key as Node).name as string : '';
                const stable = /^set[A-Z]/.test(key) || /^[A-Z][A-Z0-9_]+$/.test(key);
                patternNames(p.type === 'RestElement' ? p.argument as Node : p.value as Node).forEach(name => kinds.set(name, stable ? 'stable' : 'value'));
            }
        } else if (id.type === 'Identifier' && init && FUNCTION.test(init.type)) {
            kinds.set(id.name as string, 'function');
            functions.set(id.name as string, init);
        } else if (id.type === 'Identifier' && init && (init.type === 'Literal' || (init.type === 'TemplateLiteral' && !(init.expressions as Node[]).length))) {
            kinds.set(id.name as string, 'stable');
        } else {
            patternNames(id).forEach(name => kinds.set(name, 'value'));
        }
    }
}

/** Names a function reads from outside itself. */
function freeNames(fn: Node): Set<string> {
    const found = new Set<string>();
    const declared = (f: Node): Set<string> => {
        const names = new Set((f.params as Node[]).flatMap(p => patternNames(p)));
        const visit = (n: Node) => {
            if (n.type === 'VariableDeclarator') patternNames(n.id as Node).forEach(name => names.add(name));
            if (n.type === 'CatchClause' && n.param) patternNames(n.param as Node).forEach(name => names.add(name));
            if (n.type === 'FunctionDeclaration' || n.type === 'ClassDeclaration') { if (n.id) names.add((n.id as Node).name as string); if (n.type === 'FunctionDeclaration') return; }
            if (FUNCTION.test(n.type)) return;
            children(n).forEach(visit);
        };
        visit(f.body as Node);
        return names;
    };
    const visit = (n: Node, parent: Node | null, key: string, scopes: Set<string>[]) => {
        if (FUNCTION.test(n.type) && n !== fn) {
            const inner = [...scopes, declared(n)];
            for (const p of n.params as Node[]) visit(p, n, 'params', inner);
            visit(n.body as Node, n, 'body', inner);
            return;
        }
        if (n.type === 'Identifier') {
            const property = parent && ((parent.type === 'MemberExpression' && key === 'property' && !parent.computed)
                || (parent.type === 'Property' && key === 'key' && !parent.computed && !parent.shorthand)
                || parent.type === 'JSXAttribute' || parent.type.startsWith('TS'));
            if (!property && !scopes.some(scope => scope.has(n.name as string))) found.add(n.name as string);
            return;
        }
        if (n.type.startsWith('TS') && !/^TS(AsExpression|NonNullExpression|SatisfiesExpression|InstantiationExpression)$/.test(n.type)) return;
        for (const [childKey, value] of Object.entries(n)) {
            if (childKey === 'parent') continue;
            for (const child of Array.isArray(value) ? value : [value]) if (isNode(child)) visit(child, n, childKey, scopes);
        }
    };
    const top = [declared(fn)];
    for (const p of fn.params as Node[]) visit(p, fn, 'params', top);
    visit(fn.body as Node, fn, 'body', top);
    return found;
}

/** A binding whose value can differ from one render to the next. */
const reactive = (() => {
    const memo = new Map<string, boolean>();
    const check = (name: string, seen: Set<string>): boolean => {
        const kind = kinds.get(name);
        if (!kind || kind === 'stable') return false;
        if (kind === 'value') return true;
        if (memo.has(name)) return memo.get(name)!;
        if (seen.has(name)) return false;
        seen.add(name);
        const result = [...freeNames(functions.get(name)!)].some(read => read !== name && check(read, seen));
        memo.set(name, result);
        return result;
    };
    return (name: string) => check(name, new Set());
})();

type Hook = { line: number; hook: string; fn: Node; deps: Node | undefined; optOut: boolean };
/** Every hook call in App's body (hooks cannot sit inside a function of it). */
const hooks: Hook[] = [];
const collect = (n: Node) => {
    if (FUNCTION.test(n.type)) return;
    if (n.type === 'CallExpression' && HOOKS.has(calleeName(n.callee as Node) ?? '')) {
        const [fn, deps] = n.arguments as Node[];
        const lines = appRaw.split('\n');
        const depsLine = deps ? lineOf(deps.end) : 0;
        const optOut = !!deps && /react-hooks\/exhaustive-deps/.test(`${lines[depsLine - 2]}\n${lines[depsLine - 1]}`);
        hooks.push({ line: lineOf(n.start), hook: calleeName(n.callee as Node)!, fn, deps, optOut });
        return;
    }
    children(n).forEach(collect);
};
((app.body as Node).body as Node[]).forEach(collect);

const depRoots = (deps: Node) => (deps.elements as Node[]).map(e => text(e).split(/[.?[(]/)[0].trim());

describe('every hook of App lists the values of App it reads', () => {
    it('finds App\'s hooks', () => {
        expect(hooks.length).toBeGreaterThan(150);
        expect(kinds.get('notify')).toBe('value');
        expect(kinds.get('loadRemoteFiles')).toBe('stable');
    });

    it('lists each value it reads, or says that it runs on some of them only', () => {
        const unlisted: string[] = [];
        for (const { line, hook, fn, deps, optOut } of hooks) {
            if (!deps || deps.type !== 'ArrayExpression' || optOut || !FUNCTION.test(fn.type)) continue;
            const listed = new Set(depRoots(deps));
            const missing = [...freeNames(fn)].filter(name => !listed.has(name) && reactive(name));
            if (missing.length) unlisted.push(`App.tsx:${line} ${hook} reads ${missing.join(', ')}`);
        }
        expect(unlisted).toEqual([]);
    });

    it('leaves to run later, when it lists only some of its reads, only values that keep their identity', () => {
        // Such an effect reads the rest when it runs, which is fine; a timer,
        // a listener or an async step it starts reads them later, from the
        // render that ran it (the native menu kept the first render's
        // loaders, the keep-alive the toast setting of the connect).
        const SYNC = new Set(['map', 'filter', 'find', 'findIndex', 'some', 'every', 'forEach', 'reduce', 'flatMap', 'sort']);
        const later: string[] = [];
        for (const { line, hook, fn, optOut } of hooks) {
            if (!optOut || !/Effect$/.test(hook) || !FUNCTION.test(fn.type)) continue;
            const deferred: Node[] = [];
            const handedNames = new Set<string>();
            const visit = (n: Node, parent: Node | null) => {
                if (n.type === 'CallExpression' && !SYNC.has(calleeName(n.callee as Node) ?? '')) {
                    // An App function handed over by name runs later too.
                    const handed = (n.arguments as Node[]).flatMap(a => a.type === 'ObjectExpression'
                        ? (a.properties as Node[]).map(p => p.value as Node).filter(isNode) : [a]);
                    for (const a of handed) if (a.type === 'Identifier' && kinds.get(a.name as string) === 'function') handedNames.add(a.name as string);
                }
                if (FUNCTION.test(n.type) && n !== fn) {
                    const callee = parent?.type === 'CallExpression' ? parent.callee as Node : null;
                    const handedOver = !!callee && (parent!.arguments as Node[]).includes(n) && !SYNC.has(calleeName(callee) ?? '');
                    if (n.async || handedOver) { deferred.push(n); return; }
                }
                children(n).forEach(child => visit(child, n));
            };
            visit(fn.body as Node, fn);
            const reads = new Set([...handedNames, ...deferred.flatMap(d => [...freeNames(d)])]);
            const unstable = [...reads].filter(name => reactive(name));
            if (unstable.length) later.push(`App.tsx:${line} ${hook} reads later ${unstable.join(', ')}`);
        }
        expect(later).toEqual([]);
    });

    it('depends on no value that App builds again on every render', () => {
        // A function declared in App without a hook is a new one on every
        // render: a hook listing it would run (or be rebuilt) every time.
        const fresh: string[] = [];
        for (const { line, hook, deps } of hooks) {
            if (!deps || deps.type !== 'ArrayExpression') continue;
            const rebuilt = depRoots(deps).filter(name => kinds.get(name) === 'function');
            if (rebuilt.length) fresh.push(`App.tsx:${line} ${hook} lists ${rebuilt.join(', ')}`);
        }
        expect(fresh).toEqual([]);
    });
});
