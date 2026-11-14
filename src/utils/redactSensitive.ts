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

// The rules for credentials only. `REDACTION_PATTERNS` starts with them, in
// this order, and `redactCredentials` applies them without the identity rules.
/** A rule: a pattern and its replacement, or a function of the whole text. */
type RedactionRule = { pattern: RegExp; replacement: string } | { apply: (text: string) => string };

const applyRule = (text: string, rule: RedactionRule): string =>
    'apply' in rule ? rule.apply(text) : text.replace(rule.pattern, rule.replacement);

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

/** The userinfo spans `[start, end)` of `text`, in order (see
 *  `redactUrlUserinfo`). */
const userinfoSpans = (text: string): Array<[number, number]> => {
    const json = isJsonDocument(text);
    const spans: Array<[number, number]> = [];
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
 * characters; the real parser decides once whether the WHOLE text is JSON:
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
 * it was written with. The token is scanned raw as well, and the union of
 * both is masked, so decoding never masks less than the raw text did.
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
    // Inline credentials in URLs, FIRST of all (S1b): a later rule can
    // consume a delimiter of the authority (`x-api-key: foo@host` eats the
    // `@`, `password=abc@host/x` the `@` and the `/`), and the userinfo
    // before it would then stay. A linear scan, not a pattern (see
    // `redactUrlUserinfo`); the Rust list starts with it too.
    { apply: redactUrlUserinfo },
    // Authorization (and Proxy-Authorization) carries `scheme credentials`, and
    // the whole value goes, not its first word: `Basic dXNlcjpwYXNz` is a
    // password. First of all, ahead of the key and Bearer rules, so
    // `Authorization: Bearer sk-...` becomes one marker. The Rust scrubs apply
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
    { pattern: /[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, replacement: '***@***' },
    // IPv4 addresses, but keep loopback / unspecified for local-debug clarity
    {
        pattern: /\b(?!127\.0\.0\.1\b|0\.0\.0\.0\b)((?:\d{1,3}\.){3}\d{1,3})\b/g,
        replacement: '***.***.***.***',
    },
    // Home directories
    { pattern: /\/home\/[A-Za-z0-9._-]+/g, replacement: '/home/***' },
    { pattern: /\/Users\/[A-Za-z0-9._-]+/g, replacement: '/Users/***' },
    { pattern: /C:\\Users\\[^\\]+/gi, replacement: 'C:\\Users\\***' },
    // Generic high-entropy hex blobs (likely keys / hashes / nonces), 32+ chars
    { pattern: /\b[A-Fa-f0-9]{32,}\b/g, replacement: '***HEX-REDACTED***' },
];

export function redactSensitive(msg: string): string {
    if (!msg) return msg;
    let result = msg;
    for (const rule of REDACTION_PATTERNS) {
        result = applyRule(result, rule);
    }
    return result;
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
export function redactCredentials(msg: string): string {
    if (!msg) return msg;
    let result = msg;
    for (const rule of [...CREDENTIAL_PATTERNS, ...SIGNED_URL_PATTERNS]) {
        result = applyRule(result, rule);
    }
    return result;
}
