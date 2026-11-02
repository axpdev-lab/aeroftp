// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { parseAst } from 'vite';
import { describe, expect, it } from 'vitest';
import APP from '../App.tsx?raw';

/**
 * The backend refuses an SFTP or SCP connect to a host whose key is not in
 * known_hosts ("has no key saved"): the frontend asks first, through
 * `checkSftpHostKey` and its acceptance dialog. A connect path that skips the
 * check never opens the dialog, so a new host cannot be reached from it: the
 * AeroCloud tab did that for an SCP or SFTP profile.
 *
 * This test walks App.tsx and fails on every `provider_connect` that its
 * enclosing named function does not precede with the `isSshProtocol` gated
 * host-key check, and on every one whose function ends the current
 * connection before that check: a refused key would leave the user with no
 * connection at all. Exempt: `switchGitHubBranch`, which connects to GitHub only.
 */

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';

const EXEMPT = new Set(['switchGitHubBranch']);

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

/** Whether a call opens a backend connection with `provider_connect`. */
function isProviderConnect(node: Node): boolean {
    if (node.type !== 'CallExpression') return false;
    const callee = node.callee as Node;
    const name = callee.type === 'Identifier' ? (callee as unknown as { name: string }).name : null;
    if (name !== 'runConnect' && name !== 'invoke') return false;
    const first = (node.arguments as Node[])[0];
    return first?.type === 'Literal' && (first as unknown as { value: unknown }).value === 'provider_connect';
}

interface Connect { line: number; context: string; checked: boolean; teardownFirst: boolean }

const CHECK = /if \(isSshProtocol\([^)]*\)\) \{\s*(?:const target = normalizeProviderConnectionParams\([^)]*\);\s*)?const accepted = await [^;]*checkSftpHostKey\(/;
/** A helper that only defines the teardown, run after the check. */
const TEARDOWN_HELPER = /const endCurrentConnection = async \(\) => \{[\s\S]*?\n\s*\};/;

function providerConnects(source: string = APP): Connect[] {
    const program = parseAst(source, { lang: 'tsx' }, 'App.tsx') as unknown as Node;
    const found: Connect[] = [];
    const visit = (node: Node, parent: Node | null, context: { name: string; start: number } | null) => {
        const name = contextName(node, parent);
        const next = name ? { name, start: node.start } : context;
        if (isProviderConnect(node) && next) {
            const before = source.slice(next.start, node.start);
            // The teardown nearest the connect is the one on its path: an
            // earlier one belongs to another branch of the same function.
            const path = before.replace(TEARDOWN_HELPER, '');
            const check = path.search(CHECK);
            const teardown = path.lastIndexOf("invoke('provider_disconnect')");
            found.push({
                line: source.slice(0, node.start).split('\n').length,
                context: next.name,
                checked: check >= 0,
                teardownFirst: check >= 0 && teardown >= 0 && teardown < check,
            });
        }
        for (const value of Object.values(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, node, next));
            else if (isNode(value)) visit(value, node, next);
        }
    };
    visit(program, null, null);
    return found;
}

describe('every SSH connect path asks for the host key first', () => {
    it('finds the connect paths it checks', () => {
        const contexts = providerConnects().map(c => c.context);
        // The AeroCloud tab, Quick Connect, the session switch, a saved profile.
        expect(contexts).toEqual(expect.arrayContaining(['connectToCloudServer', 'connectToFtpImpl', 'switchSession']));
        expect(contexts.length).toBeGreaterThanOrEqual(5);
    });

    it('runs checkSftpHostKey, gated on isSshProtocol, before each provider_connect', () => {
        const unchecked = providerConnects()
            .filter(c => !c.checked && !EXEMPT.has(c.context))
            .map(c => `App.tsx:${c.line} in ${c.context}`);
        expect(unchecked).toEqual([]);
    });

    it('asks for the AeroCloud host key before it ends the current connection', () => {
        // A refused key restores the previous session tab as connected, so
        // nothing may disconnect it before the answer.
        const start = APP.indexOf('const handleCloudTabClick = async');
        const end = APP.indexOf('const changeRemoteDirectory = async', start);
        const cloudTab = APP.slice(start, end);
        const helper = cloudTab.slice(cloudTab.indexOf('const connectToCloudServer = async'), cloudTab.indexOf('const navigateToRemoteFolder = async'));
        const disconnects = (text: string) => text.split("invoke('provider_disconnect')").length - 1;
        expect(disconnects(helper)).toBe(1);
        expect(disconnects(cloudTab)).toBe(1);
        expect(helper.indexOf('checkSftpHostKey(')).toBeGreaterThan(-1);
        expect(helper.indexOf('checkSftpHostKey(')).toBeLessThan(helper.indexOf('await endCurrentConnection();'));
        expect(helper.indexOf('const endCurrentConnection')).toBeLessThan(helper.indexOf('checkSftpHostKey('));
    });

    it('never ends the current connection before the host-key check', () => {
        const early = providerConnects()
            .filter(c => c.teardownFirst && !EXEMPT.has(c.context))
            .map(c => `App.tsx:${c.line} in ${c.context}`);
        expect(early).toEqual([]);
    });

    it('asks for the host key before switchSession changes the active tab', () => {
        // A refused key must leave the user on the tab they were on: asked
        // after the switch, it left the target tab showing over the previous
        // tab's live connection, where a remote action reached the wrong server.
        const start = APP.indexOf('const switchSession = async');
        const body = APP.slice(start, APP.indexOf('\n  const ', start + 1));
        expect(start).toBeGreaterThan(-1);
        expect(body.indexOf('checkSftpHostKey(')).toBeGreaterThan(-1);
        expect(body.indexOf('checkSftpHostKey(')).toBeLessThan(body.indexOf('setActiveSessionId(sessionId)'));
    });

    it('flags a provider_connect with no host-key check before it', () => {
        const source = 'const connectSomewhere = async () => { await runConnect(\'provider_connect\', params); };';
        expect(providerConnects(source)).toEqual([{ line: 1, context: 'connectSomewhere', checked: false, teardownFirst: false }]);
    });

    it('flags a teardown before the host-key check', () => {
        const source = [
            'const connectSomewhere = async () => {',
            "  await invoke('provider_disconnect');",
            '  if (isSshProtocol(p)) {',
            '    const accepted = await checkSftpHostKey(h, 22);',
            '  }',
            "  await runConnect('provider_connect', params);",
            '};',
        ].join('\n');
        expect(providerConnects(source)).toEqual([{ line: 6, context: 'connectSomewhere', checked: true, teardownFirst: true }]);
    });
});
