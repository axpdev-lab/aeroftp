// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import { parseSync } from 'rolldown/experimental';
import APP from '../App.tsx?raw';
import { isTargetChangedRefusal } from './targetChanged';

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
        // `backendServesLiveTab` reads the connect phase, a teardown in flight and an uncertain backend.
        const phase = read.has('remoteConnectPhaseRef') || read.has('backendServesLiveTab');
        return phase && read.has('activeSessionId') && returns(guard.consequent as Node);
      });
      expect(guarded, 'provider_chmod can run on another session: its save no longer checks the session the dialog was opened on').toBe(true);
    }
  });

  it('sends one change per Apply while a change is in flight', () => {
    for (const { fn, call } of found.chmodCalls) {
      const guarded = guardsBefore(fn, call).some(guard => identifiers(guard.test as Node).has('permissionsSavingRef') && returns(guard.consequent as Node));
      expect(guarded, 'a second Apply while a permissions change is in flight sends another one').toBe(true);
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

  it('mounts one dialog per opening', () => {
    const { program } = parseSync('App.tsx', APP);
    const tags: Node[] = [];
    const visit = (node: Node) => {
      if (node.type === 'JSXOpeningElement' && (node.name as Node)?.type === 'JSXIdentifier' && (node.name as Node).name === 'PermissionsDialog') tags.push(node);
      for (const child of children(node)) visit(child);
    };
    visit(program as unknown as Node);
    expect(tags.length).toBeGreaterThan(0);
    for (const tag of tags) {
      const attrs = (tag.attributes as Node[]).map(a => ((a.name as Node)?.name as string) ?? '');
      expect(attrs, 'the Permissions dialog is reused across openings, so a mode edited for one file carries over to the next').toContain('key');
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

  /**
   * The save itself, as App.tsx has it, with the IPC and the window mocked
   * (Greptile on #1572): what the user sees for each answer of the backend.
   */
  describe('what the backend answers to the change', () => {
    const save = async (answer: () => unknown) => {
      expect(found.chmodCalls).toHaveLength(1);
      const { fn } = found.chmodCalls[0];
      const { code } = await transformWithOxc(`const onSave = ${APP.slice(fn.start, fn.end)};`, 'permissions-save.ts');
      const context = {
        permissionsDialog: { visible: true, openId: 1, sessionId: 's1', generation: 7, file: { name: 'a.txt', path: '/srv/a.txt' } },
        permissionsSavingRef: { current: false },
        backendServesLiveTab: () => true,
        activeSessionId: 's1',
        setPermissionsDialog: vi.fn(),
        notify: { error: vi.fn(), success: vi.fn() },
        t: (key: string) => key,
        invoke: vi.fn(async (command: string) => {
          if (command !== 'provider_chmod') throw new Error(`Unexpected IPC: ${command}`);
          return answer();
        }),
        loadRemoteFiles: vi.fn(),
        isTargetChangedRefusal,
      };
      const onSave = new Function(...Object.keys(context), `${code}; return onSave;`)(...Object.values(context)) as (mode: string) => Promise<void>;
      await onSave('644');
      expect(context.invoke).toHaveBeenCalledWith('provider_chmod', { path: '/srv/a.txt', mode: '644', generation: 7 });
      expect(context.permissionsSavingRef.current).toBe(false);
      return context;
    };
    const refusal = 'CONNECTION_CHANGED: the connection changed since the permissions were read; nothing was changed';

    it.each([
      ['as the backend words it', refusal],
      ['behind the words of the operation', `chmod failed: ${refusal}`],
      ['behind two wrappers', `Error: chmod failed: ${refusal}`],
      ['thrown as an Error', new Error(refusal)],
    ])('closes the dialog and says the session changed for a refusal %s', async (_label, thrown) => {
      const c = await save(() => { throw thrown; });
      expect(c.setPermissionsDialog.mock.calls).toEqual([[null]]);
      expect(c.notify.error.mock.calls).toEqual([['common.failed', 'toast.permissionsSessionChanged']]);
      expect(c.notify.success).not.toHaveBeenCalled();
      expect(c.loadRemoteFiles).not.toHaveBeenCalled();
    });

    it.each([
      ['an ordinary failure', 'Permission denied'],
      ['a failure about a file named after the marker', 'chmod failed: Permission denied on /srv/CONNECTION_CHANGED: notes.txt'],
    ])('keeps the dialog open and shows the error for %s', async (_label, thrown) => {
      const c = await save(() => { throw thrown; });
      expect(c.setPermissionsDialog).not.toHaveBeenCalled();
      expect(c.notify.error.mock.calls).toEqual([['common.failed', thrown]]);
      expect(c.notify.success).not.toHaveBeenCalled();
      expect(c.loadRemoteFiles).not.toHaveBeenCalled();
    });

    it('closes the dialog, confirms and reloads the listing when the change is applied', async () => {
      const c = await save(() => undefined);
      expect(c.setPermissionsDialog.mock.calls).toEqual([[null]]);
      expect(c.notify.error).not.toHaveBeenCalled();
      expect(c.notify.success).toHaveBeenCalledTimes(1);
      expect(c.loadRemoteFiles).toHaveBeenCalledWith(undefined, true);
    });
  });
});
