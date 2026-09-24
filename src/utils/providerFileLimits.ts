// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Compare-time check for files the remote cannot take (#347): a file larger
 * than the provider's documented single-file limit, or a name longer than its
 * documented name limit. The limits come from `get_transfer_optimization_hints`,
 * which fills them only from the provider's own documentation, so a provider
 * with no documented limit never produces a warning here.
 */

import type { CompareResult, CompareResultEntry } from './compareEndpoints';
import type { TransferOptimizationHints } from '../types';

export interface ProviderFileLimits {
    maxFileSize: number | null;
    maxNameBytes: number | null;
    maxNameChars: number | null;
}

export type WontFitReason = 'too-large' | 'name-too-long';

export interface WontFitEntry {
    entry: CompareResultEntry;
    reasons: WontFitReason[];
    /** Size of the copy that would be sent. */
    size: number | null;
}

export function limitsFromHints(hints: TransferOptimizationHints | null | undefined): ProviderFileLimits | null {
    if (!hints) return null;
    const limits: ProviderFileLimits = {
        maxFileSize: hints.max_file_size ?? null,
        maxNameBytes: hints.max_name_bytes ?? null,
        maxNameChars: hints.max_name_chars ?? null,
    };
    return limits.maxFileSize === null && limits.maxNameBytes === null && limits.maxNameChars === null
        ? null
        : limits;
}

const utf8 = new TextEncoder();

/** Characters as a person counts them (code points, not UTF-16 units). */
const charCount = (name: string): number => Array.from(name).length;

export function nameTooLong(name: string, limits: ProviderFileLimits): boolean {
    if (limits.maxNameBytes !== null && utf8.encode(name).length > limits.maxNameBytes) return true;
    if (limits.maxNameChars !== null && charCount(name) > limits.maxNameChars) return true;
    return false;
}

const baseName = (entry: CompareResultEntry): string => {
    const name = entry.name || entry.relativePath || '';
    const slash = name.lastIndexOf('/');
    return slash >= 0 ? name.slice(slash + 1) : name;
};

/**
 * Entries a mirror toward the remote would send and the remote cannot store.
 * Only the direction that ends on the remote is checked: a download to the
 * local disk has no provider limit. `local-local` pairs have no remote.
 */
export function entriesThatWillNotFit(
    result: CompareResult | null,
    pairKind: string | null | undefined,
    limits: ProviderFileLimits | null,
): WontFitEntry[] {
    if (!result || !limits) return [];
    const remoteOnRight = pairKind === 'local-remote';
    const remoteOnLeft = pairKind === 'remote-local';
    if (!remoteOnRight && !remoteOnLeft) return [];
    const outgoing = remoteOnRight
        ? [...result.buckets['only-left'], ...result.buckets['newer-left']]
        : [...result.buckets['only-right'], ...result.buckets['newer-right']];

    const out: WontFitEntry[] = [];
    for (const entry of outgoing) {
        const isDir = remoteOnRight ? entry.leftIsDir : entry.rightIsDir;
        const size = (remoteOnRight ? entry.leftSize : entry.rightSize) ?? null;
        const reasons: WontFitReason[] = [];
        if (!isDir && limits.maxFileSize !== null && size !== null && size > limits.maxFileSize) {
            reasons.push('too-large');
        }
        if (nameTooLong(baseName(entry), limits)) reasons.push('name-too-long');
        if (reasons.length > 0) out.push({ entry, reasons, size });
    }
    return out;
}
