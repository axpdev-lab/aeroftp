// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingCheckpointRestoreFileResult,
    CodingCheckpointRestoreResult,
    CodingCheckpointRestoreResultData,
} from './aiChatTypes';

const CODING_CHECKPOINT_RESTORE_KIND = 'coding_checkpoint_restore';

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

const normalizeFiles = (value: unknown): CodingCheckpointRestoreFileResult[] | null => {
    if (!Array.isArray(value)) return null;

    const files: CodingCheckpointRestoreFileResult[] = [];
    for (const item of value) {
        if (
            !isRecord(item)
            || typeof item.path !== 'string'
            || typeof item.action !== 'string'
            || typeof item.existed_at_checkpoint !== 'boolean'
        ) {
            return null;
        }

        files.push({
            path: item.path,
            action: item.action,
            existed_at_checkpoint: item.existed_at_checkpoint,
            size_bytes: finiteNumber(item.size_bytes),
            sha256: optionalString(item.sha256),
        });
    }

    return files;
};

export function normalizeCodingCheckpointRestoreResult(value: unknown): CodingCheckpointRestoreResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.checkpoint_id !== 'string'
        || typeof value.workspace_root !== 'string'
        || typeof value.dry_run !== 'boolean'
    ) {
        return null;
    }

    const files = normalizeFiles(value.files);
    if (!files) return null;

    return {
        checkpoint_id: value.checkpoint_id,
        workspace_root: value.workspace_root,
        dry_run: value.dry_run,
        files,
    };
}

export function isCodingCheckpointRestoreResultData(value: unknown): value is CodingCheckpointRestoreResultData {
    if (!isRecord(value)) return false;
    return value.kind === CODING_CHECKPOINT_RESTORE_KIND && !!normalizeCodingCheckpointRestoreResult(value.result);
}

export function getCodingCheckpointRestoreFromResultData(
    value: ChatResultData | undefined,
): CodingCheckpointRestoreResultData | null {
    if (!isCodingCheckpointRestoreResultData(value)) return null;

    const result = normalizeCodingCheckpointRestoreResult(value.result);
    if (!result) return null;

    return {
        kind: CODING_CHECKPOINT_RESTORE_KIND,
        result,
        requestedPaths: Array.isArray(value.requestedPaths)
            ? value.requestedPaths.filter((path): path is string => typeof path === 'string')
            : undefined,
    };
}

export function countCodingCheckpointRestoreActions(result: CodingCheckpointRestoreResult): Record<string, number> {
    return result.files.reduce<Record<string, number>>((counts, file) => {
        counts[file.action] = (counts[file.action] ?? 0) + 1;
        return counts;
    }, {});
}

export function summarizeCodingCheckpointRestoreResult(result: CodingCheckpointRestoreResult): string {
    const counts = countCodingCheckpointRestoreActions(result);
    const restoreCount = counts.restore ?? 0;
    const deleteCount = counts.delete ?? 0;
    const noopCount = counts.noop ?? 0;
    const otherCount = Math.max(0, result.files.length - restoreCount - deleteCount - noopCount);
    const title = result.dry_run
        ? 'Checkpoint restore dry run completed'
        : 'Checkpoint restore completed';

    const actionLine = result.dry_run
        ? `Would rewrite ${restoreCount}, delete ${deleteCount}, skip ${noopCount}`
        : `Rewrote ${restoreCount}, deleted ${deleteCount}, skipped ${noopCount}`;
    const lines = [
        `**${title}**`,
        `Checkpoint: \`${result.checkpoint_id}\``,
        `Workspace: \`${result.workspace_root}\``,
        `${result.files.length} file(s). ${actionLine}${otherCount ? `, other ${otherCount}` : ''}.`,
    ];

    lines.push('');
    lines.push('Review the checkpoint restore card below for file-level actions.');
    return lines.join('\n');
}
