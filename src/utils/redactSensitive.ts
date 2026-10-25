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
// RFC 7235 `credentials`: a scheme, then a token68 or a comma-separated list
// of `name=value` parameters whose values may be quoted (Digest, Hawk, ...) or
// hold a `;` (AWS SigV4: `SignedHeaders=host;x-amz-date`).
// A value that is already the marker stays as it is: redacting a redacted line
// again must not take the word after it for a credential.
const AUTH_PARAM = String.raw`[A-Za-z0-9_-]+[ \t]*=[ \t]*(?:"(?:[^"\\]|\\.)*"|[^\s,'"<>\\]+)`;
const AUTHORIZATION_CREDENTIALS = new RegExp(
    String.raw`\bauthorization["']?[ \t]*[:=][ \t]*(?:\*\*\*REDACTED\*\*\*|[^\s,;'"<>\\]+(?:[ \t]+(?:${AUTH_PARAM}(?:[ \t]*,[ \t]*${AUTH_PARAM})*|[A-Za-z0-9._~+/-]+=*))?)`,
    'gi',
);

export const REDACTION_PATTERNS: ReadonlyArray<{ pattern: RegExp; replacement: string }> = [
    // API keys (Anthropic, OpenAI projects, generic sk-*)
    { pattern: /sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}/g, replacement: 'sk-***REDACTED***' },
    { pattern: /sk_(live|test)_[A-Za-z0-9]{16,}/g, replacement: 'sk_***REDACTED***' },
    // Authorization (and Proxy-Authorization) carries `scheme credentials`, and
    // the whole value goes, not its first word: `Basic dXNlcjpwYXNz` is a
    // password. Ahead of the Bearer rule, so `Authorization: Bearer <token>`
    // becomes one marker. The Rust twin applies the same three rules.
    // JSON inside a string: the value ends at the first lone `\"`, and a Digest
    // parameter's `\\\"` belongs to it.
    { pattern: /(\\"authorization\\"[ \t]*:[ \t]*\\")(?:[^\\]|\\\\\\")*/gi, replacement: '$1***REDACTED***' },
    // A quoted value (JSON, Debug) ends at its closing quote, so the keys after
    // it stay; a line cut inside the value has no closing quote and ends there.
    { pattern: /(\bauthorization["']?[ \t]*[:=][ \t]*)(?:"(?:[^"\\]|\\.)*"?|'[^']*'?)/gi, replacement: '$1"***REDACTED***"' },
    { pattern: AUTHORIZATION_CREDENTIALS, replacement: 'authorization: ***REDACTED***' },
    // OAuth Bearer / x-api-key headers
    { pattern: /\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*/g, replacement: 'Bearer ***REDACTED***' },
    { pattern: /\bx-api-key\s*[:=]\s*[^\s,;'"<>]+/gi, replacement: 'x-api-key: ***REDACTED***' },
    // JWT-shaped tokens (3 base64url segments separated by '.')
    { pattern: /\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}/g, replacement: '***JWT-REDACTED***' },
    // Inline credentials in URLs: ftp://user:pass@host -> ftp://user:***@host
    { pattern: /\b((?:ftps?|sftp|https?|webdav):\/\/[^:\s@/]+:)[^@\s]+(@)/gi, replacement: '$1***REDACTED***$2' },
    // Serialized secret fields in JSON: "password":"..." -> "password":"***REDACTED***"
    {
        pattern:
            /("(?:password|passwd|pwd|secret|secret_key|secretkey|api_key|apikey|access_key|token|access_token|refresh_token|id_token|passphrase|private_key|privatekey|client_secret|totp_secret|consumer_secret)"\s*:\s*)"[^"]*"/gi,
        replacement: '$1"***REDACTED***"',
    },
    // Secret fields in key=value / key: value form
    {
        pattern:
            /(\b(?:pass(?:word|wd|phrase)?|pwd|secret(?:_?key)?|client_secret|totp_secret|consumer_secret|private_?key|api[_-]?key|access_key|(?:access|refresh|id)_token|token)\s*[:=]\s*)[^\s,;'"<>]+/gi,
        replacement: '$1***REDACTED***',
    },
    // Email addresses
    { pattern: /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, replacement: '***@***' },
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
    for (const { pattern, replacement } of REDACTION_PATTERNS) {
        result = result.replace(pattern, replacement);
    }
    return result;
}
