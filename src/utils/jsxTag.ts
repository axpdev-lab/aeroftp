// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Reading one JSX element out of source text, for the tests that scan source.
 *
 * The repo pins several invariants by scanning the source rather than by
 * rendering, because vitest runs on `environment: node` with no DOM harness. That
 * works only if the scan reads *the element it means*. Two ways of faking it have
 * already produced tests that passed while checking nothing:
 *
 * - a window of N characters or lines around an anchor, which reaches into the
 *   neighbouring element, so a prop belonging to a sibling is read as this one's
 *   (`modalLayerSweep.test.ts` carries the same warning about `className`);
 * - a non-greedy `<Name[\s\S]*?/>`, which stops at the *first* `/>` in range. On
 *   `<ProviderThumbnail … fallback={<ImageThumbnail … />} />` that is the inner
 *   element's, so the match is the outer tag's name attached to the inner tag's
 *   props, and an assertion about either one is answered by the other.
 *
 * These read the element's own span with one lexer, tracking string and template
 * literals as well as brace depth, so neither a nested element inside a prop nor
 * a delimiter inside a literal can end the tag that contains it.
 */

/** Where the scanner is: ordinary code, inside a literal, or inside a comment. */
type Mode = 'code' | 'line-comment' | 'block-comment' | '"' | "'" | '`';

/**
 * True when the character at `i` is escaped, i.e. preceded by an odd number of
 * backslashes.
 *
 * `source[i - 1] !== '\\'` is the tempting version and it is wrong on an even
 * run: in `"C:\\"` the final quote closes the string, because the two
 * backslashes escape each other. This codebase is full of Windows paths, so that
 * is not a hypothetical.
 */
function isEscaped(source: string, i: number): boolean {
    let run = 0;
    for (let k = i - 1; k >= 0 && source[k] === '\\'; k--) run++;
    return run % 2 === 1;
}

/**
 * Blank out comment bodies, keeping every offset so indices still line up.
 *
 * String-aware on purpose: a regex that hunts for `//` finds it inside
 * `'https://…'` and inside any literal that happens to contain one, and then
 * blanks the rest of the line, silently deleting the code an assertion was about.
 * Guarding the URL case alone (`[^:]//`) covers the common example and none of
 * the others.
 */
export function withoutComments(source: string): string {
    const out = source.split('');
    let mode: Mode = 'code';
    for (let i = 0; i < source.length; i++) {
        const c = source[i];
        const next = source[i + 1];
        switch (mode) {
            case 'code':
                if (c === '/' && next === '/') { mode = 'line-comment'; out[i] = ' '; }
                else if (c === '/' && next === '*') { mode = 'block-comment'; out[i] = ' '; }
                else if (c === '"' || c === "'" || c === '`') mode = c;
                break;
            case 'line-comment':
                if (c === '\n') mode = 'code';
                else out[i] = ' ';
                break;
            case 'block-comment':
                if (c === '*' && next === '/') { out[i] = ' '; out[i + 1] = ' '; i++; mode = 'code'; }
                else if (c !== '\n') out[i] = ' ';
                break;
            default:
                if (c === mode && !isEscaped(source, i)) mode = 'code';
                break;
        }
    }
    return out.join('');
}

/**
 * The full text of the JSX tag opening at `at`, up to and including its own `>`.
 *
 * Returns null when the tag never closes, which is a malformed input rather than
 * a passing test.
 */
export function jsxTagAt(source: string, at: number): string | null {
    let depth = 0;
    let quote: '"' | "'" | '`' | null = null;
    for (let i = at; i < source.length; i++) {
        const c = source[i];
        if (quote) {
            if (c === quote && !isEscaped(source, i)) quote = null;
            continue;
        }
        if (c === '"' || c === "'" || c === '`') { quote = c; continue; }
        if (c === '{') { depth++; continue; }
        if (c === '}') { depth--; continue; }
        // Only a `>` outside every prop expression closes this tag. Inside one,
        // it belongs to a nested element or to an arrow function.
        if (c === '>' && depth === 0) return source.slice(at, i + 1);
    }
    return null;
}

/** Every `<Name …>` tag in the source, in order. Comments are not stripped here:
 *  the caller decides, since some assertions are about comments. */
export function jsxTags(source: string, name: string): string[] {
    const out: string[] = [];
    const open = new RegExp(`<${name}(?![A-Za-z0-9_])`, 'g');
    for (const m of source.matchAll(open)) {
        const tag = jsxTagAt(source, m.index ?? 0);
        if (tag) out.push(tag);
    }
    return out;
}

/**
 * The `<Name …>` tag whose own span contains `needle`.
 *
 * This is the anchor that a character window gets wrong: it answers "which
 * element carries this prop", not "what text sits near it".
 */
export function jsxTagContaining(source: string, name: string, needle: string): string | null {
    const idx = source.indexOf(needle);
    if (idx === -1) return null;
    const open = new RegExp(`<${name}(?![A-Za-z0-9_])`, 'g');
    let found: string | null = null;
    for (const m of source.matchAll(open)) {
        const start = m.index ?? 0;
        if (start > idx) break;
        const tag = jsxTagAt(source, start);
        if (tag && start + tag.length > idx) found = tag;
    }
    return found;
}
