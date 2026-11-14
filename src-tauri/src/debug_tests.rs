// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Diagnostic test runner for the in-app DebugPanel.
//!
//! Each Tauri command implements one self-contained probe. They all return
//! the same `TestResult` shape so the frontend can render them uniformly.
//! Operations that touch a remote server (provider_selftest) live inside a
//! dedicated `aeroftp-self-test/` namespace and clean up after themselves.

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::sync::OnceLock;
use std::time::Instant;

use crate::authorization_redaction;
use crate::credential_store::CredentialStore;
use crate::AppState;

#[derive(Serialize, Clone)]
pub struct TestResult {
    pub status: String, // "pass" | "fail" | "warn" | "skipped"
    pub duration_ms: u64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl TestResult {
    fn elapsed(t0: Instant) -> u64 {
        t0.elapsed().as_millis() as u64
    }

    fn pass(t0: Instant, msg: impl Into<String>) -> Self {
        Self {
            status: "pass".into(),
            duration_ms: Self::elapsed(t0),
            message: msg.into(),
            details: None,
        }
    }

    fn warn(t0: Instant, msg: impl Into<String>) -> Self {
        Self {
            status: "warn".into(),
            duration_ms: Self::elapsed(t0),
            message: msg.into(),
            details: None,
        }
    }

    fn fail(t0: Instant, msg: impl Into<String>) -> Self {
        Self {
            status: "fail".into(),
            duration_ms: Self::elapsed(t0),
            message: msg.into(),
            details: None,
        }
    }

    fn skipped(t0: Instant, msg: impl Into<String>) -> Self {
        Self {
            status: "skipped".into(),
            duration_ms: Self::elapsed(t0),
            message: msg.into(),
            details: None,
        }
    }

    #[allow(dead_code)]
    fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

// ─── T1: active connection probe (FTP NOOP) ────────────────────────────────

#[tauri::command]
pub async fn debug_test_connectivity(
    state: tauri::State<'_, AppState>,
) -> Result<TestResult, String> {
    let t0 = Instant::now();
    let mut ftp = state.ftp_manager.lock().await;
    if !ftp.is_connected() {
        return Ok(TestResult::skipped(
            t0,
            "No active FTP/SFTP session to probe",
        ));
    }
    match ftp.noop().await {
        Ok(_) => Ok(TestResult::pass(t0, "Active session is healthy (NOOP OK)")),
        Err(e) => Ok(TestResult::fail(t0, format!("NOOP failed: {}", e))),
    }
}

// ─── T2: vault round-trip ──────────────────────────────────────────────────

#[tauri::command]
pub async fn debug_test_vault_roundtrip() -> Result<TestResult, String> {
    let t0 = Instant::now();
    let store = match CredentialStore::from_cache() {
        Some(s) => s,
        None => {
            return Ok(TestResult::skipped(
                t0,
                "Vault locked: unlock to run this test",
            ))
        }
    };

    let key = format!("__aeroftp_debug_test_roundtrip_{}__", uuid::Uuid::new_v4());
    let value = "round_trip_test_value_OK";

    if let Err(e) = store.store(&key, value) {
        return Ok(TestResult::fail(t0, format!("Write failed: {}", e)));
    }

    let read_value = match store.get(&key) {
        Ok(v) => v,
        Err(e) => {
            let _ = store.delete(&key);
            return Ok(TestResult::fail(t0, format!("Read failed: {}", e)));
        }
    };

    let _ = store.delete(&key);

    if read_value == value {
        Ok(TestResult::pass(t0, "Vault write/read/delete cycle OK"))
    } else {
        Ok(TestResult::fail(
            t0,
            "Read value did not match written value",
        ))
    }
}

// ─── T3: known_hosts read ──────────────────────────────────────────────────

#[tauri::command]
pub async fn debug_test_known_hosts() -> Result<TestResult, String> {
    let t0 = Instant::now();
    let path = match dirs::home_dir().map(|d| d.join(".ssh").join("known_hosts")) {
        Some(p) => p,
        None => return Ok(TestResult::warn(t0, "No home directory detected")),
    };

    if !path.exists() {
        return Ok(TestResult::skipped(
            t0,
            "~/.ssh/known_hosts not present (no SFTP hosts trusted yet)",
        ));
    }

    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(TestResult::fail(
                t0,
                format!("Cannot read known_hosts: {}", e),
            ))
        }
    };

    let entries = content
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .count();

    Ok(TestResult::pass(
        t0,
        format!("known_hosts readable, {} entries", entries),
    ))
}

// ─── T4: AeroVault v2 round-trip (in-memory tempdir) ───────────────────────

#[tauri::command]
pub async fn debug_test_aerovault_roundtrip() -> Result<TestResult, String> {
    let t0 = Instant::now();

    let tmp = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            return Ok(TestResult::fail(
                t0,
                format!("Cannot create tempdir: {}", e),
            ))
        }
    };
    let vault_path = tmp.path().join("debug-roundtrip.aerovault");
    let vault_str = vault_path.to_string_lossy().to_string();
    let password = "debug-roundtrip-temp-password-2026".to_string();

    let opts = aerovault::CreateOptions::new(&vault_str, password.clone())
        .with_mode(aerovault::EncryptionMode::Standard);
    if let Err(e) = aerovault::Vault::create(opts) {
        return Ok(TestResult::fail(t0, format!("Vault create failed: {}", e)));
    }

    let vault = match aerovault::Vault::open(&vault_str, &password) {
        Ok(v) => v,
        Err(e) => return Ok(TestResult::fail(t0, format!("Vault reopen failed: {}", e))),
    };

    let entries = match vault.list() {
        Ok(e) => e.len(),
        Err(e) => return Ok(TestResult::fail(t0, format!("Vault list failed: {}", e))),
    };
    drop(vault);
    drop(tmp); // RAII cleanup

    Ok(TestResult::pass(
        t0,
        format!(
            "AeroVault create + reopen + list OK ({} entries on fresh vault)",
            entries
        ),
    ))
}

