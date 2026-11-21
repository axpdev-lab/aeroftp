// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useLayoutEffect, useRef, useState } from 'react';

/**
 * A promise that settles once the component has committed every update
 * queued before the call.
 *
 * An `await` on a connect or a tab switch resumes in a microtask, before
 * React renders the state those calls set: a `useLatestCallback` body read
 * then is still the previous render's. The barrier queues one more update and
 * settles in the layout effect of the commit that carries it; React applies
 * updates in order, so that commit carries every earlier update too, and the
 * `useLatestCallback` bodies of that commit are already stored (they are
 * layout effects of an earlier hook call, and the promise resumes after the
 * whole layout phase).
 */
export function useCommitBarrier(): () => Promise<void> {
    const [committed, setCommitted] = useState(0);
    const requested = useRef(0);
    const waiting = useRef<Array<{ at: number; settle: () => void }>>([]);
    useLayoutEffect(() => {
        waiting.current = waiting.current.filter(entry => {
            if (entry.at > committed) return true;
            entry.settle();
            return false;
        });
    }, [committed]);
    return useCallback(() => new Promise<void>(settle => {
        const at = ++requested.current;
        waiting.current.push({ at, settle });
        setCommitted(at);
    }), []);
}
