// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type {
    ChatResultData,
    CodingSearchMatch,
    CodingSearchResult,
    CodingSearchResultData,
    CodingSearchSubmatch,
} from './aiChatTypes';

const CODING_SEARCH_KIND = 'coding_search';

const isRecord = (value: unknown): value is Record<string, unknown> => (
    !!value && typeof value === 'object' && !Array.isArray(value)
);

const finiteNumber = (value: unknown): number => (
    typeof value === 'number' && Number.isFinite(value) ? value : 0
);

const optionalNumber = (value: unknown): number | null | undefined => {
    if (value === null) return null;
    return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
};

const optionalString = (value: unknown): string | null | undefined => {
    if (value === null) return null;
    return typeof value === 'string' ? value : undefined;
};

const stringArray = (value: unknown): string[] => (
    Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
);

function normalizeSubmatch(value: unknown): CodingSearchSubmatch | null {
    if (!isRecord(value)) return null;
    if (typeof value.text !== 'string') return null;
    return {
        start: finiteNumber(value.start),
        end: finiteNumber(value.end),
        text: value.text,
    };
}

function normalizeMatch(value: unknown): CodingSearchMatch | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.file !== 'string'
        || typeof value.line !== 'number'
        || typeof value.column !== 'number'
        || typeof value.line_text !== 'string'
        || !Array.isArray(value.submatches)
    ) {
        return null;
    }
    const submatches: CodingSearchSubmatch[] = [];
    for (const item of value.submatches) {
        const normalized = normalizeSubmatch(item);
        if (!normalized) return null;
        submatches.push(normalized);
    }
    return {
        file: value.file,
        line: value.line,
        column: value.column,
        line_text: value.line_text,
        submatches,
    };
}

export function normalizeCodingSearchResult(value: unknown): CodingSearchResult | null {
    if (!isRecord(value)) return null;
    if (
        typeof value.workspace_root !== 'string'
        || typeof value.pattern !== 'string'
        || typeof value.program !== 'string'
        || typeof value.case_insensitive !== 'boolean'
        || typeof value.fixed_strings !== 'boolean'
        || typeof value.timed_out !== 'boolean'
        || typeof value.truncated !== 'boolean'
        || !Array.isArray(value.matches)
    ) {
        return null;
    }
    const matches: CodingSearchMatch[] = [];
    for (const item of value.matches) {
        const normalized = normalizeMatch(item);
        if (!normalized) return null;
        matches.push(normalized);
    }
    return {
        workspace_root: value.workspace_root,
        pattern: value.pattern,
        path: optionalString(value.path),
        globs: stringArray(value.globs),
        case_insensitive: value.case_insensitive,
        fixed_strings: value.fixed_strings,
        program: value.program,
        args: stringArray(value.args),
        exit_code: optionalNumber(value.exit_code),
        timed_out: value.timed_out,
        timeout_secs: finiteNumber(value.timeout_secs),
        duration_ms: finiteNumber(value.duration_ms),
        total_matches: finiteNumber(value.total_matches),
        file_count: finiteNumber(value.file_count),
        matches,
        truncated: value.truncated,
    };
}

export function isCodingSearchResultData(value: unknown): value is CodingSearchResultData {
    if (!isRecord(value) || value.kind !== CODING_SEARCH_KIND) return false;
    return !!normalizeCodingSearchResult(value.result);
}

export function getCodingSearchFromResultData(
    value: ChatResultData | undefined,
): CodingSearchResultData | null {
    if (!isCodingSearchResultData(value)) return null;
    const result = normalizeCodingSearchResult(value.result);
    if (!result) return null;
    return { kind: CODING_SEARCH_KIND, result };
}

export function searchMatchLocation(match: CodingSearchMatch): string {
    return `${match.file}:${match.line}:${match.column}`;
}

export function summarizeCodingSearchResult(result: CodingSearchResult): string {
    if (result.timed_out) {
        return [
            `**Search timed out**`,
            `\`${result.pattern}\` timed out after ${result.timeout_secs}s`,
            '',
            'Review the search card below.',
        ].join('\n');
    }
    const lines = [
        `**Search \`${result.pattern}\`: ${result.total_matches} match(es)**`,
        `${result.file_count} file(s)`,
    ];
    if (result.truncated) lines.push('Results were truncated.');
    lines.push('');
    lines.push('Review the search card below.');
    return lines.join('\n');
}
