// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    diagnosticLocation,
    getCodingDiagnosticsFromResultData,
    isCodingDiagnosticsResultData,
    normalizeCodingDiagnosticsResult,
    summarizeCodingDiagnosticsResult,
} from './aiChatCodingDiagnostics';

const errorResult = {
    workspace_root: '/repo',
    source: 'cargo',
    program: 'cargo',
    args: ['check', '--message-format=json'],
    exit_code: 101,
    timed_out: false,
    timeout_secs: 600,
    duration_ms: 4200,
    success: false,
    error_count: 1,
    warning_count: 1,
    diagnostics: [
        {
            file: 'src/lib.rs',
            line: 12,
            column: 5,
            severity: 'error',
            code: 'E0432',
            message: 'unresolved import `foo`',
        },
        {
            file: 'src/a.rs',
            line: 3,
            column: 9,
            severity: 'warning',
            code: null,
            message: 'unused variable',
        },
    ],
    truncated: false,
};

const cleanResult = {
    ...errorResult,
    exit_code: 0,
    success: true,
    error_count: 0,
    warning_count: 0,
    diagnostics: [],
};

const timeoutResult = {
    ...errorResult,
    exit_code: null,
    timed_out: true,
    success: false,
    error_count: 0,
    warning_count: 0,
    diagnostics: [],
};

describe('aiChatCodingDiagnostics', () => {
    it('normalizes a result with diagnostics', () => {
        const result = normalizeCodingDiagnosticsResult(errorResult);
        expect(result?.source).toBe('cargo');
        expect(result?.diagnostics).toHaveLength(2);
        expect(result?.diagnostics[0].code).toBe('E0432');
        expect(result?.diagnostics[1].code).toBeNull();
        expect(result?.error_count).toBe(1);
    });

    it('rejects malformed input', () => {
        expect(normalizeCodingDiagnosticsResult(null)).toBeNull();
        expect(normalizeCodingDiagnosticsResult({ source: 'cargo' })).toBeNull();
        // A diagnostic missing required fields invalidates the whole result.
        expect(normalizeCodingDiagnosticsResult({
            ...errorResult,
            diagnostics: [{ severity: 'error' }],
        })).toBeNull();
    });

    it('builds a readable location string', () => {
        const result = normalizeCodingDiagnosticsResult(errorResult)!;
        expect(diagnosticLocation(result.diagnostics[0])).toBe('src/lib.rs:12:5');
        expect(diagnosticLocation({ severity: 'error', message: 'x' })).toBe('(no location)');
    });

    it('summarizes clean, error, and timed-out states', () => {
        const error = normalizeCodingDiagnosticsResult(errorResult)!;
        const clean = normalizeCodingDiagnosticsResult(cleanResult)!;
        const timed = normalizeCodingDiagnosticsResult(timeoutResult)!;
        expect(summarizeCodingDiagnosticsResult(error)).toContain('1 error(s)');
        expect(summarizeCodingDiagnosticsResult(clean)).toContain('clean');
        expect(summarizeCodingDiagnosticsResult(timed)).toContain('timed out');
    });

    it('guards and extracts result data', () => {
        const data = { kind: 'coding_diagnostics', result: errorResult };
        expect(isCodingDiagnosticsResultData(data)).toBe(true);
        expect(isCodingDiagnosticsResultData({ kind: 'coding_verify', result: errorResult })).toBe(false);
        expect(getCodingDiagnosticsFromResultData(data as never)?.result.source).toBe('cargo');
    });
});
