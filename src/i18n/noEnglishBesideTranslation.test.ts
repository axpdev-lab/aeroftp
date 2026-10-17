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
 * in a template literal, where the literal text holds a word. A variable used
 * there is followed to its declaration in scope (block, function, loop and catch
 * scopes, with parameters shadowing outer names): the
 * vault repair dialog put `const prefix = dry ? 'Dry-run: ' : ''` in front of
 * its translated result, and a portable device named itself `|| 'Portable
 * device'` in a translated error. Shortcut key names (`(Ctrl+S)`), Aero product names,
 * rclone crypt and a version prefix are not prose and pass. The text belongs in
 * the translation, with the variable part as a `{placeholder}`. Text next to
 * `{t()}` in JSX is not covered.
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

const NEUTRAL = /\b(?:Ctrl|Shift|Alt|Cmd|Meta|Esc|Enter|Tab|Aero[A-Z][A-Za-z]*|AERO[A-Z]+|(?:rclone|RCLONE)(?:[ -](?:crypt|CRYPT))?)\b/g;
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

// The texts a declaration can evaluate to: a literal, either branch of a
// condition, the value side of `&&`, or either side of `||` / `??`. A fallback
// after t() is not one: t() always returns a non-empty string, so it never shows.
function texts(node: Node): string[] {
    const text = literalText(node);
    if (text !== null) return [text];
    if (node.type === 'ConditionalExpression') return [...texts(node.consequent as Node), ...texts(node.alternate as Node)];
    if (node.type === 'LogicalExpression') {
        if (node.operator === '&&') return texts(node.right as Node);
        if (!containsT(node.left as Node)) return [...texts(node.left as Node), ...texts(node.right as Node)];
    }
    return [];
}

// Nodes that open a lexical scope for `const`, `let`, parameters and catch bindings.
const SCOPES = new Set([
    'BlockStatement', 'StaticBlock', 'ForStatement', 'ForInStatement', 'ForOfStatement', 'CatchClause',
    'FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression',
]);

// The names a binding pattern introduces: `a`, `{ a, b: c }`, `[a, ...d]`, `a = 1`, `a: string`.
function bindingNames(pattern: Node): string[] {
    if (pattern.type === 'Identifier') return [(pattern as unknown as { name: string }).name];
    if (pattern.type === 'AssignmentPattern') return bindingNames(pattern.left as Node);
    if (pattern.type === 'RestElement') return bindingNames(pattern.argument as Node);
    if (pattern.type === 'TSParameterProperty') return bindingNames(pattern.parameter as Node);
    if (pattern.type === 'ArrayPattern') return (pattern.elements as Array<Node | null>).filter(isNode).flatMap(bindingNames);
    if (pattern.type === 'ObjectPattern') {
        return (pattern.properties as Node[]).flatMap((property) =>
            bindingNames((property.type === 'Property' ? property.value : property) as Node));
    }
    return [];
}

function offenders(sources: Record<string, string>): string[] {
    const out: string[] = [];
    for (const [file, source] of Object.entries(sources)) {
        const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
        const report = (node: Node) => {
            const line = source.slice(0, node.start).split('\n').length;
            out.push(`${file.replace(/^\//, '')}:${line} ${source.slice(node.start, node.end).replace(/\s+/g, ' ').slice(0, 90)}`);
        };
        // One map per lexical scope, innermost last. A name resolves to the closest
        // scope that declares it; a parameter shadows with no value (null).
        const scopes: Array<Map<string, Node | null>> = [new Map()];
        const proseVariable = (node: Node) => {
            if (node.type !== 'Identifier') return false;
            const name = (node as unknown as { name: string }).name;
            const scope = [...scopes].reverse().find((candidate) => candidate.has(name));
            const init = scope?.get(name);
            return !!init && texts(init).some(isProse);
        };
        const visit = (node: Node) => {
            const opensScope = SCOPES.has(node.type);
            if (opensScope) {
                const scope = new Map<string, Node | null>();
                for (const param of (node.params as Node[] | undefined) ?? []) bindingNames(param).forEach((name) => scope.set(name, null));
                if (node.type === 'CatchClause' && isNode(node.param)) bindingNames(node.param).forEach((name) => scope.set(name, null));
                scopes.push(scope);
            }
            if (node.type === 'VariableDeclarator') {
                const scope = scopes[scopes.length - 1];
                if ((node.id as Node).type === 'Identifier') {
                    scope.set((node.id as unknown as { name: string }).name, isNode(node.init) ? node.init : null);
                } else {
                    bindingNames(node.id as Node).forEach((name) => scope.set(name, null));
                }
            }
            if (node.type === 'BinaryExpression' && node.operator === '+') {
                const left = node.left as Node;
                const right = node.right as Node;
                const other = containsT(left) ? right : containsT(right) ? left : null;
                const text = other ? literalText(other) : null;
                if ((text !== null && isProse(text)) || (other && proseVariable(other))) report(node);
            }
            if (node.type === 'TemplateLiteral' && (node.expressions as Node[]).some(containsT)) {
                const quasis = node.quasis as Array<{ value: { cooked: string } }>;
                const expressions = node.expressions as Node[];
                if (quasis.some((quasi) => isProse(quasi.value.cooked)) || expressions.some(proseVariable)) {
                    report(node);
                }
            }
            children(node).forEach(visit);
            if (opensScope) scopes.pop();
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
            "const prefix = dry ? 'Dry-run: ' : '';",
            "const j = `${prefix}${t('vault.noDamageNothing')}`;",
            "const label = isAero ? 'AEROCRYPT' : 'RCLONE CRYPT';",
            "const k = `${label} · ${t('aerocrypt.lock')}`;",
            "const tail = dry && ' (no changes written)';",
            "const m = t('vault.close') + tail;",
            "const name = profile?.name || device.displayName || 'Portable device';",
            "const n = `${t('sidebar.portable_open_failed')}: ${name}`;",
            "const shown = t('vault.close') || 'Close';",
            "const o = t('vault.open') + shown;",
            "const prefix2 = 'Error: ';",
            "function helper() { const prefix2 = ''; return prefix2; }",
            "const p = `${prefix2}${t('vault.close')}`;",
            "const quiet = '';",
            "function loud() { const quiet = 'Loud text'; return quiet; }",
            "const q = `${quiet}${t('vault.close')}`;",
            "const label2 = 'Some text';",
            "const fmt = (label2: string) => `${label2}${t('vault.close')}`;",
        ].join('\n');
        expect(offenders({ 'sample.ts': sample })).toEqual([
            "sample.ts:1 t('vault.scrubErrorCorrection') + ' (verify cipher hashes)'",
            "sample.ts:2 t('vault.remote.open') + ': .aerovault'",
            "sample.ts:3 `AeroVault ON: ${t('toolbar.aerovaultOverlayActive') || 'fallback'}`",
            "sample.ts:4 'Error: ' + t('common.failed')",
            "sample.ts:11 `${prefix}${t('vault.noDamageNothing')}`",
            "sample.ts:15 t('vault.close') + tail",
            "sample.ts:17 `${t('sidebar.portable_open_failed')}: ${name}`",
            "sample.ts:22 `${prefix2}${t('vault.close')}`",
        ]);
    });

    // Parsing every source file takes about 3 s on an idle machine and over 10 s
    // under a load average of 30, past vitest's default 5 s.
    it('no source file of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(offenders(SOURCES)).toEqual([]);
    }, 30_000);
});
