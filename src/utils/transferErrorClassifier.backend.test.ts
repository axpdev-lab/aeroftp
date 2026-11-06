// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// #1506 row 164: the batch circuit breaker classifies a failure with the
// backend's classifier, which reads the OS error code, and keeps the English
// copy only for when the command cannot be reached.

import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import breakerHook from '../hooks/useCircuitBreaker.ts?raw';
import { classifyErrorFast, classifyTransferFailure, FATAL_ERROR_KINDS } from './transferErrorClassifier';

// Italian Windows, full disk: what the backend's `classify_sync_error`
// answers for it (pinned there by `an_os_error_is_classified_by_its_code_in_any_language`).
const FULL_DISK_IT = 'Upload failed: IO error: Spazio su disco insufficiente. (os error 112)';
const BACKEND = { kind: 'disk_error', message: FULL_DISK_IT, retryable: false, file_path: null };

describe('classifyTransferFailure', () => {
    it('takes the backend answer, which knows a translated full disk', async () => {
        const invoke = vi.fn().mockResolvedValue(BACKEND);
        const verdict = await classifyTransferFailure(FULL_DISK_IT, invoke);
        expect(verdict).toEqual({ kind: 'disk_error', retryable: false });
        expect(FATAL_ERROR_KINDS.has(verdict.kind)).toBe(true);
        expect(invoke).toHaveBeenCalledWith('classify_transfer_error', { rawError: FULL_DISK_IT, filePath: null });
        // The English copy alone missed it: unknown and retryable, so the
        // batch went on against a full disk.
        expect(classifyErrorFast(FULL_DISK_IT)).toEqual({ kind: 'unknown', retryable: true });
    });

    it('falls back to the local copy when the command cannot be reached', async () => {
        const invoke = vi.fn().mockRejectedValue(new Error('no IPC'));
        expect(await classifyTransferFailure('530 Login incorrect', invoke)).toEqual(classifyErrorFast('530 Login incorrect'));
    });

    it('falls back on an answer without a kind', async () => {
        const invoke = vi.fn().mockResolvedValue({ message: 'x' });
        expect(await classifyTransferFailure('Connection reset by peer', invoke)).toEqual(classifyErrorFast('Connection reset by peer'));
    });
});

describe('the breaker uses it', () => {
    it('recordFailure classifies through the backend, and App awaits every call', () => {
        expect(breakerHook).toContain('await classifyTransferFailure(errorMessage, invoke)');
        expect(breakerHook).not.toMatch(/classifyErrorFast\(/);
        const calls = [...app.matchAll(/(await )?circuitBreaker\.recordFailure\(/g)];
        expect(calls.length).toBe(3);
        expect(calls.filter(([, awaited]) => !awaited)).toEqual([]);
    });
});