// ─── T5: plugin integrity walk ─────────────────────────────────────────────

#[tauri::command]
pub async fn debug_test_plugin_integrity(app: tauri::AppHandle) -> Result<TestResult, String> {
    let t0 = Instant::now();
    let plugins = match crate::plugins::list_plugins(app).await {
        Ok(p) => p,
        Err(e) => {
            return Ok(TestResult::fail(
                t0,
                format!("Cannot enumerate plugins: {}", e),
            ))
        }
    };

    if plugins.is_empty() {
        return Ok(TestResult::skipped(
            t0,
            "No plugins installed (nothing to verify)",
        ));
    }

    Ok(TestResult::pass(
        t0,
        format!(
            "Plugin manifest walk OK: {} plugin(s) enumerated",
            plugins.len()
        ),
    ))
}

// ─── Bundle export (Pass 5) ────────────────────────────────────────────────
//
// Mirror of the frontend `redactSensitive` patterns (src/utils/redactSensitive.ts)
// so the `AeroFTP.log` tail we pull off disk before zipping is sanitized in the
// same way as the live buffer. The frontend-supplied buffers (logs, network)
// arrive already redacted; we re-apply here only to the on-disk raw log. Both
// tables run src/shared/redaction-cases.json, so a rule changed on one side
// only turns the other side's test red.

/// What a rule puts where it matches.
#[derive(Clone, Copy)]
enum Replacement {
    /// A credential: the match, its kept label (`$1`) left out, becomes the
    /// text (`$1` expanded).
    Text(&'static str),
    /// The same for an identity rule (an address, a home folder, a hash):
    /// see [`redact`].
    Identity(&'static str),
    /// The e-mail rule, an identity rule whose spans know their `@` (U1).
    Email(&'static str),
    /// The userinfo of a URL, masked by [`redact_url_userinfo`].
    UrlUserinfo,
    /// Every IPv4 address except the loopback and unspecified ones, which the
    /// log keeps for local debugging. The `regex` crate has no look-around, so
    /// the exception is decided on the match, not in the pattern.
    Ipv4,
}

/// Names of the fields whose value is a secret, in JSON and in key=value form,
/// written in snake_case, kebab-case or camelCase (`access_token`, `api-key`,
/// `clientSecret`). A name may follow snake or kebab words (`session_token`,
/// `prod_db_admin_user_password`) but not an open prefix: `nextToken`, `tokens`
/// and `bypass` stay. The prefix is bounded on purpose, 8 segments of at most 24
/// characters: `-` is not a word character, so an unbounded prefix would start a
/// match after every hyphen and cost a quadratic scan on a long kebab run (the
/// class of the email rule below). The rules are case-insensitive; the frontend
/// twin writes the same text.
macro_rules! secret_name {
    () => {
        r"(?:(?:[A-Za-z0-9]{1,24}[_-]){0,8}(?:password|passwd|passphrase|pwd|secret(?:[_-]?(?:access[_-]?)?key)?|api[_-]?key|access[_-]?key|(?:access|auth|refresh|id)[_-]?token|token|private[_-]?key|(?:client|totp|consumer)[_-]?secret)|pass)"
    };
}

/// The value after a key: the marker (a redacted line stays as it is), a string
/// whose quotes are escaped (`\"v\"`, from a Debug or JSON dump of the line), a
/// quoted string that ends at its closing quote (an escaped quote does not end
/// it; a line cut inside the value has no closing quote and ends there), or a
/// bare word up to the next delimiter.
macro_rules! secret_value {
    () => {
        concat!(
            r#"(?:\*\*\*REDACTED\*\*\*|\\""#,
            authorization_redaction::escaped_body!(),
            r#"(?:\\")?|"(?:[^"\\]|\\.)*"?|'(?:[^'\\]|\\.)*'?|[^\s,;'"<>]+)"#
        )
    };
}

const REDACTION_RULES: &[(&str, Replacement)] = &[
    // Inline credentials in URLs: a linear scan, not a pattern (see
    // `redact_url_userinfo`); the pattern only names the rule. Read on the
    // original text like every rule (T3).
    (r"://", Replacement::UrlUserinfo),
    // Authorization first, ahead of every key and Bearer rule below, so
    // `Authorization: Bearer sk-...` is one marker. Shared with
    // `ai::sanitize_error_message`.
    (
        authorization_redaction::IN_JSON_STRING,
        Replacement::Text("$1***REDACTED***"),
    ),
    (
        authorization_redaction::QUOTED,
        Replacement::Text("$1\"***REDACTED***\""),
    ),
    (
        authorization_redaction::credentials!(r"\*\*\*REDACTED\*\*\*"),
        Replacement::Text("authorization: ***REDACTED***"),
    ),
    (
        r"sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}",
        Replacement::Text("sk-***REDACTED***"),
    ),
    (
        r"sk_(live|test)_[A-Za-z0-9]{16,}",
        Replacement::Text("sk_***REDACTED***"),
    ),
    (
        r"\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*",
        Replacement::Text("Bearer ***REDACTED***"),
    ),
    (
        concat!(r"(?i)\bx-api-key\s*[:=]\s*", secret_value!()),
        Replacement::Text("x-api-key: ***REDACTED***"),
    ),
    (
        r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}",
        Replacement::Text("***JWT-REDACTED***"),
    ),
    // Serialized secret fields in JSON: "password":"..." -> "password":"***REDACTED***".
    // The value ends at its closing quote; an escaped quote inside it does not end it.
    (
        concat!(
            r#"(?i)(""#,
            secret_name!(),
            r#""\s*:\s*)"(?:[^"\\]|\\.)*"?"#
        ),
        Replacement::Text("$1\"***REDACTED***\""),
    ),
    // The same inside a JSON string, where each quote is escaped: the value ends
    // at the first lone `\"`, and an inner `\\\"` belongs to it.
    (
        concat!(
            r#"(?i)(\\""#,
            secret_name!(),
            r#"\\"[ \t]*:[ \t]*\\")"#,
            authorization_redaction::escaped_body!()
        ),
        Replacement::Text("$1***REDACTED***"),
    ),
    // MCP-06: generic key=value secrets (password=, secret:, token=, ...).
    // Keeps the key and its separator for debugging context, scrubs the value.
    (
        concat!(r"(?i)(\b", secret_name!(), r"\s*[:=]\s*)", secret_value!(),),
        Replacement::Text("$1***REDACTED***"),
    ),
    (
        r"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
        Replacement::Email("***@***"),
    ),
    (r"\b(?:\d{1,3}\.){3}\d{1,3}\b", Replacement::Ipv4),
    (r"/home/[A-Za-z0-9._-]+", Replacement::Identity("/home/***")),
    (
        r"/Users/[A-Za-z0-9._-]+",
        Replacement::Identity("/Users/***"),
    ),
    (
        // Greedy to the next backslash, as on main, so a name with a space
        // is masked whole; a credential it runs into is only partly
        // covered, so the span is kept and joins the union (U1). A line
        // break ends it.
        r"(?i)C:\\Users\\[^\\\r\n]+",
        Replacement::Identity("C:\\Users\\***"),
    ),
    (
        r"\b[A-Fa-f0-9]{32,}\b",
        Replacement::Identity("***HEX-REDACTED***"),
    ),
];

/// A rule that does not compile would drop out of the policy without a word
/// (IPv4 did, for a look-around the `regex` crate rejects), so it stops the
/// first redaction instead; `every_redaction_rule_compiles` runs it in CI.
fn redaction_patterns() -> &'static [(regex::Regex, Replacement)] {
    static PATTERNS: OnceLock<Vec<(regex::Regex, Replacement)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        REDACTION_RULES
            .iter()
            .map(|(pattern, replacement)| {
                let re = regex::Regex::new(pattern)
                    .unwrap_or_else(|e| panic!("redaction rule {pattern:?} does not compile: {e}"));
                (re, *replacement)
            })
            .collect()
    })
}

