// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// ─── Privacy redaction pipeline ────────────────────────────────────────────
// Applied to every log entry (JS console, backend Rust, IPC argument summaries)
// before it reaches the buffer. Default: always on, no toggle. The Rust log file
// on disk keeps the raw form for offline forensic use; everything that appears
// in the panel UI, copy-to-clipboard, and future exports is sanitized first.
//
// Patterns are ordered most-specific-first so a Bearer token does not get
// double-replaced by the JWT heuristic. Replacement strings are stable so
// duplicate detection across log lines still works after redaction.
//
// The on-disk log tail is redacted by the Rust twin (`redaction_patterns` in
// src-tauri/src/debug_tests.rs). Both run src/shared/redaction-cases.json, so
// a rule changed on one side only turns the other side's test red.

// The body of a value whose quotes are escaped (`\"v\"`, from a Debug or JSON
// dump of the line), up to its closing `\"`. One level down every backslash is
// doubled: `\\` is an escape of the value itself and takes the next character
// with it (`\\\"` is a quote inside the value, `\\\\` a backslash), so a
// backslash in the value does not end it early; any other `\x` is one character.
// The Rust twin writes the same text (`escaped_body!` in
// src-tauri/src/authorization_redaction.rs).
const ESCAPED_BODY = String.raw`(?:[^\\]|\\[^"\\]|\\\\(?:[^\\]|\\.))*`;

// RFC 7235 `credentials`: a scheme, then either a comma-separated list of
// `name=value` parameters (a value may be quoted, as in Digest and Hawk, or hold
// a `;`, as in AWS SigV4's `SignedHeaders=host;x-amz-date`) or any other opaque
// credential up to the next delimiter (a token68, `AWS AKIA...:c2ln...`).
// A value that is already the marker stays as it is: redacting a redacted line
// again must not take the word after it for a credential.
const AUTH_PARAM = String.raw`[A-Za-z0-9_-]+[ \t]*=[ \t]*(?:"(?:[^"\\]|\\.)*"|[^\s,'"<>\\]+)`;
const AUTHORIZATION_CREDENTIALS = new RegExp(
    String.raw`\bauthorization["']?[ \t]*[:=][ \t]*(?:\*\*\*REDACTED\*\*\*|[^\s,;'"<>\\]+(?:[ \t]+(?:${AUTH_PARAM}(?:[ \t]*,[ \t]*${AUTH_PARAM})*|[^\s,'"<>\\]+))?)`,
    'gi',
);

// Names of the fields whose value is a secret, in JSON and in key=value form,
// written in snake_case, kebab-case or camelCase (`access_token`, `api-key`,
// `clientSecret`). A name may follow snake or kebab words (`session_token`,
// `prod_db_admin_user_password`) but not an open prefix: `nextToken`, `tokens`
// and `bypass` stay. The prefix is bounded on purpose, 8 segments of at most 24
// characters: `-` is not a word character, so an unbounded prefix would start a
// match after every hyphen and cost a quadratic scan on a long kebab run (the
// class of the email rule below). The rules are case-insensitive; the Rust twin
// writes the same text.
const SECRET_NAME = String.raw`(?:(?:[A-Za-z0-9]{1,24}[_-]){0,8}(?:password|passwd|passphrase|pwd|secret(?:[_-]?(?:access[_-]?)?key)?|api[_-]?key|access[_-]?key|(?:access|auth|refresh|id)[_-]?token|token|private[_-]?key|(?:client|totp|consumer)[_-]?secret)|pass)`;

// The value after a key: the marker (a redacted line stays as it is), a string
// whose quotes are escaped (`\"v\"`, from a Debug or JSON dump of the line), a
// quoted string that ends at its closing quote (an escaped quote does not end
// it; a line cut inside the value has no closing quote and ends there), or a
// bare word up to the next delimiter.
const SECRET_VALUE = String.raw`(?:\*\*\*REDACTED\*\*\*|\\"${ESCAPED_BODY}(?:\\")?|"(?:[^"\\]|\\.)*"?|'(?:[^'\\]|\\.)*'?|[^\s,;'"<>]+)`;

// The rules for credentials only. `REDACTION_PATTERNS` holds them and the
// identity rules; `redactCredentials` applies them without the identity rules.
/**
 * A rule: a pattern and its replacement (`$1`, the kept label, is the only
 * reference), or a scan that gives its spans and one replacement for each.
 */
