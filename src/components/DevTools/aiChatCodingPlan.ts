// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingPlanArtifact,
    CodingPlanResultData,
    CodingPlanRiskLevel,
    CodingPlanScope,
    CodingPlanStep,
} from './aiChatTypes';

const CODING_PLAN_KIND = 'coding_plan';

type ExtractedCodingPlan = {
    plan: CodingPlanArtifact | null;
    content: string;
};

const isRecord = (value: unknown): value is Record<string, unknown> => (
    !!value && typeof value === 'object' && !Array.isArray(value)
);

const asString = (value: unknown): string | undefined => (
    typeof value === 'string' && value.trim() ? value.trim() : undefined
);

const asStringArray = (value: unknown, maxItems = 12): string[] => {
    if (Array.isArray(value)) {
        return value
            .map(item => asString(item))
            .filter((item): item is string => !!item)
            .slice(0, maxItems);
    }

    const single = asString(value);
    return single ? [single] : [];
};

const normalizeRiskLevel = (value: unknown): CodingPlanRiskLevel => {
    const normalized = asString(value)?.toLowerCase();
    if (normalized === 'low' || normalized === 'medium' || normalized === 'high') {
        return normalized;
    }
    return 'medium';
};

const normalizeScope = (value: unknown): CodingPlanScope => {
    const normalized = asString(value)?.toLowerCase().replace(/[-\s]/g, '_');
    if (
        normalized === 'single_file' ||
        normalized === 'multi_file' ||
        normalized === 'investigation' ||
        normalized === 'unknown'
    ) {
        return normalized;
    }
    return 'unknown';
};

const normalizeSteps = (value: unknown): CodingPlanStep[] => {
    if (!Array.isArray(value)) return [];

    return value
        .map((item, index): CodingPlanStep | null => {
            if (typeof item === 'string') {
                const title = item.trim();
                return title ? { id: String(index + 1), title } : null;
            }
            if (!isRecord(item)) return null;

            const title = asString(item.title) || asString(item.name) || asString(item.action);
            if (!title) return null;

            return {
                id: asString(item.id) || String(index + 1),
                title,
                description: asString(item.description) || asString(item.detail),
                files: asStringArray(item.files || item.paths, 8),
            };
        })
        .filter((step): step is CodingPlanStep => !!step)
        .slice(0, 8);
};

export function normalizeCodingPlanArtifact(value: unknown): CodingPlanArtifact | null {
    if (!isRecord(value)) return null;

    const kind = asString(value.kind)?.toLowerCase();
    if (kind && kind !== CODING_PLAN_KIND) return null;

    const steps = normalizeSteps(value.steps || value.plan);
    if (steps.length === 0) return null;

    const title = asString(value.title) || 'Coding Plan';
    const summary = asString(value.summary) || asString(value.goal) || title;
    const files = asStringArray(value.files || value.targetFiles || value.target_files, 16);

    return {
        kind: CODING_PLAN_KIND,
        title,
        summary,
        riskLevel: normalizeRiskLevel(value.riskLevel || value.risk_level || value.risk),
        scope: normalizeScope(value.scope),
        files,
        steps,
        verification: asStringArray(value.verification || value.verify || value.checks, 10),
        questions: asStringArray(value.questions || value.openQuestions || value.open_questions, 6),
        warnings: asStringArray(value.warnings || value.risks, 6),
    };
}

export function isCodingPlanResultData(value: unknown): value is CodingPlanResultData {
    if (!isRecord(value)) return false;
    return value.kind === CODING_PLAN_KIND && !!normalizeCodingPlanArtifact(value.plan);
}

export function getCodingPlanFromResultData(value: ChatResultData | undefined): CodingPlanArtifact | null {
    if (!isCodingPlanResultData(value)) return null;
    return normalizeCodingPlanArtifact(value.plan);
}

const parseCodingPlanFromFence = (body: string): CodingPlanArtifact | null => {
    try {
        return normalizeCodingPlanArtifact(JSON.parse(body));
    } catch {
        return null;
    }
};

export function extractCodingPlanArtifact(content: string): ExtractedCodingPlan {
    if (!content.trim()) return { plan: null, content };

    const fencePattern = /(```|~~~)([^\n]*)\n([\s\S]*?)\1/g;
    let match: RegExpExecArray | null;
    let plan: CodingPlanArtifact | null = null;
    const rangesToStrip: Array<[number, number]> = [];

    while ((match = fencePattern.exec(content)) !== null) {
        const info = match[2].trim().toLowerCase();
        const body = match[3].trim();
        const looksLikeCodingPlan = info.includes(CODING_PLAN_KIND)
            || (info.includes('json') && body.includes(`"${CODING_PLAN_KIND}"`));

        if (!looksLikeCodingPlan) continue;

        const parsed = parseCodingPlanFromFence(body);
        if (!parsed) continue;

        if (!plan) plan = parsed;
        rangesToStrip.push([match.index, match.index + match[0].length]);
    }

    if (rangesToStrip.length === 0) {
        return { plan: null, content };
    }

    let stripped = '';
    let cursor = 0;
    for (const [start, end] of rangesToStrip) {
        stripped += content.slice(cursor, start);
        cursor = end;
    }
    stripped += content.slice(cursor);

    return {
        plan,
        content: stripped.replace(/\n{3,}/g, '\n\n').trim(),
    };
}
