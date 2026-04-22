//! SFTP/SSH Host Key Verification (TOFU UX)
//!
//! Pre-check probe for host key verification before actual connection.
//! Returns fingerprint + algorithm to frontend for user approval dialog.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use russh::client::{self, Config, Handler};
use russh::keys::{self, known_hosts, HashAlg, PublicKey};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Pending keys awaiting user acceptance (host:port → (PublicKey, timestamp))
static PENDING_KEYS: LazyLock<Mutex<HashMap<String, (PublicKey, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maximum age for pending keys before auto-cleanup (5 minutes)
const PENDING_KEY_TTL: Duration = Duration::from_secs(300);

/// Result of a host key probe
#[derive(Debug, Clone, Serialize)]
pub struct HostKeyInfo {
    /// "known" | "unknown" | "changed"
    pub status: String,
    /// SHA-256 fingerprint: "SHA256:base64..."
    pub fingerprint: String,
    /// Algorithm: "ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256", etc.
    pub algorithm: String,
    /// For "changed" status: line number in known_hosts
    pub changed_line: Option<usize>,
}

/// Key for the PENDING_KEYS map
fn pending_key(host: &str, port: u16) -> String {
    format!("{}:{}", host, port)
}

/// Remove expired entries from PENDING_KEYS
fn cleanup_expired(map: &mut HashMap<String, (PublicKey, Instant)>) {
    let now = Instant::now();
    map.retain(|_, (_, ts)| now.duration_since(*ts) < PENDING_KEY_TTL);
}

/// Minimal SSH handler that captures host key info without saving
struct ProbeHandler {
    host: String,
    port: u16,
    result: Arc<Mutex<Option<HostKeyInfo>>>,
}

impl Handler for ProbeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let algorithm = server_public_key.algorithm().as_str().to_string();

        match known_hosts::check_known_hosts(&self.host, self.port, server_public_key) {
            Ok(true) => {
                // Key is already known and matches
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostKeyInfo {
                    status: "known".to_string(),
                    fingerprint,
                    algorithm,
                    changed_line: None,
                });
                // Return true so the probe connection succeeds (we'll drop it immediately)
                Ok(true)
            }
            Ok(false) => {
                // Unknown key — TOFU needed
                tracing::info!(
                    "Host key probe: unknown key for {}:{} ({})",
                    self.host,
                    self.port,
                    algorithm
                );
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostKeyInfo {
                    status: "unknown".to_string(),
                    fingerprint,
                    algorithm,
                    changed_line: None,
                });
                // Store key for later acceptance
                let key = pending_key(&self.host, self.port);
                let mut map = PENDING_KEYS.lock().unwrap_or_else(|e| e.into_inner());
                cleanup_expired(&mut map);
                map.insert(key, (server_public_key.clone(), Instant::now()));
                // Reject probe connection (we just needed the key info)
                Ok(false)
            }
            Err(keys::Error::KeyChanged { line }) => {
                // Key changed — possible MITM
                tracing::warn!(
                    "Host key probe: key CHANGED for {}:{} at line {} ({})",
                    self.host,
                    self.port,
                    line,
                    algorithm
                );
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostKeyInfo {
                    status: "changed".to_string(),
                    fingerprint,
                    algorithm,
                    changed_line: Some(line),
                });
                // Store new key for potential acceptance
                let key = pending_key(&self.host, self.port);
                let mut map = PENDING_KEYS.lock().unwrap_or_else(|e| e.into_inner());
                cleanup_expired(&mut map);
                map.insert(key, (server_public_key.clone(), Instant::now()));
                Ok(false)
            }
            Err(e) => {
                tracing::error!(
                    "Host key probe: verification error for {}:{}: {}",
                    self.host,
                    self.port,
                    e
                );
                *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(HostKeyInfo {
                    status: "error".to_string(),
                    fingerprint,
                    algorithm,
                    changed_line: None,
                });
                Ok(false)
            }
        }
    }
}

/// Probe a host's SSH key without authenticating.
/// Returns the key status, fingerprint, and algorithm.
#[tauri::command]
pub async fn sftp_check_host_key(host: String, port: u16) -> Result<HostKeyInfo, String> {
    let result = Arc::new(Mutex::new(None::<HostKeyInfo>));
    let handler = ProbeHandler {
        host: host.clone(),
        port,
        result: result.clone(),
    };

    let config = Config {
        inactivity_timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    };

    let addr = format!("{}:{}", host, port);

    // Probe connection — may fail for unknown/changed keys (handler returns false)
    // or succeed for known keys (we drop the handle immediately)
    let probe = tokio::time::timeout(
        Duration::from_secs(10),
        client::connect(Arc::new(config), &*addr, handler),
    )
    .await;

    // For known keys, probe succeeds — drop the handle
    if let Ok(Ok(_handle)) = probe {
        drop(_handle);
    }
    // For unknown/changed keys, probe fails — that's expected

    // Retrieve captured result
    let info = result
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| {
            format!(
                "Failed to retrieve host key from {}:{} — connection may have timed out",
                host, port
            )
        })?;

    Ok(info)
}