/// In valid JSON, the index of the quote that ends the string token whose
/// content starts at `from` (a backslash escapes the next byte), and whether
/// the token holds an escape.
fn scan_string_token(bytes: &[u8], from: usize) -> (usize, bool) {
    let mut k = from;
    let mut escaped = false;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => {
                escaped = true;
                k += 2;
            }
            b'"' => return (k, escaped),
            _ => k += 1,
        }
    }
    (bytes.len(), escaped)
}

/// The content of the valid JSON string token `[from, end)`, decoded, and
/// for each decoded byte the index of the raw character or escape it comes
/// from (one more entry: `end`). A lone surrogate escape decodes to U+FFFD:
/// `serde_json` refuses it into a `String` where `JSON.parse` keeps it, and
/// either way it is one character that is no delimiter, so the decision is
/// the TS twin's (a shared case pins it).
fn decode_string_token(text: &str, from: usize, end: usize) -> (String, Vec<usize>) {
    let bytes = text.as_bytes();
    let hex = |at: usize| u32::from_str_radix(&text[at..at + 4], 16).unwrap_or(0xFFFD);
    let mut decoded = String::with_capacity(end - from);
    let mut raw = Vec::with_capacity(end - from + 1);
    let mut k = from;
    while k < end {
        let start = k;
        let ch = if bytes[k] != b'\\' {
            let ch = text[k..].chars().next().unwrap_or('\u{FFFD}');
            k += ch.len_utf8();
            ch
        } else {
            let escape = bytes[k + 1];
            k += 2;
            match escape {
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let unit = hex(k);
                    k += 4;
                    let low = (bytes.get(k) == Some(&b'\\') && bytes.get(k + 1) == Some(&b'u'))
                        .then(|| hex(k + 2))
                        .filter(|low| {
                            (0xD800..0xDC00).contains(&unit) && (0xDC00..0xE000).contains(low)
                        });
                    match low {
                        Some(low) => {
                            k += 6;
                            char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                                .unwrap_or('\u{FFFD}')
                        }
                        None => char::from_u32(unit).unwrap_or('\u{FFFD}'),
                    }
                }
                other => other as char,
            }
        };
        raw.extend(std::iter::repeat_n(start, ch.len_utf8()));
        decoded.push(ch);
    }
    raw.push(end);
    (decoded, raw)
}

