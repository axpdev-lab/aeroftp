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
};

/** The i18n key to show beside the figure, or null when it is a full answer. */
export type UsedScanBoundKey = 'transfer.cancelled' | 'statusBar.usedScanTruncated';

export function usedScanBoundKey(res: UsedScanBoundFlags): UsedScanBoundKey | null {
    if (res.cancelled) return 'transfer.cancelled';
    if (res.truncated) return 'statusBar.usedScanTruncated';
    return null;
}
