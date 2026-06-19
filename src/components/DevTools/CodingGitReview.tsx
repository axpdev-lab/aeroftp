// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { CheckCircle2, FileDiff, GitBranch, GitCommit, GitPullRequest, ShieldAlert } from 'lucide-react';
import { statusCounts } from './aiChatCodingGit';
import type {
    CodingGitCommitResult,
    CodingGitDiffResult,
    CodingGitFileStatus,
    CodingGitResultData,
    CodingGitStageResult,
    CodingGitStatusResult,
} from './aiChatTypes';

interface CodingGitReviewProps {
    data: CodingGitResultData;
}

interface CodingGitApprovalPreviewProps {
    toolName: string;
    workspaceRoot?: string;
    paths?: string[];
    dryRun?: boolean;
    commitMessage?: string;
}

const chipClass = 'rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65';

const isStatusResult = (data: CodingGitResultData): data is CodingGitResultData & { result: CodingGitStatusResult } => (
    data.toolName === 'coding_git_status'
);

const isDiffResult = (data: CodingGitResultData): data is CodingGitResultData & { result: CodingGitDiffResult } => (
    data.toolName === 'coding_git_diff'
);

const isStageResult = (data: CodingGitResultData): data is CodingGitResultData & { result: CodingGitStageResult } => (
    data.toolName === 'coding_git_stage'
);

const isCommitResult = (data: CodingGitResultData): data is CodingGitResultData & { result: CodingGitCommitResult } => (
    data.toolName === 'coding_git_commit'
);

const FileGroup: React.FC<{ title: string; entries: CodingGitFileStatus[]; tone: string }> = ({ title, entries, tone }) => {
    if (entries.length === 0) return null;
    return (
        <div className="rounded-lg border border-white/10 bg-white/5 p-2">
            <div className={`mb-1 text-[10px] font-semibold uppercase tracking-wide ${tone}`}>
                {title} ({entries.length})
            </div>
            <div className="space-y-1">
                {entries.slice(0, 12).map(entry => (
                    <div key={`${title}-${entry.path}-${entry.index_status}-${entry.worktree_status}`} className="flex items-center gap-2">
                        <span className="w-7 shrink-0 rounded border border-white/10 bg-black/20 px-1 py-0.5 text-center font-mono text-[10px] text-white/55">
                            {entry.index_status}{entry.worktree_status}
                        </span>
                        <span className="min-w-0 truncate font-mono text-[11px] text-white/75" title={entry.path}>
                            {entry.path}
                        </span>
                    </div>
                ))}
                {entries.length > 12 && (
                    <div className="text-[10px] text-white/40">+{entries.length - 12} more</div>
                )}
            </div>
        </div>
    );
};

const StatusSummary: React.FC<{ status: CodingGitStatusResult }> = ({ status }) => (
    <>
        <div className="mt-3 grid gap-2 sm:grid-cols-4">
            <div className="rounded-lg border border-emerald-400/20 bg-emerald-400/10 p-2">
                <div className="text-[10px] uppercase tracking-wide text-emerald-200/70">Staged</div>
                <div className="mt-1 text-lg font-semibold text-emerald-100">{status.staged.length}</div>
            </div>
            <div className="rounded-lg border border-amber-400/20 bg-amber-400/10 p-2">
                <div className="text-[10px] uppercase tracking-wide text-amber-200/70">Unstaged</div>
                <div className="mt-1 text-lg font-semibold text-amber-100">{status.unstaged.length}</div>
            </div>
            <div className="rounded-lg border border-sky-400/20 bg-sky-400/10 p-2">
                <div className="text-[10px] uppercase tracking-wide text-sky-200/70">Untracked</div>
                <div className="mt-1 text-lg font-semibold text-sky-100">{status.untracked.length}</div>
            </div>
            <div className="rounded-lg border border-red-400/20 bg-red-400/10 p-2">
                <div className="text-[10px] uppercase tracking-wide text-red-200/70">Conflicted</div>
                <div className="mt-1 text-lg font-semibold text-red-100">{status.conflicted.length}</div>
            </div>
        </div>
        <div className="mt-3 grid gap-2">
            <FileGroup title="staged" entries={status.staged} tone="text-emerald-200/75" />
            <FileGroup title="unstaged" entries={status.unstaged} tone="text-amber-200/75" />
            <FileGroup title="untracked" entries={status.untracked} tone="text-sky-200/75" />
            <FileGroup title="conflicted" entries={status.conflicted} tone="text-red-200/75" />
        </div>
        {status.clean && (
            <div className="mt-3 rounded-lg border border-emerald-400/20 bg-emerald-400/10 p-2 text-emerald-100/80">
                Working tree is clean.
            </div>
        )}
    </>
);

