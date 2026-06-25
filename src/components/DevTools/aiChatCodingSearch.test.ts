// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    getCodingSearchFromResultData,
    isCodingSearchResultData,
    normalizeCodingSearchResult,
    searchMatchLocation,
    summarizeCodingSearchResult,
} from './aiChatCodingSearch';

const matchResult = {
    workspace_root: '/repo',
    pattern: 'compute',
    path: 'src',
    globs: ['*.rs'],
    case_insensitive: false,
    fixed_strings: false,
    program: 'rg',
    args: ['--json', '-e', 'compute', '--', 'src'],
    exit_code: 0,
    timed_out: false,
    timeout_secs: 60,
    duration_ms: 120,
    total_matches: 2,
    file_count: 1,
    matches: [
        {
            file: 'src/lib.rs',
            line: 42,
            column: 9,
            line_text: '    let value = compute();',
            submatches: [{ start: 16, end: 23, text: 'compute' }],
        },
        {
            file: 'src/lib.rs',
            line: 88,
            column: 5,
            line_text: 'fn compute() {}',
            submatches: [{ start: 3, end: 10, text: 'compute' }],
        },
    ],
    truncated: false,
};

const emptyResult = {
    ...matchResult,
    total_matches: 0,
    file_count: 0,
    matches: [],
};

const timeoutResult = {
    ...matchResult,
    exit_code: null,
    timed_out: true,
    total_matches: 0,
    file_count: 0,
    matches: [],
};

describe('aiChatCodingSearch', () => {
    it('normalizes a result with matches', () => {
        const result = normalizeCodingSearchResult(matchResult);
        expect(result?.pattern).toBe('compute');
        expect(result?.matches).toHaveLength(2);
        expect(result?.matches[0].submatches[0].text).toBe('compute');
        expect(result?.file_count).toBe(1);
        expect(result?.path).toBe('src');
    });

    it('rejects malformed input', () => {
        expect(normalizeCodingSearchResult(null)).toBeNull();
        expect(normalizeCodingSearchResult({ pattern: 'x' })).toBeNull();
        // A match missing required fields invalidates the whole result.
        expect(normalizeCodingSearchResult({
            ...matchResult,
            matches: [{ file: 'a.rs' }],
        })).toBeNull();
    });

    it('builds a readable location string', () => {
        const result = normalizeCodingSearchResult(matchResult)!;
        expect(searchMatchLocation(result.matches[0])).toBe('src/lib.rs:42:9');
    });

    it('summarizes match, empty, and timed-out states', () => {
        const hit = normalizeCodingSearchResult(matchResult)!;
        const empty = normalizeCodingSearchResult(emptyResult)!;
        const timed = normalizeCodingSearchResult(timeoutResult)!;
        expect(summarizeCodingSearchResult(hit)).toContain('2 match(es)');
        expect(summarizeCodingSearchResult(empty)).toContain('0 match(es)');
        expect(summarizeCodingSearchResult(timed)).toContain('timed out');
    });

    it('guards and extracts result data', () => {
        const data = { kind: 'coding_search', result: matchResult };
        expect(isCodingSearchResultData(data)).toBe(true);
        expect(isCodingSearchResultData({ kind: 'coding_diagnostics', result: matchResult })).toBe(false);
        expect(getCodingSearchFromResultData(data as never)?.result.pattern).toBe('compute');
    });
});
