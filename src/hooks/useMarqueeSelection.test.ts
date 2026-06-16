// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
  rectsIntersect,
  selectItemsInMarquee,
  type MarqueeItem,
  type MarqueeRectEdges,
} from './useMarqueeSelection';

const r = (left: number, top: number, right: number, bottom: number): MarqueeRectEdges => ({
  left,
  top,
  right,
  bottom,
});

describe('rectsIntersect', () => {
  it('detects overlap', () => {
    expect(rectsIntersect(r(0, 0, 10, 10), r(5, 5, 15, 15))).toBe(true);
  });

  it('treats touching edges as non-overlapping', () => {
    expect(rectsIntersect(r(0, 0, 10, 10), r(10, 0, 20, 10))).toBe(false);
  });

  it('returns false for fully separated rects', () => {
    expect(rectsIntersect(r(0, 0, 10, 10), r(100, 100, 110, 110))).toBe(false);
  });

  it('handles containment', () => {
    expect(rectsIntersect(r(0, 0, 100, 100), r(40, 40, 60, 60))).toBe(true);
  });
});

const item = (name: string, index: number, rect: MarqueeRectEdges): MarqueeItem => ({
  name,
  index,
  rect,
});

describe('selectItemsInMarquee', () => {
  const items: MarqueeItem[] = [
    item('a.txt', 0, r(0, 0, 200, 20)),
    item('b.txt', 1, r(0, 20, 200, 40)),
    item('c.txt', 2, r(0, 40, 200, 60)),
    item('d.txt', 3, r(0, 60, 200, 80)),
  ];

  it('selects only the intersected rows', () => {
    const { names, lastIndex } = selectItemsInMarquee(r(10, 25, 50, 55), items);
    expect([...names].sort()).toEqual(['b.txt', 'c.txt']);
    expect(lastIndex).toBe(2);
  });

  it('returns an empty selection when the marquee misses everything', () => {
    const { names, lastIndex } = selectItemsInMarquee(r(300, 300, 320, 320), items);
    expect(names.size).toBe(0);
    expect(lastIndex).toBeNull();
  });

  it('unions onto the base set for an additive marquee', () => {
    const { names } = selectItemsInMarquee(r(10, 65, 50, 75), items, ['a.txt']);
    expect([...names].sort()).toEqual(['a.txt', 'd.txt']);
  });

  it('never selects the parent ".." entry', () => {
    const withParent = [item('..', -1, r(0, 0, 200, 20)), ...items];
    const { names } = selectItemsInMarquee(r(0, 0, 200, 80), withParent);
    expect(names.has('..')).toBe(false);
    expect(names.size).toBe(4);
  });

  it('reports the last intersected index as the anchor', () => {
    const { lastIndex } = selectItemsInMarquee(r(0, 0, 200, 100), items);
    expect(lastIndex).toBe(3);
  });
});
