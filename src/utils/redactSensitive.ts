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

const URL_CREDENTIAL_SCHEMES = new Set(['ftp', 'ftps', 'sftp', 'scp', 'http', 'https', 'webdav']);
const isWordChar = (code: number) => (code >= 48 && code <= 57) || (code >= 65 && code <= 90)
    || (code >= 97 && code <= 122) || code === 95;
const isAsciiLetter = (code: number) => (code >= 65 && code <= 90) || (code >= 97 && code <= 122);
const isSpace = (code: number) => code === 32 || (code >= 9 && code <= 13);
const isSchemeChar = (code: number) => isAsciiLetter(code) || (code >= 48 && code <= 57)
    || code === 43 || code === 45 || code === 46;
/** Backslashes that run back from just before `end`. */
const backslashesBefore = (text: string, end: number): number => {
    let n = 0;
    while (end - 1 - n >= 0 && text.charCodeAt(end - 1 - n) === 92) n += 1;
    return n;
};

/**
 * The escape level of a quote preceded by `run` backslashes: 0 a real quote,
 * 1 a quote of JSON inside a string (`\"`), 2 one more level down
 * (`\\\"`). Each odd run is one escape; an even run is escaped backslashes
 * before a quote of that level (`\\"` is level 0).
 */
const quoteLevel = (run: number): number => {
    let level = 0;
    while (run % 2 === 1) {
        level += 1;
        run = (run - 1) / 2;
    }
    return level;
};

/**
 * Masks the userinfo of `ftp`, `ftps`, `sftp`, `scp`, `http(s)` and `webdav`
 * URLs in one pass over the text, so its time is linear whatever the input.
 * After `scheme://` the authority runs to the first `/`, `?`, `#`, ASCII
 * space or unescaped double quote (a backslash escapes the next character,
 * as inside a JSON string). Which quote ends it is decided by the string the
 * URL sits in (Q3, the whole rule; the cases of
 * src/shared/redaction-cases.json cover each cell):
 *
 * | the URL starts                           | its level | a quote ends the authority   |
 * |------------------------------------------|-----------|------------------------------|
 * | in bare text, no quote before it         | 0         | `"` (level 0)                |
 * | after a `"` (plain JSON), at its start   | 0         | `"`; `\"` is the value's     |
 * | after a `"`, text before it (mid-string) | 0         | `"`; `\"` is the value's     |
 * | after a `\"` (JSON in a string), start   | 1         | `\"` or `"`; `\\\"` the value's |
 * | after a `\"`, mid-string                 | 1         | `\"` or `"`; `\\\"` the value's |
 * | after a `\\\"` (two levels down)          | 2         | a quote of level 2 or less   |
 *
 * The level of the string is the level of the LAST quote before the scheme
 * (the one that opened it, or a closing one when the URL is outside strings),
 * leaving out the quotes inside an authority already read. A quote ends the
 * authority when its own level is that level or less: a deeper one is part
 * of the value. Nothing else about what follows a quote is read, so a
 * password holding `\",`, `\":` or `\"}` is masked whole.
 * With an `@` in it, the userinfo is everything up
 * to the LAST `@`, and all of it becomes the placeholder, user name included:
 * a token is often the user name (`ghp_x:x-oauth-basic@`, `token:@`). An
 * authority without `@` (`host:443`) stays.
 * The Rust twin is `redact_url_userinfo` in src-tauri/src/debug_tests.rs.
 */
export function redactUrlUserinfo(text: string, anyScheme = false): string {
    let out = '';
    let copied = 0;
    let i = 0;
    // The level of the string the scan is in: the last quote read, outside
    // the authorities (`untracked`).
    let level = 0;
    let untracked = 0;
    while (i + 3 <= text.length) {
        if (text.charCodeAt(i) === 34 && i >= untracked) {
            level = quoteLevel(backslashesBefore(text, i));
        }
        if (text.charCodeAt(i) !== 58 || text.charCodeAt(i + 1) !== 47 || text.charCodeAt(i + 2) !== 47) {
            i += 1;
            continue;
        }
        // `anyScheme` (redactCredentials): any scheme name, `s3://` too, up
        // to 32 characters back, so the scan stays linear.
        let start = i;
        const reach = anyScheme ? 32 : 7;
        const schemeChar = anyScheme ? isSchemeChar : isAsciiLetter;
        while (start > 0 && i - start < reach && schemeChar(text.charCodeAt(start - 1))) start -= 1;
        while (anyScheme && start < i && !isAsciiLetter(text.charCodeAt(start))) start += 1;
        const bounded = start === 0 || !isWordChar(text.charCodeAt(start - 1));
        const named = anyScheme ? start < i : URL_CREDENTIAL_SCHEMES.has(text.slice(start, i).toLowerCase());
        if (!bounded || !named) {
            i += 3;
            continue;
        }
        const authority = i + 3;
        let k = authority;
        let run = 0;
        let lastAt = -1;
        while (k < text.length) {
            const code = text.charCodeAt(k);
            if (code === 47 || code === 63 || code === 35 || isSpace(code)) break;
            if (code === 34 && quoteLevel(run) <= level) break;
            if (code === 64) lastAt = k;
            run = code === 92 ? run + 1 : 0;
            k += 1;
        }
        // The quotes of this authority are the value's: they never set the
        // level for a later URL.
        untracked = k;
        if (lastAt < 0) {
            // No userinfo: go on right after this `://`. The authority ended
            // at a `/`, and a scheme whose `://` starts just before it (one
            // past an escaped JSON quote, `\",\"b\":\"https://`) is still
            // scanned (L7). Nothing between holds a `://`, so each character
            // is read at most twice.
            i = authority;
            continue;
        }
        out += text.slice(copied, authority) + '***REDACTED***';
        copied = lastAt;
        i = lastAt + 1;
    }
    return out + text.slice(copied);
}

const CREDENTIAL_PATTERNS: ReadonlyArray<RedactionRule> = [
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
    // Inline credentials in URLs: a linear scan, not a pattern (see
    // `redactUrlUserinfo`).
    { apply: redactUrlUserinfo },
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
    // The whole userinfo of a URL of any scheme (`s3://` too), by the same
    // linear scanner as the shared rule: never past the authority, a quote
    // or a JSON field boundary (T11).
    { apply: (text: string) => redactUrlUserinfo(text, true) },
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
