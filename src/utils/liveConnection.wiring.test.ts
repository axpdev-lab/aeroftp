// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { parseAst } from 'vite';
import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';

/**
 * The global `connectionParams` state of App.tsx is the connection FORM. A
 * connect from a saved profile empties it, and the servers screen can fill it
 * with another profile while a session stays live, so a reader that runs in a
 * session and takes the connection's facts from it reports another connection,
 * or none: the Debug Panel showed `Protocol SFTP`, `Server -`, `Username -` on a
 * live WebDAV session. Those readers take `liveConnectionParams`, the active
 * session's params (see resolveLiveConnectionParams).
 *
 * This test walks App.tsx and fails on every read of the form state outside
 * the places that are about the form itself:
 * - the derivation of `liveConnectionParams`, which falls back to the form
 *   when no session is active;
 * - `connectToFtpImpl`, the Quick Connect flow, which builds a connection
 *   FROM the form;
 * - the IntroHub `connectionParams` prop, which renders the form.
 */

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';

const FORM_STATE = 'connectionParams';
const FORM_CONTEXTS = new Set(['liveConnectionParams', 'connectToFtpImpl', '<IntroHub connectionParams>']);

const lineOf = (pos: number) => APP.slice(0, pos).split('\n').length;

/** The name a node gives to the code below it, if it names a context. */
function contextName(node: Node, parent: Node | null): string | null {
    if (node.type === 'VariableDeclarator' && (node.id as Node).type === 'Identifier' && node.init) {
        return (node.id as unknown as { name: string }).name;
    }
    if (node.type === 'FunctionDeclaration' && isNode(node.id)) return (node.id as unknown as { name: string }).name;
    if (node.type === 'JSXAttribute' && parent?.type === 'JSXOpeningElement') {
        const element = (parent.name as unknown as { name?: string }).name ?? 'element';
        const attribute = (node.name as unknown as { name?: string }).name ?? 'attribute';
        return `<${element} ${attribute}>`;
    }
    return null;
}

/** Whether `fn` declares a parameter that shadows the form state. */
function shadows(fn: Node): boolean {
    const params = (fn.params as Node[] | undefined) ?? [];
    return params.some(p => p.type === 'Identifier' && (p as unknown as { name: string }).name === FORM_STATE);
}

interface Read { line: number; context: string[] }

function formStateReads(source: string = APP): Read[] {
    const program = parseAst(source, { lang: 'tsx' }, 'App.tsx') as unknown as Node;
    const reads: Read[] = [];
    const visit = (node: Node, parent: Node | null, key: string | null, context: string[]) => {
        if (/Function/.test(node.type) && shadows(node)) return;
        const name = contextName(node, parent);
        const next = name ? [...context, name] : context;
        if (node.type === 'Identifier' && (node as unknown as { name: string }).name === FORM_STATE) {
            const isProperty = parent?.type === 'MemberExpression' && key === 'property' && !parent.computed;
            const isKey = parent?.type === 'Property' && key === 'key' && !parent.shorthand;
            const isDeclaration = parent?.type === 'ArrayPattern';
            // `x as T`, `x!`, `x satisfies T` and `x<T>` still read x.
            const isType = !!parent?.type.startsWith('TS') && !(key === 'expression' && /^TS(AsExpression|NonNullExpression|SatisfiesExpression|InstantiationExpression)$/.test(parent.type));
            if (!isProperty && !isKey && !isDeclaration && !isType) reads.push({ line: source.slice(0, node.start).split('\n').length, context: next });
        }
        for (const [childKey, value] of Object.entries(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, node, childKey, next));
            else if (isNode(value)) visit(value, node, childKey, next);
        }
    };
    visit(program, null, null, []);
    return reads;
}

