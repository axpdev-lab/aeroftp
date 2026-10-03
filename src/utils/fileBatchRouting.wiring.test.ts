// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { parseSync } from 'rolldown/experimental';
import APP from '../App.tsx?raw';

/**
 * Issue #591, wiring guard. A multi-file transfer in the GUI ran one file at
 * a time on every protocol because each entry point looped over its items
 * and awaited `downloadFile` / `uploadFile` once per item. The parallel
 * branch that existed sat behind `!usesProviderApi(protocol)`, which no
 * connected session satisfies.
 *
 * The property checked here: every handler in App.tsx that calls
 * `downloadFile` or `uploadFile` INSIDE A LOOP must also route its items
 * through `splitForFileBatch`, so the plain files leave as one backend batch
 * and only folders (or a lone file) take the loop. A loop is a `for`/`while`
 * statement or the callback of an iterating array method (`map`, `forEach`,
 * `flatMap`, so `Promise.all(items.map(...))` counts), and the call may be
 * direct or through a member (`transferFnsRef.current.downloadFile(...)`).
 * A call outside any loop (double-click, a per-row retry callback) is a single
 * transfer and is fine.
 *
 * Occurrences are classified on the real syntax tree (oxc, via rolldown), not
 * by text search: a comment or a string that mentions `downloadFile(` cannot
 * satisfy or fail the check. A handler added later that loops over items
 * without the split fails here by name.
 */

type Node = { type: string; [key: string]: unknown };

const LOOP_TYPES = new Set(['ForStatement', 'ForOfStatement', 'ForInStatement', 'WhileStatement', 'DoWhileStatement']);
const FUNCTION_TYPES = new Set(['ArrowFunctionExpression', 'FunctionExpression', 'FunctionDeclaration']);
const TRANSFER_FNS = new Set(['downloadFile', 'uploadFile']);
const ITERATING_METHODS = new Set(['map', 'forEach', 'flatMap']);

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

function calleeName(node: Node): string | null {
  if (node.type !== 'CallExpression') return null;
  const callee = node.callee as Node;
  if (callee?.type === 'Identifier') return callee.name as string;
  if (callee?.type === 'MemberExpression' && !callee.computed) {
    const property = callee.property as Node;
    if (property?.type === 'Identifier') return property.name as string;
  }
  return null;
}

/** A function passed straight to `.map` / `.forEach` / `.flatMap` runs once
 *  per item: it is a loop body. */
function isIteratingCallback(fn: Node, parent: Node | undefined): boolean {
  if (!parent || parent.type !== 'CallExpression') return false;
  if (!(parent.arguments as Node[] | undefined)?.includes(fn)) return false;
  const name = calleeName(parent);
  return name !== null && ITERATING_METHODS.has(name);
}

function containsCallTo(node: Node, name: string): boolean {
  if (calleeName(node) === name) return true;
  return children(node).some(child => containsCallTo(child, name));
}

/** Name of the `const name = <function>` (or `= withBatchLatch(<function>)`)
 *  that owns `fn`, looking at the declarator just above it on the stack. */
function handlerName(stack: Node[], fnIndex: number): string | null {
  for (let i = fnIndex - 1; i >= 0; i--) {
    const node = stack[i];
    if (FUNCTION_TYPES.has(node.type)) return null; // an anonymous inner function
    if (node.type === 'VariableDeclarator') {
      const id = node.id as Node;
      return id?.type === 'Identifier' ? (id.name as string) : null;
    }
  }
  return null;
}

interface LoopCall {
  handler: string;
  callee: string;
  handlerNode: Node;
}

function loopTransferCalls(source: string): LoopCall[] {
  const { program, errors } = parseSync('App.tsx', source);
  expect(errors, 'App.tsx must parse cleanly').toEqual([]);
  const found: LoopCall[] = [];
  const stack: Node[] = [];
  const visit = (node: Node) => {
    stack.push(node);
    const name = calleeName(node);
    if (name && TRANSFER_FNS.has(name)) {
      // Walk up to the nearest function boundary, noting any loop on the way.
      // An iterating callback is itself a loop: keep walking past it.
      let inLoop = false;
      for (let i = stack.length - 2; i >= 0; i--) {
        const ancestor = stack[i];
        if (LOOP_TYPES.has(ancestor.type)) inLoop = true;
        if (FUNCTION_TYPES.has(ancestor.type) && isIteratingCallback(ancestor, stack[i - 1])) {
          inLoop = true;
          continue;
        }
        if (FUNCTION_TYPES.has(ancestor.type)) {
          if (inLoop) {
            // The loop is inside this function; the handler is the named
            // function that encloses it (possibly this one).
            let owner: string | null = null;
            let ownerNode: Node | null = null;
            for (let j = i; j >= 0; j--) {
              if (!FUNCTION_TYPES.has(stack[j].type)) continue;
              const named = handlerName(stack, j);
              if (named) {
                owner = named;
                ownerNode = stack[j];
                break;
              }
            }
            if (owner && ownerNode) found.push({ handler: owner, callee: name, handlerNode: ownerNode });
          }
          break;
        }
      }
    }
    for (const child of children(node)) visit(child);
    stack.pop();
  };
  visit(program as unknown as Node);
  return found;
}

