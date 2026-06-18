// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Container-width-aware column count for a CSS grid.
 *
 * Tailwind's `md:`/`lg:`/`xl:`/`2xl:` breakpoints react to the *viewport*
 * width, so they ignored the My Servers filter sidebar entirely: opening the
 * sidebar shrank the grid container but the column count stayed put, squashing
 * the cards. This hook instead measures the actual grid container via a
 * `ResizeObserver`, so the column count tracks the space genuinely available
 * (sidebar open/closed, window resize, fullscreen) and the cards keep a stable
 * width.
 *
 * The returned count is clamped to `[min, max]`: a small window still shows at
 * least `min` (slightly wider cards) and a wide display caps at `max` instead
 * of stretching forever.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

interface ResponsiveColumnsOptions {
    /** Lower bound on the column count (cards stretch wider below this). */
    min: number;
    /** Upper bound on the column count (cards stop narrowing past this). */
    max: number;
    /** Ideal per-card width in px used to derive how many columns fit. */
    ideal: number;
    /** Inter-column gap in px (matches the grid `gap-*` utility). */
    gap?: number;
}

type AttachRef = (el: HTMLElement | null) => void;

export function useResponsiveColumns(
    { min, max, ideal, gap = 12 }: ResponsiveColumnsOptions,
): [number, AttachRef] {
    const [cols, setCols] = useState(min);
    const observerRef = useRef<ResizeObserver | null>(null);

    const compute = useCallback((width: number) => {
        if (width <= 0) return;
        // How many ideal-width cards fit, accounting for the inter-column gap:
        // total = n*ideal + (n-1)*gap  =>  n = (width + gap) / (ideal + gap).
        const fit = Math.floor((width + gap) / (ideal + gap));
        const clamped = Math.max(min, Math.min(max, fit));
        setCols(prev => (prev === clamped ? prev : clamped));
    }, [min, max, ideal, gap]);

    // Callback ref so the observer rebinds whenever the measured node mounts or
    // unmounts (the grid container is torn down on the grid<->list view switch).
    const attachRef = useCallback<AttachRef>((el) => {
        observerRef.current?.disconnect();
        observerRef.current = null;
        if (!el) return;
        compute(el.clientWidth);
        const ro = new ResizeObserver((entries) => {
            for (const entry of entries) compute(entry.contentRect.width);
        });
        ro.observe(el);
        observerRef.current = ro;
    }, [compute]);

    useEffect(() => () => observerRef.current?.disconnect(), []);

    return [cols, attachRef];
}