/// Text that is not JSON as a whole, read as ONE string body (X2): the
/// escapes JSON defines (`\/`, `\\`, `\"`, the letters, `\uXXXX` with four
/// hex digits, a surrogate pair joined, a lone one U+FFFD as in
/// [`decode_string_token`]) are decoded, each decoded byte mapped to the
/// raw escape or character it comes from (one more entry: the end); any
/// other backslash is literal. One pass, linear. `None` when nothing
/// decodes, which ends the recursion.
fn decode_body(text: &str) -> Option<(String, Vec<usize>)> {
    let bytes = text.as_bytes();
    let hex = |at: usize| {
        text.get(at..at + 4)
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
    };
    let mut decoded = String::with_capacity(text.len());
    let mut raw = Vec::with_capacity(text.len() + 1);
    let mut changed = false;
    let mut k = 0;
    while k < bytes.len() {
        let start = k;
        let escaped = if bytes[k] == b'\\' {
            match bytes.get(k + 1) {
                Some(b'"') => Some((2, '"')),
                Some(b'\\') => Some((2, '\\')),
                Some(b'/') => Some((2, '/')),
                Some(b'b') => Some((2, '\u{8}')),
                Some(b'f') => Some((2, '\u{c}')),
                Some(b'n') => Some((2, '\n')),
                Some(b'r') => Some((2, '\r')),
                Some(b't') => Some((2, '\t')),
                Some(b'u') => hex(k + 2).map(|unit| {
                    let low = (bytes.get(k + 6) == Some(&b'\\') && bytes.get(k + 7) == Some(&b'u'))
                        .then(|| hex(k + 8))
                        .flatten()
                        .filter(|low| {
                            (0xD800..0xDC00).contains(&unit) && (0xDC00..0xE000).contains(low)
                        });
                    match low {
                        Some(low) => (
                            12,
                            char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                                .unwrap_or('\u{FFFD}'),
                        ),
                        None => (6, char::from_u32(unit).unwrap_or('\u{FFFD}')),
                    }
                }),
                _ => None,
            }
        } else {
            None
        };
        let ch = match escaped {
            Some((len, ch)) => {
                k += len;
                changed = true;
                ch
            }
            None => {
                let ch = text[k..].chars().next().unwrap_or('\u{FFFD}');
                k += ch.len_utf8();
                ch
            }
        };
        raw.extend(std::iter::repeat_n(start, ch.len_utf8()));
        decoded.push(ch);
    }
    raw.push(bytes.len());
    changed.then_some((decoded, raw))
}

/// The userinfo spans `[start, end)` of `text`, in order (see
/// [`redact_url_userinfo`]).
fn userinfo_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let json = serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok();
    let mut spans = Vec::new();
    if !json {
        // X2: no whole-document JSON gates a mask. Text that is not JSON as
        // a whole (a prefix before a JSON dump, a suffix after it, a trailing
        // comma, single quotes) is also scanned decoded, as one string body,
        // by these same rules, and the spans are mapped back; the raw scan
        // below still runs, so decoding never masks less.
        if let Some((decoded, raw)) = decode_body(text) {
            spans.extend(
                userinfo_spans(&decoded)
                    .into_iter()
                    .map(|(a, b)| (raw[a], raw[b])),
            );
        }
    }
    let mut i = 0;
    // The string token the scan is in (valid JSON only): its closing quote.
    let mut token_end: Option<usize> = None;
    while i + 3 <= bytes.len() {
        if json && token_end.is_none_or(|end| i > end) && bytes[i] == b'"' {
            let (end, escaped) = scan_string_token(bytes, i + 1);
            token_end = Some(end);
            if escaped {
                // What the token says, mapped back onto the raw characters
                // and escapes; the raw scan below still runs on the token
                // too, so decoding never masks less (an escaped newline ends
                // the decoded authority, not the raw one).
                let (decoded, raw) = decode_string_token(text, i + 1, end);
                spans.extend(
                    userinfo_spans(&decoded)
                        .into_iter()
                        .map(|(a, b)| (raw[a], raw[b])),
                );
            }
            i += 1;
            continue;
        }
        if &bytes[i..i + 3] != b"://" {
            i += 1;
            continue;
        }
        let authority = i + 3;
        let limit = match token_end {
            Some(end) if json && i < end => end,
            _ => bytes.len(),
        };
        let mut k = authority;
        let mut last_at = None;
        while k < limit {
            let b = bytes[k];
            // Whitespace does not end it (S1b): a credential with a space
            // in it would leak the part before the space.
            if matches!(b, b'/' | b'?' | b'#') {
                break;
            }
            if b == b'@' {
                last_at = Some(k);
            }
            k += 1;
        }
        let Some(at) = last_at else {
            // No userinfo: go on right after this `://`; each byte is read at
            // most twice.
            i = authority;
            continue;
        };
        spans.push((authority, at));
        i = at + 1;
    }
    spans
}

