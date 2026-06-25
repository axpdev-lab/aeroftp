// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingDiagnostic,
    CodingDiagnosticsResult,
    CodingDiagnosticsResultData,
} from './aiChatTypes';

const CODING_DIAGNOSTICS_KIND = 'coding_diagnostics';

const isRecord = (value: unknown): value is Record<string, unknown> => (
    !!value && typeof value === 'object' && !Array.isArray(value)
);

const finiteNumber = (value: unknown): number => (
    typeof value === 'number' && Number.isFinite(value) ? value : 0
);

const optionalNumber = (value: unknown): number | null | undefined => {
    if (value === null) return null;
    return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
};

const optionalString = (value: unknown): string | null | undefined => {
    if (value === null) return null;
    return typeof value === 'string' ? value : undefined;
};

const stringArray = (value: unknown): string[] => (
    Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
);

function normalizeDiagnostic(value: unknown): CodingDiagnostic | null {
    if (!isRecord(value)) return null;
    if (typeof value.severity !== 'string' || typeof value.message !== 'string') {
        return null;
    }
    return {
        file: optionalString(value.file),
        line: optionalNumber(value.line),
        column: optionalNumber(value.column),
        severity: value.severity,
        code: optionalString(value.code),
        message: value.message,
    };
}

export function normalizeCodingDiagnosticsResult(value: unknown): CodingDiagnosticsResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.source !== 'string'
        || typeof value.program !== 'string'
        || typeof value.timed_out !== 'boolean'
        || typeof value.success !== 'boolean'
        || typeof value.truncated !== 'boolean'
        || !Array.isArray(value.diagnostics)
    ) {
        return null;
    }
    const diagnostics: CodingDiagnostic[] = [];
    for (const item of value.diagnostics) {
        const normalized = normalizeDiagnostic(item);
        if (!normalized) return null;
        diagnostics.push(normalized);
    }
    return {
        workspace_root: value.workspace_root,
        source: value.source,
        program: value.program,
        args: stringArray(value.args),
        exit_code: optionalNumber(value.exit_code),
        timed_out: value.timed_out,
        timeout_secs: finiteNumber(value.timeout_secs),
        duration_ms: finiteNumber(value.duration_ms),
        success: value.success,
        error_count: finiteNumber(value.error_count),
        warning_count: finiteNumber(value.warning_count),
        diagnostics,
        truncated: value.truncated,
    };
}

export function isCodingDiagnosticsResultData(value: unknown): value is CodingDiagnosticsResultData {
    if (!isRecord(value) || value.kind !== CODING_DIAGNOSTICS_KIND) return false;
    return !!normalizeCodingDiagnosticsResult(value.result);
}

export function getCodingDiagnosticsFromResultData(
    value: ChatResultData | undefined,
): CodingDiagnosticsResultData | null {
    if (!isCodingDiagnosticsResultData(value)) return null;
    const result = normalizeCodingDiagnosticsResult(value.result);
    if (!result) return null;
    return { kind: CODING_DIAGNOSTICS_KIND, result };
}

export function diagnosticLocation(diagnostic: CodingDiagnostic): string {
    if (!diagnostic.file) return '(no location)';
    const line = diagnostic.line != null ? `:${diagnostic.line}` : '';
    const column = diagnostic.line != null && diagnostic.column != null ? `:${diagnostic.column}` : '';
    return `${diagnostic.file}${line}${column}`;
}

export function summarizeCodingDiagnosticsResult(result: CodingDiagnosticsResult): string {
    if (result.timed_out) {
        return [
            `**Diagnostics (${result.source}) timed out**`,
            `Timed out after ${result.timeout_secs}s`,
            '',
            'Review the diagnostics card below.',
        ].join('\n');
    }
    const state = result.success ? 'clean' : `${result.error_count} error(s)`;
    const lines = [
        `**Diagnostics (${result.source}): ${state}**`,
        `${result.error_count} error(s), ${result.warning_count} warning(s)`,
    ];
    if (result.truncated) lines.push('Diagnostics list was truncated.');
    lines.push('');
    lines.push('Review the diagnostics card below.');
    return lines.join('\n');
}
