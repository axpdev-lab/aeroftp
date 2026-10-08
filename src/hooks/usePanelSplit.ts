// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * usePanelSplit: shared resize logic for the app's two side-by-side panel
 * layouts (connected Local/Remote and AeroFile dual-local).
 *
 * The hook owns a persisted *desired* left-panel percentage and derives a
 * *rendered* percentage clamped against the live container width, so a saved
 * preference survives temporary viewport/sidebar squeezes: narrowing clamps
 * the rendering, widening restores the preference.
 *
 * The pure helpers below carry all boundary math and storage parsing so the
 * behavior is testable in the node environment without a DOM.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

/** Global percent bounds, matching the historical 0.1..0.9 drag clamp. */
export const SPLIT_MIN_PERCENT = 10;
export const SPLIT_MAX_PERCENT = 90;
export const SPLIT_DEFAULT_PERCENT = 50;
/** Keyboard step for ArrowLeft/ArrowRight, in percent (was 0.1 flex = 5%). */
export const SPLIT_KEYBOARD_STEP_PERCENT = 5;
/** Candidate minimum usable width per panel; verified against the path bars. */
export const SPLIT_MIN_PANEL_PX = 160;
/** Rendered separator width (Tailwind w-1). */
export const SPLIT_SEPARATOR_PX = 4;

/** Legacy AeroFile dual-local storage is a left flex-grow in 0.2..1.8. */
export const DUAL_LOCAL_MIN_FLEX = 0.2;
export const DUAL_LOCAL_MAX_FLEX = 1.8;

export interface SplitBounds {
    minPercent: number;
    maxPercent: number;
}

const GLOBAL_BOUNDS: SplitBounds = { minPercent: SPLIT_MIN_PERCENT, maxPercent: SPLIT_MAX_PERCENT };
const EQUAL_FALLBACK: SplitBounds = { minPercent: SPLIT_DEFAULT_PERCENT, maxPercent: SPLIT_DEFAULT_PERCENT };

/**
 * Bounds for the left panel percentage given the live container width.
 * The separator is excluded from the usable width; each panel must keep at
 * least minPanelPx, within the global 10..90 range. A container too narrow
 * for both minima gets a defined equal fallback (50/50) so widths never go
 * negative and the saved preference is left untouched.
 */
export function computeSplitBounds(containerWidthPx: number, separatorWidthPx: number, minPanelPx: number): SplitBounds {
    const usable = containerWidthPx - separatorWidthPx;
    if (!Number.isFinite(usable) || usable <= 0) return EQUAL_FALLBACK;
    const minPxPercent = (minPanelPx / usable) * 100;
    const minPercent = Math.max(SPLIT_MIN_PERCENT, minPxPercent);
    const maxPercent = Math.min(SPLIT_MAX_PERCENT, 100 - minPxPercent);
    if (minPercent > maxPercent) return EQUAL_FALLBACK;
    return { minPercent, maxPercent };
}

export function clampSplitPercent(percent: number, bounds: SplitBounds): number {
    return Math.min(bounds.maxPercent, Math.max(bounds.minPercent, percent));
}

/**
 * Default storage parser: a plain percent string. Missing, non-finite or
 * out-of-range values yield null so the caller falls back to the default.
 */
export function parseStoredSplitPercent(raw: string | null): number | null {
    if (raw == null) return null;
    const n = Number.parseFloat(raw);
    if (!Number.isFinite(n) || n < SPLIT_MIN_PERCENT || n > SPLIT_MAX_PERCENT) return null;
    return n;
}

/** Legacy dual-local adapters: left flex 0.2..1.8 maps to 10..90 percent. */
export function dualLocalFlexToPercent(raw: string | null): number | null {
    if (raw == null) return null;
    const n = Number.parseFloat(raw);
    if (!Number.isFinite(n) || n < DUAL_LOCAL_MIN_FLEX || n > DUAL_LOCAL_MAX_FLEX) return null;
    return n * 50;
}

export function percentToDualLocalFlex(percent: number): string {
    return String(percent / 50);
}

/**
 * Keyboard model for the separator. Returns the next desired percent, or
 * null when the key is not handled. Arrow keys step from the *rendered*
 * percent so the first keypress always moves the divider visibly even when
 * the saved preference is currently clamped by a narrow container; Home/End
 * jump to the live bounds; Enter/Space reset to 50/50.
 */
export function applySplitKey(renderedPercent: number, key: string, bounds: SplitBounds, step: number = SPLIT_KEYBOARD_STEP_PERCENT): number | null {
    switch (key) {
        case 'ArrowLeft':
            return clampSplitPercent(renderedPercent - step, bounds);
        case 'ArrowRight':
            return clampSplitPercent(renderedPercent + step, bounds);
        case 'Home':
            return bounds.minPercent;
        case 'End':
            return bounds.maxPercent;
        case 'Enter':
        case ' ':
            return clampSplitPercent(SPLIT_DEFAULT_PERCENT, bounds);
        default:
            return null;
    }
}

/**
 * Pointer position to left-panel percent, clamped to the live bounds.
 * Returns null when the container has no measurable width.
 */
export function dragPercentFromPointer(clientX: number, containerLeft: number, containerWidth: number, bounds: SplitBounds): number | null {
    if (!Number.isFinite(containerWidth) || containerWidth <= 0) return null;
    return clampSplitPercent(((clientX - containerLeft) / containerWidth) * 100, bounds);
}

