// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingGitCommitResult,
    CodingGitDiffResult,
    CodingGitDiffStat,
    CodingGitFileStatus,
    CodingGitResult,
    CodingGitResultData,
    CodingGitStageResult,
    CodingGitStatusResult,
} from './aiChatTypes';

export type CodingGitToolName =
    | 'coding_git_status'
    | 'coding_git_diff'
    | 'coding_git_stage'
    | 'coding_git_commit';

const CODING_GIT_KIND = 'coding_git';

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

const stringArray = (value: unknown): string[] => (
    Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
);

const normalizeFileStatuses = (value: unknown): CodingGitFileStatus[] | null => {
    if (!Array.isArray(value)) return null;
    const entries: CodingGitFileStatus[] = [];
    for (const item of value) {
        if (
            !isRecord(item)
            || typeof item.path !== 'string'
            || typeof item.index_status !== 'string'
            || typeof item.worktree_status !== 'string'
        ) {
            return null;
        }
        entries.push({
            path: item.path,
            index_status: item.index_status,
            worktree_status: item.worktree_status,
        });
    }
    return entries;
};

export function normalizeCodingGitStatusResult(value: unknown): CodingGitStatusResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.clean !== 'boolean'
        || typeof value.truncated !== 'boolean'
    ) {
        return null;
    }

    const staged = normalizeFileStatuses(value.staged);
    const unstaged = normalizeFileStatuses(value.unstaged);
    const untracked = normalizeFileStatuses(value.untracked);
    const conflicted = normalizeFileStatuses(value.conflicted);
    if (!staged || !unstaged || !untracked || !conflicted) return null;

    return {
        workspace_root: value.workspace_root,
        repo_root: value.repo_root,
        branch: optionalString(value.branch),
        head: optionalString(value.head),
        upstream: optionalString(value.upstream),
        ahead: finiteNumber(value.ahead),
        behind: finiteNumber(value.behind),
        clean: value.clean,
        staged,
        unstaged,
        untracked,
        conflicted,
        total: finiteNumber(value.total),
        truncated: value.truncated,
        raw: stringArray(value.raw),
    };
}

const normalizeDiffStats = (value: unknown): CodingGitDiffStat[] | null => {
    if (!Array.isArray(value)) return null;
    const stats: CodingGitDiffStat[] = [];
    for (const item of value) {
        if (!isRecord(item) || typeof item.path !== 'string' || typeof item.binary !== 'boolean') {
            return null;
        }
        stats.push({
            path: item.path,
            additions: finiteNumber(item.additions),
            deletions: finiteNumber(item.deletions),
            binary: item.binary,
        });
    }
    return stats;
};

export function normalizeCodingGitDiffResult(value: unknown): CodingGitDiffResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.staged !== 'boolean'
        || typeof value.diff !== 'string'
        || typeof value.truncated !== 'boolean'
    ) {
        return null;
    }

    const stats = normalizeDiffStats(value.stats);
    if (!stats) return null;

    return {
        workspace_root: value.workspace_root,
        repo_root: value.repo_root,
        staged: value.staged,
        paths: stringArray(value.paths),
        file_count: finiteNumber(value.file_count),
        total_additions: finiteNumber(value.total_additions),
        total_deletions: finiteNumber(value.total_deletions),
        stats,
        diff: value.diff,
        truncated: value.truncated,
    };
}

export function normalizeCodingGitStageResult(value: unknown): CodingGitStageResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.success !== 'boolean'
        || typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.dry_run !== 'boolean'
        || typeof value.staged !== 'boolean'
        || typeof value.message !== 'string'
    ) {
        return null;
    }

    const before = normalizeCodingGitStatusResult(value.before);
    if (!before) return null;
    const after = value.after == null ? null : normalizeCodingGitStatusResult(value.after);
    if (value.after != null && !after) return null;

    return {
        success: value.success,
        workspace_root: value.workspace_root,
        repo_root: value.repo_root,
        dry_run: value.dry_run,
        staged: value.staged,
        paths: stringArray(value.paths),
        before,
        after,
        message: value.message,
    };
}

export function normalizeCodingGitCommitResult(value: unknown): CodingGitCommitResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.success !== 'boolean'
        || typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.dry_run !== 'boolean'
        || typeof value.committed !== 'boolean'
        || typeof value.message !== 'string'
        || typeof value.stdout !== 'string'
        || typeof value.stderr !== 'string'
    ) {
        return null;
    }

    const before = normalizeCodingGitStatusResult(value.before);
    if (!before) return null;
    const after = value.after == null ? null : normalizeCodingGitStatusResult(value.after);
    if (value.after != null && !after) return null;

    return {
        success: value.success,
        workspace_root: value.workspace_root,
        repo_root: value.repo_root,
        dry_run: value.dry_run,
        committed: value.committed,
        commit_hash: optionalString(value.commit_hash),
        message: value.message,
        stdout: value.stdout,
        stderr: value.stderr,
        before,
        after,
    };
}

