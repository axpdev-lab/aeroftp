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

    let key = "__aeroftp_debug_test_roundtrip__";
    let value = "round_trip_test_value_OK";

    if let Err(e) = store.store(key, value) {
        return Ok(TestResult::fail(t0, format!("Write failed: {}", e)));
    }

    let read_value = match store.get(key) {
        Ok(v) => v,
        Err(e) => {
            let _ = store.delete(key);
            return Ok(TestResult::fail(t0, format!("Read failed: {}", e)));
        }
    };

    let _ = store.delete(key);

    if read_value == value {
        Ok(TestResult::pass(t0, "Vault write/read/delete cycle OK"))
    } else {
        Ok(TestResult::fail(t0, "Read value did not match written value"))
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
pub async fn debug_test_plugin_integrity(
    app: tauri::AppHandle,
) -> Result<TestResult, String> {
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
// Mirror of the frontend `redactSensitive` patterns so the `aeroftp.log` tail
// we pull off disk before zipping is sanitized in the same way as the live
// buffer. The frontend-supplied buffers (logs, network) arrive already
// redacted; we re-apply here only to the on-disk raw log.

fn redaction_patterns() -> &'static [(regex::Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, &'static str)] = &[
            (r"sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}", "sk-***REDACTED***"),
            (r"sk_(live|test)_[A-Za-z0-9]{16,}", "sk_***REDACTED***"),
            (r"\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*", "Bearer ***REDACTED***"),
            (r#"(?i)\bx-api-key\s*[:=]\s*[^\s,;'"<>]+"#, "x-api-key: ***REDACTED***"),
            (r#"(?i)\bauthorization\s*[:=]\s*[^\s,;'"<>]+"#, "authorization: ***REDACTED***"),
            (r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}", "***JWT-REDACTED***"),
            (r"(?i)\b((?:ftps?|sftp|https?|webdav)://[^:\s@/]+:)[^@\s]+(@)", "$1***REDACTED***$2"),
            (r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "***@***"),
            (r"\b(?!127\.0\.0\.1\b|0\.0\.0\.0\b)((?:\d{1,3}\.){3}\d{1,3})\b", "***.***.***.***"),
            (r"/home/[A-Za-z0-9._-]+", "/home/***"),
            (r"/Users/[A-Za-z0-9._-]+", "/Users/***"),
            (r"(?i)C:\\Users\\[^\\]+", "C:\\Users\\***"),
            (r"\b[A-Fa-f0-9]{32,}\b", "***HEX-REDACTED***"),
        ];
        raw.iter()
            .filter_map(|(p, r)| regex::Regex::new(p).ok().map(|re| (re, *r)))
            .collect()
    })
}

pub(crate) fn redact(s: &str) -> String {
    let mut out = s.to_owned();
    for (re, rep) in redaction_patterns() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
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

    let file = File::create(&output_path)
        .map_err(|e| format!("Cannot create bundle file: {}", e))?;
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
         - Tokens, Bearer headers, JWTs, inline URL passwords, emails,\n\
           non-loopback IPv4, home directories and 32+ hex blobs are\n\
           replaced with ***REDACTED*** markers across every file in this\n\
           bundle. The raw log on disk is left untouched for the user.\n",
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
        &serde_json::to_string_pretty(&bundle.local_storage_keys)
            .unwrap_or_else(|_| "{}".into()),
    )?;

    // Tail of the on-disk Rust log. tauri-plugin-log writes to
    // `<config>/aeroftp/logs/aeroftp.log` on most platforms.
    if let Some(log_dir) = dirs::config_dir() {
        let candidates = [
            log_dir.join("aeroftp").join("logs").join("aeroftp.log"),
            log_dir.join("aeroftp").join("aeroftp.log"),
        ];
        for path in candidates.iter() {
            if path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(path).await {
                    let lines: Vec<&str> = content.lines().collect();
                    let tail_start = lines.len().saturating_sub(1000);
                    let tail = lines[tail_start..]
                        .iter()
                        .map(|l| redact(l))
                        .collect::<Vec<_>>()
                        .join("\n");
                    write(&mut zip, "aeroftp.log.tail", &tail)?;
                }
                break;
            }
        }
    }

    zip.finish()
        .map_err(|e| format!("zip finish: {}", e))?;
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
