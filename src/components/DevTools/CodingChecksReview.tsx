// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { CheckCircle2, ListChecks, PlayCircle, TerminalSquare, TimerReset, XCircle } from 'lucide-react';
import { commandLine } from './aiChatCodingChecks';
import type { CodingRunCheckResult, CodingRunCheckResultData, CodingVerifyResultData } from './aiChatTypes';

interface CodingChecksReviewProps {
    data: CodingRunCheckResultData;
}

interface CodingVerifyReviewProps {
    data: CodingVerifyResultData;
}

interface CodingChecksApprovalPreviewProps {
    workspaceRoot?: string;
    check?: string;
    filter?: string;
    timeoutSecs?: number;
}

const chipClass = 'rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65';

const OutputBlock: React.FC<{ title: string; body: string; truncated: boolean }> = ({ title, body, truncated }) => {
    if (!body.trim()) return null;
    return (
        <div className="rounded-lg border border-white/10 bg-black/30 p-2">
            <div className="mb-1 flex items-center justify-between text-[10px] font-semibold uppercase tracking-wide text-white/55">
                <span>{title}</span>
                {truncated && <span className="text-amber-300/70">truncated</span>}
            </div>
            <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-snug text-white/75">
                {body}
            </pre>
        </div>
    );
};

export const CodingChecksReview: React.FC<CodingChecksReviewProps> = ({ data }) => {
    const { result } = data;
    const state = result.timed_out ? 'timed-out' : result.success ? 'passed' : 'failed';
    const tone = result.timed_out
        ? 'text-amber-200'
        : result.success
            ? 'text-emerald-200'
            : 'text-red-200';
    const StateIcon = result.timed_out ? TimerReset : result.success ? CheckCircle2 : XCircle;

    return (
        <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
            <div className="flex items-center gap-2">
                <TerminalSquare className="h-4 w-4 text-white/55" />
                <span className="text-xs font-semibold text-white/85">{result.label}</span>
                <span className={`ml-auto flex items-center gap-1 text-[11px] font-semibold ${tone}`}>
                    <StateIcon className="h-3.5 w-3.5" />
                    {state}
                </span>
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-1.5">
                <span className={chipClass}>{result.check}</span>
                {result.filter && <span className={chipClass}>filter: {result.filter}</span>}
                {result.timed_out ? (
                    <span className={chipClass}>timeout {result.timeout_secs}s</span>
                ) : (
                    <>
                        <span className={chipClass}>exit {result.exit_code ?? 'n/a'}</span>
                        <span className={chipClass}>{(result.duration_ms / 1000).toFixed(1)}s</span>
                    </>
                )}
            </div>

            <div className="mt-2 rounded-lg border border-white/10 bg-black/20 px-2 py-1">
                <span className="break-all font-mono text-[11px] text-white/60" title={commandLine(result)}>
                    {commandLine(result)}
                </span>
            </div>

            <div className="mt-2 space-y-2">
                <OutputBlock title="stdout" body={result.stdout} truncated={result.stdout_truncated} />
                <OutputBlock title="stderr" body={result.stderr} truncated={result.stderr_truncated} />
            </div>
        </div>
    );
};

const checkState = (result: CodingRunCheckResult): { label: string; tone: string; Icon: typeof CheckCircle2 } => {
    if (result.timed_out) return { label: 'timed-out', tone: 'text-amber-200', Icon: TimerReset };
    if (result.success) return { label: 'passed', tone: 'text-emerald-200', Icon: CheckCircle2 };
    return { label: 'failed', tone: 'text-red-200', Icon: XCircle };
};

