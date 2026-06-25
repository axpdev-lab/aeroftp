// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingRunCheckResult,
    CodingRunCheckResultData,
} from './aiChatTypes';

const CODING_RUN_CHECKS_KIND = 'coding_run_checks';

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

export function normalizeCodingRunCheckResult(value: unknown): CodingRunCheckResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.success !== 'boolean'
        || typeof value.workspace_root !== 'string'
        || typeof value.check !== 'string'
        || typeof value.label !== 'string'
        || typeof value.program !== 'string'
        || typeof value.timed_out !== 'boolean'
        || typeof value.stdout !== 'string'
        || typeof value.stderr !== 'string'
        || typeof value.stdout_truncated !== 'boolean'
        || typeof value.stderr_truncated !== 'boolean'
    ) {
        return null;
    }

    return {
        success: value.success,
        workspace_root: value.workspace_root,
        check: value.check,
        label: value.label,
        program: value.program,
        args: stringArray(value.args),
        filter: optionalString(value.filter),
        exit_code: optionalNumber(value.exit_code),
        timed_out: value.timed_out,
        timeout_secs: finiteNumber(value.timeout_secs),
        duration_ms: finiteNumber(value.duration_ms),
        stdout: value.stdout,
        stderr: value.stderr,
        stdout_truncated: value.stdout_truncated,
        stderr_truncated: value.stderr_truncated,
    };
}

export function isCodingRunCheckResultData(value: unknown): value is CodingRunCheckResultData {
    if (!isRecord(value) || value.kind !== CODING_RUN_CHECKS_KIND) return false;
    return !!normalizeCodingRunCheckResult(value.result);
}

export function getCodingRunCheckFromResultData(
    value: ChatResultData | undefined,
): CodingRunCheckResultData | null {
    if (!isCodingRunCheckResultData(value)) return null;
    const result = normalizeCodingRunCheckResult(value.result);
    if (!result) return null;
    return { kind: CODING_RUN_CHECKS_KIND, result };
}

export function commandLine(result: CodingRunCheckResult): string {
    return [result.program, ...result.args].join(' ');
}

export function summarizeCodingRunCheckResult(result: CodingRunCheckResult): string {
    const state = result.timed_out
        ? 'timed out'
        : result.success
            ? 'passed'
            : 'failed';
    const lines = [
        `**${result.label} ${state}**`,
        `\`${commandLine(result)}\``,
    ];
    if (result.timed_out) {
        lines.push(`Timed out after ${result.timeout_secs}s`);
    } else if (result.exit_code != null) {
        lines.push(`Exit code: ${result.exit_code} (${(result.duration_ms / 1000).toFixed(1)}s)`);
    }
    lines.push('');
    lines.push('Review the check output card below.');
    return lines.join('\n');
}
