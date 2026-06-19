// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useMemo } from 'react';
import { CheckCircle2, FileCode2, RotateCcw, ShieldAlert } from 'lucide-react';
import type { CodingCheckpointRestoreResult } from './aiChatTypes';

interface CodingCheckpointRestoreReviewProps {
    result?: CodingCheckpointRestoreResult | null;
    checkpointId?: string;
    paths?: string[];
    dryRun?: boolean;
    mode?: 'approval' | 'result';
}

const formatBytes = (bytes: number): string => {
    if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

const countActions = (result?: CodingCheckpointRestoreResult | null): Record<string, number> => {
    if (!result) return {};
    return result.files.reduce<Record<string, number>>((counts, file) => {
        counts[file.action] = (counts[file.action] ?? 0) + 1;
        return counts;
    }, {});
};

const actionClasses = (action: string): string => {
    switch (action) {
        case 'restore':
            return 'border-sky-400/30 bg-sky-400/10 text-sky-200';
        case 'delete':
            return 'border-red-400/30 bg-red-400/10 text-red-200';
        case 'noop':
            return 'border-white/15 bg-white/5 text-white/60';
        default:
            return 'border-purple-400/30 bg-purple-400/10 text-purple-200';
    }
};

const actionLabel = (action: string, dryRun: boolean): string => {
    switch (action) {
        case 'restore':
            return dryRun ? 'would rewrite' : 'rewritten';
        case 'delete':
            return dryRun ? 'would delete' : 'deleted';
        case 'noop':
            return dryRun ? 'would skip' : 'skipped';
        default:
            return action;
    }
};

export const CodingCheckpointRestoreReview: React.FC<CodingCheckpointRestoreReviewProps> = ({
    result,
    checkpointId,
    paths,
    dryRun,
    mode = 'result',
}) => {
    const actionCounts = useMemo(() => countActions(result), [result]);
    const selectedPaths = useMemo(
        () => (Array.isArray(paths) ? paths.filter(path => typeof path === 'string' && path.trim().length > 0) : []),
        [paths],
    );
    const effectiveDryRun = result?.dry_run ?? !!dryRun;
    const effectiveCheckpointId = result?.checkpoint_id ?? checkpointId;
    const restoreCount = actionCounts.restore ?? 0;
    const deleteCount = actionCounts.delete ?? 0;
    const noopCount = actionCounts.noop ?? 0;
    const otherCount = result ? Math.max(0, result.files.length - restoreCount - deleteCount - noopCount) : 0;

    if (!result && !effectiveCheckpointId) return null;

    const title = !result
        ? effectiveDryRun ? 'Checkpoint Restore Dry Run Review' : 'Checkpoint Restore Apply Review'
        : effectiveDryRun ? 'Checkpoint Restore Dry Run Complete' : 'Checkpoint Restore Applied';
    const Icon = result ? CheckCircle2 : effectiveDryRun ? RotateCcw : ShieldAlert;
    const shellClass = result
        ? effectiveDryRun
            ? 'border-cyan-400/30 bg-cyan-400/5'
            : 'border-emerald-400/30 bg-emerald-400/5'
        : effectiveDryRun
            ? 'border-cyan-400/30 bg-cyan-400/5'
            : 'border-red-400/35 bg-red-400/10';
    const iconClass = result
        ? effectiveDryRun
            ? 'text-cyan-300'
            : 'text-emerald-300'
        : effectiveDryRun
            ? 'text-cyan-300'
            : 'text-red-300';

    return (
        <div className={`mt-3 rounded-lg border p-3 text-xs ${shellClass}`}>
            <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
                    <div className={`flex items-center gap-2 text-sm font-semibold ${iconClass}`}>
                        <Icon size={14} />
                        <span>{title}</span>
                    </div>
                    {effectiveCheckpointId && (
                        <p className="mt-1 break-all font-mono text-[11px] text-white/55">
                            {effectiveCheckpointId}
                        </p>
                    )}
                    {result?.workspace_root && (
                        <p className="mt-1 truncate font-mono text-[11px] text-white/45" title={result.workspace_root}>
                            {result.workspace_root}
                        </p>
                    )}
                </div>
                <div className="flex flex-wrap items-center gap-1.5">
                    <span className={`rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide ${
                        effectiveDryRun
                            ? 'border-cyan-400/30 bg-cyan-400/10 text-cyan-200'
                            : 'border-red-400/30 bg-red-400/10 text-red-200'
                    }`}>
                        {effectiveDryRun ? 'dry run' : 'apply'}
                    </span>
                    {result && (
                        <span className="rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65">
                            {result.files.length} file{result.files.length === 1 ? '' : 's'}
                        </span>
                    )}
                </div>
            </div>

            {!result && (
                <div className={`mt-3 rounded-lg border p-2 ${
                    effectiveDryRun
                        ? 'border-cyan-400/20 bg-cyan-400/5 text-cyan-100/75'
                        : 'border-red-400/25 bg-red-400/10 text-red-50/85'
                }`}>
                    <div className="flex items-start gap-2">
                        {effectiveDryRun
                            ? <RotateCcw size={13} className="mt-0.5 shrink-0 text-cyan-300" />
                            : <ShieldAlert size={13} className="mt-0.5 shrink-0 text-red-300" />}
                        <span>
                            {effectiveDryRun
                                ? 'This previews checkpoint restore actions without changing files.'
                                : 'This high-danger restore can overwrite current file contents and delete files that did not exist at the checkpoint.'}
                        </span>
                    </div>
                </div>
            )}

            {result && (
                <div className="mt-3 grid gap-2 sm:grid-cols-3">
                    <div className="rounded-lg border border-sky-400/20 bg-sky-400/10 p-2">
                        <div className="text-[10px] uppercase tracking-wide text-sky-200/70">
                            {effectiveDryRun ? 'Would rewrite' : 'Rewritten'}
                        </div>
                        <div className="mt-1 text-lg font-semibold text-sky-100">{restoreCount}</div>
                    </div>
                    <div className="rounded-lg border border-red-400/20 bg-red-400/10 p-2">
                        <div className="text-[10px] uppercase tracking-wide text-red-200/70">
                            {effectiveDryRun ? 'Would delete' : 'Deleted'}
                        </div>
                        <div className="mt-1 text-lg font-semibold text-red-100">{deleteCount}</div>
                    </div>
                    <div className="rounded-lg border border-white/10 bg-white/5 p-2">
                        <div className="text-[10px] uppercase tracking-wide text-white/45">
                            {effectiveDryRun ? 'Would skip' : 'Skipped'}
                        </div>
                        <div className="mt-1 text-lg font-semibold text-white/75">{noopCount}</div>
                    </div>
                    {otherCount > 0 && (
                        <div className="rounded-lg border border-purple-400/20 bg-purple-400/10 p-2 sm:col-span-3">
                            <div className="text-[10px] uppercase tracking-wide text-purple-200/70">Other actions</div>
                            <div className="mt-1 text-lg font-semibold text-purple-100">{otherCount}</div>
                        </div>
                    )}
                </div>
            )}

            {selectedPaths.length > 0 ? (
                <div className="mt-3">
                    <div className="mb-1 text-[10px] uppercase tracking-wide text-white/40">
                        Requested path subset
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                        {selectedPaths.slice(0, 12).map(path => (
                            <span key={path} className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[10px] text-white/70">
                                {path}
                            </span>
                        ))}
                        {selectedPaths.length > 12 && (
                            <span className="rounded border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/50">
                                +{selectedPaths.length - 12} more
                            </span>
                        )}
                    </div>
                </div>
            ) : !result && (
                <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2 text-white/60">
                    Scope: all files captured in the checkpoint.
                </div>
            )}

            {result && result.files.length > 0 && (
                <div className="mt-3 space-y-2">
                    {result.files.map(file => (
                        <div key={`${file.path}-${file.action}`} className="rounded-lg border border-white/10 bg-white/5 p-2">
                            <div className="flex flex-wrap items-center gap-2">
                                <FileCode2 size={12} className="text-white/45" />
                                <span className="min-w-0 flex-1 truncate font-mono text-white/80" title={file.path}>
                                    {file.path}
                                </span>
                                <span className={`rounded-full border px-2 py-0.5 text-[10px] uppercase tracking-wide ${actionClasses(file.action)}`}>
                                    {actionLabel(file.action, effectiveDryRun)}
                                </span>
                            </div>
                            <div className="mt-1 flex flex-wrap gap-2 text-[10px] text-white/50">
                                <span>{file.existed_at_checkpoint ? 'existed at checkpoint' : 'absent at checkpoint'}</span>
                                <span>{formatBytes(file.size_bytes)}</span>
                                {file.sha256 && (
                                    <span className="font-mono" title={file.sha256}>
                                        sha256 {file.sha256.slice(0, 16)}
                                    </span>
                                )}
                            </div>
                        </div>
                    ))}
                </div>
            )}

            {result && result.files.length === 0 && (
                <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2 text-white/60">
                    No files were included in this restore result.
                </div>
            )}
        </div>
    );
};

export default CodingCheckpointRestoreReview;
