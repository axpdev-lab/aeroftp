// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { classifyErrorFast, FATAL_ERROR_KINDS, PER_FILE_ERROR_KINDS } from './transferErrorClassifier';

describe('classifyErrorFast: a file over the destination limit', () => {
    // Zoho WorkDrive answered a 287 MB upload this way. It used to be `unknown`
    // (retryable), so the same file was sent again.
    it.each([
        'Upload failed (413 Payload Too Large): <html>',
        'HTTP 413 Request Entity Too Large',
        'File too large: File size limit exceeded. Maximum allowed file size: 100 MB',
    ])('classifies %s as file_too_large, not retryable', (raw) => {
        expect(classifyErrorFast(raw)).toEqual({ kind: 'file_too_large', retryable: false });
    });

    // One oversized file must not stop the batch: the other files still fit.
    it('is a per-file kind, not a fatal one', () => {
        expect(PER_FILE_ERROR_KINDS.has('file_too_large')).toBe(true);
        expect(FATAL_ERROR_KINDS.has('file_too_large')).toBe(false);
    });

    it('leaves a real quota refusal fatal', () => {
        expect(classifyErrorFast('552 Insufficient storage space').kind).toBe('quota_exceeded');
    });
});
