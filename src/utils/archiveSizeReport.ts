// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { formatBytes } from './formatters';
import { translate } from '../i18n';
import type { TranslationFunction } from '../i18n/types';

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
export function formatCompressDetails(inputBytes: number, outputBytes: number, t: TranslationFunction = translate): string {
    if (outputBytes <= 0) return formatBytes(inputBytes);
    const { savedPercent } = computeCompressionRatio(inputBytes, outputBytes);
    const pct = Math.round(savedPercent);
    const sizes = { input: formatBytes(inputBytes), output: formatBytes(outputBytes), percent: Math.abs(pct) };
    return pct >= 0 ? t('activity.compress_details_saved', sizes) : t('activity.compress_details_grew', sizes);
}

/**
 * Activity-log detail line for a finished extraction. Extraction expands data,
 * so the figure is labelled `expanded`; the output total is only known when the
 * backend emitted a determinate progress total (`outputBytes > 0`), otherwise
 * just the archive (input) size is reported.
 */
export function formatExtractDetails(inputBytes: number, outputBytes: number, t: TranslationFunction = translate): string {
    if (outputBytes <= 0) return formatBytes(inputBytes);
    return t('activity.extract_details', { input: formatBytes(inputBytes), output: formatBytes(outputBytes) });
}
