// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * What the storage chip in the status bar says: the used space, over the
 * total when there is one, and the file count in words. The count used to sit
 * after a `·`, and "1234 · 5 GB" read as a multiplication or as a fraction
 * whose denominator had dropped to zero (#958).
 */
export function quotaChipLabel(
    quota: { used: number; total: number; files?: number | null },
    filesWord: string,
    formatBytes: (bytes: number) => string,
): string {
    const amount = quota.total > 0
        ? `${formatBytes(quota.used)} / ${formatBytes(quota.total)}`
        : formatBytes(quota.used);
    return quota.files != null ? `${amount} (${quota.files.toLocaleString()} ${filesWord})` : amount;
}
