// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingPatchDiagnostic,
    CodingPatchFileResult,
    CodingPatchResult,
    CodingPatchResultData,
} from './aiChatTypes';

const CODING_PATCH_KIND = 'coding_patch';

const isRecord = (value: unknown): value is Record<string, unknown> => (
    !!value && typeof value === 'object' && !Array.isArray(value)
);

const finiteNumber = (value: unknown): number => (
    typeof value === 'number' && Number.isFinite(value) ? value : 0
);

const optionalString = (value: unknown): string | null | undefined => {
    if (value === null) return null;
    return typeof value === 'string' ? value : undefined;
};

const normalizeFiles = (value: unknown): CodingPatchFileResult[] | null => {
    if (!Array.isArray(value)) return null;

    const files: CodingPatchFileResult[] = [];
    for (const item of value) {
        if (!isRecord(item) || typeof item.path !== 'string' || typeof item.status !== 'string') {
            return null;
        }
        files.push({
            path: item.path,
            status: item.status,
            hunks: finiteNumber(item.hunks),
            additions: finiteNumber(item.additions),
            deletions: finiteNumber(item.deletions),
            old_size_bytes: finiteNumber(item.old_size_bytes),
            new_size_bytes: finiteNumber(item.new_size_bytes),
        });
    }
    return files;
};

const normalizeDiagnostics = (value: unknown): CodingPatchDiagnostic[] => {
    if (!Array.isArray(value)) return [];

    return value
        .map((item): CodingPatchDiagnostic | null => {
            if (!isRecord(item) || typeof item.message !== 'string') return null;
            const hunkIndex = item.hunk_index;
            return {
                path: optionalString(item.path),
                hunk_index: hunkIndex === null
                    ? null
                    : typeof hunkIndex === 'number' && Number.isFinite(hunkIndex)
                        ? hunkIndex
                        : undefined,
                message: item.message,
                expected: optionalString(item.expected),
                actual: optionalString(item.actual),
            };
        })
        .filter((item): item is CodingPatchDiagnostic => !!item);
};

export function normalizeCodingPatchResult(value: unknown): CodingPatchResult | null {
    if (!isRecord(value)) return null;
    if (typeof value.success !== 'boolean' || typeof value.dry_run !== 'boolean') return null;

    const files = normalizeFiles(value.files);
    if (!files) return null;

    return {
        success: value.success,
        dry_run: value.dry_run,
        checkpoint_id: optionalString(value.checkpoint_id),
        files,
        diagnostics: normalizeDiagnostics(value.diagnostics),
        warnings: Array.isArray(value.warnings)
            ? value.warnings.filter((warning): warning is string => typeof warning === 'string')
            : [],
    };
}

export function isCodingPatchResultData(value: unknown): value is CodingPatchResultData {
    if (!isRecord(value)) return false;
    return value.kind === CODING_PATCH_KIND && !!normalizeCodingPatchResult(value.result);
}

export function getCodingPatchFromResultData(value: ChatResultData | undefined): CodingPatchResultData | null {
    if (!isCodingPatchResultData(value)) return null;

    const result = normalizeCodingPatchResult(value.result);
    if (!result) return null;

    return {
        kind: CODING_PATCH_KIND,
        result,
        workspaceRoot: typeof value.workspaceRoot === 'string' ? value.workspaceRoot : undefined,
        patch: typeof value.patch === 'string' ? value.patch : undefined,
    };
}

export function summarizeCodingPatchResult(result: CodingPatchResult): string {
    const totals = result.files.reduce(
        (acc, file) => ({
            additions: acc.additions + file.additions,
            deletions: acc.deletions + file.deletions,
            hunks: acc.hunks + file.hunks,
        }),
        { additions: 0, deletions: 0, hunks: 0 },
    );

    const action = result.dry_run ? 'Patch dry run' : 'Patch apply';
    const state = result.success
        ? result.dry_run ? 'passed' : 'completed'
        : result.dry_run ? 'found conflicts' : 'was blocked';
    const lines = [
        `**${action} ${state}**`,
        `${result.files.length} file(s), ${totals.hunks} hunk(s), +${totals.additions}/-${totals.deletions}`,
    ];

    if (result.checkpoint_id) {
        lines.push(`Checkpoint: \`${result.checkpoint_id}\``);
    }
    if (result.diagnostics.length > 0) {
        lines.push(`Diagnostics: ${result.diagnostics.length}`);
    }
    if (result.warnings.length > 0) {
        lines.push(`Warnings: ${result.warnings.length}`);
    }

    lines.push('');
    lines.push('Review the patch card below for file details, diagnostics, and restore guidance.');
    return lines.join('\n');
}