export type RedactionRule = (
    | { pattern: RegExp; replacement: string }
    | { spans: (text: string) => Array<[number, number]>; replacement: string }
) & {
    /** An identity rule (an address, a home folder, a hash), not a
     *  credential: see `applyRules`. */
    identity?: true;
    /** The e-mail rule: its spans know where their `@` is (U1). */
    email?: true;
    /** The URL userinfo scan (U1). */
    userinfo?: true;
};

/** A part of the original text a rule masks, and what replaces it; an
 *  e-mail span also knows its `@`, a URL userinfo span that it is one. */
export type Span = { start: number; end: number; replacement: string; at?: number; userinfo?: true };

/**
 * The spans one rule masks, read on the ORIGINAL text (T3). A match keeps the
 * leading part of it the replacement repeats (a field's label, `$1`), so that
 * part is not a span: the label stays visible, as it did.
 */
const spansOf = (text: string, rule: RedactionRule): Span[] => {
    if ('spans' in rule) {
        return rule.spans(text).map(([start, end]) => ({ start, end, replacement: rule.replacement }));
    }
    const spans: Span[] = [];
    for (const match of text.matchAll(rule.pattern)) {
        const replaced = rule.replacement.replace(/\$1/g, () => match[1] ?? '');
        const whole = match[0];
        let kept = 0;
        while (kept < whole.length && kept < replaced.length && whole[kept] === replaced[kept]) kept += 1;
        const start = (match.index ?? 0) + kept;
        const span = { start, end: (match.index ?? 0) + whole.length, replacement: replaced.slice(kept) };
        if (span.end > span.start || span.replacement) spans.push(span);
    }
    return spans;
};

/**
 * The identity spans the credentials do not cover ENTIRELY (`applyRules`,
 * U1): one covered only in part is kept and joins the union, so nothing an
 * identity rule hid becomes visible. One named exception, the one the fixed
 * point needs: an e-mail span whose local part lies inside a URL userinfo
 * span that ends at its `@` (`https://token@host`) is dropped; the userinfo
 * already masks the secret, and the host stays. Linear in the spans after
 * the sort: the credentials are merged once into sorted disjoint ranges,
 * then the identity spans are walked in start order with one index that
 * only moves forward; the userinfo spans are found by their end. `steps`,
 * for the tests, counts the comparisons.
 */
export const uncoveredIdentity = (
    identity: ReadonlyArray<Span>,
    credentials: ReadonlyArray<Span>,
    steps = { count: 0 },
): Span[] => {
    const covered: Array<[number, number]> = [];
    const userinfoEnds = new Map<number, number>();
    for (const span of [...credentials].sort((a, b) => a.start - b.start)) {
        steps.count += 1;
        if (span.userinfo) userinfoEnds.set(span.end, Math.min(span.start, userinfoEnds.get(span.end) ?? span.start));
        if (span.end <= span.start) continue;
        const last = covered[covered.length - 1];
        if (last && span.start <= last[1]) last[1] = Math.max(last[1], span.end);
        else covered.push([span.start, span.end]);
    }
    const kept: Span[] = [];
    let next = 0;
    for (const span of [...identity].sort((a, b) => a.start - b.start)) {
        while (next < covered.length && covered[next][1] <= span.start) {
            steps.count += 1;
            next += 1;
        }
        steps.count += 1;
        const inside = next < covered.length && covered[next][0] <= span.start && span.end <= covered[next][1];
        const userinfoStart = span.at === undefined ? undefined : userinfoEnds.get(span.at);
        const mailInUserinfo = userinfoStart !== undefined && userinfoStart <= span.start;
        if (!inside && !mailInUserinfo) kept.push(span);
    }
    return kept;
};

/**
 * Every rule's spans, all read on the original text, masked once (T3). No
 * rule sees another's output, so none can consume a delimiter another needs,
 * in either direction (a `key: value` rule eating a URL's `@`, the URL scan
 * eating a `password=` label), and the order of the rules changes nothing.
 * - Credentials (every rule but the identity ones): the masked part is the
 *   UNION of their spans, so a credential any rule finds is masked whole.
 * - Identity rules (addresses, home folders, hashes): a span of theirs that
 *   the credentials cover entirely is dropped; one they cover in part joins
 *   the union; an e-mail-shaped `token@host` whose local part is a URL's
 *   userinfo is that URL's, masked as such, and its host stays
 *   (`uncoveredIdentity`, U1).
 * Spans that overlap merge into one, replaced by the replacement of the
 * span that starts first; on equal starts, of the longer one; then of the
 * replacement that sorts first.
 */