describe('in-session readers use the live connection, not the connection form', () => {
    it('reads the form state only where the form is the subject', () => {
        const reads = formStateReads();
        expect(reads.length, 'the form state is read somewhere (the scan works)').toBeGreaterThan(0);
        const outside = reads
            .filter(read => !read.context.some(name => FORM_CONTEXTS.has(name)))
            .map(read => `App.tsx:${read.line} in ${read.context.filter(n => n !== 'App').slice(-2).join(' > ') || 'App'}`);
        expect(outside).toEqual([]);
    });

    it('counts a read behind `as` or `!`, and not the name in a type', () => {
        const sample = 'function App() { const a = connectionParams as Params; const b = connectionParams!.server; type T = typeof connectionParams; }';
        expect(formStateReads(sample)).toHaveLength(2);
    });

    it('derives the live params from the active session through the shared helper', () => {
        expect(APP).toMatch(/const liveConnectionParams = useMemo\(\s*\(\) => resolveLiveConnectionParams\(sessions, activeSessionId, connectionParams\)/);
    });

    it('feeds the Debug Panel the live connection, with no invented protocol', () => {
        const at = APP.indexOf('<DebugPanel');
        expect(at, 'DebugPanel is rendered by App.tsx').toBeGreaterThan(-1);
        const element = APP.slice(at, APP.indexOf('/>', at));
        expect(element).toContain('server: liveConnectionParams.server');
        expect(element).toContain('username: liveConnectionParams.username');
        expect(element).toContain('protocol: liveConnectionParams.protocol');
        // An unknown protocol is shown as unknown ('-'), never as SFTP.
        expect(element).not.toMatch(/\|\|\s*'sftp'/);
    });

    it('defaults no connection protocol to SFTP anywhere in App.tsx', () => {
        expect(APP.match(/protocol\s*(\|\||\?\?)\s*'sftp'/g) ?? []).toEqual([]);
    });
});

/** A function, or a call that wraps one (`asConnectPhase(async () => ...)`, `useCallback(...)`). */
function isFunctionLike(node: unknown): boolean {
    if (!isNode(node)) return false;
    if (node.type === 'ArrowFunctionExpression' || node.type === 'FunctionExpression') return true;
    return node.type === 'CallExpression' && ((node.arguments as Node[]) ?? []).some(isFunctionLike);
}

/** The name of a function-valued node: a const, a function declaration or a JSX handler prop. */
function functionName(node: Node, parent: Node | null): string | null {
    if (node.type === 'VariableDeclarator' && isFunctionLike(node.init)) return contextName(node, parent);
    if (node.type === 'FunctionDeclaration') return contextName(node, parent);
    if (node.type === 'JSXAttribute' && isFunctionLike((node.value as Node | null)?.expression)) return contextName(node, parent);
    return null;
}

const calleeName = (node: Node) => (node.callee as Node).type === 'Identifier' ? (node.callee as unknown as { name: string }).name : null;

interface Listing { fn: string; line: number; protocolArg: string | null }

/**
 * Every `loadRemoteFiles` call in a function that opens a connection (calls
 * `runConnect`), with the source of its first argument.
 */
function connectListings(): { connectFunctions: string[]; listings: Listing[] } {
    const program = parseAst(APP, { lang: 'tsx' }, 'App.tsx') as unknown as Node;
    const connecting = new Set<string>();
    const calls: Listing[] = [];
    const visit = (node: Node, parent: Node | null, fns: string[]) => {
        const name = functionName(node, parent);
        const next = name ? [...fns, name] : fns;
        if (node.type === 'CallExpression' && next.length > 0) {
            if (calleeName(node) === 'runConnect') connecting.add(next[next.length - 1]);
            if (calleeName(node) === 'loadRemoteFiles') {
                const first = ((node.arguments as Node[]) ?? [])[0];
                const protocolArg = first && !(first.type === 'Identifier' && (first as unknown as { name: string }).name === 'undefined')
                    ? APP.slice(first.start, first.end)
                    : null;
                for (const fn of next) calls.push({ fn, line: lineOf(node.start), protocolArg });
            }
        }
        for (const value of Object.values(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, node, next));
            else if (isNode(value)) visit(value, node, next);
        }
    };
    visit(program, null, []);
    return { connectFunctions: [...connecting].sort(), listings: calls.filter(call => connecting.has(call.fn)) };
}

describe('a connect lists the connection it opens', () => {
    /**
     * A connect lists before it creates its session tab, so the tab it
     * replaces is still the active session and `liveConnectionParams` is
     * still that tab's. A listing there that does not pass the protocol it is
     * opening takes the old session's: an FTP connect from the servers screen
     * over an S3 session listed through the provider API.
     */
    it('passes the connecting protocol to every listing in a function that opens a connection', () => {
        const { connectFunctions, listings } = connectListings();
        expect(connectFunctions, 'the scan finds the connect flows').toEqual(expect.arrayContaining(['connectToFtpImpl', '<IntroHub onSavedServerConnect>']));
        expect(listings.length, 'the connect flows list the remote folder').toBeGreaterThan(0);
        const unnamed = listings.filter(call => !call.protocolArg).map(call => `App.tsx:${call.line} in ${call.fn}`);
        expect(unnamed).toEqual([]);
    });

    it('decides whether AeroCloud can keep the live backend on the whole target', () => {
        const at = APP.indexOf('const isSameServer');
        expect(at, 'the AeroCloud reuse check exists').toBeGreaterThan(-1);
        expect(APP.slice(at, APP.indexOf(';', at))).toMatch(/^const isSameServer = isSameConnection\(/);
    });
});
