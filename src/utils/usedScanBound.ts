// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Which word a used-storage figure carries when it is a lower bound.
 *
 * Mirrors `size_bound_marker` in `src-tauri/src/used_scan.rs`, order included.
 * A cancelled scan is marked truncated as well, because the backend records a
 * stopped walk as a lower bound rather than a zero, so the cancel has to be
 * named first: named second, a scan the user stopped reads as a listing the
 * provider cut short. The Rust side states the rule ("two different states and
 * must not share a word") and this keeps the same rule checkable here, where
 * an inline condition could be reordered without anything turning red.
 *
 * `cancelled` is additive, as in `usedScanPersist`: a backend that omits the
 * field is treated as not-cancelled, and `truncated` still speaks for itself.
 */
export type UsedScanBoundFlags = {
    truncated: boolean;
    cancelled?: boolean;
    /** Folders the walk could not list (additive, like `cancelled`). */
    unreadable_dirs?: number;
    /** True when the depth or entry cap stopped the walk (additive). */
    hit_cap?: boolean;
};

/** The i18n key to show beside the figure, or null when it is a full answer. */
export type UsedScanBoundKey =
    | 'transfer.cancelled'
    | 'statusBar.usedScanTruncated'
    | 'statusBar.usedScanUnreadable';

/**
 * Folders that could not be read are named apart from the caps, as the Rust
 * `UsedScan` does: the remedy is the opposite (raising a limit cannot help),
 * and a Proton Drive scan with one unopenable Photos section said "scan limit
 * reached" after 23 files. The cap wins when both happened, since it is the
 * one the user can act on. The key takes `{count}`.
 */
export function usedScanBoundKey(res: UsedScanBoundFlags): UsedScanBoundKey | null {
    if (res.cancelled) return 'transfer.cancelled';
    if (res.hit_cap) return 'statusBar.usedScanTruncated';
    if ((res.unreadable_dirs ?? 0) > 0) return 'statusBar.usedScanUnreadable';
    if (res.truncated) return 'statusBar.usedScanTruncated';
    return null;
}
