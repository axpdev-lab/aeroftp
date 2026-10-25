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
    Text(&'static str),
    /// Every IPv4 address except the loopback and unspecified ones, which the
    /// log keeps for local debugging. The `regex` crate has no look-around, so
    /// the exception is decided on the match, not in the pattern.
    Ipv4,
}

/// One RFC 7235 `auth-param`: `name=value`, the value a token or a quoted
/// string. A token may hold a `;` (AWS SigV4: `SignedHeaders=host;x-amz-date`).
macro_rules! auth_param {
    () => {
        r#"[A-Za-z0-9_-]+[ \t]*=[ \t]*(?:"(?:[^"\\]|\\.)*"|[^\s,'"<>\\]+)"#
    };
}

const REDACTION_RULES: &[(&str, Replacement)] = &[
    (
        r"sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}",
        Replacement::Text("sk-***REDACTED***"),
    ),
    (
        r"sk_(live|test)_[A-Za-z0-9]{16,}",
        Replacement::Text("sk_***REDACTED***"),
    ),
    // Authorization (and Proxy-Authorization) carries `scheme credentials`, and
    // the whole value goes, not its first word: `Basic dXNlcjpwYXNz` is a
    // password. Ahead of the Bearer rule, so `Authorization: Bearer <token>`
    // becomes one marker.
    // JSON inside a string: the value ends at the first lone `\"`, and a Digest
    // parameter's `\\\"` belongs to it.
    (
        r#"(?i)(\\"authorization\\"[ \t]*:[ \t]*\\")(?:[^\\]|\\\\\\")*"#,
        Replacement::Text("$1***REDACTED***"),
    ),
    // A quoted value (JSON, Debug) ends at its closing quote, so the keys after
    // it stay; a line cut inside the value has no closing quote and ends there.
    (
        r#"(?i)(\bauthorization["']?[ \t]*[:=][ \t]*)(?:"(?:[^"\\]|\\.)*"?|'[^']*'?)"#,
        Replacement::Text("$1\"***REDACTED***\""),
    ),
    // A bare value is `credentials`: a scheme, then a token68 or a
    // comma-separated list of parameters (Digest, Hawk, ...). A value that is
    // already the marker stays as it is, so redacting a redacted line again
    // does not take the word after it for a credential.
    (
        concat!(
            r#"(?i)\bauthorization["']?[ \t]*[:=][ \t]*(?:\*\*\*REDACTED\*\*\*|[^\s,;'"<>\\]+(?:[ \t]+(?:"#,
            auth_param!(),
            r"(?:[ \t]*,[ \t]*",
            auth_param!(),
            r")*|[A-Za-z0-9._~+/-]+=*))?)",
        ),
        Replacement::Text("authorization: ***REDACTED***"),
    ),
    (
        r"\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*",
        Replacement::Text("Bearer ***REDACTED***"),
    ),
    (
        r#"(?i)\bx-api-key\s*[:=]\s*[^\s,;'"<>]+"#,
        Replacement::Text("x-api-key: ***REDACTED***"),
    ),
    (
        r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}",
        Replacement::Text("***JWT-REDACTED***"),
    ),
    (
        r"(?i)\b((?:ftps?|sftp|https?|webdav)://[^:\s@/]+:)[^@\s]+(@)",
        Replacement::Text("$1***REDACTED***$2"),
    ),
    // Serialized secret fields in JSON: "password":"..." -> "password":"***REDACTED***"
    (
        r#"(?i)("(?:password|passwd|pwd|secret|secret_key|secretkey|api_key|apikey|access_key|token|access_token|refresh_token|id_token|passphrase|private_key|privatekey|client_secret|totp_secret|consumer_secret)"\s*:\s*)"[^"]*""#,
        Replacement::Text("$1\"***REDACTED***\""),
    ),
    // MCP-06: generic key=value secrets (password=, secret:, token=, ...).
    // Keeps the key and its separator for debugging context, scrubs the value.
    (
        r#"(?i)(\b(?:pass(?:word|wd|phrase)?|pwd|secret(?:_?key)?|client_secret|totp_secret|consumer_secret|private_?key|api[_-]?key|access_key|(?:access|refresh|id)_token|token)\s*[:=]\s*)[^\s,;'"<>]+"#,
        Replacement::Text("$1***REDACTED***"),
    ),
    (
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
        Replacement::Text("***@***"),
    ),
    (r"\b(?:\d{1,3}\.){3}\d{1,3}\b", Replacement::Ipv4),
    (r"/home/[A-Za-z0-9._-]+", Replacement::Text("/home/***")),
    (r"/Users/[A-Za-z0-9._-]+", Replacement::Text("/Users/***")),
    (
        r"(?i)C:\\Users\\[^\\]+",
        Replacement::Text("C:\\Users\\***"),
    ),
    (
        r"\b[A-Fa-f0-9]{32,}\b",
        Replacement::Text("***HEX-REDACTED***"),
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

pub(crate) fn redact(s: &str) -> String {
    let mut out = s.to_owned();
    for (re, replacement) in redaction_patterns() {
        out = match replacement {
            Replacement::Text(text) => re.replace_all(&out, *text).into_owned(),
            Replacement::Ipv4 => re
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    if matches!(&caps[0], "127.0.0.1" | "0.0.0.0") {
                        caps[0].to_owned()
                    } else {
                        "***.***.***.***".to_owned()
                    }
                })
                .into_owned(),
        };
    }
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

    #[test]
    fn redact_follows_the_shared_cases() {
        let wrong = mismatches(|case| &case.input);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_redacted_line_redacts_to_itself() {
        let wrong = mismatches(|case| &case.expected);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The policy once lost its IPv4 rule without a word: `regex` rejects the
    /// look-around it was written with and the rule was dropped on the error.
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
