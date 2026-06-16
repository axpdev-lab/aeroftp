// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useMarqueeSelection: rubber-band (marquee) multi-selection for a scrollable
 * file list, plus the "click blank space to deselect" affordance.
 *
 * Requested by Ehud in #270 (AeroFile cluster):
 *   - 17310733: clicking the empty area clears the current selection.
 *   - 17310748: dragging a rubber-band rectangle selects the intersected files.
 *
 * The hook is view-agnostic: it queries the container for elements matching
 * `itemSelector` (each must carry `data-file-name`, optionally `data-file-index`)
 * and computes intersection geometry in the container's content space, so it
 * works for the list (`tr[data-file-row]`), grid and large-icon (`[data-file-card]`)
 * layouts without per-view code.
 *
 * The selection state itself lives in App.tsx (Set<string> of file names); the
 * hook only drives it through `setSelected` / `setLastIndex`, mirroring the
 * existing click selection model.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

/** A simple axis-aligned rectangle (content-space pixels). */
export interface MarqueeRectEdges {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** One selectable item, resolved to content-space coordinates. */
export interface MarqueeItem {
  name: string;
  index: number;
  rect: MarqueeRectEdges;
}

/** Box passed to the renderer (left/top/width/height, content-space). */
export interface MarqueeBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Standard AABB overlap test (touching edges do not count as overlap). */
export function rectsIntersect(a: MarqueeRectEdges, b: MarqueeRectEdges): boolean {
  return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
}

/**
 * Pure selection core: given a marquee rectangle and the candidate item rects,
 * return the set of selected names plus the index of the last item that fell
 * inside (used as the keyboard-selection anchor). When `base` is provided the
 * hits are unioned onto it (additive marquee with a modifier key held).
 *
 * Kept free of the DOM so the geometry is unit-testable.
 */
export function selectItemsInMarquee(
  marquee: MarqueeRectEdges,
  items: MarqueeItem[],
  base?: Iterable<string>,
): { names: Set<string>; lastIndex: number | null } {
  const names = new Set<string>(base ?? []);
  let lastIndex: number | null = null;
  for (const item of items) {
    if (item.name === '..') continue;
    if (rectsIntersect(marquee, item.rect)) {
      names.add(item.name);
      lastIndex = item.index;
    }
  }
  return { names, lastIndex };
}

interface DragState {
  /** Anchor point, content-space. */
  startX: number;
  startY: number;
  /** Anchor point, viewport-space (for the move threshold). */
  startClientX: number;
  startClientY: number;
  /** Latest pointer position, viewport-space (for auto-scroll + recompute). */
  pointerClientX: number;
  pointerClientY: number;
  /** Modifier held at drag start: union onto the prior selection. */
  additive: boolean;
  /** Selection snapshot at drag start (only consumed when `additive`). */
  base: Set<string>;
  /** True once the pointer has travelled past the activation threshold. */
  moved: boolean;
}

const DRAG_THRESHOLD = 5; // px the pointer must travel before a marquee begins
const AUTOSCROLL_EDGE = 32; // px from the top/bottom edge that starts auto-scroll
const AUTOSCROLL_MAX_SPEED = 20; // px per frame at the very edge

const INTERACTIVE_SELECTOR =
  'button, input, textarea, select, a, [role="button"], [role="menuitem"], [contenteditable="true"]';
const ITEM_GUARD_SELECTOR = '[data-file-row], [data-file-card], [data-marquee-skip]';

export interface UseMarqueeSelectionOptions {
  /** The scrollable element wrapping the file rows/cards (must be position:relative). */
  containerRef: React.RefObject<HTMLElement | null>;
  /** CSS selector for each selectable element (must carry data-file-name). */
  itemSelector: string;
  /** Current selection, used to seed an additive (modifier-held) marquee. */
  selected: Set<string>;
  /** Replace/extend the selection. */
  setSelected: React.Dispatch<React.SetStateAction<Set<string>>>;
  /** Track the anchor index after the marquee settles (optional). */
  setLastIndex?: (i: number | null) => void;
  /** Fired once when an actual marquee begins (after the move threshold). */
  onMarqueeStart?: () => void;
  /** Fired on a plain click in blank space (the deselect affordance). */
  onBlankClick?: () => void;
  /** Disable the whole behaviour (e.g. trash view or an active inline rename). */
  disabled?: boolean;
}

export interface UseMarqueeSelectionResult {
  /** Attach to the scroll container's onMouseDown. */
  onMouseDown: (e: React.MouseEvent) => void;
  /** The marquee rectangle to render, or null when idle. */
  box: MarqueeBox | null;
  /** True while a marquee drag is in progress. */
  active: boolean;
}

export function useMarqueeSelection(
  opts: UseMarqueeSelectionOptions,
): UseMarqueeSelectionResult {
  const {
    containerRef,
    itemSelector,
    setSelected,
    setLastIndex,
    onMarqueeStart,
    onBlankClick,
    disabled,
  } = opts;

  const dragRef = useRef<DragState | null>(null);
  const rafRef = useRef<number | null>(null);
  const autoScrollRafRef = useRef<number | null>(null);
  const [box, setBox] = useState<MarqueeBox | null>(null);
  const [active, setActive] = useState(false);

  // Keep the latest selection in a ref so the mousedown snapshot is current
  // without making onMouseDown depend on (and re-create with) every change.
  const selectedRef = useRef(opts.selected);
  selectedRef.current = opts.selected;

  // Stable trampolines so add/removeEventListener always match, even if the
  // concrete handlers are re-created across renders mid-drag.
  const moveHandlerRef = useRef<((e: MouseEvent) => void) | null>(null);
  const upHandlerRef = useRef<(() => void) | null>(null);
  const onWindowMove = useRef((e: MouseEvent) => moveHandlerRef.current?.(e)).current;
  const onWindowUp = useRef(() => upHandlerRef.current?.()).current;

  const recompute = useCallback(() => {
    const container = containerRef.current;
    const drag = dragRef.current;
    if (!container || !drag) return;

    const contRect = container.getBoundingClientRect();
    const { scrollLeft, scrollTop } = container;
    // Derive the current pointer position in content-space from the last
    // viewport coords, so the rectangle keeps tracking while auto-scrolling.
    const curX = drag.pointerClientX - contRect.left + scrollLeft;
    const curY = drag.pointerClientY - contRect.top + scrollTop;

    // Clamp the rectangle to the content box so the absolutely-positioned
    // overlay never extends the container's own scrollable overflow. Items
    // always live inside these bounds, so clamping does not change the hits.
    const maxX = container.scrollWidth;
    const maxY = container.scrollHeight;
    const clamp = (v: number, hi: number) => Math.max(0, Math.min(v, hi));
    const left = clamp(Math.min(drag.startX, curX), maxX);
    const top = clamp(Math.min(drag.startY, curY), maxY);
    const right = clamp(Math.max(drag.startX, curX), maxX);
    const bottom = clamp(Math.max(drag.startY, curY), maxY);
    setBox({ left, top, width: right - left, height: bottom - top });

    const marquee: MarqueeRectEdges = { left, top, right, bottom };
    const items: MarqueeItem[] = [];
    const els = container.querySelectorAll<HTMLElement>(itemSelector);
    els.forEach((el) => {
      const name = el.getAttribute('data-file-name');
      if (!name) return;
      const r = el.getBoundingClientRect();
      const elLeft = r.left - contRect.left + scrollLeft;
      const elTop = r.top - contRect.top + scrollTop;
      const idxAttr = el.getAttribute('data-file-index');
      items.push({
        name,
        index: idxAttr !== null ? parseInt(idxAttr, 10) : -1,
        rect: { left: elLeft, top: elTop, right: elLeft + r.width, bottom: elTop + r.height },
      });
    });

    const { names, lastIndex } = selectItemsInMarquee(
      marquee,
      items,
      drag.additive ? drag.base : undefined,
    );
    setSelected(names);
    if (setLastIndex) setLastIndex(lastIndex);
  }, [containerRef, itemSelector, setSelected, setLastIndex]);

  const stopAutoScroll = useCallback(() => {
    if (autoScrollRafRef.current !== null) {
      cancelAnimationFrame(autoScrollRafRef.current);
      autoScrollRafRef.current = null;
    }
  }, []);

  const autoScrollTick = useCallback(() => {
    const container = containerRef.current;
    const drag = dragRef.current;
    if (!container || !drag) {
      autoScrollRafRef.current = null;
      return;
    }
    const contRect = container.getBoundingClientRect();
    const y = drag.pointerClientY;
    let delta = 0;
    if (y < contRect.top + AUTOSCROLL_EDGE) {
      const intensity = Math.min(1, (contRect.top + AUTOSCROLL_EDGE - y) / AUTOSCROLL_EDGE);
      delta = -Math.ceil(intensity * AUTOSCROLL_MAX_SPEED);
    } else if (y > contRect.bottom - AUTOSCROLL_EDGE) {
      const intensity = Math.min(1, (y - (contRect.bottom - AUTOSCROLL_EDGE)) / AUTOSCROLL_EDGE);
      delta = Math.ceil(intensity * AUTOSCROLL_MAX_SPEED);
    }
    if (delta !== 0) {
      const before = container.scrollTop;
      container.scrollTop = before + delta;
      if (container.scrollTop !== before) recompute();
      autoScrollRafRef.current = requestAnimationFrame(autoScrollTick);
    } else {
      autoScrollRafRef.current = null;
    }
  }, [containerRef, recompute]);

  const maybeAutoScroll = useCallback(() => {
    const container = containerRef.current;
    const drag = dragRef.current;
    if (!container || !drag || autoScrollRafRef.current !== null) return;
    const contRect = container.getBoundingClientRect();
    const y = drag.pointerClientY;
    if (y < contRect.top + AUTOSCROLL_EDGE || y > contRect.bottom - AUTOSCROLL_EDGE) {
      autoScrollRafRef.current = requestAnimationFrame(autoScrollTick);
    }
  }, [containerRef, autoScrollTick]);

  const handleMove = useCallback(
    (e: MouseEvent) => {
      const drag = dragRef.current;
      if (!drag) return;
      drag.pointerClientX = e.clientX;
      drag.pointerClientY = e.clientY;
      if (!drag.moved) {
        const dx = e.clientX - drag.startClientX;
        const dy = e.clientY - drag.startClientY;
        if (dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD) return;
        drag.moved = true;
        setActive(true);
        onMarqueeStart?.();
      }
      // Suppress native text selection while rubber-banding.
      e.preventDefault();
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(() => {
          rafRef.current = null;
          recompute();
        });
      }
      maybeAutoScroll();
    },
    [recompute, maybeAutoScroll, onMarqueeStart],
  );