/// Masks the userinfo of every URL: any `://`, whatever scheme comes before
/// it (`s3://`, `postgres://`, none at all); a `://` that is not a URL only
/// over-masks (S1). Where the authority ends is the TS twin's rule
/// (`redactUrlUserinfo`, D1): the real parser decides once whether the WHOLE
/// text is JSON (`serde_json` into `IgnoredAny`: syntax only, nothing is
/// built or re-serialized). In valid JSON the authority ends at `/`, `?`,
/// `#` or the end of its string token; anything else is plain text, where no
/// quote ever ends it; whitespace never does (S1b). A string token of valid JSON that
/// holds an escape is also scanned decoded by these same rules, a document
/// serialized inside it decoded in turn, and the userinfo found there is
/// mapped back onto the raw characters and escapes; the token is scanned raw
/// as well and the union of both is masked (S1). Text that is not JSON as a
/// whole is scanned decoded too, all of it read as one string body
/// ([`decode_body`], X2), so a prefix, a suffix, a trailing comma or single
/// quotes around an escaped URL never leave it unmasked; malformed escapes
/// are literal. Each level is one linear pass and the recursion stops at the
/// first level that decodes nothing; a level is shorter than the one before
/// by one character per escape, so a line full of backslashes, halved at
/// every level, costs twice its length in all. With an
/// `@` in it, the userinfo is everything up to the LAST `@`, and all of it
/// becomes the placeholder. Both twins run src/shared/redaction-cases.json,
/// the parser-agreement and escape cases included.
// What `redact` does with the URL rule alone; the tests time and check it.
#[cfg(test)]
fn redact_url_userinfo(text: &str) -> String {
    // The decoded and the raw scan of a token can find the same userinfo, or
    // overlapping ones: their union is masked once.
    let mut spans = userinfo_spans(text);
    spans.sort_by_key(|&(start, _)| start);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut n = 0;
    while n < spans.len() {
        let (start, mut end) = spans[n];
        n += 1;
        while n < spans.len() && spans[n].0 < end {
            end = end.max(spans[n].1);
            n += 1;
        }
        out.push_str(&text[copied..start]);
        out.push_str("***REDACTED***");
        copied = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// A part of the original text a rule masks, and what replaces it; an
/// e-mail span also knows its `@`, a URL userinfo span that it is one (U1).
struct Span {
    start: usize,
    end: usize,
    replacement: String,
    at: Option<usize>,
    userinfo: bool,
}

/// The spans of one pattern rule, read on the ORIGINAL text (T3): each match,
/// the leading part its replacement repeats (a field's label, `$1`) left out,
/// so the label stays visible.
fn pattern_spans(re: &regex::Regex, text: &str, template: &str) -> Vec<Span> {
    re.captures_iter(text)
        .filter_map(|caps| {
            let whole = caps.get(0)?;
            let replaced = template.replace("$1", caps.get(1).map_or("", |g| g.as_str()));
            let kept: usize = whole
                .as_str()
                .chars()
                .zip(replaced.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a.len_utf8())
                .sum();
            let span = Span {
                start: whole.start() + kept,
                end: whole.end(),
                replacement: replaced[kept..].to_string(),
                at: None,
                userinfo: false,
            };
            (span.end > span.start || !span.replacement.is_empty()).then_some(span)
        })
        .collect()
}

/// The identity spans the credentials do not cover ENTIRELY
/// ([`redact_with`], U1), the TS twin's `uncoveredIdentity`: one covered only
/// in part is kept and joins the union, so nothing an identity rule hid
/// becomes visible. One named exception, the one the fixed point needs: an
/// e-mail span whose local part lies inside a URL userinfo span that ends at
/// its `@` (`https://token@host`) is dropped; the userinfo already masks the
/// secret, and the host stays. Linear in the spans after the sort: the
/// credentials are merged once into sorted disjoint ranges, then the
/// identity spans are walked in start order with one index that only moves
/// forward; the userinfo spans are found by their end. `steps`, for the
/// tests, counts the comparisons.
fn uncovered_identity(
    mut identity: Vec<Span>,
    credentials: &[Span],
    steps: &mut usize,
) -> Vec<Span> {
    let mut sorted: Vec<&Span> = credentials.iter().collect();
    sorted.sort_by_key(|span| span.start);
    let mut covered: Vec<(usize, usize)> = Vec::new();
    let mut userinfo_ends: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for span in sorted {
        *steps += 1;
        if span.userinfo {
            let start = userinfo_ends.entry(span.end).or_insert(span.start);
            *start = (*start).min(span.start);
        }
        if span.end <= span.start {
            continue;
        }
        match covered.last_mut() {
            Some(last) if span.start <= last.1 => last.1 = last.1.max(span.end),
            _ => covered.push((span.start, span.end)),
        }
    }
    identity.sort_by_key(|span| span.start);
    let mut next = 0;
    identity.retain(|span| {
        while next < covered.len() && covered[next].1 <= span.start {
            *steps += 1;
            next += 1;
        }
        *steps += 1;
        let inside =
            next < covered.len() && covered[next].0 <= span.start && span.end <= covered[next].1;
        let mail_in_userinfo = span
            .at
            .and_then(|at| userinfo_ends.get(&at))
            .is_some_and(|&start| start <= span.start);
        !inside && !mail_in_userinfo
    });
    identity
}

/// Every rule's spans, all read on the original text, masked once: the TS
/// twin's `applyRules` (T3). No rule sees another's output, so none can
/// consume a delimiter another needs, and the order of the rules changes
/// nothing. The credential spans (every rule but the identity ones) are
/// masked as their UNION; an identity span the credentials cover entirely
/// is dropped, one they cover in part joins the union, and an e-mail whose
/// local part is a URL's userinfo is that URL's ([`uncovered_identity`],
/// U1). Overlapping spans merge into one,
/// replaced by the replacement of the span that starts first; on equal
/// starts, of the longer one; then of the replacement that sorts first.
pub(crate) fn redact(s: &str) -> String {
    redact_with(s, redaction_patterns())
}

fn redact_with(s: &str, rules: &[(regex::Regex, Replacement)]) -> String {
    let mut credentials = Vec::new();
    let mut identity = Vec::new();
    for (re, replacement) in rules {
        match replacement {
            Replacement::Text(text) => credentials.extend(pattern_spans(re, s, text)),
            Replacement::UrlUserinfo => {
                credentials.extend(userinfo_spans(s).into_iter().map(|(start, end)| Span {
                    start,
                    end,
                    replacement: "***REDACTED***".to_string(),
                    at: None,
                    userinfo: true,
                }))
            }
            Replacement::Identity(text) => identity.extend(pattern_spans(re, s, text)),
            Replacement::Email(text) => {
                identity.extend(pattern_spans(re, s, text).into_iter().map(|span| Span {
                    at: s[span.start..].find('@').map(|at| span.start + at),
                    ..span
                }))
            }
            Replacement::Ipv4 => identity.extend(
                re.find_iter(s)
                    .filter(|m| !matches!(m.as_str(), "127.0.0.1" | "0.0.0.0"))
                    .map(|m| Span {
                        start: m.start(),
                        end: m.end(),
                        replacement: "***.***.***.***".to_string(),
                        at: None,
                        userinfo: false,
                    }),
            ),
        }
    }
    let identity = uncovered_identity(identity, &credentials, &mut 0);
    let mut spans = credentials;
    spans.extend(identity);
    spans.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.replacement.cmp(&b.replacement))
    });
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut n = 0;
    while n < spans.len() {
        let first = &spans[n];
        let mut end = first.end;
        n += 1;
        // Touching spans merge too (U1): two masks side by side would
        // read as a home folder again on a second pass.
        while n < spans.len() && spans[n].start <= end {
            end = end.max(spans[n].end);
            n += 1;
        }
        out.push_str(&s[copied..first.start]);
        out.push_str(&first.replacement);
        copied = end;
    }
    out.push_str(&s[copied..]);
    out
}

