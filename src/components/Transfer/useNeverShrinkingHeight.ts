// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useLayoutEffect, useRef } from 'react';

/** Long enough for the speed graph's 0.3 s transition (`.tpb-graph-wrapper`). */
const SETTLE_MS = 350;

/**
 * Keep an element at the tallest height it has reached while mounted.
 *
 * The transfer card is anchored at the bottom, so any change of its height moves
 * its top bar. Its content changes with every file of a sync: the rows of the
 * files in flight come and go, and the speed graph's toggle disappears at each
 * file's start and returns with its first progress. With a fast connection and
 * many small files the bar jumped once per file (#347).
 *
 * Runs on every render, at the rate of the progress events, so it reads the
 * height once and writes only when the element has grown: an element held at its
 * tallest height cannot read less, and WebKitGTK shows repeated layout writes as
 * flicker. `release` gives the height back, for a change the user asked for, and
 * waits for its animation to end before reading again: a height read halfway
 * through closing the graph would hold the card open. A new `scope` starts over,
 * for content of another kind that should not inherit the old height.
 *
 * The graph's toggle lives below the card and renders nothing here, so both
 * ends of its animation are measured on a timer (`measureAfterAnimation`): the
 * next file could otherwise shrink the card before its new height was read.
 */
export function useNeverShrinkingHeight<T extends HTMLElement>(scope: unknown) {
    const ref = useRef<T>(null);
    const tallest = useRef(0);
    const settlesAt = useRef(0);
    const currentScope = useRef(scope);
    const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

    const measure = useCallback(() => {
        const element = ref.current;
        if (!element || performance.now() < settlesAt.current) return;
        const height = element.getBoundingClientRect().height;
        if (height > tallest.current) {
            tallest.current = height;
            element.style.minHeight = `${height}px`;
        }
    }, []);

    useLayoutEffect(() => {
        const element = ref.current;
        if (element && currentScope.current !== scope) {
            currentScope.current = scope;
            tallest.current = 0;
            element.style.minHeight = '';
        }
        measure();
    });

    useEffect(() => () => clearTimeout(timer.current), []);

    const measureAfterAnimation = useCallback(() => {
        clearTimeout(timer.current);
        timer.current = setTimeout(measure, SETTLE_MS);
    }, [measure]);

    const release = useCallback(() => {
        tallest.current = 0;
        settlesAt.current = performance.now() + SETTLE_MS;
        if (ref.current) ref.current.style.minHeight = '';
        measureAfterAnimation();
    }, [measureAfterAnimation]);

    return { ref, release, measureAfterAnimation };
}
