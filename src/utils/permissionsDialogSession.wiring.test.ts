// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { parseSync } from 'rolldown/experimental';
import APP from '../App.tsx?raw';

/**
 * Wiring guard for the Permissions dialog (#1144 review). `provider_chmod`
 * acts on whatever session the backend holds when Apply is clicked, and the
 * call carries no session. A dialog opened on one tab's listing and applied
 * after a tab switch or a reconnect would change the same path on another
 * server.
 *
 * The property checked here, on the real syntax tree (oxc, via rolldown):
 * every function that calls `invoke('provider_chmod', ...)` first runs an
 * `if` whose test reads both `remoteConnectPhaseRef` and `activeSessionId`
 * and whose branch returns, and passes the connection `generation` the
 * backend re-checks under its provider lock (the frontend check alone
 * races a switch that lands while the command waits for the lock); every
 * `setPermissionsDialog({...})` that opens the dialog records the
 * `sessionId` and the `generation` it was opened on.
 */

type Node = { type: string; start: number; end: number; [key: string]: unknown };

function children(node: Node): Node[] {
  const out: Node[] = [];
  for (const value of Object.values(node)) {
    if (Array.isArray(value)) {
      for (const item of value) if (item && typeof item === 'object' && 'type' in item) out.push(item as Node);
    } else if (value && typeof value === 'object' && 'type' in (value as object)) {
      out.push(value as Node);
    }
  }
  return out;
}

const FUNCTION_TYPES = new Set(['ArrowFunctionExpression', 'FunctionExpression', 'FunctionDeclaration']);

function identifiers(node: Node, into = new Set<string>()): Set<string> {
  if (node.type === 'Identifier') into.add(node.name as string);
  for (const child of children(node)) identifiers(child, into);
  return into;
}

function returns(node: Node): boolean {
  if (node.type === 'ReturnStatement') return true;
  return children(node).some(returns);
}

function isCallTo(node: Node, name: string): boolean {
  const callee = node.type === 'CallExpression' ? (node.callee as Node) : null;
  return callee?.type === 'Identifier' && callee.name === name;
}

interface Found {
  chmodCalls: { fn: Node; call: Node }[];
  dialogOpens: Node[];
}

function scan(source: string): Found {
  const { program, errors } = parseSync('App.tsx', source);
  expect(errors, 'App.tsx must parse cleanly').toEqual([]);
  const found: Found = { chmodCalls: [], dialogOpens: [] };
  const stack: Node[] = [];
  const visit = (node: Node) => {
    stack.push(node);
    if (isCallTo(node, 'invoke')) {
      const first = (node.arguments as Node[])[0];
      if (first?.type === 'Literal' && first.value === 'provider_chmod') {
        const fn = [...stack].reverse().find(n => FUNCTION_TYPES.has(n.type));
        if (fn) found.chmodCalls.push({ fn, call: node });
      }
    }
    if (isCallTo(node, 'setPermissionsDialog')) {
      const arg = (node.arguments as Node[])[0];
      if (arg?.type === 'ObjectExpression') found.dialogOpens.push(arg);
    }
    for (const child of children(node)) visit(child);
    stack.pop();
  };
  visit(program as unknown as Node);
  return found;
}

/** `if` statements inside `fn` that end before `call` starts. */
function guardsBefore(fn: Node, call: Node): Node[] {
  const out: Node[] = [];
  const visit = (node: Node) => {
    if (node.type === 'IfStatement' && node.end <= call.start) out.push(node);
    for (const child of children(node)) visit(child);
  };
  visit(fn);
  return out;
}

describe('Permissions dialog session binding', () => {
  const found = scan(APP);

  it('finds the provider_chmod call and the dialog opens it guards', () => {
    expect(found.chmodCalls.length).toBeGreaterThan(0);
    expect(found.dialogOpens.length).toBeGreaterThan(0);
  });

  it('refuses a save when the session changed or a connect/switch is running', () => {
    for (const { fn, call } of found.chmodCalls) {
      const guarded = guardsBefore(fn, call).some(guard => {
        const read = identifiers(guard.test as Node);
        return read.has('remoteConnectPhaseRef') && read.has('activeSessionId') && returns(guard.consequent as Node);
      });
      expect(guarded, 'provider_chmod can run on another session: its save no longer checks the session the dialog was opened on').toBe(true);
    }
  });

  it('hands the backend the connection generation to re-check under its lock', () => {
    for (const { call } of found.chmodCalls) {
      const args = (call.arguments as Node[])[1];
      const keys = args?.type === 'ObjectExpression' ? (args.properties as Node[]).map(p => ((p.key as Node)?.name as string) ?? '') : [];
      expect(keys, 'provider_chmod is called without the generation, so a switch during the call reaches the new connection').toContain('generation');
    }
  });

  it('drops an opening that a later one overtook', () => {
    const { program } = parseSync('App.tsx', APP);
    const opens: { fn: Node; call: Node }[] = [];
    const stack: Node[] = [];
    const visit = (node: Node) => {
      stack.push(node);
      if (isCallTo(node, 'setPermissionsDialog')) {
        const arg = (node.arguments as Node[])[0];
        const fn = [...stack].reverse().find(n => FUNCTION_TYPES.has(n.type));
        if (arg?.type === 'ObjectExpression' && fn) opens.push({ fn, call: node });
      }
      for (const child of children(node)) visit(child);
      stack.pop();
    };
    visit(program as unknown as Node);
    expect(opens.length).toBeGreaterThan(0);
    for (const { fn, call } of opens) {
      const guarded = guardsBefore(fn, call).some(guard => identifiers(guard.test as Node).has('permissionsOpenSeqRef') && returns(guard.consequent as Node));
      expect(guarded, 'an earlier Permissions opening that resolves late can replace the dialog of the file picked after it').toBe(true);
    }
  });

  it('records the session every time the dialog opens', () => {
    for (const arg of found.dialogOpens) {
      const keys = (arg.properties as Node[]).map(p => ((p.key as Node)?.name as string) ?? '');
      if (!keys.includes('visible')) continue;
      expect(keys, 'the Permissions dialog opens without the session it belongs to').toContain('sessionId');
      expect(keys, 'the Permissions dialog opens without the connection generation it belongs to').toContain('generation');
    }
  });
});
