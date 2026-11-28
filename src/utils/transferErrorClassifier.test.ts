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

    it('lets a typed FileTooLarge label decide over a reason that says quota', () => {
        // The label is ours and decides: one oversized file must not open the
        // circuit breaker for the batch.
        expect(
            classifyErrorFast(
                'File too large: the server refused this file as larger than it accepts, often a per-file limit of the account plan (HTTP 413). Server says: over quota for this folder',
            ),
        ).toEqual({ kind: 'file_too_large', retryable: false });
        expect(
            classifyErrorFast(
                'upload failed: File too large: the server refused this file (HTTP 403). Server says: Storage full',
            ),
        ).toEqual({ kind: 'file_too_large', retryable: false });
    });

    it('lets a typed QuotaExceeded label decide over a reason that says file too large', () => {
        expect(
            classifyErrorFast(
                'Storage quota exceeded: the account is full or has reached its storage limit (HTTP 413). Server says: file too large for the remaining space',
            ),
        ).toEqual({ kind: 'quota_exceeded', retryable: false });
    });

    it('ignores a label inside a named path', () => {
        // Behind a path, not behind wrappers: the substring checks classify
        // the text as before.
        expect(
            classifyErrorFast('/docs/a.txt: File too large: Server says: quota reached'),
        ).toEqual({ kind: 'quota_exceeded', retryable: false });
    });
});
