// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useMemo, useState } from 'react';
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronRight, FileCode2, FileDiff, RotateCcw, ShieldAlert, XCircle } from 'lucide-react';
import type { CodingPatchResult } from './aiChatTypes';

interface CodingPatchReviewProps {
    result?: CodingPatchResult | null;
    patchText?: string;
    workspaceRoot?: string;
    dryRun?: boolean;
    mode?: 'approval' | 'result';
}

type PatchTextSummary = {
    files: string[];
    hunks: number;
    additions: number;
    deletions: number;
};

const MAX_DIFF_LINES = 600;

const formatBytes = (bytes: number): string => {
    if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

const statusClasses = (status: string): string => {
    switch (status) {
        case 'created':
            return 'border-emerald-400/30 bg-emerald-400/10 text-emerald-200';
        case 'deleted':
            return 'border-red-400/30 bg-red-400/10 text-red-200';
        default:
            return 'border-sky-400/30 bg-sky-400/10 text-sky-200';
    }
};

const summarizePatchText = (patchText?: string): PatchTextSummary => {
    if (!patchText) return { files: [], hunks: 0, additions: 0, deletions: 0 };

    const files = new Set<string>();
    let hunks = 0;
    let additions = 0;
    let deletions = 0;

    for (const line of patchText.split(/\r?\n/)) {
        if (line.startsWith('+++ ')) {
            const raw = line.slice(4).trim();
            if (raw && raw !== '/dev/null') files.add(raw.replace(/^b\//, ''));
        } else if (line.startsWith('--- ')) {
            const raw = line.slice(4).trim();
            if (raw && raw !== '/dev/null') files.add(raw.replace(/^a\//, ''));
        } else if (line.startsWith('@@')) {
            hunks += 1;
        } else if (line.startsWith('+')) {
            additions += 1;
        } else if (line.startsWith('-')) {
            deletions += 1;
        }
    }

    return { files: Array.from(files), hunks, additions, deletions };
};

const diffLineClass = (line: string): string => {
    if (line.startsWith('@@')) return 'bg-cyan-500/10 text-cyan-200';
    if (line.startsWith('+++') || line.startsWith('---')) return 'bg-white/5 text-white/70';
    if (line.startsWith('+')) return 'bg-emerald-500/10 text-emerald-200';
    if (line.startsWith('-')) return 'bg-red-500/10 text-red-200';
    if (line.startsWith('diff ') || line.startsWith('index ')) return 'text-purple-200';
    return 'text-white/50';
};

export const CodingPatchReview: React.FC<CodingPatchReviewProps> = ({
    result,
    patchText,
    workspaceRoot,
    dryRun,
    mode = 'result',
}) => {
    const patchSummary = useMemo(() => summarizePatchText(patchText), [patchText]);
    const diffLines = useMemo(() => patchText ? patchText.split(/\r?\n/) : [], [patchText]);
    const [showDiff, setShowDiff] = useState(mode === 'approval' && !!patchText);

    const effectiveDryRun = result?.dry_run ?? !!dryRun;
    const resultFiles = result?.files ?? [];
    const fileCount = result ? resultFiles.length : patchSummary.files.length;
    const additions = result
        ? resultFiles.reduce((sum, file) => sum + file.additions, 0)
        : patchSummary.additions;
    const deletions = result
        ? resultFiles.reduce((sum, file) => sum + file.deletions, 0)
        : patchSummary.deletions;
    const hunks = result
        ? resultFiles.reduce((sum, file) => sum + file.hunks, 0)
        : patchSummary.hunks;

    if (!result && !patchText) return null;

    const isSuccess = result?.success;
    const isFailure = result && !result.success;
    const title = !result
        ? effectiveDryRun ? 'Patch Dry Run Review' : 'Patch Apply Review'
        : isSuccess
            ? effectiveDryRun ? 'Patch Dry Run Passed' : 'Patch Applied'
            : effectiveDryRun ? 'Patch Dry Run Found Conflicts' : 'Patch Apply Blocked';
    const Icon = isFailure ? XCircle : isSuccess ? CheckCircle2 : effectiveDryRun ? FileDiff : ShieldAlert;
    const shellClass = isFailure
        ? 'border-red-400/30 bg-red-400/5'
        : isSuccess
            ? 'border-emerald-400/30 bg-emerald-400/5'
            : effectiveDryRun
                ? 'border-cyan-400/30 bg-cyan-400/5'
                : 'border-amber-400/30 bg-amber-400/5';
    const iconClass = isFailure
        ? 'text-red-300'
        : isSuccess
            ? 'text-emerald-300'
            : effectiveDryRun
                ? 'text-cyan-300'
                : 'text-amber-300';

    return (
        <div className={`mt-3 rounded-lg border p-3 text-xs ${shellClass}`}>
            <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
                    <div className={`flex items-center gap-2 text-sm font-semibold ${iconClass}`}>
                        <Icon size={14} />
                        <span>{title}</span>
                    </div>
                    {workspaceRoot && (
                        <p className="mt-1 truncate font-mono text-[11px] text-white/50" title={workspaceRoot}>
                            {workspaceRoot}
                        </p>
                    )}
                </div>
                <div className="flex flex-wrap items-center gap-1.5">
                    <span className={`rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide ${
                        effectiveDryRun
                            ? 'border-cyan-400/30 bg-cyan-400/10 text-cyan-200'
                            : 'border-amber-400/30 bg-amber-400/10 text-amber-200'
                    }`}>
                        {effectiveDryRun ? 'dry run' : 'apply'}
                    </span>
                    <span className="rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65">
                        {fileCount} file{fileCount === 1 ? '' : 's'}
                    </span>
                    <span className="rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65">
                        {hunks} hunk{hunks === 1 ? '' : 's'}
                    </span>
                    <span className="rounded-full border border-emerald-400/20 bg-emerald-400/10 px-2 py-0.5 text-[10px] text-emerald-200">
                        +{additions}
                    </span>
                    <span className="rounded-full border border-red-400/20 bg-red-400/10 px-2 py-0.5 text-[10px] text-red-200">
                        -{deletions}
                    </span>
                </div>
            </div>

            {!result && (
                <div className={`mt-3 rounded-lg border p-2 ${effectiveDryRun ? 'border-cyan-400/20 bg-cyan-400/5 text-cyan-100/75' : 'border-amber-400/25 bg-amber-400/10 text-amber-100/80'}`}>
                    <div className="flex items-start gap-2">
                        {effectiveDryRun ? <FileDiff size={13} className="mt-0.5 shrink-0 text-cyan-300" /> : <ShieldAlert size={13} className="mt-0.5 shrink-0 text-amber-300" />}
                        <span>
                            {effectiveDryRun
                                ? 'This validates the patch without changing files.'
                                : 'This high-danger apply request writes files after the backend creates a pre-apply checkpoint.'}
                        </span>
                    </div>
                </div>
            )}

            {result?.checkpoint_id && (
                <div className="mt-3 rounded-lg border border-emerald-400/25 bg-emerald-400/10 p-2 text-emerald-50/80">
                    <div className="flex items-start gap-2">
                        <RotateCcw size={13} className="mt-0.5 shrink-0 text-emerald-300" />
                        <div className="min-w-0">
                            <div className="font-medium text-emerald-200">Pre-apply checkpoint created</div>
                            <div className="mt-1 break-all font-mono text-[11px] text-emerald-50/75">
                                {result.checkpoint_id}
                            </div>
                            <div className="mt-1 text-[11px] text-emerald-50/60">
                                Restore is manual: use coding_checkpoint_restore with this checkpoint id after review.
                            </div>
                        </div>
                    </div>
                </div>
            )}

            {resultFiles.length > 0 && (
                <div className="mt-3 space-y-2">
                    {resultFiles.map(file => (
                        <div key={`${file.path}-${file.status}`} className="rounded-lg border border-white/10 bg-white/5 p-2">
                            <div className="flex flex-wrap items-center gap-2">
                                <FileCode2 size={12} className="text-white/45" />
                                <span className="min-w-0 flex-1 truncate font-mono text-white/80" title={file.path}>
                                    {file.path}
                                </span>
                                <span className={`rounded-full border px-2 py-0.5 text-[10px] uppercase tracking-wide ${statusClasses(file.status)}`}>
                                    {file.status}
                                </span>
                            </div>
                            <div className="mt-1 flex flex-wrap gap-2 text-[10px] text-white/50">
                                <span>{file.hunks} hunk{file.hunks === 1 ? '' : 's'}</span>
                                <span className="text-emerald-300">+{file.additions}</span>
                                <span className="text-red-300">-{file.deletions}</span>
                                <span>{formatBytes(file.old_size_bytes)} -&gt; {formatBytes(file.new_size_bytes)}</span>
                            </div>
                        </div>
                    ))}
                </div>
            )}

            {!result && patchSummary.files.length > 0 && (
                <div className="mt-3 flex flex-wrap gap-1.5">
                    {patchSummary.files.slice(0, 12).map(file => (
                        <span key={file} className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[10px] text-white/70">
                            {file}
                        </span>
                    ))}
                    {patchSummary.files.length > 12 && (
                        <span className="rounded border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/50">
                            +{patchSummary.files.length - 12} more
                        </span>
                    )}
                </div>
            )}

            {result && result.diagnostics.length > 0 && (
                <div className="mt-3 rounded-lg border border-red-400/25 bg-red-400/10 p-2">
                    <div className="mb-2 flex items-center gap-1 font-medium text-red-200">
                        <AlertTriangle size={12} />
                        <span>Diagnostics</span>
                    </div>
                    <div className="space-y-2">
                        {result.diagnostics.map((diagnostic, index) => (
                            <div key={`${diagnostic.path || 'patch'}-${index}`} className="rounded border border-red-300/10 bg-black/10 p-2">
                                <div className="text-red-50/80">
                                    <span className="font-medium">{diagnostic.message}</span>
                                    {diagnostic.path && (
                                        <span className="ml-1 font-mono text-red-50/60">{diagnostic.path}</span>
                                    )}
                                    {diagnostic.hunk_index != null && (
                                        <span className="ml-1 text-red-50/50">hunk {diagnostic.hunk_index + 1}</span>
                                    )}
                                </div>
                                {(diagnostic.expected != null || diagnostic.actual != null) && (
                                    <div className="mt-2 grid gap-1 font-mono text-[10px]">
                                        {diagnostic.expected != null && (
                                            <div className="rounded bg-red-950/30 px-2 py-1 text-red-100/75">
                                                expected: {diagnostic.expected}
                                            </div>
                                        )}
                                        <div className="rounded bg-white/5 px-2 py-1 text-white/60">
                                            actual: {diagnostic.actual ?? 'EOF'}
                                        </div>
                                    </div>
                                )}
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {result && result.warnings.length > 0 && (
                <div className="mt-3 rounded-lg border border-amber-400/25 bg-amber-400/10 p-2 text-amber-50/80">
                    <div className="mb-1 flex items-center gap-1 font-medium text-amber-200">
                        <AlertTriangle size={12} />
                        <span>Warnings</span>
                    </div>
                    <ul className="space-y-1">
                        {result.warnings.map((warning, index) => (
                            <li key={`${warning}-${index}`}>{warning}</li>
                        ))}
                    </ul>
                </div>
            )}

            {patchText && (
                <div className="mt-3 rounded-lg border border-white/10 bg-black/15">
                    <button
                        onClick={() => setShowDiff(prev => !prev)}
                        className="flex w-full items-center gap-2 px-3 py-2 text-left text-[11px] font-medium text-white/70 hover:text-white"
                        aria-expanded={showDiff}
                    >
                        {showDiff ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                        <FileDiff size={12} className="text-purple-300" />
                        <span>{showDiff ? 'Hide unified diff' : 'Show unified diff'}</span>
                        {diffLines.length > MAX_DIFF_LINES && (
                            <span className="ml-auto text-[10px] text-white/40">
                                first {MAX_DIFF_LINES} of {diffLines.length} lines
                            </span>
                        )}
                    </button>
                    {showDiff && (
                        <div className="max-h-72 overflow-auto border-t border-white/10 font-mono text-[11px] leading-relaxed">
                            {diffLines.slice(0, MAX_DIFF_LINES).map((line, index) => (
                                <div key={`${index}-${line.slice(0, 24)}`} className={`flex min-w-max px-2 ${diffLineClass(line)}`}>
                                    <span className="mr-3 w-10 shrink-0 select-none text-right text-white/25">{index + 1}</span>
                                    <span className="whitespace-pre">{line || ' '}</span>
                                </div>
                            ))}
                            {diffLines.length > MAX_DIFF_LINES && (
                                <div className="px-3 py-2 text-[10px] text-white/45">
                                    Diff preview truncated for display.
                                </div>
                            )}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

export default CodingPatchReview;
