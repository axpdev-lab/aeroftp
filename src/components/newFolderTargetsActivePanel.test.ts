// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { parseSync } from 'rolldown/experimental';
import APP from '../App.tsx?raw';

/**
 * `createFolder(isRemote)` creates the folder on the server when its argument
 * is true. The command palette's New Folder passed `activePanel !== 'remote'`,
 * the opposite of the toolbar and the keyboard shortcut: with the remote panel
 * active it made a local folder, and with a local panel active it tried the
 * server. Every entry point that picks the side from the active panel passes
 * `activePanel === 'remote'`.
 *
 * The calls are found on the syntax tree, so a comment or a string that
 * mentions `createFolder(` cannot satisfy or fail the check.
 */
type Node = { type: string; start: number; end: number; [key: string]: unknown };

function createFolderCalls(source: string): Array<{ arg: string; line: number }> {
  const { program, errors } = parseSync('App.tsx', source);
  expect(errors, 'App.tsx must parse cleanly').toEqual([]);
  const found: Array<{ arg: string; line: number }> = [];
  const visit = (node: Node) => {
    if (node.type === 'CallExpression') {
      const callee = node.callee as Node;
      const [first] = node.arguments as Node[];
      if (callee.type === 'Identifier' && callee.name === 'createFolder' && first) {
        found.push({
          arg: source.slice(first.start, first.end),
          line: source.slice(0, node.start).split('\n').length,
        });
      }
    }
    for (const value of Object.values(node)) {
      const items = Array.isArray(value) ? value : [value];
      for (const item of items) if (item && typeof item === 'object' && 'type' in item) visit(item as Node);
    }
  };
  visit(program as unknown as Node);
  return found;
}

describe('New Folder creates the folder on the active panel', () => {
  it("every entry point that reads the active panel passes activePanel === 'remote'", () => {
    const fromActivePanel = createFolderCalls(APP).filter(call => call.arg.includes('activePanel'));
    // The keyboard shortcut (two keys), the toolbar and the command palette.
    expect(fromActivePanel.length).toBeGreaterThanOrEqual(4);
    for (const call of fromActivePanel) {
      expect(call.arg, `createFolder at App.tsx:${call.line}`).toBe("activePanel === 'remote'");
    }
  });
});
