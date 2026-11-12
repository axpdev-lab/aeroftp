// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Native cancellation acknowledgement is independent of the JS runner's exit.
 * Publish that exit separately so another executor cannot rearm shared flags
 * until the stopped runner has finished observing them. */
export async function runTrackedSync<T>(
    pendingRef: { current: Promise<void> | null },
    run: () => Promise<T>,
): Promise<T> {
    let settle!: () => void;
    const pending = new Promise<void>(resolve => { settle = resolve; });
    pendingRef.current = pending;
    try {
        return await run();
    } finally {
        if (pendingRef.current === pending) pendingRef.current = null;
        settle();
    }
}

/** Optional panel refreshes must not retain a finished transfer's running latch.
 * Every asynchronous result uses the supplied signal before applying state. */
export async function runCompletionRefresh(
    refresh: (signal: AbortSignal) => Promise<void>,
    controller: AbortController,
    timeoutMs = 2000,
): Promise<void> {
    const { signal } = controller;
    if (signal.aborted) return;
    let stopped!: () => void;
    const cancellation = new Promise<void>(resolve => { stopped = resolve; });
    signal.addEventListener('abort', stopped, { once: true });
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
        await Promise.race([refresh(signal), cancellation]);
    } catch (error) {
        if (!signal.aborted) throw error;
    } finally {
        clearTimeout(timer);
        signal.removeEventListener('abort', stopped);
    }
}
