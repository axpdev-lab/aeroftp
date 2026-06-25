// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingGitDiffStat,
    CodingGitHistoryResult,
    CodingGitHistoryResultData,
    CodingGitLogEntry,
    CodingGitLogResult,
    CodingGitShowResult,
} from './aiChatTypes';

export type CodingGitHistoryToolName = 'coding_git_log' | 'coding_git_show';

const CODING_GIT_HISTORY_KIND = 'coding_git_history';

const isRecord = (value: unknown): value is Record<string, unknown> => (
    !!value && typeof value === 'object' && !Array.isArray(value)
);

const finiteNumber = (value: unknown): number => (
    typeof value === 'number' && Number.isFinite(value) ? value : 0
);

const stringArray = (value: unknown): string[] => (
    Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
);

const normalizeLogEntries = (value: unknown): CodingGitLogEntry[] | null => {
    if (!Array.isArray(value)) return null;
    const entries: CodingGitLogEntry[] = [];
    for (const item of value) {
        if (
            !isRecord(item)
            || typeof item.hash !== 'string'
            || typeof item.short_hash !== 'string'
            || typeof item.author !== 'string'
            || typeof item.date !== 'string'
            || typeof item.subject !== 'string'
        ) {
            return null;
        }
        entries.push({
            hash: item.hash,
            short_hash: item.short_hash,
            author: item.author,
            date: item.date,
            subject: item.subject,
        });
    }
    return entries;
};

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

export function normalizeCodingGitLogResult(value: unknown): CodingGitLogResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.truncated !== 'boolean'
    ) {
        return null;
    }
    const commits = normalizeLogEntries(value.commits);
    if (!commits) return null;
    return {
        workspace_root: value.workspace_root,
        repo_root: value.repo_root,
        paths: stringArray(value.paths),
        max_count: finiteNumber(value.max_count),
        commits,
        truncated: value.truncated,
    };
}

export function normalizeCodingGitShowResult(value: unknown): CodingGitShowResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.repo_root !== 'string'
        || typeof value.commit !== 'string'
        || typeof value.hash !== 'string'
        || typeof value.short_hash !== 'string'
        || typeof value.author !== 'string'
        || typeof value.date !== 'string'
        || typeof value.subject !== 'string'
        || typeof value.body !== 'string'
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
        commit: value.commit,
        hash: value.hash,
        short_hash: value.short_hash,
        author: value.author,
        date: value.date,
        subject: value.subject,
        body: value.body,
        stats,
        total_additions: finiteNumber(value.total_additions),
        total_deletions: finiteNumber(value.total_deletions),
        diff: value.diff,
        truncated: value.truncated,
    };
}

export function normalizeCodingGitHistoryResult(
    toolName: string,
    value: unknown,
): CodingGitHistoryResult | null {
    switch (toolName) {
        case 'coding_git_log':
            return normalizeCodingGitLogResult(value);
        case 'coding_git_show':
            return normalizeCodingGitShowResult(value);
        default:
            return null;
    }
}

export function isCodingGitHistoryToolName(toolName: string): toolName is CodingGitHistoryToolName {
    return toolName === 'coding_git_log' || toolName === 'coding_git_show';
}

export function getCodingGitHistoryFromResultData(
    value: ChatResultData | undefined,
): CodingGitHistoryResultData | null {
    if (
        !value
        || typeof value !== 'object'
        || (value as { kind?: string }).kind !== CODING_GIT_HISTORY_KIND
    ) {
        return null;
    }
    const data = value as CodingGitHistoryResultData;
    if (!isCodingGitHistoryToolName(data.toolName)) return null;
    const result = normalizeCodingGitHistoryResult(data.toolName, data.result);
    if (!result) return null;
    return { kind: CODING_GIT_HISTORY_KIND, toolName: data.toolName, result };
}

export function summarizeCodingGitHistoryResult(toolName: string, result: CodingGitHistoryResult): string {
    if (toolName === 'coding_git_log') {
        const log = result as CodingGitLogResult;
        const lines = [
            `**Git log** (${log.commits.length} commit(s))`,
        ];
        if (log.truncated) lines.push(`Showing newest ${log.max_count}; more exist.`);
        lines.push('');
        lines.push('Review the git log card below.');
        return lines.join('\n');
    }
    const show = result as CodingGitShowResult;
    const lines = [
        `**Git commit \`${show.short_hash}\`**`,
        show.subject,
        `${show.stats.length} file(s), +${show.total_additions}/-${show.total_deletions}`,
    ];
    if (show.truncated) lines.push('Diff output was truncated.');
    lines.push('');
    lines.push('Review the git commit card below.');
    return lines.join('\n');
}
