// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * A refresh that waits a moment and then runs only if nothing else refreshed
 * in the meantime.
 *
 * A finished upload refreshes the remote panel, and so does every upload loop
 * once its last file is done: a one-file upload listed the folder twice, which
 * on a provider driven through a CLI (Proton Drive) is two runs of about two
 * seconds each. The event's refresh now waits `delayMs` and stands down when
 * another listing started after the event arrived.
 */
export function createDeferredRefresh(
    run: () => void,
    lastRefreshStartedAt: () => number,
    delayMs = 800,
): { schedule: () => void; cancel: () => void } {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
        if (timer !== undefined) clearTimeout(timer);
        timer = undefined;
    };
    const schedule = () => {
        const requestedAt = Date.now();
        cancel();
        timer = setTimeout(() => {
            timer = undefined;
            if (lastRefreshStartedAt() >= requestedAt) return;
            run();
        }, delayMs);
    };
    return { schedule, cancel };
}
