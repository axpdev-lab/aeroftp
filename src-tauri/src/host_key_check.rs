//! SFTP/SSH Host Key Verification (TOFU UX)
//!
//! Pre-check probe for host key verification before actual connection.
//! Returns fingerprint + algorithm to frontend for user approval dialog.

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
                    self.host, self.port, algorithm
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
                    self.host, self.port, line, algorithm
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
                    self.host, self.port, e
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

/// Remove a host key entry from ~/.ssh/known_hosts (for key-changed case).
/// Uses the line number from the KeyChanged error for precise removal.
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

    let lines: Vec<&str> = content.lines().collect();

    // `line` from russh's KeyChanged is 0-based line index
    if line >= lines.len() {
        return Err(format!(
            "Line {} out of range (file has {} lines)",
            line,
            lines.len()
        ));
    }

    // Verify the line actually contains our host before removing
    let target_line = lines[line];
    let host_pattern = if port != 22 {
        format!("[{}]:{}", host, port)
    } else {
        host.clone()
    };

    // For hashed entries (|1|...), we can't verify by hostname — trust the line number
    let is_hashed = target_line.starts_with("|1|");
    if !is_hashed && !target_line.starts_with(&host_pattern) {
        return Err(format!(
            "Line {} does not match host {} — file may have been modified",
            line, host
        ));
    }

    // Remove the line and write back atomically
    let new_lines: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != line)
        .map(|(_, l)| *l)
        .collect();

    let temp_path = known_hosts_path.with_extension("tmp");
    std::fs::write(&temp_path, new_lines.join("\n") + "\n")
        .map_err(|e| format!("Write error: {}", e))?;
    std::fs::rename(&temp_path, &known_hosts_path)
        .map_err(|e| format!("Rename error: {}", e))?;

    tracing::info!(
        "Removed old host key for {}:{} from known_hosts line {}",
        host,
        port,
        line
    );
    Ok(())
}