export const applyRules = (text: string, rules: ReadonlyArray<RedactionRule>): string => {
    const credentials = rules
        .filter((rule) => !rule.identity)
        .flatMap((rule) => spansOf(text, rule).map((span) => (rule.userinfo ? { ...span, userinfo: true as const } : span)));
    const identity = uncoveredIdentity(
        rules
            .filter((rule) => rule.identity)
            .flatMap((rule) => spansOf(text, rule).map((span) => (rule.email ? { ...span, at: text.indexOf('@', span.start) } : span))),
        credentials,
    );
    const spans = [...credentials, ...identity]
        .sort((a, b) => a.start - b.start || b.end - a.end
            || (a.replacement < b.replacement ? -1 : a.replacement > b.replacement ? 1 : 0));
    let out = '';
    let copied = 0;
    for (let n = 0; n < spans.length;) {
        const first = spans[n];
        let end = first.end;
        // Touching spans merge too (U1): two masks side by side would read
        // as a home folder again on a second pass.
        for (n += 1; n < spans.length && spans[n].start <= end; n += 1) end = Math.max(end, spans[n].end);
        out += text.slice(copied, first.start) + first.replacement;
        copied = end;
    }
    return out + text.slice(copied);
};

/** Whether the whole text is one valid JSON value, by the real parser
 *  (validation only: nothing is re-serialized, the output keeps the input). */
const isJsonDocument = (text: string): boolean => {
    try {
        JSON.parse(text);
        return true;
    } catch {
        return false;
    }
};

/** In valid JSON, the index of the quote that ends the string token whose
 *  content starts at `from` (a backslash escapes the next character), and
 *  whether the token holds an escape. */
const scanStringToken = (text: string, from: number): [end: number, escaped: boolean] => {
    let k = from;
    let escaped = false;
    while (k < text.length) {
        const code = text.charCodeAt(k);
        if (code === 92) {
            escaped = true;
            k += 2;
            continue;
        }
        if (code === 34) return [k, escaped];
        k += 1;
    }
    return [text.length, escaped];
};

/** The content of the valid JSON string token `[from, end)`, decoded as
 *  `JSON.parse` decodes it, and for each decoded code unit the index of the
 *  raw character or escape it comes from (one more entry: `end`). */