export function normalizeCodingGitResult(
    toolName: string,
    value: unknown,
): CodingGitResult | null {
    switch (toolName) {
        case 'coding_git_status':
            return normalizeCodingGitStatusResult(value);
        case 'coding_git_diff':
            return normalizeCodingGitDiffResult(value);
        case 'coding_git_stage':
            return normalizeCodingGitStageResult(value);
        case 'coding_git_commit':
            return normalizeCodingGitCommitResult(value);
        default:
            return null;
    }
}

export function isCodingGitToolName(toolName: string): toolName is CodingGitToolName {
    return (
        toolName === 'coding_git_status'
        || toolName === 'coding_git_diff'
        || toolName === 'coding_git_stage'
        || toolName === 'coding_git_commit'
    );
}

export function isCodingGitResultData(value: unknown): value is CodingGitResultData {
    if (!isRecord(value) || value.kind !== CODING_GIT_KIND || typeof value.toolName !== 'string') {
        return false;
    }
    return isCodingGitToolName(value.toolName) && !!normalizeCodingGitResult(value.toolName, value.result);
}

export function getCodingGitFromResultData(value: ChatResultData | undefined): CodingGitResultData | null {
    if (!isCodingGitResultData(value)) return null;

    const result = normalizeCodingGitResult(value.toolName, value.result);
    if (!result) return null;

    return {
        kind: CODING_GIT_KIND,
        toolName: value.toolName,
        result,
        requestedPaths: Array.isArray(value.requestedPaths)
            ? value.requestedPaths.filter((path): path is string => typeof path === 'string')
            : undefined,
        commitMessage: typeof value.commitMessage === 'string' ? value.commitMessage : undefined,
    };
}

export function statusCounts(status: CodingGitStatusResult): string {
    return [
        `${status.staged.length} staged`,
        `${status.unstaged.length} unstaged`,
        `${status.untracked.length} untracked`,
        `${status.conflicted.length} conflicted`,
    ].join(', ');
}

function summarizeStatus(result: CodingGitStatusResult): string {
    const branch = result.branch || 'detached';
    const relation = result.upstream
        ? ` tracking \`${result.upstream}\`${result.ahead || result.behind ? ` (+${result.ahead}/-${result.behind})` : ''}`
        : '';
    const lines = [
        result.clean ? '**Git working tree clean**' : '**Git status**',
        `Branch: \`${branch}\`${relation}`,
        `Changes: ${statusCounts(result)}`,
    ];
    if (result.head) lines.push(`HEAD: \`${result.head}\``);
    lines.push('');
    lines.push('Review the git card below for file groups.');
    return lines.join('\n');
}

function summarizeDiff(result: CodingGitDiffResult): string {
    const scope = result.staged ? 'staged' : 'unstaged';
    const lines = [
        `**Git ${scope} diff**`,
        `${result.file_count} file(s), +${result.total_additions}/-${result.total_deletions}`,
    ];
    if (result.truncated) lines.push('Diff output was truncated.');
    lines.push('');
    lines.push('Review the git diff card below before staging or committing.');
    return lines.join('\n');
}

function summarizeStage(result: CodingGitStageResult): string {
    const action = result.dry_run ? 'Git stage dry run' : 'Git stage';
    const state = result.success ? (result.dry_run ? 'completed' : 'updated index') : 'failed';
    const lines = [
        `**${action} ${state}**`,
        `${result.paths.length} requested path(s)`,
        `Before: ${statusCounts(result.before)}`,
    ];
    if (result.after) lines.push(`After: ${statusCounts(result.after)}`);
    lines.push('');
    lines.push('Review the git stage card below for staged file details.');
    return lines.join('\n');
}

function summarizeCommit(result: CodingGitCommitResult): string {
    const action = result.dry_run ? 'Git commit dry run' : 'Git commit';
    const state = result.success
        ? result.committed ? 'created' : 'ready'
        : 'blocked';
    const lines = [
        `**${action} ${state}**`,
        result.commit_hash ? `Commit: \`${result.commit_hash.slice(0, 12)}\`` : result.message,
        `Before: ${statusCounts(result.before)}`,
    ];
    if (result.after) lines.push(`After: ${statusCounts(result.after)}`);
    lines.push('');
    lines.push('Review the git commit card below for staged inputs and output.');
    return lines.join('\n');
}

export function summarizeCodingGitResult(toolName: string, result: CodingGitResult): string {
    switch (toolName) {
        case 'coding_git_status':
            return summarizeStatus(result as CodingGitStatusResult);
        case 'coding_git_diff':
            return summarizeDiff(result as CodingGitDiffResult);
        case 'coding_git_stage':
            return summarizeStage(result as CodingGitStageResult);
        case 'coding_git_commit':
            return summarizeCommit(result as CodingGitCommitResult);
        default:
            return 'Git tool completed.';
    }
}
