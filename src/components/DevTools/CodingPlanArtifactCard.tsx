// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { AlertTriangle, CheckCircle2, FileCode, HelpCircle, ListChecks, Shield } from 'lucide-react';
import type { CodingPlanArtifact, CodingPlanRiskLevel } from './aiChatTypes';

const riskClasses: Record<CodingPlanRiskLevel, string> = {
    low: 'border-emerald-400/30 bg-emerald-400/10 text-emerald-200',
    medium: 'border-amber-400/30 bg-amber-400/10 text-amber-200',
    high: 'border-red-400/30 bg-red-400/10 text-red-200',
};

const formatScope = (scope: CodingPlanArtifact['scope']): string => (
    scope.replace(/_/g, ' ')
);

interface CodingPlanArtifactCardProps {
    plan: CodingPlanArtifact;
}

export const CodingPlanArtifactCard: React.FC<CodingPlanArtifactCardProps> = ({ plan }) => (
    <div className="mt-3 rounded-lg border border-sky-400/30 bg-sky-400/5 p-3 text-xs">
        <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-sm font-semibold text-sky-200">
                    <ListChecks size={14} />
                    <span>Coding Plan</span>
                </div>
                <h4 className="mt-2 text-sm font-semibold text-white">{plan.title}</h4>
                <p className="mt-1 text-sky-100/80">{plan.summary}</p>
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
                <span className={`rounded-full border px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide ${riskClasses[plan.riskLevel]}`}>
                    {plan.riskLevel} risk
                </span>
                <span className="rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] uppercase tracking-wide text-white/60">
                    {formatScope(plan.scope)}
                </span>
            </div>
        </div>

        {plan.files.length > 0 && (
            <div className="mt-3">
                <div className="mb-1 flex items-center gap-1 font-medium text-sky-200">
                    <FileCode size={12} />
                    <span>Likely Files</span>
                </div>
                <div className="flex flex-wrap gap-1.5">
                    {plan.files.map(file => (
                        <span key={file} className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[10px] text-white/70">
                            {file}
                        </span>
                    ))}
                </div>
            </div>
        )}

        <div className="mt-3 space-y-2">
            {plan.steps.map((step, index) => (
                <div key={`${step.id}-${index}`} className="rounded-lg border border-white/10 bg-white/5 p-2">
                    <div className="flex items-start gap-2">
                        <span className="mt-0.5 flex h-5 min-w-5 items-center justify-center rounded-full bg-sky-400/20 text-[10px] font-semibold text-sky-200">
                            {step.id || index + 1}
                        </span>
                        <div className="min-w-0">
                            <div className="font-medium text-white">{step.title}</div>
                            {step.description && (
                                <p className="mt-1 text-white/65">{step.description}</p>
                            )}
                            {step.files && step.files.length > 0 && (
                                <p className="mt-1 font-mono text-[10px] text-white/45">
                                    {step.files.join(', ')}
                                </p>
                            )}
                        </div>
                    </div>
                </div>
            ))}
        </div>

        {plan.verification.length > 0 && (
            <div className="mt-3 rounded-lg border border-emerald-400/20 bg-emerald-400/5 p-2">
                <div className="mb-1 flex items-center gap-1 font-medium text-emerald-200">
                    <CheckCircle2 size={12} />
                    <span>Verification</span>
                </div>
                <ul className="space-y-1 text-emerald-50/75">
                    {plan.verification.map((item, index) => (
                        <li key={`${item}-${index}`}>{item}</li>
                    ))}
                </ul>
            </div>
        )}

        {plan.warnings.length > 0 && (
            <div className="mt-3 rounded-lg border border-amber-400/20 bg-amber-400/5 p-2">
                <div className="mb-1 flex items-center gap-1 font-medium text-amber-200">
                    <AlertTriangle size={12} />
                    <span>Risks</span>
                </div>
                <ul className="space-y-1 text-amber-50/75">
                    {plan.warnings.map((item, index) => (
                        <li key={`${item}-${index}`}>{item}</li>
                    ))}
                </ul>
            </div>
        )}

        {plan.questions.length > 0 && (
            <div className="mt-3 rounded-lg border border-white/10 bg-white/5 p-2">
                <div className="mb-1 flex items-center gap-1 font-medium text-white/75">
                    <HelpCircle size={12} />
                    <span>Open Questions</span>
                </div>
                <ul className="space-y-1 text-white/65">
                    {plan.questions.map((item, index) => (
                        <li key={`${item}-${index}`}>{item}</li>
                    ))}
                </ul>
            </div>
        )}

        <div className="mt-3 flex items-center gap-1.5 text-[10px] text-sky-100/55">
            <Shield size={11} />
            <span>Review-only artifact. Edits and mutating tools still use the current approval mode.</span>
        </div>
    </div>
);

export default CodingPlanArtifactCard;
