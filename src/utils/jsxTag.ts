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

/** Word characters, for reading the identifier that precedes a `/`. */
const WORD = /[A-Za-z0-9_$]/;

/**
 * Keywords after which a `/` opens a regex rather than dividing. `return /x/`
 * is a regex; `total / count` is division, and so is anything after a plain
 * identifier.
 */
const REGEX_AFTER_KEYWORD = new Set([
    'await', 'case', 'delete', 'do', 'else', 'in', 'instanceof', 'new',
    'of', 'return', 'throw', 'typeof', 'void', 'yield',
]);

/**
 * Whether the `/` at `i` opens a regex literal, by the usual lexer heuristic:
 * it does unless the previous meaningful character could end an expression.
 *
 * The ambiguous cases are decided towards division on purpose. Calling a
 * division a regex swallows real code up to the next `/` and hides it from the
 * scan; calling a regex a division only leaves this file where it already was.
 */
function opensRegex(source: string, i: number): boolean {
    // Two shapes that are JSX, not regexes, and would each swallow the rest of
    // the file up to the next `/`: the `/>` that closes an element, and the `</`
    // that opens a closing tag. `/>/` and `</` are legal regex starts in plain
    // JS, but this lexer exists to read TSX, where those readings never win.
    if (source[i + 1] === '>') return false;
    if (source[i - 1] === '<') return false;
    let k = i - 1;
    while (k >= 0 && /\s/.test(source[k])) k--;
    if (k < 0) return true;
    const previous = source[k];
    if (WORD.test(previous)) {
        let start = k;
        while (start >= 0 && WORD.test(source[start])) start--;
        return REGEX_AFTER_KEYWORD.has(source.slice(start + 1, k + 1));
    }
    // `)`, `]` and `}` end an expression or a block, `.` a member access:
    // a `/` after any of them is division.
    return !')]}.'.includes(previous);
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
 *
 * A regex literal is its own state for the same reason. `/https?:\/\//` ends in
 * two adjacent slashes, so without one this scan read them as the start of a
 * line comment and blanked the rest of the line: every tag span that ran past
 * such a regex was truncated, and the pins that read those spans passed on
 * text that no longer contained what they were asserting about.
 */
/**
 * Memoized per source string.
 *
 * These helpers exist to let a test scan real source files, and the scans
 * classify the same text more than once: `withoutComments(source)` lexes the
 * file, then each `jsxTags(code, name)` lexes the stripped result again, once
 * per component name. The thumbnail pin globs 285 tsx files, 6.3 MB, so the
 * repeats were lexing roughly 19 MB per run and the test kept timing out on a
 * loaded machine (twice, in two different sessions, on the default 5s budget).
 * Caching the result cuts the repeats without changing what any caller sees:
 * `classify` is pure, and this module is imported only by tests, so the cache
 * lives for one short test process.
 */
const CLASSIFY_CACHE = new Map<string, Uint8Array>();

export function classify(source: string): Uint8Array {
    const cached = CLASSIFY_CACHE.get(source);
    if (cached) return cached;
    const computed = classifyUncached(source);
    CLASSIFY_CACHE.set(source, computed);
    return computed;
}

function classifyUncached(source: string): Uint8Array {
    const out = new Uint8Array(source.length);
    // Each frame is either a template literal, or the code inside a `${}` with
    // the brace depth reached so far, so that object literals and blocks inside
    // the interpolation do not close it early.
    const stack: ({ kind: 'template' } | { kind: 'interp'; depth: number })[] = [];
    let mode: 'code' | 'line' | 'block' | '"' | "'" | 'template' | 'regex' = 'code';
    // Inside a regex, a `/` in a character class does not close the literal.
    let inCharClass = false;

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
            case 'regex':
                out[i] = Ctx.Literal;
                if (c === '\n') {
                    // An unterminated regex is malformed input, not a reason to
                    // swallow the rest of the file: a literal never spans lines.
                    out[i] = Ctx.Code;
                    mode = 'code';
                    inCharClass = false;
                    continue;
                }
                if (isEscaped(source, i)) continue;
                if (c === '[') inCharClass = true;
                else if (c === ']') inCharClass = false;
                else if (c === '/' && !inCharClass) mode = 'code';
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
                // Closing this template hands control back to code either way:
                // to the interpolation that is still running if the template was
                // opened inside one, or to the surrounding code if it was not.
                // The frame that says which is already on the stack and is
                // popped by the `}` that closes the interpolation.
                if (c === '`' && !isEscaped(source, i)) mode = 'code';
                continue;
            default: {
                const top = stack[stack.length - 1];
                // Comments first: a regex can start with neither `/` (empty) nor
                // `*` (nothing to repeat), so these two never steal one.
                if (c === '/' && next === '/') { mode = 'line'; out[i] = Ctx.Comment; continue; }
                if (c === '/' && next === '*') { mode = 'block'; out[i] = Ctx.Comment; continue; }
                if (c === '/' && opensRegex(source, i)) {
                    out[i] = Ctx.Literal;
                    mode = 'regex';
                    inCharClass = false;
                    continue;
                }
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

/**
 * Escape a tag name for literal use inside a `RegExp`.
 *
 * The name is usually a bare identifier, which is why the missing escape went
 * unnoticed, but a JSX member expression is a legal tag name: `<Menu.Item …>`
 * searched unescaped matches `MenuXItem` too, and a name carrying `[` or `(`
 * builds a pattern that throws or, worse, quietly matches the wrong element and
 * answers an assertion with someone else's props.
 */
function escapeRegExp(text: string): string {
    return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Every `<Name …>` tag in the source, in order. Comments are not stripped here:
 *  the caller decides, since some assertions are about comments. */
export function jsxTags(source: string, name: string): string[] {
    const ctx = classify(source);
    const out: string[] = [];
    const open = new RegExp(`<${escapeRegExp(name)}(?![A-Za-z0-9_])`, 'g');
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
    const open = new RegExp(`<${escapeRegExp(name)}(?![A-Za-z0-9_])`, 'g');
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