const decodeStringToken = (text: string, from: number, end: number): [decoded: string, raw: number[]] => {
    const units: string[] = [];
    const raw: number[] = [];
    let k = from;
    while (k < end) {
        raw.push(k);
        if (text.charCodeAt(k) !== 92) {
            units.push(text[k]);
            k += 1;
            continue;
        }
        const escape = text[k + 1];
        if (escape === 'u') {
            units.push(String.fromCharCode(parseInt(text.slice(k + 2, k + 6), 16)));
            k += 6;
            continue;
        }
        units.push({ b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' }[escape] ?? escape);
        k += 2;
    }
    raw.push(end);
    return [units.join(''), raw];
};

const BODY_ESCAPES: Record<string, string> = { '"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' };

/** Text that is not JSON as a whole, read as ONE string body (X2): the
 *  escapes JSON defines (`\/`, `\\`, `\"`, the letters, `\uXXXX` with
 *  four hex digits) are decoded, each decoded code unit mapped to the raw
 *  escape or character it comes from (one more entry: the end); any other
 *  backslash is literal. One pass, linear. `null` when nothing decodes,
 *  which ends the recursion. */
const decodeBody = (text: string): [decoded: string, raw: number[]] | null => {
    const units: string[] = [];
    const raw: number[] = [];
    let changed = false;
    let k = 0;
    while (k < text.length) {
        raw.push(k);
        if (text.charCodeAt(k) === 92 && k + 1 < text.length) {
            const escape = text[k + 1];
            const simple = BODY_ESCAPES[escape];
            if (simple !== undefined) {
                units.push(simple);
                k += 2;
                changed = true;
                continue;
            }
            const hex = text.slice(k + 2, k + 6);
            if (escape === 'u' && /^[0-9a-fA-F]{4}$/.test(hex)) {
                units.push(String.fromCharCode(parseInt(hex, 16)));
                k += 6;
                changed = true;
                continue;
            }
        }
        units.push(text[k]);
        k += 1;
    }
    raw.push(text.length);
    return changed ? [units.join(''), raw] : null;
};

/** The userinfo spans `[start, end)` of `text`, in order (see
 *  `redactUrlUserinfo`). */
const userinfoSpans = (text: string): Array<[number, number]> => {
    const json = isJsonDocument(text);
    const spans: Array<[number, number]> = [];
    if (!json) {
        // X2: no whole-document JSON gates a mask. Text that is not JSON as
        // a whole (a prefix before a JSON dump, a suffix after it, a trailing
        // comma, single quotes) is also scanned decoded, as one string body,
        // by these same rules, and the spans are mapped back; the raw scan
        // below still runs, so decoding never masks less.
        const body = decodeBody(text);
        if (body) {
            const [decoded, raw] = body;
            for (const [a, b] of userinfoSpans(decoded)) spans.push([raw[a], raw[b]]);
        }
    }
    let i = 0;
    // The string token the scan is in (valid JSON only): its closing quote.
    let tokenEnd = -1;
    while (i + 3 <= text.length) {
        const code = text.charCodeAt(i);
        if (json && i > tokenEnd && code === 34) {
            const [end, escaped] = scanStringToken(text, i + 1);
            tokenEnd = end;
            if (escaped) {
                // What the token says, mapped back onto the raw characters
                // and escapes; the raw scan below still runs on the token
                // too, so decoding never masks less (an escaped newline ends
                // the decoded authority, not the raw one).
                const [decoded, raw] = decodeStringToken(text, i + 1, end);
                for (const [a, b] of userinfoSpans(decoded)) spans.push([raw[a], raw[b]]);
            }
            i += 1;
            continue;
        }
        if (code !== 58 || text.charCodeAt(i + 1) !== 47 || text.charCodeAt(i + 2) !== 47) {
            i += 1;
            continue;
        }
        const authority = i + 3;
        const limit = json && i < tokenEnd ? tokenEnd : text.length;
        let k = authority;
        let lastAt = -1;
        while (k < limit) {
            const c = text.charCodeAt(k);
            if (c === 47 || c === 63 || c === 35) break;
            if (c === 64) lastAt = k;
            k += 1;
        }
        if (lastAt < 0) {
            // No userinfo: go on right after this `://`. Nothing between it
            // and the authority's end holds a `://` that matters twice, so
            // each character is read at most twice.
            i = authority;
            continue;
        }
        spans.push([authority, lastAt]);
        i = lastAt + 1;
    }
    return spans;
};

/**
 * Masks the userinfo of every URL, in time linear in the text for each
 * level of escaping. Any `://` counts, whatever scheme comes before it
 * (`s3://`, `postgres://`, none at all): the scope is every URL, and a
 * `://` that is not one only over-masks (S1). With an `@` in the authority,
 * the userinfo is everything up to the LAST `@`, and all of it becomes the
 * placeholder, user name included: a token is often the user name
 * (`ghp_x:x-oauth-basic@`, `token:@`). An authority without `@`
 * (`host:443`) stays.
 *
 * Where the authority ends (D1). Structure is never inferred from
 * characters; the real parser decides once whether the WHOLE text is JSON
 * (which decides only where an authority ends, never whether escapes are
 * decoded):
 * - valid JSON (`JSON.parse`): a plain string tokenizer, exact on valid
 *   JSON, gives each URL the string token it sits in. The authority ends at
 *   `/`, `?`, `#`, or the end of that token; inside the token no quote ends
 *   it (they are all escaped content);
 * - anything else is plain text: no quote ever ends the authority, only
 *   `/`, `?`, `#` or the end of the text.
 * Whitespace does not end it either (S1b): a credential with a space in it
 * (`user:x-api-key: foo@host`, `bob:pass word@host`) is not a valid URL,
 * yet a log can hold one, and the part before the space would leak.
 * A string token of valid JSON that holds an escape (`\/`, `\u0040`,
 * `\"`...) is also scanned DECODED, by these same rules, so a document
 * serialized inside it is decoded in turn, once per level (S1). The
 * userinfo found there is mapped back onto the raw characters and escapes
 * it was written with. Text that is not JSON as a whole is scanned decoded
 * too, all of it read as one string body (X2): `log: {"url":"https:\/\/
 * bob:pw@host"}` is masked although no `://` is written in it. Malformed
 * escapes are literal. Each level is one linear pass and the recursion
 * stops at the first level that decodes nothing; a level is shorter than
 * the one before by one character per escape, so a line full of
 * backslashes, halved at every level, costs twice its length in all. The
 * raw text is scanned as well, and the union of both is masked, so
 * decoding never masks less than the raw text did.
 * Accepted consequence: a URL with no path followed by a later `@` before
 * the next `/`, `?`, `#` (in its JSON token, or anywhere in text that is
 * not JSON as a whole) over-masks into what follows, the words and fields
 * between them included. Nothing can leak: leaking is the only failure that
 * matters. No `key=` / `key:` rule remains: in plain text the quote around
 * such a value never ends the authority, and the `/` after it does.
 * The Rust twin is `redact_url_userinfo` in src-tauri/src/debug_tests.rs;
 * both run src/shared/redaction-cases.json.
 */
export function redactUrlUserinfo(text: string): string {
    // The decoded and the raw scan of a token can find the same userinfo, or
    // overlapping ones: their union is masked once.
    const spans = userinfoSpans(text).sort((x, y) => x[0] - y[0]);
    let out = '';
    let copied = 0;
    for (let n = 0; n < spans.length;) {
        const start = spans[n][0];
        let end = spans[n][1];
        for (n += 1; n < spans.length && spans[n][0] < end; n += 1) end = Math.max(end, spans[n][1]);
        out += text.slice(copied, start) + '***REDACTED***';
        copied = end;
    }
    return out + text.slice(copied);
}

const CREDENTIAL_PATTERNS: ReadonlyArray<RedactionRule> = [
    // Inline credentials in URLs: a linear scan, not a pattern (see
    // `redactUrlUserinfo`). Read on the original text like every rule, so no
    // other rule can take its `@` and it takes no other rule's label (T3).
    { spans: userinfoSpans, replacement: '***REDACTED***', userinfo: true },
    // Authorization (and Proxy-Authorization) carries `scheme credentials`, and
    // the whole value goes, not its first word: `Basic dXNlcjpwYXNz` is a
    // password. Its span covers the whole value, so `Authorization: Bearer
    // sk-...` becomes one marker: the Bearer and key spans inside it merge
    // into it, which starts first (T3). The Rust scrubs apply
    // the same three rules (src-tauri/src/authorization_redaction.rs).
    // JSON inside a string: the value ends at the first lone `\"`, and a Digest
    // parameter's `\\\"` belongs to it.
    { pattern: new RegExp(String.raw`(\\"authorization\\"[ \t]*:[ \t]*\\")${ESCAPED_BODY}`, 'gi'), replacement: '$1***REDACTED***' },
    // A quoted value (JSON, Debug) ends at its closing quote, so the keys after
    // it stay; a line cut inside the value has no closing quote and ends there.
    // A value whose quotes are escaped (`authorization=\"Basic ...\"`) is read first.
    {
        pattern: new RegExp(String.raw`(\bauthorization["']?[ \t]*[:=][ \t]*)(?:\\"${ESCAPED_BODY}(?:\\")?|"(?:[^"\\]|\\.)*"?|'[^']*'?)`, 'gi'),
        replacement: '$1"***REDACTED***"',
    },
    { pattern: AUTHORIZATION_CREDENTIALS, replacement: 'authorization: ***REDACTED***' },
    // API keys (Anthropic, OpenAI projects, generic sk-*)
    { pattern: /sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}/g, replacement: 'sk-***REDACTED***' },
    { pattern: /sk_(live|test)_[A-Za-z0-9]{16,}/g, replacement: 'sk_***REDACTED***' },
    // OAuth Bearer / x-api-key headers
    { pattern: /\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*/g, replacement: 'Bearer ***REDACTED***' },
    { pattern: new RegExp(String.raw`\bx-api-key\s*[:=]\s*${SECRET_VALUE}`, 'gi'), replacement: 'x-api-key: ***REDACTED***' },
    // JWT-shaped tokens (3 base64url segments separated by '.')
    { pattern: /\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}/g, replacement: '***JWT-REDACTED***' },
    // Serialized secret fields in JSON: "password":"..." -> "password":"***REDACTED***".
    // The value ends at its closing quote; an escaped quote inside it does not end it.
    {
        pattern: new RegExp(String.raw`("${SECRET_NAME}"\s*:\s*)"(?:[^"\\]|\\.)*"?`, 'gi'),
        replacement: '$1"***REDACTED***"',
    },
    // The same inside a JSON string, where each quote is escaped: the value ends
    // at the first lone `\"`, and an inner `\\\"` belongs to it.
    {
        pattern: new RegExp(String.raw`(\\"${SECRET_NAME}\\"[ \t]*:[ \t]*\\")${ESCAPED_BODY}`, 'gi'),
        replacement: '$1***REDACTED***',
    },
    // Secret fields in key=value / key: value form
    {
        pattern: new RegExp(String.raw`(\b${SECRET_NAME}\s*[:=]\s*)${SECRET_VALUE}`, 'gi'),
        replacement: '$1***REDACTED***',
    },
];