describe('the wiring guard sees the loop shapes it claims to see', () => {
  const offenders = (source: string) =>
    [...new Set(loopTransferCalls(source).map(c => c.handler))];

  it('a for-of loop', () => {
    expect(offenders('const h = async (xs) => { for (const x of xs) await downloadFile(x); };')).toEqual(['h']);
  });

  it('Promise.all over a map callback', () => {
    expect(offenders('const h = async (xs) => { await Promise.all(xs.map(x => downloadFile(x))); };')).toEqual(['h']);
  });

  it('forEach through a member call', () => {
    expect(offenders('const h = (xs) => { xs.forEach(x => { void transferFnsRef.current.uploadFile(x); }); };')).toEqual(['h']);
  });

  it('a per-row retry callback created inside a map is a single transfer', () => {
    const src = 'const h = (xs) => { xs.map(x => retry.set(x, async () => { await downloadFile(x); })); };';
    expect(offenders(src)).toEqual([]);
  });

  it('a call outside any loop is a single transfer', () => {
    expect(offenders('const h = async (x) => { await downloadFile(x); };')).toEqual([]);
  });
});

describe('multi-file GUI entry points use the parallel batch (#591)', () => {
  const calls = loopTransferCalls(APP);

  it('finds the looping entry points (the check is not vacuous)', () => {
    const handlers = new Set(calls.map(c => c.handler));
    // download/upload selection, clipboard paste, the transfer planner.
    expect(handlers.size).toBeGreaterThanOrEqual(4);
  });

  it('every handler that transfers in a loop splits its items first', () => {
    const offenders = [...new Map(calls.map(c => [c.handler, c])).values()]
      .filter(c => !containsCallTo(c.handlerNode, 'splitForFileBatch'))
      .map(c => `${c.handler} loops over ${c.callee}() without splitForFileBatch`);
    expect(offenders).toEqual([]);
  });

  it('the dead native-batch gate is gone', () => {
    // `!isProvider && ...` was false for every connected protocol, FTP included.
    const { program } = parseSync('App.tsx', APP);
    const identifiers: string[] = [];
    const walk = (node: Node) => {
      if (node.type === 'Identifier') identifiers.push(node.name as string);
      for (const child of children(node)) walk(child);
    };
    walk(program as unknown as Node);
    expect(identifiers).not.toContain('canUseNativeDownloadBatch');
    expect(identifiers).not.toContain('canUseNativeUploadBatch');
  });
});

describe('runFileBatch keeps one writer per destination', () => {
  it('routes a file whose destination an earlier file of the transfer claimed out of the batch', () => {
    // The destination is where the file lands: the local path of a download,
    // the remote path of an upload. Without the claim, two picked files with
    // one name from different folders shared a batch and one destination.
    const start = APP.indexOf('const runFileBatch = async (');
    const end = APP.indexOf('const launchBatch = async', start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const body = APP.slice(start, end);
    expect(body).toContain('const destination = isDownload ? entry.local_path : entry.remote_path;');
    expect(body).toMatch(/keepsSingleFilePath\(\{[^}]*destinationClaimed: claimedDestinations\.has\(destination\),[^}]*\}\);\s*claimedDestinations\.add\(destination\);/);
  });
});

describe('a cut on a legacy session keeps per-file calls', () => {
  it('clipboardPaste tells the split when the paste deletes its sources', () => {
    const start = APP.indexOf('const clipboardPaste = async (');
    expect(start).toBeGreaterThan(-1);
    const body = APP.slice(start, APP.indexOf('runFileBatch(', start));
    expect(body).toContain("fileBatchSessionFlags(pasteDirection, { cut: operation === 'cut' })");
  });

  it('fileBatchSessionFlags marks a cut on a session that is not a provider session', () => {
    const start = APP.indexOf('const fileBatchSessionFlags = (');
    const body = APP.slice(start, APP.indexOf('};', start));
    expect(body).toMatch(/legacyCut: !!options\.cut && !usesProviderApi\(protocol\)/);
  });
});

describe('#591: Start and Retry re-arm the cancel state, the runners never do', () => {
  const body = (name: string) => {
    const start = APP.indexOf(`const ${name} = async`);
    expect(start, name).toBeGreaterThan(-1);
    return APP.slice(start, APP.indexOf('\n    };\n', start));
  };

  it('registers every file-batch row callback through rearmedOnUserAction', () => {
    expect(APP).toMatch(/for \(const id of batchIds\) retryCallbacksRef\.current\.set\(id, rearmedOnUserAction\(\w+, batchDispatcher\.callbackFor\(id\)\)\)/);
    expect(APP).toMatch(/for \(const id of singleIds\) retryCallbacksRef\.current\.set\(id, rearmedOnUserAction\(\w+, singleDispatcher\.callbackFor\(id\)\)\)/);
  });

  it('keeps the runners from clearing a Stop pressed during the run', () => {
    for (const runner of ['launchBatch', 'runSingles']) {
      expect(body(runner), runner).not.toMatch(/batchCancelledRef\.current = false|cancelLevelRef\.current = 0/);
    }
  });
});
