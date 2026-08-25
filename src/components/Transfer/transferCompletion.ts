// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The single answer to "is this transfer finished?", shared by the progress
 * card and the minimized chip.
 *
 * It lives in its own module, away from the React components, for two reasons:
 * the two surfaces used to decide this independently and disagreed, and vitest
 * runs on `environment: node`, so a predicate that cannot be imported without
 * pulling in React and lucide cannot be pinned by a real test.
 *
 * The defect this replaces: the lock that keeps a live transfer from being
 * dismissed was driven by the *rounded* percentage while the auto-dismiss timer
 * next to it used the raw one. At 99.6 the rounded value is already 100, so the
 * card unlocked and swapped the padlock for a close button wired to `onCancel`,
 * which cancels the transfer rather than dismissing the card. The chip was
 * worse: the same rounded value also drove its timer, so it dismissed itself
 * after three seconds on a transfer that was still running.
 */

/** The lane fields completion depends on. Structural, so both the card's lane
 *  type and any future caller satisfy it without importing it. */
export interface CompletionLane {
    state?: 'active' | 'completed' | 'error';
}

/**
 * True only when the transfer is really over: the raw percentage has reached
 * 100 and no lane is still moving bytes.
 *
 * A non-finite percentage counts as incomplete on purpose. The consequence of
 * being wrong here is asymmetric: staying locked one render too long is a
 * cosmetic delay, unlocking one render too early cancels a live transfer.
 */
export function isTransferComplete(percentage: number, lanes: readonly CompletionLane[] = []): boolean {
    if (!Number.isFinite(percentage) || percentage < 100) return false;
    return lanes.every((lane) => lane.state !== 'active');
}