const DiffSummary: React.FC<{ diff: CodingGitDiffResult }> = ({ diff }) => (
    <>
        <div className="mt-3 flex flex-wrap gap-1.5">
            <span className={chipClass}>{diff.file_count} file{diff.file_count === 1 ? '' : 's'}</span>
            <span className="rounded-full border border-emerald-400/20 bg-emerald-400/10 px-2 py-0.5 text-[10px] text-emerald-100">
                +{diff.total_additions}
            </span>
            <span className="rounded-full border border-red-400/20 bg-red-400/10 px-2 py-0.5 text-[10px] text-red-100">
                -{diff.total_deletions}
            </span>
            {diff.truncated && (
                <span className="rounded-full border border-amber-400/20 bg-amber-400/10 px-2 py-0.5 text-[10px] text-amber-100">
                    truncated
                </span>
            )}
        </div>
        {diff.stats.length > 0 && (
            <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2">
                <div className="space-y-1">
                    {diff.stats.slice(0, 12).map(stat => (
                        <div key={stat.path} className="flex items-center gap-2">
                            <span className="w-16 shrink-0 font-mono text-[10px] text-emerald-200/70">+{stat.additions}</span>
                            <span className="w-16 shrink-0 font-mono text-[10px] text-red-200/70">-{stat.deletions}</span>
                            <span className="min-w-0 truncate font-mono text-[11px] text-white/75" title={stat.path}>{stat.path}</span>
                            {stat.binary && <span className="text-[10px] text-white/40">binary</span>}
                        </div>
                    ))}
                    {diff.stats.length > 12 && <div className="text-[10px] text-white/40">+{diff.stats.length - 12} more</div>}
                </div>
            </div>
        )}
        {diff.diff ? (
            <pre className="mt-3 max-h-80 overflow-auto rounded-lg border border-white/10 bg-black/30 p-2 text-[11px] leading-relaxed text-white/75">
                {diff.diff}
            </pre>
        ) : (
            <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2 text-white/55">
                No {diff.staged ? 'staged' : 'unstaged'} diff output.
            </div>
        )}
    </>
);

