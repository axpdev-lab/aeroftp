// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// One local navigation at a time. A second one started while the first is
// loading is refused (#401), which suits a click on a folder. A file the OS
// opened has already left the backend queue, so it waits for the gate
// instead of being dropped.

/** How long a file the OS opened waits for a listing still loading: a folder
 *  on a dead network mount may never answer, and the drain must not wait on
 *  it for good. */
export const OS_OPEN_NAV_WAIT_MS = 15_000;

export interface NavigationGate {
    /** Takes the gate; false while another navigation holds it. */
    enter(): boolean;
    /** Frees the gate and wakes whoever waits for it. */
    leave(): void;
    /** True once no navigation is in flight; false when `timeoutMs` ran out first. */
    whenFree(timeoutMs: number): Promise<boolean>;
    /** Takes the gate as soon as it is free; false when `timeoutMs` ran out first. */
    enterWhenFree(timeoutMs: number): Promise<boolean>;
}

export function createNavigationGate(): NavigationGate {
    let held: { settled: Promise<void>; release: () => void } | null = null;

    const enter = (): boolean => {
        if (held) return false;
        let release!: () => void;
        held = { settled: new Promise<void>(resolve => { release = resolve; }), release };
        return true;
    };

    const leave = (): void => {
        const finished = held;
        held = null;
        finished?.release();
    };

    const whenFree = async (timeoutMs: number): Promise<boolean> => {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const expired = new Promise<boolean>(resolve => { timer = setTimeout(() => resolve(false), timeoutMs); });
        const free = (async () => {
            // A navigation that takes the gate as the last one leaves it is
            // waited for as well.
            while (held) await held.settled;
            return true;
        })();
        try {
            return await Promise.race([free, expired]);
        } finally {
            clearTimeout(timer);
        }
    };

    const enterWhenFree = async (timeoutMs: number): Promise<boolean> => {
        const deadline = Date.now() + timeoutMs;
        // `enter` runs synchronously right after each wake-up, so nothing can
        // take the gate between "free" and "mine"; a navigation that got in
        // first is waited for again.
        while (!enter()) {
            const left = deadline - Date.now();
            if (left <= 0 || !(await whenFree(left))) return false;
        }
        return true;
    };

    return { enter, leave, whenFree, enterWhenFree };
}
