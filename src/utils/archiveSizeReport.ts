// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { formatBytes } from './formatters';

/**
 * Size accounting for a compression operation. `savedBytes` is positive when the
 * archive is smaller than the input (the normal case) and negative when an
 * already-compressed input grows (e.g. storing JPEGs in a zip). `savedPercent`
 * is in the range (-inf, 100]; it is `0` when the input size is unknown (0) so
 * callers never divide by zero or surface NaN.
 */
export interface CompressionRatio {
    inputBytes: number;
    outputBytes: number;
    savedBytes: number;
    savedPercent: number;
}

export function computeCompressionRatio(inputBytes: number, outputBytes: number): CompressionRatio {
    const savedBytes = inputBytes - outputBytes;
    const savedPercent = inputBytes > 0 ? (savedBytes / inputBytes) * 100 : 0;
    return { inputBytes, outputBytes, savedBytes, savedPercent };
}

/**
 * Activity-log detail line for a finished compression, e.g.
 * `"42.0 MB → 18.3 MB (saved 56%)"`. When the output size is unknown (0, e.g.
 * the post-success stat failed) only the input size is reported, never a fake
 * ratio.
 */
export function formatCompressDetails(inputBytes: number, outputBytes: number): string {
    if (outputBytes <= 0) return formatBytes(inputBytes);
    const { savedPercent } = computeCompressionRatio(inputBytes, outputBytes);
    const pct = Math.round(savedPercent);
    const verb = pct >= 0 ? 'saved' : 'grew';
    return `${formatBytes(inputBytes)} → ${formatBytes(outputBytes)} (${verb} ${Math.abs(pct)}%)`;
}

/**
 * Activity-log detail line for a finished extraction. Extraction expands data,
 * so the figure is labelled `expanded`; the output total is only known when the
 * backend emitted a determinate progress total (`outputBytes > 0`), otherwise
 * just the archive (input) size is reported.
 */
export function formatExtractDetails(inputBytes: number, outputBytes: number): string {
    if (outputBytes <= 0) return formatBytes(inputBytes);
    return `${formatBytes(inputBytes)} → ${formatBytes(outputBytes)} (expanded)`;
}