/// Accept a pending host key and save it to ~/.ssh/known_hosts
#[tauri::command]
pub async fn sftp_accept_host_key(host: String, port: u16) -> Result<(), String> {
    let key_id = pending_key(&host, port);
    let pubkey = {
        let mut map = PENDING_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&key_id)
            .map(|(k, _)| k)
            .ok_or_else(|| format!("No pending key for {}:{}", host, port))?
    };

    known_hosts::learn_known_hosts(&host, port, &pubkey)
        .map_err(|e| format!("Failed to save host key: {}", e))?;

    tracing::info!("Host key accepted and saved for {}:{}", host, port);
    Ok(())
}

/// The hostname portion of a known_hosts line is what appears before the
/// first whitespace: `"[localhost]:2222"` for non-default ports, plain
/// `"example.com"` for port 22. Returned borrowed so callers can compare
/// without allocating.
fn known_hosts_host_pattern(host: &str, port: u16) -> String {
    if port != 22 {
        format!("[{}]:{}", host, port)
    } else {
        host.to_string()
    }
}

/// Parse one non-comment known_hosts line into `(host_pattern, algorithm)`.
/// Returns `None` for blank lines, comments (`#`), or hashed entries
/// (`|1|...`), where we cannot verify by plaintext host match.
fn parse_known_hosts_line(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("|1|") {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let host = parts.next()?;
    let alg = parts.next()?;
    Some((host, alg))
}

/// Compute the rewritten `known_hosts` content after removing the entry
/// that triggered `KeyChanged` plus any other still-stale entry for the
/// same `(host, port, algorithm)` tuple. Defense in depth against the
/// case where the file accumulated duplicates from earlier Accept
/// attempts (see the `sftp_remove_host_key` bug from before this fix).
///
/// - `russh_line` is the **1-based** line number from
///   [`russh::keys::Error::KeyChanged`] — russh's parser initialises
///   `let mut line = 1;` and increments per newline, so the returned
///   index is 1-based and blank/comment lines still count.
/// - `pending_alg` is the algorithm of the key the caller is about to
///   accept (taken from [`PENDING_KEYS`]). When `Some`, any leftover
///   entry for `(host, port, alg)` is removed too; when `None`, only
///   the russh-pointed line is removed.
///
/// Returns `(new_content, removed_count)`. Fails only when `russh_line`
/// is zero (invalid — russh never emits line 0) or out of range, or
/// when the pointed line's host pattern mismatches `host`/`port`
/// (corruption guard: the file changed under our feet).
fn rewrite_known_hosts_removing(
    content: &str,
    host: &str,
    port: u16,
    russh_line: usize,
    pending_alg: Option<&str>,
) -> Result<(String, usize), String> {
    if russh_line == 0 {
        return Err(
            "Invalid line 0 (russh KeyChanged uses 1-based line numbering)".to_string(),
        );
    }
    let target_idx = russh_line - 1;
    let lines: Vec<&str> = content.lines().collect();
    if target_idx >= lines.len() {
        return Err(format!(
            "Line {} out of range (file has {} lines)",
            russh_line,
            lines.len()
        ));
    }

    let host_pattern = known_hosts_host_pattern(host, port);

    // Corruption guard: the russh-pointed line must still be a
    // plaintext entry for our host (hashed entries pass through since
    // we can't verify them, and also russh resolves them internally).
    let target_line = lines[target_idx];
    let target_hashed = target_line.trim_start().starts_with("|1|");
    if !target_hashed {
        match parse_known_hosts_line(target_line) {
            Some((line_host, _)) if line_host == host_pattern => {}
            _ => {
                return Err(format!(
                    "Line {} does not match host {} — file may have been modified",
                    russh_line, host_pattern
                ));
            }
        }
    }

    // Filter pass: drop the russh-pointed line unconditionally, plus any
    // other plaintext entry for (host, alg) when `pending_alg` is known.
    // Hashed entries are preserved (we can't check them by host name
    // without the salt — russh's internal check still owns them).
    let mut out = Vec::with_capacity(lines.len());
    let mut removed = 0usize;
    for (i, &l) in lines.iter().enumerate() {
        if i == target_idx {
            removed += 1;
            continue;
        }
        if let Some(alg) = pending_alg {
            if let Some((line_host, line_alg)) = parse_known_hosts_line(l) {
                if line_host == host_pattern && line_alg == alg {
                    removed += 1;
                    continue;
                }
            }
        }
        out.push(l);
    }

    let mut new_content = out.join("\n");
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    Ok((new_content, removed))
}

/// Remove a host key entry from ~/.ssh/known_hosts (for key-changed case).
///
/// `line` is the 1-based line number reported by
/// [`russh::keys::Error::KeyChanged`]. In addition to removing that
/// line, any other plaintext entry for the same `(host, port,
/// algorithm)` that matches the pending key's algorithm is stripped
/// too, so a pre-existing stale entry cannot re-trigger the
/// "Host Key Changed" dialog on the next connection.
#[tauri::command]
pub async fn sftp_remove_host_key(host: String, port: u16, line: usize) -> Result<(), String> {
    let known_hosts_path = dirs::home_dir()
        .ok_or("No home directory found")?
        .join(".ssh")
        .join("known_hosts");

    if !known_hosts_path.exists() {
        return Ok(()); // Nothing to remove
    }

    let content =
        std::fs::read_to_string(&known_hosts_path).map_err(|e| format!("Read error: {}", e))?;

    // Peek at the pending key (captured earlier by `sftp_check_host_key`)
    // so we can also prune other stale entries for the same algorithm.
    // If the pending key is gone (TTL expired, caller skipped check) we
    // still remove the russh-pointed line — partial-fix beats nothing.
    let pending_alg: Option<String> = {
        let map = PENDING_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&pending_key(&host, port))
            .map(|(k, _)| k.algorithm().as_str().to_string())
    };

    let (new_content, removed) = rewrite_known_hosts_removing(
        &content,
        &host,
        port,
        line,
        pending_alg.as_deref(),
    )?;

    // Atomic write: temp + rename on the same filesystem.
    let temp_path = known_hosts_path.with_extension("tmp");
    std::fs::write(&temp_path, &new_content).map_err(|e| format!("Write error: {}", e))?;
    std::fs::rename(&temp_path, &known_hosts_path).map_err(|e| format!("Rename error: {}", e))?;

    tracing::info!(
        "Removed {} stale host key entr{} for {}:{} (russh line {}, alg {:?})",
        removed,
        if removed == 1 { "y" } else { "ies" },
        host,
        port,
        line,
        pending_alg
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_OLD: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOLDSTALEKEYDOESNOTMATCHSERVERXXXXXXXXXXXXXX";
    const KEY_NEW: &str = "AAAAC3NzaC1lZDI1NTE5AAAAINEWKEYMATCHESSERVERXXXXXXXXXXXXXXXXXXXXXXX";

    #[test]
    fn host_pattern_strips_default_port() {
        assert_eq!(known_hosts_host_pattern("example.com", 22), "example.com");
        assert_eq!(
            known_hosts_host_pattern("localhost", 2222),
            "[localhost]:2222"
        );
    }

    #[test]
    fn parse_skips_comments_blanks_and_hashed() {
        assert!(parse_known_hosts_line("").is_none());
        assert!(parse_known_hosts_line("   ").is_none());
        assert!(parse_known_hosts_line("# comment").is_none());
        assert!(parse_known_hosts_line("|1|abc=|def= ssh-ed25519 XXXX").is_none());
        let (h, a) = parse_known_hosts_line("[localhost]:2222 ssh-ed25519 AAAA").unwrap();
        assert_eq!(h, "[localhost]:2222");
        assert_eq!(a, "ssh-ed25519");
    }

    #[test]
    fn rewrite_rejects_line_zero() {
        // russh uses 1-based line numbers — line 0 is never legal and
        // would have indexed the first line under the old off-by-one.
        let content = format!("[localhost]:2222 ssh-ed25519 {KEY_OLD}\n");
        let err = rewrite_known_hosts_removing(&content, "localhost", 2222, 0, None);
        assert!(err.is_err(), "line 0 must be rejected");
    }

    #[test]
    fn rewrite_uses_one_based_indexing() {
        // Regression pin for the off-by-one bug: russh's `KeyChanged
        // { line: 1 }` must remove the FIRST line, not the second.
        let content = format!(
            "[localhost]:2222 ssh-ed25519 {KEY_OLD}\n[other.com] ssh-rsa AAAAB\n"
        );
        let (out, n) =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 1, None).unwrap();
        assert_eq!(n, 1);
        assert!(
            !out.contains(KEY_OLD),
            "line 1 (the OLD key) must be removed; got:\n{out}"
        );
        assert!(
            out.contains("[other.com]"),
            "unrelated entries must survive; got:\n{out}"
        );
    }

    #[test]
    fn rewrite_also_prunes_duplicate_same_alg_entries() {
        // The real-world symptom: known_hosts accumulated duplicate
        // ssh-ed25519 entries for [localhost]:2222 across multiple
        // failed Accept attempts. After the fix a single call must
        // leave the file free of any plaintext entry for that
        // (host, port, alg) so the next TOFU goes to "unknown", not
        // "changed".
        let content = format!(
            "[localhost]:2222 ssh-ed25519 {KEY_OLD}\n\
             [127.0.0.1]:2222 ssh-ed25519 {KEY_NEW}\n\
             [localhost]:2222 ssh-ed25519 {KEY_NEW}\n\
             [localhost]:2222 ssh-ed25519 {KEY_NEW}\n"
        );
        let (out, n) = rewrite_known_hosts_removing(
            &content,
            "localhost",
            2222,
            1,
            Some("ssh-ed25519"),
        )
        .unwrap();
        assert_eq!(n, 3, "russh-pointed line + 2 stale dups = 3 removed");
        // Every `[localhost]:2222 ssh-ed25519` entry is gone:
        assert!(
            !out.contains("[localhost]:2222 ssh-ed25519"),
            "all same-alg entries for that host:port must be gone; got:\n{out}"
        );
        // But [127.0.0.1]:2222 survives — different host pattern:
        assert!(
            out.contains("[127.0.0.1]:2222"),
            "different-host entries must survive; got:\n{out}"
        );
    }

    #[test]
    fn rewrite_preserves_entries_with_different_algorithm() {
        // If the server offers ssh-ed25519 but the file has a stale
        // ssh-rsa entry for the same host:port, `pending_alg` targets
        // only ssh-ed25519 — the ssh-rsa entry must survive (it may
        // still be valid, and russh picks the alg from the negotiation).
        let content = format!(
            "[localhost]:2222 ssh-ed25519 {KEY_OLD}\n\
             [localhost]:2222 ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB\n"
        );
        let (out, n) = rewrite_known_hosts_removing(
            &content,
            "localhost",
            2222,
            1,
            Some("ssh-ed25519"),
        )
        .unwrap();
        assert_eq!(n, 1);
        assert!(!out.contains(KEY_OLD));
        assert!(out.contains("ssh-rsa"));
    }

    #[test]
    fn rewrite_rejects_line_pointing_to_wrong_host() {
        // Corruption guard: if the russh-pointed line is NOT for our
        // host:port, the file has been modified under our feet — bail
        // loudly rather than deleting an unrelated entry.
        let content = format!(
            "[other.com]:22 ssh-ed25519 {KEY_OLD}\n[localhost]:2222 ssh-ed25519 {KEY_NEW}\n"
        );
        let err =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 1, None);
        assert!(err.is_err(), "wrong-host line must be rejected");
    }

    #[test]
    fn rewrite_tolerates_hashed_target_line() {
        // Hashed entries (|1|...) can't be verified by plain host name.
        // When russh points at one, we remove it on faith — russh
        // resolved the salt internally and already confirmed the match.
        let content =
            "|1|salt=|hash= ssh-ed25519 AAAAC3Nz\n[other.com] ssh-rsa AAAAB\n".to_string();
        let (out, n) =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 1, None).unwrap();
        assert_eq!(n, 1);
        assert!(!out.contains("|1|salt="));
        assert!(out.contains("[other.com]"));
    }

    #[test]
    fn rewrite_out_of_range_line_rejected() {
        let content = "[localhost]:2222 ssh-ed25519 AAAA\n".to_string();
        let err =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 999, None);
        assert!(err.is_err(), "out-of-range line must be rejected");
    }

    #[test]
    fn rewrite_keeps_trailing_newline_when_non_empty() {
        let content = format!(
            "[localhost]:2222 ssh-ed25519 {KEY_OLD}\n[other.com] ssh-rsa AAAAB\n"
        );
        let (out, _) =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 1, None).unwrap();
        assert!(
            out.ends_with('\n'),
            "non-empty known_hosts must keep a trailing newline; got {out:?}"
        );
    }

    #[test]
    fn rewrite_produces_empty_content_when_file_had_single_entry() {
        let content = format!("[localhost]:2222 ssh-ed25519 {KEY_OLD}\n");
        let (out, n) =
            rewrite_known_hosts_removing(&content, "localhost", 2222, 1, None).unwrap();
        assert_eq!(n, 1);
        assert!(
            out.is_empty(),
            "removing the only line must yield empty content (no stray newline); got {out:?}"
        );
    }
}
