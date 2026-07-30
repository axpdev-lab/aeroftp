// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { reorderInheritingIndex, reorderVisibleInFull } from './reorderByIndex';

const ids = (list: { id: string }[]) => list.map((s) => s.id);
const row = (id: string) => ({ id });

describe('reorderInheritingIndex', () => {
  const list = ['a', 'b', 'c', 'd', 'e'];

  it('dragging down inherits the drop target index (no classic to-1)', () => {
    // Pin #453: move b(1) onto e(4) -> b must land at index 4, not 3.
    expect(reorderInheritingIndex(list, 1, 4)).toEqual(['a', 'c', 'd', 'e', 'b']);
  });

  it('dragging up inherits the drop target index', () => {
    // Move e(4) onto b(1) -> e at index 1.
    expect(reorderInheritingIndex(list, 4, 1)).toEqual(['a', 'e', 'b', 'c', 'd']);
  });

  it('adjacent swap both directions', () => {
    expect(reorderInheritingIndex(list, 2, 3)).toEqual(['a', 'b', 'd', 'c', 'e']);
    expect(reorderInheritingIndex(list, 3, 2)).toEqual(['a', 'b', 'd', 'c', 'e']);
  });

  it('append sentinel (to === length) puts the item last', () => {
    expect(reorderInheritingIndex(list, 0, list.length)).toEqual(['b', 'c', 'd', 'e', 'a']);
  });

  it('no-op when from === to or out of range', () => {
    expect(reorderInheritingIndex(list, 2, 2)).toEqual(list);
    expect(reorderInheritingIndex(list, -1, 2)).toEqual(list);
    expect(reorderInheritingIndex(list, 9, 2)).toEqual(list);
  });
});

describe('reorderVisibleInFull', () => {
  // Full vault order. SFTP filter shows b, d, e (indices 1, 3, 4).
  const full = [row('a'), row('b'), row('c'), row('d'), row('e')];
  const visible = [row('b'), row('d'), row('e')];

  it('reorders only the visible subset and keeps others in place', () => {
    // Move e (vis 2) onto b (vis 0) -> visible becomes e, b, d.
    // Full slots for visible stay 1,3,4: a, e, c, b, d.
    const next = reorderVisibleInFull(full, visible, 2, 0);
    expect(ids(next)).toEqual(['a', 'e', 'c', 'b', 'd']);
  });

  it('dragging down within the filter inherits the visible target slot', () => {
    // Move b (0) onto e (2) among visible -> b, d, e becomes d, e, b.
    const next = reorderVisibleInFull(full, visible, 0, 2);
    expect(ids(next)).toEqual(['a', 'd', 'c', 'e', 'b']);
  });

  it('full list (no filter) matches reorderInheritingIndex', () => {
    const all = full;
    const next = reorderVisibleInFull(all, all, 1, 4);
    expect(ids(next)).toEqual(ids(reorderInheritingIndex(all, 1, 4)));
  });

  it('no-op when the visible order would not change', () => {
    const next = reorderVisibleInFull(full, visible, 1, 1);
    expect(next).not.toBe(full);
    expect(ids(next)).toEqual(ids(full));
  });
});
