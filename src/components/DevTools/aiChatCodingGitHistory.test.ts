// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    getCodingGitHistoryFromResultData,
    normalizeCodingGitLogResult,
    normalizeCodingGitShowResult,
    summarizeCodingGitHistoryResult,
} from './aiChatCodingGitHistory';

const logResult = {
    workspace_root: '/repo',
    repo_root: '/repo',
    paths: [],
    max_count: 20,
    commits: [
        { hash: 'abc123', short_hash: 'abc', author: 'Jane', date: '2026-06-25T10:00:00+00:00', subject: 'first' },
        { hash: 'def456', short_hash: 'def', author: 'John', date: '2026-06-24T10:00:00+00:00', subject: 'second' },
    ],
    truncated: false,
};

const showResult = {
    workspace_root: '/repo',
    repo_root: '/repo',
    commit: 'HEAD',
    hash: 'abc123',
    short_hash: 'abc',
    author: 'Jane',
    date: '2026-06-25T10:00:00+00:00',
    subject: 'add alpha',
    body: 'longer body',
    stats: [{ path: 'a.txt', additions: 3, deletions: 1, binary: false }],
    total_additions: 3,
    total_deletions: 1,
    diff: '@@ -0,0 +1 @@\n+alpha',
    truncated: false,
};

describe('aiChatCodingGitHistory', () => {
    it('normalizes a git log result', () => {
        const result = normalizeCodingGitLogResult(logResult);
        expect(result?.commits).toHaveLength(2);
        expect(result?.commits[0].subject).toBe('first');
        expect(result?.truncated).toBe(false);
    });

    it('normalizes a git show result with stats and diff', () => {
        const result = normalizeCodingGitShowResult(showResult);
        expect(result?.subject).toBe('add alpha');
        expect(result?.stats[0].path).toBe('a.txt');
        expect(result?.total_additions).toBe(3);
        expect(result?.diff).toContain('alpha');
    });

    it('rejects malformed input', () => {
        expect(normalizeCodingGitLogResult({ workspace_root: '/repo' })).toBeNull();
        expect(normalizeCodingGitShowResult(null)).toBeNull();
    });

    it('extracts result data by tool name', () => {
        const data = { kind: 'coding_git_history', toolName: 'coding_git_log', result: logResult };
        expect(getCodingGitHistoryFromResultData(data as never)?.toolName).toBe('coding_git_log');
        const wrong = { kind: 'coding_git', toolName: 'coding_git_log', result: logResult };
        expect(getCodingGitHistoryFromResultData(wrong as never)).toBeNull();
    });

    it('summarizes log and show results', () => {
        const log = normalizeCodingGitLogResult(logResult)!;
        const show = normalizeCodingGitShowResult(showResult)!;
        expect(summarizeCodingGitHistoryResult('coding_git_log', log)).toContain('Git log');
        expect(summarizeCodingGitHistoryResult('coding_git_show', show)).toContain('abc');
    });
});
