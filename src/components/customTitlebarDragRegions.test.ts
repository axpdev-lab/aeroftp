// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Guard for #511. The titlebar drag fillers are invisible by design, so a
// broken one looks exactly like a working one: it still reserves its width and
// the bar still renders correctly. The first fix for #511 shipped that way,
// with all three fillers 0px tall, because `h-full` is `height: 100%` and a
// percentage cannot resolve against a parent whose own height is `auto`.
// Only the titlebar has a definite height (`h-9`), so a filler is safe when it
// is a direct child of the bar, and otherwise needs `self-stretch` plus an
// unbroken `h-full` chain on the containers above it.
//
// These assertions read the source rather than the rendered DOM on purpose:
// vitest runs in `node` here, and jsdom has no layout engine, so a rendered
// test would report height 0 for a correct filler and pass for a broken one.

// The source arrives through Vite's `?raw` rather than `node:fs`: the app
// tsconfig has no node types, so a filesystem read would fail `tsc --noEmit`.
import { describe, expect, it } from 'vitest';
import SOURCE from './CustomTitlebar.tsx?raw';

/** className of every element that carries `data-tauri-drag-region`. */
const dragRegionClasses = (src: string): string[] =>
  [...src.matchAll(/data-tauri-drag-region[^>]*?className="([^"]*)"/gs)].map((m) => m[1]);

describe('titlebar drag fillers (#511)', () => {
  it('finds every drag region declared in the titlebar', () => {
    // Left logo cluster, center spacer, reserved-slot filler, two inter-cluster spacers.
    expect(dragRegionClasses(SOURCE)).toHaveLength(5);
  });

  it('gives the right-hand fillers a hit area instead of collapsing them', () => {
    const rightHandFillers = dragRegionClasses(SOURCE).filter((cls) => cls.includes('self-stretch'));
    // The reserved 96px slot filler plus the two 12px inter-cluster spacers.
    expect(rightHandFillers).toHaveLength(3);
    // Counted by role, not in aggregate: three fillers that all stretch would
    // also satisfy the total while the slot filler had lost its `flex-1` and
    // stopped claiming the empty width, or a spacer had lost its `w-3` and
    // become a zero-width region that is just as ungrabbable as a zero-height
    // one.
    expect(rightHandFillers.filter((cls) => cls.includes('flex-1'))).toHaveLength(1);
    expect(
      rightHandFillers.filter((cls) => cls.includes('w-3') && cls.includes('shrink-0')),
    ).toHaveLength(2);
    for (const cls of rightHandFillers) {
      // `h-full` next to `self-stretch` wins and puts the height back to 0.
      expect(cls).not.toContain('h-full');
    }
  });

  it('keeps the height chain unbroken from the bar down to the fillers', () => {
    // The bar is the only definite height in the chain.
    expect(SOURCE).toContain('aero-titlebar flex items-center h-9');
    // The 3-cluster container, parent of both inter-cluster spacers.
    expect(SOURCE).toContain('<div className="flex items-center h-full">');
    // The reserved page-nav slot, parent of the filler that sits next to AeroVault.
    expect(SOURCE).toContain('min-w-[96px] gap-1 h-full');
  });
});
