// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useLayoutEffect, useRef } from 'react';

/**
 * A function with one identity for the life of the component that always runs
 * the body of the render that committed last.
 *
 * For actions that run later: from a click, an event listener, a timer, an
 * effect or a callback built in an earlier render. Whoever holds it, from
 * whichever render, reads the state of the last commit, and a listener or an
 * effect that depends on it never has to subscribe again. The body is stored
 * in a layout effect, so a call made while rendering would run the previous
 * render's body: call it from handlers and effects only.
 */
export function useLatestCallback<Args extends unknown[], Result>(
    fn: (...args: Args) => Result,
): (...args: Args) => Result {
    const latest = useRef(fn);
    useLayoutEffect(() => {
        latest.current = fn;
    });
    return useCallback((...args: Args) => latest.current(...args), []);
}