export interface UsePanelSplitOptions {
    /** Outer flex container holding both panels and the separator. */
    containerRef: React.RefObject<HTMLElement | null>;
    /** localStorage key for the persisted desired ratio. */
    storageKey: string;
    defaultPercent?: number;
    minPanelPx?: number;
    separatorWidthPx?: number;
    /** Custom storage reader (returns percent or null for the default). */
    fromStorage?: (raw: string | null) => number | null;
    /** Custom storage writer. */
    toStorage?: (percent: number) => string;
}

export interface PanelSplitDividerHandlers {
    onMouseDown: (e: React.MouseEvent) => void;
    onKeyDown: (e: React.KeyboardEvent) => void;
    onDoubleClick: () => void;
    ariaValueMin: number;
    ariaValueMax: number;
    ariaValueNow: number;
}

export interface UsePanelSplitResult {
    /** Rendered left-panel percent, clamped to the live bounds. */
    leftPercent: number;
    rightPercent: number;
    /** Persisted preference, independent of temporary clamping. */
    desiredPercent: number;
    dragging: boolean;
    reset: () => void;
    dividerHandlers: PanelSplitDividerHandlers;
}

export function usePanelSplit(options: UsePanelSplitOptions): UsePanelSplitResult {
    const {
        containerRef,
        storageKey,
        defaultPercent = SPLIT_DEFAULT_PERCENT,
        minPanelPx = SPLIT_MIN_PANEL_PX,
        separatorWidthPx = SPLIT_SEPARATOR_PX,
        fromStorage,
        toStorage,
    } = options;

    const [desiredPercent, setDesiredPercent] = useState<number>(() => {
        let raw: string | null = null;
        try {
            raw = localStorage.getItem(storageKey);
        } catch {
            raw = null;
        }
        const parsed = fromStorage ? fromStorage(raw) : parseStoredSplitPercent(raw);
        return parsed != null && Number.isFinite(parsed) ? clampSplitPercent(parsed, GLOBAL_BOUNDS) : defaultPercent;
    });

    useEffect(() => {
        try {
            localStorage.setItem(storageKey, toStorage ? toStorage(desiredPercent) : String(desiredPercent));
        } catch {
            // Storage may be unavailable (private mode); the split still works in-session.
        }
    }, [storageKey, desiredPercent, toStorage]);

    // Live container width via ResizeObserver (window resize fallback), so the
    // rendered ratio re-clamps when the window or sidebar changes size.
    const [containerWidth, setContainerWidth] = useState(0);
    useEffect(() => {
        const el = containerRef.current;
        if (!el) return;
        const update = () => {
            const w = el.getBoundingClientRect().width;
            if (Number.isFinite(w) && w > 0) setContainerWidth(w);
        };
        update();
        if (typeof ResizeObserver === 'undefined') {
            window.addEventListener('resize', update);
            return () => window.removeEventListener('resize', update);
        }
        const observer = new ResizeObserver(update);
        observer.observe(el);
        return () => observer.disconnect();
    }, [containerRef]);

    const bounds = computeSplitBounds(containerWidth, separatorWidthPx, minPanelPx);
    const leftPercent = clampSplitPercent(desiredPercent, bounds);

    const [dragging, setDragging] = useState(false);
    const dragCleanupRef = useRef<(() => void) | null>(null);

    const onMouseDown = useCallback((e: React.MouseEvent) => {
        e.preventDefault();
        const container = containerRef.current;
        if (!container) return;
        // End any earlier drag before starting a new one (defensive: mouseup
        // normally cleans up, but a lost pointer must not leak listeners).
        dragCleanupRef.current?.();
        const previousUserSelect = document.body.style.userSelect;
        document.body.style.userSelect = 'none';
        setDragging(true);
        const onMove = (ev: MouseEvent) => {
            const rect = container.getBoundingClientRect();
            const liveBounds = computeSplitBounds(rect.width, separatorWidthPx, minPanelPx);
            const next = dragPercentFromPointer(ev.clientX, rect.left, rect.width, liveBounds);
            if (next != null) setDesiredPercent(next);
        };
        const cleanup = () => {
            window.removeEventListener('mousemove', onMove);
            window.removeEventListener('mouseup', onUp);
            window.removeEventListener('blur', onUp);
            document.body.style.userSelect = previousUserSelect;
            setDragging(false);
            dragCleanupRef.current = null;
        };
        const onUp = () => cleanup();
        dragCleanupRef.current = cleanup;
        window.addEventListener('mousemove', onMove);
        window.addEventListener('mouseup', onUp);
        window.addEventListener('blur', onUp);
    }, [containerRef, minPanelPx, separatorWidthPx]);

    // No leaked listeners or stale userSelect if the panel unmounts mid-drag.
    useEffect(() => () => {
        dragCleanupRef.current?.();
    }, []);

    const onKeyDown = useCallback((e: React.KeyboardEvent) => {
        const next = applySplitKey(leftPercent, e.key, bounds);
        if (next == null) return;
        e.preventDefault();
        setDesiredPercent(next);
    }, [leftPercent, bounds.minPercent, bounds.maxPercent]); // eslint-disable-line react-hooks/exhaustive-deps

    const reset = useCallback(() => {
        setDesiredPercent(clampSplitPercent(SPLIT_DEFAULT_PERCENT, GLOBAL_BOUNDS));
    }, []);

    const ariaValueMin = Math.round(bounds.minPercent);
    const ariaValueMax = Math.round(bounds.maxPercent);
    const ariaValueNow = Math.min(ariaValueMax, Math.max(ariaValueMin, Math.round(leftPercent)));

    return {
        leftPercent,
        rightPercent: 100 - leftPercent,
        desiredPercent,
        dragging,
        reset,
        dividerHandlers: {
            onMouseDown,
            onKeyDown,
            onDoubleClick: reset,
            ariaValueMin,
            ariaValueMax,
            ariaValueNow,
        },
    };
}
