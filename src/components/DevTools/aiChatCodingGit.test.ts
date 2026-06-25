// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    normalizeCodingGitCommitResult,
    normalizeCodingGitDiffResult,
    normalizeCodingGitResult,
    normalizeCodingGitStageResult,
    normalizeCodingGitStatusResult,
    summarizeCodingGitResult,
} from './aiChatCodingGit';

const statusResult = {
    workspace_root: '/repo',
    repo_root: '/repo',
    branch: 'main',
    head: 'abc1234',
    upstream: 'origin/main',
    ahead: 1,
    behind: 0,
    clean: false,
    staged: [{ path: 'src/a.ts', index_status: 'M', worktree_status: ' ' }],
    unstaged: [{ path: 'src/b.ts', index_status: ' ', worktree_status: 'M' }],
    untracked: [{ path: 'notes.md', index_status: '?', worktree_status: '?' }],
    conflicted: [],
    total: 3,
    truncated: false,
    raw: ['M  src/a.ts'],
};

describe('aiChatCodingGit', () => {
    it('normalizes git status results', () => {
        const result = normalizeCodingGitStatusResult(statusResult);

        expect(result?.branch).toBe('main');
        expect(result?.staged).toHaveLength(1);
        expect(result?.unstaged[0].path).toBe('src/b.ts');
    });

    it('normalizes git diff results', () => {
        const result = normalizeCodingGitDiffResult({
            workspace_root: '/repo',
            repo_root: '/repo',
            staged: true,
            paths: ['src/a.ts'],
            file_count: 1,
            total_additions: 2,
            total_deletions: 1,
            stats: [{ path: 'src/a.ts', additions: 2, deletions: 1, binary: false }],
            diff: 'diff --git a/src/a.ts b/src/a.ts',
            truncated: false,
        });

        expect(result?.staged).toBe(true);
        expect(result?.stats[0].additions).toBe(2);
    });

    it('normalizes stage and commit results', () => {
        const stage = normalizeCodingGitStageResult({
            success: true,
            workspace_root: '/repo',
            repo_root: '/repo',
            dry_run: false,
            staged: true,
            paths: ['src/a.ts'],
            before: statusResult,
            after: { ...statusResult, unstaged: [] },
            message: 'Git index updated for the requested path(s).',
        });
        const commit = normalizeCodingGitCommitResult({
            success: true,
            workspace_root: '/repo',
            repo_root: '/repo',
            dry_run: false,
            committed: true,
            commit_hash: 'abcdef123456',
            message: 'add git tools',
            stdout: '[main abcdef1] add git tools',
            stderr: '',
            before: statusResult,
            after: { ...statusResult, clean: true, staged: [], unstaged: [], untracked: [] },
        });

        expect(stage?.staged).toBe(true);
        expect(commit?.commit_hash).toBe('abcdef123456');
    });

    it('summarizes status and commit results', () => {
        const status = normalizeCodingGitResult('coding_git_status', statusResult);
        const commit = normalizeCodingGitResult('coding_git_commit', {
            success: false,
            workspace_root: '/repo',
            repo_root: '/repo',
            dry_run: false,
            committed: false,
            commit_hash: null,
            message: 'No staged changes to commit. Use coding_git_stage first.',
            stdout: '',
            stderr: '',
            before: { ...statusResult, staged: [] },
            after: null,
        });

        expect(status && summarizeCodingGitResult('coding_git_status', status)).toContain('Git status');
        expect(commit && summarizeCodingGitResult('coding_git_commit', commit)).toContain('blocked');
    });
});
