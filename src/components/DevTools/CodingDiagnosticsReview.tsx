// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { AlertTriangle, CheckCircle2, Stethoscope, TimerReset, XCircle } from 'lucide-react';
import { diagnosticLocation } from './aiChatCodingDiagnostics';
import type { CodingDiagnostic, CodingDiagnosticsResultData } from './aiChatTypes';

interface CodingDiagnosticsReviewProps {
    data: CodingDiagnosticsResultData;
}

const chipClass = 'rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65';

const MAX_VISIBLE = 50;

const severityTone = (severity: string): string => {
    if (severity === 'error') return 'text-red-200';
    if (severity === 'warning') return 'text-amber-200';
    return 'text-white/60';
};

const DiagnosticRow: React.FC<{ diagnostic: CodingDiagnostic }> = ({ diagnostic }) => {
    const Icon = diagnostic.severity === 'error' ? XCircle : AlertTriangle;
    const tone = severityTone(diagnostic.severity);
    return (
        <div className="rounded-lg border border-white/10 bg-white/5 p-2">
            <div className="flex items-center gap-2">
                <Icon className={`h-3.5 w-3.5 shrink-0 ${tone}`} />
                <span
                    className="min-w-0 flex-1 truncate font-mono text-[10px] text-white/55"
                    title={diagnosticLocation(diagnostic)}
                >
                    {diagnosticLocation(diagnostic)}
                </span>
                {diagnostic.code && <span className={chipClass}>{diagnostic.code}</span>}
            </div>
            <div className="mt-1 whitespace-pre-wrap break-words text-[11px] text-white/80">
                {diagnostic.message}
            </div>
        </div>
    );
};

export const CodingDiagnosticsReview: React.FC<CodingDiagnosticsReviewProps> = ({ data }) => {
    const { result } = data;
    const tone = result.timed_out
        ? 'text-amber-200'
        : result.success
            ? 'text-emerald-200'
            : 'text-red-200';
    const StateIcon = result.timed_out ? TimerReset : result.success ? CheckCircle2 : XCircle;
    const stateLabel = result.timed_out
        ? 'timed-out'
        : result.success
            ? 'clean'
            : `${result.error_count} error(s)`;
    const visible = result.diagnostics.slice(0, MAX_VISIBLE);
    const hidden = result.diagnostics.length - visible.length;

    return (
        <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
            <div className="flex items-center gap-2">
                <Stethoscope className="h-4 w-4 text-white/55" />
                <span className="text-xs font-semibold text-white/85">Diagnostics</span>
                <span className={`ml-auto flex items-center gap-1 text-[11px] font-semibold ${tone}`}>
                    <StateIcon className="h-3.5 w-3.5" />
                    {stateLabel}
                </span>
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-1.5">
                <span className={chipClass}>{result.source}</span>
                <span className={chipClass}>{result.error_count} error(s)</span>
                <span className={chipClass}>{result.warning_count} warning(s)</span>
                {result.timed_out ? (
                    <span className={chipClass}>timeout {result.timeout_secs}s</span>
                ) : (
                    <span className={chipClass}>{(result.duration_ms / 1000).toFixed(1)}s</span>
                )}
            </div>

            {result.diagnostics.length > 0 ? (
                <div className="mt-2 space-y-2">
                    {visible.map((diagnostic, idx) => (
                        <DiagnosticRow key={`${diagnostic.file ?? 'none'}-${idx}`} diagnostic={diagnostic} />
                    ))}
                    {hidden > 0 && (
                        <div className="text-[10px] text-white/40">+{hidden} more</div>
                    )}
                    {result.truncated && (
                        <div className="text-[10px] text-amber-300/70">Diagnostics list was truncated.</div>
                    )}
                </div>
            ) : (
                <div className="mt-2 text-[11px] text-white/55">
                    {result.timed_out ? `Timed out after ${result.timeout_secs}s.` : 'No diagnostics reported.'}
                </div>
            )}
        </div>
    );
};

export default CodingDiagnosticsReview;