export const CodingGitReview: React.FC<CodingGitReviewProps> = ({ data }) => {
    const result = data.result;
    const title = isStatusResult(data)
        ? 'Git Status'
        : isDiffResult(data)
            ? data.result.staged ? 'Git Staged Diff' : 'Git Unstaged Diff'
            : isStageResult(data)
                ? data.result.dry_run ? 'Git Stage Dry Run' : 'Git Stage Applied'
                : isCommitResult(data)
                    ? data.result.dry_run ? 'Git Commit Dry Run' : data.result.committed ? 'Git Commit Created' : 'Git Commit Review'
                    : 'Git Review';
    const Icon = isDiffResult(data)
        ? FileDiff
        : isStageResult(data)
            ? GitPullRequest
            : isCommitResult(data)
                ? GitCommit
                : GitBranch;
    const shellClass = isCommitResult(data) && !data.result.success
        ? 'border-red-400/35 bg-red-400/10'
        : isStageResult(data) && !data.result.success
            ? 'border-red-400/35 bg-red-400/10'
            : isDiffResult(data)
                ? 'border-sky-400/30 bg-sky-400/5'
                : 'border-emerald-400/30 bg-emerald-400/5';
    const iconClass = isCommitResult(data) && !data.result.success
        ? 'text-red-300'
        : isDiffResult(data)
            ? 'text-sky-300'
            : 'text-emerald-300';
    const status = isStatusResult(data)
        ? data.result
        : isStageResult(data) || isCommitResult(data)
            ? data.result.after ?? data.result.before
            : undefined;

    return (
        <div className={`mt-3 rounded-lg border p-3 text-xs ${shellClass}`}>
            <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
                    <div className={`flex items-center gap-2 text-sm font-semibold ${iconClass}`}>
                        <Icon size={14} />
                        <span>{title}</span>
                    </div>
                    <p className="mt-1 truncate font-mono text-[11px] text-white/45" title={result.workspace_root}>
                        {result.workspace_root}
                    </p>
                </div>
                <div className="flex flex-wrap items-center gap-1.5">
                    {status?.branch && <span className={chipClass}>{status.branch}</span>}
                    {status?.head && <span className={chipClass}>{status.head}</span>}
                    {isDiffResult(data) && <span className={chipClass}>{data.result.staged ? 'staged' : 'unstaged'}</span>}
                    {(isStageResult(data) || isCommitResult(data)) && data.result.dry_run && (
                        <span className="rounded-full border border-cyan-400/30 bg-cyan-400/10 px-2 py-0.5 text-[10px] text-cyan-100">
                            dry run
                        </span>
                    )}
                    {isCommitResult(data) && data.result.commit_hash && (
                        <span className={chipClass}>{data.result.commit_hash.slice(0, 12)}</span>
                    )}
                </div>
            </div>

            {isStatusResult(data) && <StatusSummary status={data.result} />}
            {isDiffResult(data) && <DiffSummary diff={data.result} />}
            {isStageResult(data) && (
                <>
                    <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2 text-white/70">
                        {data.result.message}
                    </div>
                    <div className="mt-3 flex flex-wrap gap-1.5">
                        {data.result.paths.map(path => (
                            <span key={path} className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[10px] text-white/70">
                                {path}
                            </span>
                        ))}
                    </div>
                    <div className="mt-3 text-[11px] text-white/55">Before: {statusCounts(data.result.before)}</div>
                    {data.result.after && <div className="mt-1 text-[11px] text-white/55">After: {statusCounts(data.result.after)}</div>}
                    <StatusSummary status={data.result.after ?? data.result.before} />
                </>
            )}
            {isCommitResult(data) && (
                <>
                    <div className={`mt-3 rounded-lg border p-2 ${data.result.success ? 'border-white/10 bg-white/5 text-white/70' : 'border-red-400/25 bg-red-400/10 text-red-50/85'}`}>
                        <div className="flex items-start gap-2">
                            {data.result.success ? <CheckCircle2 size={13} className="mt-0.5 shrink-0 text-emerald-300" /> : <ShieldAlert size={13} className="mt-0.5 shrink-0 text-red-300" />}
                            <span>{data.result.message}</span>
                        </div>
                    </div>
                    <div className="mt-3 text-[11px] text-white/55">Before: {statusCounts(data.result.before)}</div>
                    {data.result.after && <div className="mt-1 text-[11px] text-white/55">After: {statusCounts(data.result.after)}</div>}
                    {data.result.stdout.trim() && (
                        <pre className="mt-3 max-h-40 overflow-auto rounded-lg border border-white/10 bg-black/30 p-2 text-[11px] text-white/70">
                            {data.result.stdout.trim()}
                        </pre>
                    )}
                    {data.result.stderr.trim() && (
                        <pre className="mt-3 max-h-40 overflow-auto rounded-lg border border-red-400/20 bg-red-400/10 p-2 text-[11px] text-red-100/80">
                            {data.result.stderr.trim()}
                        </pre>
                    )}
                    <StatusSummary status={data.result.after ?? data.result.before} />
                </>
            )}
        </div>
    );
};

export const CodingGitApprovalPreview: React.FC<CodingGitApprovalPreviewProps> = ({
    toolName,
    workspaceRoot,
    paths,
    dryRun,
    commitMessage,
}) => {
    if (toolName !== 'coding_git_stage' && toolName !== 'coding_git_commit') return null;
    const isCommit = toolName === 'coding_git_commit';
    const Icon = isCommit ? GitCommit : GitPullRequest;
    return (
        <div className="mt-3 rounded-lg border border-red-400/30 bg-red-400/10 p-3 text-xs">
            <div className="flex items-center gap-2 text-sm font-semibold text-red-200">
                <Icon size={14} />
                <span>{isCommit ? 'Git Commit Approval' : 'Git Stage Approval'}</span>
            </div>
            {workspaceRoot && (
                <p className="mt-1 truncate font-mono text-[11px] text-white/45" title={workspaceRoot}>
                    {workspaceRoot}
                </p>
            )}
            <div className="mt-2 rounded-lg border border-red-400/25 bg-red-400/10 p-2 text-red-50/85">
                {isCommit
                    ? 'This creates a git commit from the current staged index. Review coding_git_status and coding_git_diff with staged=true first.'
                    : dryRun
                        ? 'This previews which requested paths would be staged; the index will not change.'
                        : 'This changes the git index for the requested paths.'}
            </div>
            {isCommit && commitMessage && (
                <div className="mt-2 rounded-lg border border-white/10 bg-white/5 p-2">
                    <div className="mb-1 text-[10px] uppercase tracking-wide text-white/40">Commit message</div>
                    <div className="font-mono text-[11px] text-white/75">{commitMessage}</div>
                </div>
            )}
            {!isCommit && paths && paths.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1.5">
                    {paths.slice(0, 12).map(path => (
                        <span key={path} className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[10px] text-white/70">
                            {path}
                        </span>
                    ))}
                    {paths.length > 12 && (
                        <span className="rounded border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/50">
                            +{paths.length - 12} more
                        </span>
                    )}
                </div>
            )}
        </div>
    );
};

export default CodingGitReview;
