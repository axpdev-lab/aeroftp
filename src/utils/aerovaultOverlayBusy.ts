// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Frontend wrapper for the AeroVault overlay busy-lock backed by the
// `aerovault_overlay_busy_acquire` / `aerovault_overlay_busy_release`
// Tauri commands. Used by the unified transfer planner (Z.3.6) so an
// overlay↔fs or overlay↔remote batch can outlive the overlay session's
// idle-eviction window without losing its session handle mid-flight.

import { invoke } from '@tauri-apps/api/core';

/**
 * Acquire a busy hold on the overlay session. Returns the new hold
 * count (>= 1). Throws if the session is not registered (e.g. it was
 * already evicted before the acquire call landed).
 */
export async function acquireOverlayBusy(sessionId: string): Promise<number> {
    return invoke<number>('aerovault_overlay_busy_acquire', { sessionId });
}

/**
 * Release a previously-acquired busy hold. Saturating subtraction on
 * the backend means a stray double-release is harmless, but pair every
 * `acquire` with exactly one `release` so the bookkeeping stays clean.
 */
export async function releaseOverlayBusy(sessionId: string): Promise<number> {
    return invoke<number>('aerovault_overlay_busy_release', { sessionId });
}

/**
 * Run a function while holding a busy lock on the overlay session.
 * The lock is released in a `finally` so failures inside `fn` still
 * release the hold. Returns whatever `fn` returns.
 *
 * Use this whenever the unified transfer planner is about to drive a
 * batch transfer (overlay↔local, overlay↔overlay, overlay↔remote): the
 * sweeper will respect the hold and not evict the overlay session out
 * from under the transfer.
 *
 * @example
 *   const report = await withOverlayBusyLock(plan.source.sessionId, async () => {
 *       return invoke('overlay_transfer_batch', { plan });
 *   });
 */
export async function withOverlayBusyLock<T>(
    sessionId: string,
    fn: () => Promise<T>,
): Promise<T> {
    await acquireOverlayBusy(sessionId);
    try {
        return await fn();
    } finally {
        try {
            await releaseOverlayBusy(sessionId);
        } catch {
            // Release failures are non-fatal: the worst case is a stale
            // hold that the backend's saturating subtraction has
            // already capped at zero, or a session that was already
            // evicted. Either way the user has a working result.
        }
    }
}
