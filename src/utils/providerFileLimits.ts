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
    /** Limits the provider states for the whole path or object key. */
    maxPathBytes: number | null;
    maxPathChars: number | null;
}

export type WontFitReason = 'too-large' | 'name-too-long' | 'path-too-long';

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
        maxPathBytes: hints.max_path_bytes ?? null,
        maxPathChars: hints.max_path_chars ?? null,
    };
    return Object.values(limits).every(v => v === null) ? null : limits;
}

const utf8 = new TextEncoder();

/** Characters as a person counts them (code points, not UTF-16 units). */
const charCount = (name: string): number => Array.from(name).length;

const overLimit = (text: string, bytes: number | null, chars: number | null): boolean =>
    (bytes !== null && utf8.encode(text).length > bytes)
    || (chars !== null && charCount(text) > chars);

export function nameTooLong(name: string, limits: ProviderFileLimits): boolean {
    return overLimit(name, limits.maxNameBytes, limits.maxNameChars);
}

/**
 * The path the entry would have on the remote, without the leading slash:
 * the remote folder being compared plus the entry's relative path. Leaving
 * the slash out can only make the path shorter than the provider counts it,
 * so a warning based on it is never false.
 */
export function remoteDestinationPath(remoteBase: string, entry: CompareResultEntry): string {
    const base = remoteBase.replace(/^\/+|\/+$/g, '');
    const rel = (entry.relativePath || entry.name || '').replace(/^\/+/, '');
    return base ? `${base}/${rel}` : rel;
}

const baseName = (entry: CompareResultEntry): string => {
    const name = entry.name || entry.relativePath || '';
    const slash = name.lastIndexOf('/');
    return slash >= 0 ? name.slice(slash + 1) : name;
};

/**
 * Entries whose local copy a sync toward the remote may send and the remote
 * cannot store. Every bucket with a local copy counts except `same`: Mirror
 * also sends the local copy of `conflict` and of entries newer on the remote.
 * Only the direction that ends on the remote is checked: a download to the
 * local disk has no provider limit. `local-local` pairs have no remote.
 */
export function entriesThatWillNotFit(
    result: CompareResult | null,
    pairKind: string | null | undefined,
    limits: ProviderFileLimits | null,
    remoteBase = '',
): WontFitEntry[] {
    if (!result || !limits) return [];
    const remoteOnRight = pairKind === 'local-remote';
    const remoteOnLeft = pairKind === 'remote-local';
    if (!remoteOnRight && !remoteOnLeft) return [];
    const outgoing = remoteOnRight
        ? [...result.buckets['only-left'], ...result.buckets['newer-left'], ...result.buckets['newer-right'], ...result.buckets.conflict]
        : [...result.buckets['only-right'], ...result.buckets['newer-right'], ...result.buckets['newer-left'], ...result.buckets.conflict];

    const out: WontFitEntry[] = [];
    for (const entry of outgoing) {
        const isDir = remoteOnRight ? entry.leftIsDir : entry.rightIsDir;
        const size = (remoteOnRight ? entry.leftSize : entry.rightSize) ?? null;
        const reasons: WontFitReason[] = [];
        if (!isDir && limits.maxFileSize !== null && size !== null && size > limits.maxFileSize) {
            reasons.push('too-large');
        }
        if (nameTooLong(baseName(entry), limits)) reasons.push('name-too-long');
        if (overLimit(remoteDestinationPath(remoteBase, entry), limits.maxPathBytes, limits.maxPathChars)) {
            reasons.push('path-too-long');
        }
        if (reasons.length > 0) out.push({ entry, reasons, size });
    }
    return out;
}