export const CodingVerifyReview: React.FC<CodingVerifyReviewProps> = ({ data }) => {
    const { result } = data;
    const passed = result.checks.filter(c => c.success).length;
    const tone = result.overall_success ? 'text-emerald-200' : 'text-red-200';
    const OverallIcon = result.overall_success ? CheckCircle2 : XCircle;

    return (
        <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
            <div className="flex items-center gap-2">
                <ListChecks className="h-4 w-4 text-white/55" />
                <span className="text-xs font-semibold text-white/85">Verification</span>
                <span className={`ml-auto flex items-center gap-1 text-[11px] font-semibold ${tone}`}>
                    <OverallIcon className="h-3.5 w-3.5" />
                    {passed}/{result.checks.length} passed
                </span>
            </div>
            {result.stopped_early && (
                <div className="mt-1 text-[10px] text-amber-300/70">Stopped at the first failing check.</div>
            )}
            <div className="mt-2 space-y-2">
                {result.checks.map((check, idx) => {
                    const state = checkState(check);
                    return (
                        <div key={`${check.check}-${idx}`} className="rounded-lg border border-white/10 bg-white/5 p-2">
                            <div className="flex items-center gap-2">
                                <state.Icon className={`h-3.5 w-3.5 ${state.tone}`} />
                                <span className="text-[11px] font-semibold text-white/85">{check.label}</span>
                                <span className={`ml-auto text-[10px] font-semibold ${state.tone}`}>{state.label}</span>
                            </div>
                            <div className="mt-1 break-all font-mono text-[10px] text-white/50" title={commandLine(check)}>
                                {commandLine(check)}
                                {!check.timed_out && check.exit_code != null && ` · exit ${check.exit_code} · ${(check.duration_ms / 1000).toFixed(1)}s`}
                            </div>
                            {!check.success && (
                                <div className="mt-2 space-y-2">
                                    <OutputBlock title="stdout" body={check.stdout} truncated={check.stdout_truncated} />
                                    <OutputBlock title="stderr" body={check.stderr} truncated={check.stderr_truncated} />
                                </div>
                            )}
                        </div>
                    );
                })}
            </div>
        </div>
    );
};

export const CodingChecksApprovalPreview: React.FC<CodingChecksApprovalPreviewProps> = ({
    workspaceRoot,
    check,
    filter,
    timeoutSecs,
}) => (
    <div className="mt-2 rounded-lg border border-amber-400/20 bg-amber-400/5 p-2">
        <div className="flex items-center gap-2 text-[11px] font-semibold text-amber-100">
            <PlayCircle className="h-3.5 w-3.5" />
            Run project check
        </div>
        <div className="mt-1 space-y-0.5 text-[11px] text-white/70">
            {check && <div>Check: <span className="font-mono text-white/85">{check}</span></div>}
            {workspaceRoot && (
                <div className="truncate" title={workspaceRoot}>Workspace: <span className="font-mono text-white/85">{workspaceRoot}</span></div>
            )}
            {filter && <div>Filter: <span className="font-mono text-white/85">{filter}</span></div>}
            {typeof timeoutSecs === 'number' && <div>Timeout: {timeoutSecs}s</div>}
        </div>
        <div className="mt-1 text-[10px] text-amber-200/70">
            Runs a curated build/test/lint command in the workspace. Build scripts and tests execute real code.
        </div>
    </div>
);

export const CodingVerifyApprovalPreview: React.FC<{
    workspaceRoot?: string;
    checks?: string[];
    continueOnFailure?: boolean;
}> = ({ workspaceRoot, checks, continueOnFailure }) => (
    <div className="mt-2 rounded-lg border border-amber-400/20 bg-amber-400/5 p-2">
        <div className="flex items-center gap-2 text-[11px] font-semibold text-amber-100">
            <ListChecks className="h-3.5 w-3.5" />
            Run verification pass
        </div>
        <div className="mt-1 space-y-0.5 text-[11px] text-white/70">
            {checks && checks.length > 0 && (
                <div>Checks: <span className="font-mono text-white/85">{checks.join(', ')}</span></div>
            )}
            {workspaceRoot && (
                <div className="truncate" title={workspaceRoot}>Workspace: <span className="font-mono text-white/85">{workspaceRoot}</span></div>
            )}
            <div>{continueOnFailure ? 'Runs all checks even if one fails.' : 'Stops at the first failing check.'}</div>
        </div>
        <div className="mt-1 text-[10px] text-amber-200/70">
            Runs curated build/test/lint commands in the workspace. Build scripts and tests execute real code.
        </div>
    </div>
);

export default CodingChecksReview;
