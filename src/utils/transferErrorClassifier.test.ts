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

    it('classifies the typed WebDAV 507 QuotaExceeded display as quota_exceeded', () => {
        // ProviderError::QuotaExceeded renders "Storage quota exceeded: {0}";
        // the WebDAV 507 message names the plan limit and quotes the server.
        expect(
            classifyErrorFast(
                'Storage quota exceeded: the account is full or has reached a plan limit, such as a storage quota or a maximum number of files (HTTP 507). Server says: File limit reached \u{2014} upgrade your plan',
            ),
        ).toEqual({ kind: 'quota_exceeded', retryable: false });
    });

    it('checks quota before the file-size phrases, matching sync.rs', () => {
        // A quota error whose quoted server reason mentions a size limit must
        // stay quota_exceeded, so the circuit breaker stops the batch.
        expect(
            classifyErrorFast(
                'Storage quota exceeded: the account is full. Server says: file size limit reached',
            ),
        ).toEqual({ kind: 'quota_exceeded', retryable: false });
    });
});
