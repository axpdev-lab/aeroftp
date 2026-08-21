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
 * Everything here reads one classification of the source, produced once by
 * `classify`, rather than each function doing its own approximate scan. Three
 * flaws have already been found at this level, each of which would have made a
 * pin pass for the wrong reason, so the lexing lives in one place with its own
 * tests.
 */

/** What the character at an index belongs to. */
export const enum Ctx {
    /** Syntax: identifiers, punctuation, and the `${` and `}` of an interpolation. */
    Code = 0,
    /** Inside a `//` or a block comment, including its delimiters. */
    Comment = 1,
    /** The body of a string or template literal, including its quotes. */
    Literal = 2,
}

/** True when the character at `i` is escaped: preceded by an odd backslash run. */
function isEscaped(source: string, i: number): boolean {
    let run = 0;
    for (let k = i - 1; k >= 0 && source[k] === '\\'; k--) run++;
    return run % 2 === 1;
}

/**
 * Classify every character of the source once.
 *
 * A template literal is not one flat region: `` `a ${ b // c\n } d` `` contains
 * real code between `${` and its matching `}`, and a comment inside it is a real
 * comment. Staying in literal mode across an interpolation leaves that comment in
 * the text, which is the false positive a comment-stripping scan exists to
 * prevent. So the state is a stack: a template can hold an interpolation, and an
 * interpolation can hold another template, to any depth.
 */
export function classify(source: string): Uint8Array {
    const out = new Uint8Array(source.length);
    // Each frame is either a template literal, or the code inside a `${}` with
    // the brace depth reached so far, so that object literals and blocks inside
    // the interpolation do not close it early.
    const stack: ({ kind: 'template' } | { kind: 'interp'; depth: number })[] = [];
    let mode: 'code' | 'line' | 'block' | '"' | "'" | 'template' = 'code';

    for (let i = 0; i < source.length; i++) {
        const c = source[i];
        const next = source[i + 1];

        switch (mode) {
            case 'line':
                out[i] = Ctx.Comment;
                if (c === '\n') { out[i] = Ctx.Code; mode = 'code'; }
                continue;
            case 'block':
                out[i] = Ctx.Comment;
                if (c === '*' && next === '/') { out[i + 1] = Ctx.Comment; i++; mode = 'code'; }
                continue;
            case '"':
            case "'":
                out[i] = Ctx.Literal;
                if (c === mode && !isEscaped(source, i)) mode = 'code';
                continue;
            case 'template':
                if (c === '$' && next === '{' && !isEscaped(source, i)) {
                    out[i] = Ctx.Literal;      // the `$` is still template text
                    out[i + 1] = Ctx.Code;     // the `{` opens real code
                    i++;
                    stack.push({ kind: 'template' });
                    stack.push({ kind: 'interp', depth: 0 });
                    mode = 'code';
                    continue;
                }
                out[i] = Ctx.Literal;
                if (c === '`' && !isEscaped(source, i)) {
                    // Closing this template. If it was opened inside an
                    // interpolation, that interpolation is still running.
                    const outer = stack[stack.length - 1];
                    mode = outer && outer.kind === 'interp' ? 'code' : 'code';
                }
                continue;
            default: {
                const top = stack[stack.length - 1];
                if (c === '/' && next === '/') { mode = 'line'; out[i] = Ctx.Comment; continue; }
                if (c === '/' && next === '*') { mode = 'block'; out[i] = Ctx.Comment; continue; }
                if (c === '"' || c === "'") { out[i] = Ctx.Literal; mode = c; continue; }
                if (c === '`') { out[i] = Ctx.Literal; mode = 'template'; continue; }
                if (top && top.kind === 'interp') {
                    if (c === '{') top.depth++;
                    else if (c === '}') {
                        if (top.depth === 0) {
                            // Closes the interpolation: back into the template
                            // that opened it.
                            stack.pop();
                            const tpl = stack.pop();
                            out[i] = Ctx.Code;
                            mode = tpl ? 'template' : 'code';
                            continue;
                        }
                        top.depth--;
                    }
                }
                out[i] = Ctx.Code;
            }
        }
    }
    return out;
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
    const ctx = classify(source);
    const out = source.split('');
    for (let i = 0; i < source.length; i++) {
        if (ctx[i] === Ctx.Comment && source[i] !== '\n') out[i] = ' ';
    }
    return out.join('');
}

/**
 * The full text of the JSX tag opening at `at`, up to and including its own `>`.
 *
 * Returns null when the tag never closes, which is a malformed input rather than
 * a passing test.
 */
export function jsxTagAt(source: string, at: number, ctx = classify(source)): string | null {
    let depth = 0;
    for (let i = at; i < source.length; i++) {
        if (ctx[i] !== Ctx.Code) continue;
        const c = source[i];
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
    const ctx = classify(source);
    const out: string[] = [];
    const open = new RegExp(`<${name}(?![A-Za-z0-9_])`, 'g');
    for (const m of source.matchAll(open)) {
        const start = m.index ?? 0;
        if (ctx[start] !== Ctx.Code) continue;
        const tag = jsxTagAt(source, start, ctx);
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
    const ctx = classify(source);
    const open = new RegExp(`<${name}(?![A-Za-z0-9_])`, 'g');
    let found: string | null = null;
    for (const m of source.matchAll(open)) {
        const start = m.index ?? 0;
        if (start > idx) break;
        if (ctx[start] !== Ctx.Code) continue;
        const tag = jsxTagAt(source, start, ctx);
        if (tag && start + tag.length > idx) found = tag;
    }
    return found;
}