export const REDACTION_PATTERNS: ReadonlyArray<RedactionRule> = [
    ...CREDENTIAL_PATTERNS,
    // Email addresses. The local part is at most 64 characters (RFC 5321), which
    // also keeps a long run of such characters without an `@` (a hex or base64url
    // dump) from costing a quadratic scan on every start position.
    { pattern: /[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, replacement: '***@***', identity: true, email: true },
    // IPv4 addresses, but keep loopback / unspecified for local-debug clarity
    {
        pattern: /\b(?!127\.0\.0\.1\b|0\.0\.0\.0\b)((?:\d{1,3}\.){3}\d{1,3})\b/g,
        replacement: '***.***.***.***',
        identity: true,
    },
    // Home directories
    { pattern: /\/home\/[A-Za-z0-9._-]+/g, replacement: '/home/***', identity: true },
    { pattern: /\/Users\/[A-Za-z0-9._-]+/g, replacement: '/Users/***', identity: true },
    // The account's folder name, greedy to the next backslash, as on main,
    // so a name with a space (`John Smith`) is masked whole. A credential
    // it runs into is only partly covered, so the span is kept and joins
    // the union (U1). A line break ends it.
    { pattern: /C:\\Users\\[^\\\r\n]+/gi, replacement: 'C:\\Users\\***', identity: true },
    // Generic high-entropy hex blobs (likely keys / hashes / nonces), 32+ chars
    { pattern: /\b[A-Fa-f0-9]{32,}\b/g, replacement: '***HEX-REDACTED***', identity: true },
];

export function redactSensitive(msg: string): string {
    if (!msg) return msg;
    return applyRules(msg, REDACTION_PATTERNS);
}

// Signed URL parameters and the userinfo of any URL. They are not part of
// `REDACTION_PATTERNS`, whose rules the Rust twin mirrors. `key` and `sig` go
// only as a query parameter, so an object key named in the error text stays.
const SIGNED_URL_PATTERNS: ReadonlyArray<RedactionRule> = [
    { pattern: /\b(X-Amz-(?:Signature|Credential|Security-Token)|signature)=[^&\s"'<>]+/gi, replacement: '$1=***REDACTED***' },
    { pattern: /([?&](?:key|sig))=[^&\s"'<>]+/gi, replacement: '$1=***REDACTED***' },
    // The userinfo of a URL of any scheme is the shared rule's, in
    // `CREDENTIAL_PATTERNS` (S1): both entry points mask the same URLs.
];

/**
 * Masks credentials in an error text a user will read (a sync journal, report
 * or status line) and keeps the rest of it: paths, hosts, codes and the error
 * wording. Unlike `redactSensitive`, emails, addresses, home folders and hashes stay.
 */
export const CREDENTIAL_RULES: ReadonlyArray<RedactionRule> = [...CREDENTIAL_PATTERNS, ...SIGNED_URL_PATTERNS];

export function redactCredentials(msg: string): string {
    if (!msg) return msg;
    return applyRules(msg, CREDENTIAL_RULES);
}
