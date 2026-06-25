// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    commandLine,
    getCodingRunCheckFromResultData,
    getCodingVerifyFromResultData,
    isCodingRunCheckResultData,
    isCodingVerifyResultData,
    normalizeCodingRunCheckResult,
    normalizeCodingVerifyResult,
    summarizeCodingRunCheckResult,
    summarizeCodingVerifyResult,
} from './aiChatCodingChecks';

const passResult = {
    success: true,
    workspace_root: '/repo',
    check: 'cargo-test',
    label: 'Cargo Test',
    program: 'cargo',
    args: ['test', 'my_filter'],
    filter: 'my_filter',
    exit_code: 0,
    timed_out: false,
    timeout_secs: 600,
    duration_ms: 2500,
    stdout: 'test result: ok. 8 passed',
    stderr: '',
    stdout_truncated: false,
    stderr_truncated: false,
};

const failResult = {
    ...passResult,
    success: false,
    filter: null,
    args: ['clippy'],
    check: 'cargo-clippy',
    label: 'Cargo Clippy',
    exit_code: 101,
    stderr: 'error: could not compile',
};

const timeoutResult = {
    ...passResult,
    success: false,
    timed_out: true,
    exit_code: null,
};

describe('aiChatCodingChecks', () => {
    it('normalizes a passing check result', () => {
        const result = normalizeCodingRunCheckResult(passResult);
        expect(result?.success).toBe(true);
        expect(result?.exit_code).toBe(0);
        expect(result?.filter).toBe('my_filter');
        expect(result?.args).toEqual(['test', 'my_filter']);
    });

    it('preserves a null filter and failing exit code', () => {
        const result = normalizeCodingRunCheckResult(failResult);
        expect(result?.success).toBe(false);
        expect(result?.filter).toBeNull();
        expect(result?.exit_code).toBe(101);
    });

    it('rejects malformed input', () => {
        expect(normalizeCodingRunCheckResult(null)).toBeNull();
        expect(normalizeCodingRunCheckResult({ success: true })).toBeNull();
    });

    it('builds the command line from program and args', () => {
        const result = normalizeCodingRunCheckResult(passResult)!;
        expect(commandLine(result)).toBe('cargo test my_filter');
    });

    it('summarizes passed, failed, and timed-out states', () => {
        const pass = normalizeCodingRunCheckResult(passResult)!;
        const fail = normalizeCodingRunCheckResult(failResult)!;
        const timed = normalizeCodingRunCheckResult(timeoutResult)!;
        expect(summarizeCodingRunCheckResult(pass)).toContain('passed');
        expect(summarizeCodingRunCheckResult(fail)).toContain('failed');
        expect(summarizeCodingRunCheckResult(timed)).toContain('timed out');
    });

    it('guards and extracts result data', () => {
        const data = { kind: 'coding_run_checks', result: passResult };
        expect(isCodingRunCheckResultData(data)).toBe(true);
        expect(isCodingRunCheckResultData({ kind: 'coding_git', result: passResult })).toBe(false);
        expect(getCodingRunCheckFromResultData(data as never)?.result.check).toBe('cargo-test');
    });

    it('normalizes and summarizes a verify result', () => {
        const verify = {
            workspace_root: '/repo',
            overall_success: false,
            stopped_early: true,
            checks: [passResult, failResult],
        };
        const result = normalizeCodingVerifyResult(verify);
        expect(result?.checks).toHaveLength(2);
        expect(result?.overall_success).toBe(false);
        expect(result?.stopped_early).toBe(true);
        expect(summarizeCodingVerifyResult(result!)).toContain('1/2');
        expect(isCodingVerifyResultData({ kind: 'coding_verify', result: verify })).toBe(true);
        expect(getCodingVerifyFromResultData({ kind: 'coding_verify', result: verify } as never)?.result.checks).toHaveLength(2);
    });

    it('rejects a verify result with a malformed check', () => {
        const bad = { workspace_root: '/repo', overall_success: true, stopped_early: false, checks: [{ success: true }] };
        expect(normalizeCodingVerifyResult(bad)).toBeNull();
    });
});