  const handleUp = useCallback(() => {
    const drag = dragRef.current;
    window.removeEventListener('mousemove', onWindowMove);
    window.removeEventListener('mouseup', onWindowUp);
    stopAutoScroll();
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    setBox(null);
    setActive(false);
    dragRef.current = null;
    // A press-release with no drag in blank space is a deselect gesture.
    if (drag && !drag.moved) onBlankClick?.();
  }, [onWindowMove, onWindowUp, stopAutoScroll, onBlankClick]);

  // Refresh the trampoline targets every render.
  moveHandlerRef.current = handleMove;
  upHandlerRef.current = handleUp;

  const onMouseDown = useCallback(
    (e: React.MouseEvent) => {
      if (disabled) return;
      if (e.button !== 0) return; // left button only
      const container = containerRef.current;
      if (!container) return;
      const target = e.target as HTMLElement | null;
      if (!target) return;
      // Ignore presses that land on a control or on a file row/card: those are
      // handled by their own click/drag handlers. The marquee owns blank space.
      if (target.closest(INTERACTIVE_SELECTOR) || target.closest(ITEM_GUARD_SELECTOR)) {
        return;
      }
      const contRect = container.getBoundingClientRect();
      const startX = e.clientX - contRect.left + container.scrollLeft;
      const startY = e.clientY - contRect.top + container.scrollTop;
      dragRef.current = {
        startX,
        startY,
        startClientX: e.clientX,
        startClientY: e.clientY,
        pointerClientX: e.clientX,
        pointerClientY: e.clientY,
        additive: e.ctrlKey || e.metaKey || e.shiftKey,
        base: new Set(selectedRef.current),
        moved: false,
      };
      window.addEventListener('mousemove', onWindowMove);
      window.addEventListener('mouseup', onWindowUp);
    },
    [disabled, containerRef, onWindowMove, onWindowUp],
  );

  // Tidy up on unmount (a drag could be interrupted by a route/view change).
  useEffect(() => {
    return () => {
      window.removeEventListener('mousemove', onWindowMove);
      window.removeEventListener('mouseup', onWindowUp);
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      if (autoScrollRafRef.current !== null) cancelAnimationFrame(autoScrollRafRef.current);
    };
  }, [onWindowMove, onWindowUp]);

  return { onMouseDown, box, active };
}