/// The last `lines` lines of `content`, each redacted on its own.
fn redacted_tail(content: &str, lines: usize) -> String {
    let all: Vec<&str> = content.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|line| redact(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `lines` lines of the on-disk app log, redacted, and the file they
/// come from: the file the GUI log target writes, in a portable copy's own
/// folder too. `None` when that file does not exist.
pub(crate) async fn app_log_tail(lines: usize) -> Option<(std::path::PathBuf, String)> {
    let path = crate::portable::app_log_file()?;
    if !path.is_file() {
        return None;
    }
    let content = tokio::fs::read(&path)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    Some((path, redacted_tail(&content, lines)))
}

#[derive(Deserialize)]
pub struct BundleInput {
    pub logs_ndjson: String,
    pub network_ndjson: String,
    pub system_info: serde_json::Value,
    pub tests_state: serde_json::Value,
    pub local_storage_keys: serde_json::Value,
    pub app_version: String,
}

#[tauri::command]
pub async fn debug_export_bundle(
    output_path: String,
    bundle: BundleInput,
) -> Result<String, String> {
    use std::fs::File;
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    let file =
        File::create(&output_path).map_err(|e| format!("Cannot create bundle file: {}", e))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let write = |zip: &mut ZipWriter<File>, name: &str, body: &str| -> Result<(), String> {
        zip.start_file(name, opts)
            .map_err(|e| format!("zip start_file({}): {}", name, e))?;
        zip.write_all(body.as_bytes())
            .map_err(|e| format!("zip write({}): {}", name, e))?;
        Ok(())
    };

    let readme = format!(
        "AeroFTP diagnostic bundle\n\
         App version: {}\n\
         Generated: {}\n\
         \n\
         Contents:\n\
         - system_info.json      Static host + runtime info (already non-sensitive)\n\
         - logs.ndjson           Last unified log buffer entries (redacted on capture)\n\
         - network.ndjson        Last IPC + transfer events (redacted on capture)\n\
         - tests.json            Diagnostic test runner state snapshot\n\
         - localstorage.json     Frontend localStorage keys (size + preview only)\n\
         - aeroftp.log.tail      Last 1000 lines of the on-disk Rust log (re-redacted)\n\
         \n\
         Redaction policy:\n\
         - Tokens, Authorization headers (the whole value), JWTs, inline\n\
           URL passwords, emails, non-loopback IPv4, home directories and\n\
           32+ hex blobs are replaced with ***REDACTED*** markers across\n\
           every file in this bundle. The raw log on disk is left\n\
           untouched for the user.\n",
        bundle.app_version,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );

    write(&mut zip, "README.txt", &readme)?;
    write(
        &mut zip,
        "system_info.json",
        &serde_json::to_string_pretty(&bundle.system_info).unwrap_or_else(|_| "{}".into()),
    )?;
    write(&mut zip, "logs.ndjson", &bundle.logs_ndjson)?;
    write(&mut zip, "network.ndjson", &bundle.network_ndjson)?;
    write(
        &mut zip,
        "tests.json",
        &serde_json::to_string_pretty(&bundle.tests_state).unwrap_or_else(|_| "{}".into()),
    )?;
    write(
        &mut zip,
        "localstorage.json",
        &serde_json::to_string_pretty(&bundle.local_storage_keys).unwrap_or_else(|_| "{}".into()),
    )?;

    // Tail of the on-disk Rust log, read where the GUI log target writes it.
    if let Some((_, tail)) = app_log_tail(1000).await {
        write(&mut zip, "aeroftp.log.tail", &tail)?;
    }

    zip.finish().map_err(|e| format!("zip finish: {}", e))?;
    Ok(output_path)
}

// ─── T6: provider self-test (active connection, list current dir) ──────────
//
// Conservative scope: list the current remote directory only. No mkdir / put /
// delete on the live server, the user controls which server is targeted via
// the existing connection. A future extension can opt into a write-cycle in
// an `aeroftp-self-test/` namespace, gated by a checkbox in the UI.

#[tauri::command]
pub async fn debug_test_provider_selftest(
    state: tauri::State<'_, AppState>,
) -> Result<TestResult, String> {
    let t0 = Instant::now();
    let mut ftp = state.ftp_manager.lock().await;
    if !ftp.is_connected() {
        return Ok(TestResult::skipped(
            t0,
            "No active FTP/SFTP session (provider read-only test needs one)",
        ));
    }
    match ftp.list_files().await {
        Ok(files) => Ok(TestResult::pass(
            t0,
            format!("Remote listing OK ({} entries)", files.len()),
        )),
        Err(e) => Ok(TestResult::fail(t0, format!("Listing failed: {}", e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct RedactionCase {
        name: String,
        input: String,
        expected: String,
    }

    /// The cases `src/utils/redactSensitive.test.ts` runs through the frontend
    /// table: both tables apply one policy, so a rule changed on one side only
    /// turns the other side's test red.
    fn redaction_cases() -> Vec<RedactionCase> {
        serde_json::from_str(include_str!("../../src/shared/redaction-cases.json"))
            .expect("redaction cases parse")
    }

    /// Every case whose `redact` of `input(case)` is not `expected(case)`.
    fn mismatches(input: fn(&RedactionCase) -> &str) -> Vec<String> {
        redaction_cases()
            .iter()
            .filter_map(|case| {
                let got = redact(input(case));
                (got != case.expected)
                    .then(|| format!("{}: got {got:?}, want {:?}", case.name, case.expected))
            })
            .collect()
    }

    /// H9: the URL userinfo scan is linear by construction: long
    /// adversarial inputs (backslashes with no `@`, colons, bare `://`) end
    /// well under the bound, through the whole pipeline too.
    #[test]
    fn the_url_userinfo_scan_stays_linear_on_adversarial_input() {
        let inputs = [
            format!("ftp://bob:{}x", "\\".repeat(200_000)),
            format!("ftp://bob\\:{}", "\\".repeat(200_000)),
            format!("ftp://{}", ":".repeat(200_000)),
            format!("{}{}", "x".repeat(100_000), "://".repeat(100_000)),
            "ftp://ftp://".repeat(50_000),
            // D1: long whitespace and many strings, valid JSON and not, and
            // a large valid JSON document with URLs throughout.
            format!("{{\"a\":\"{}ftp://u:p@h\"}}", " ".repeat(200_000)),
            serde_json::to_string(
                &(0..20_000)
                    .map(|n| format!("ftp://u{n}:p@h{n}/x"))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            serde_json::to_string(&vec!["s"; 20_000]).unwrap() + " tail",
            "\"".repeat(200_000) + "ftp://u:p@h",
            serde_json::json!({ "rows": (0..20_000).map(|n| serde_json::json!({
                "id": n,
                "url": format!("https://user{n}:secret@host{n}.example/p"),
                "note": "x@y",
            })).collect::<Vec<_>>() })
            .to_string(),
            // S1: 20k escaped tokens, each decoded once; a document of 20k
            // URLs serialized inside one string; twelve levels of nesting.
            escaped_url_tokens(20_000),
            serde_json::json!({ "log": serde_json::to_string(
                &(0..20_000)
                    .map(|n| format!("ftp://u{n}:p@h{n}/x"))
                    .collect::<Vec<_>>(),
            )
            .unwrap() })
            .to_string(),
            (0..12).fold(format!("ftp://u:p@h/{}", "x".repeat(1000)), |inner, _| {
                serde_json::json!({ "v": inner }).to_string()
            }),
            // X2: long lines that are not JSON, full of backslashes, decoded
            // as a string body at every level: a run halves at each one.
            format!("log: {} https:\\/\\/bob:hunter2@h/p", "\\".repeat(400_000)),
            format!(
                "log: {} https:\\/\\/bob:hunter2@h/p",
                "x\\y".repeat(100_000)
            ),
            format!("log: {}", "\\/".repeat(200_000)),
            format!("log: {}\\u0040", "\\u00".repeat(100_000)),
            format!(
                "log: {{\"a\":\"{}{}\"",
                "x".repeat(200_000),
                "\\".repeat(16)
            ),
        ];
        for input in &inputs {
            let started = std::time::Instant::now();
            redact_url_userinfo(input);
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
        // X2: masked at its level, whatever the backslashes before it.
        for run in ["\\".repeat(400_000), "x\\y".repeat(100_000)] {
            let line = format!("log: {run} https:\\/\\/bob:hunter2@h/p");
            let started = std::time::Instant::now();
            assert!(!redact(&line).contains("hunter2"));
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
        }
        let finding = format!("ftp://bob:{}x", "\\".repeat(20_000));
        let started = std::time::Instant::now();
        assert_eq!(redact(&finding), finding);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    /// `[ "https:\/\/u0:p\u0040h0\/x", ... ]`: every delimiter escaped.
    fn escaped_url_tokens(count: usize) -> String {
        let tokens: Vec<String> = (0..count)
            .map(|n| format!(r#""https:\/\/u{n}:p\u0040h{n}\/x""#))
            .collect();
        format!("[{}]", tokens.join(","))
    }

    /// S1: every URL of a 20k escaped-token document is masked at its own
    /// token, and the result is a fixed point.
    #[test]
    fn every_escaped_url_token_is_masked_at_its_own_token() {
        let masked = redact_url_userinfo(&escaped_url_tokens(20_000));
        assert_eq!(masked.matches("***REDACTED***").count(), 20_000);
        assert!(!regex::Regex::new(r"u\d+:p").unwrap().is_match(&masked));
        assert_eq!(redact_url_userinfo(&masked), masked);
    }

    #[test]
    fn redact_follows_the_shared_cases() {
        let wrong = mismatches(|case| &case.input);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_matrix_of_secret_fields_never_leaks_a_value() {
        let probes = crate::authorization_redaction::matrix::secret_field_probes();
        crate::authorization_redaction::matrix::assert_redacted(redact, &probes);
    }

    #[test]
    fn the_matrix_of_authorization_schemes_never_leaks_a_credential() {
        let probes = crate::authorization_redaction::matrix::authorization_probes();
        crate::authorization_redaction::matrix::assert_redacted(redact, &probes);
    }

    #[test]
    fn what_is_not_a_secret_field_stays() {
        for line in crate::authorization_redaction::matrix::unchanged_lines() {
            assert_eq!(redact(&line), line);
        }
    }

    #[test]
    fn a_redacted_line_redacts_to_itself() {
        let wrong = mismatches(|case| &case.expected);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The policy once lost its IPv4 rule without a word: `regex` rejects the
    /// look-around it was written with and the rule was dropped on the error.
    /// T3: the identity spans a credential covers are dropped in one walk.
    /// 20k URLs whose `secret@host` is also an e-mail, and 20k e-mails beside
    /// them; and the walk's comparisons, counted: linear in the spans.
    #[test]
    fn the_identity_spans_a_credential_covers_are_dropped_in_linear_time() {
        let n = 20_000;
        let input = (0..n)
            .map(|k| format!("ftp://user{k}:secret@host{k}.example/x"))
            .chain((0..n).map(|k| format!("mail{k}@x{k}.example")))
            .collect::<Vec<_>>()
            .join(" ");
        let started = std::time::Instant::now();
        let out = redact(&input);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(!out.contains("secret") && !out.contains("user1:"));
        assert_eq!(
            regex::Regex::new(r"ftp://\*\*\*REDACTED\*\*\*@host\d+\.example/x")
                .unwrap()
                .find_iter(&out)
                .count(),
            n
        );
        assert_eq!(out.matches(" ***@***").count(), n);

        let span = |start: usize, end: usize| Span {
            start,
            end,
            replacement: "x".to_string(),
            at: None,
            userinfo: false,
        };
        let credentials: Vec<Span> = (0..n).map(|k| span(10 * k, 10 * k + 5)).collect();
        let identity: Vec<Span> = (0..2 * n).map(|k| span(5 * k, 5 * k + 3)).collect();
        let mut steps = 0;
        assert_eq!(
            uncovered_identity(identity, &credentials, &mut steps).len(),
            n
        );
        assert!(steps <= 2 * (n + 2 * n), "{steps} comparisons");
    }

    /// T3: every rule reads the original text, so the order of the rules
    /// means nothing: any order gives the same output on every shared case.
    #[test]
    fn the_order_of_the_rules_changes_nothing() {
        let rules: Vec<(regex::Regex, Replacement)> = redaction_patterns()
            .iter()
            .map(|(re, replacement)| (re.clone(), *replacement))
            .collect();
        // A fixed shuffle, so a failure can be replayed.
        let mut seed: u64 = 7;
        for _ in 0..8 {
            let mut shuffled = rules.clone();
            for i in (1..shuffled.len()).rev() {
                seed = (seed * 1103515245 + 12345) % 2147483648;
                shuffled.swap(i, (seed % (i as u64 + 1)) as usize);
            }
            for case in redaction_cases() {
                assert_eq!(
                    redact_with(&case.input, &shuffled),
                    redact(&case.input),
                    "{}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn every_redaction_rule_compiles() {
        for (pattern, _) in REDACTION_RULES {
            if let Err(e) = regex::Regex::new(pattern) {
                panic!("redaction rule {pattern:?} does not compile: {e}");
            }
        }
        assert_eq!(redaction_patterns().len(), REDACTION_RULES.len());
    }

    /// What the bundle and the MCP snapshot carry: the tail's last lines, each
    /// redacted, with a Basic credential and a public address among them.
    #[test]
    fn the_log_tail_redacts_what_it_keeps() {
        let log = "old line\n\
                   GET /x Authorization: Basic dXNlcjpwYXNz\n\
                   peer 203.0.113.9 and 127.0.0.1";
        assert_eq!(
            redacted_tail(log, 2),
            "GET /x authorization: ***REDACTED***\n\
             peer ***.***.***.*** and 127.0.0.1"
        );
        assert_eq!(redacted_tail("", 5), "");
        assert_eq!(redacted_tail("one", 5), "one");
    }
}
