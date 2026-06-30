// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Native AeroCrypt overlay: Tauri provider commands (P2a).
//!
//! These commands give the **native** AeroCrypt overlay the same GUI-facing
//! surface the rclone-crypt interop format already has
//! (`rclone_crypt_provider_*` in `lib.rs`), but built on our own audited
//! [`crate::aerocrypt`] codec instead of rclone's wire format. They open the
//! currently-connected provider and encrypt/decrypt on the fly so a dual-panel
//! GUI can browse and transfer through the overlay transparently (master plan
//! 3.6). Per-session unlock returns a `vault_id`; binding the overlay to a saved
//! profile is P3, deliberately not done here.
//!
//! Two facts make this set diverge from the rclone mirror, both inherent to the
//! native model (not a gap):
//! - Names use **AES-256-SIV deterministically over the master key alone**
//!   ([`crate::aerocrypt::names`]); there is no per-directory IV, so name
//!   encoding takes no `is_dir`/`dir_iv` argument.
//! - The overlay's salt lives in a `.aeroftp-crypt.json` config **on the
//!   remote** (rclone keeps its config locally). Unlocking an existing overlay
//!   therefore needs to read that file first, which is why this set adds
//!   [`aerocrypt_provider_read_config`] on top of the eight rclone-parallel
//!   commands.

use std::collections::HashMap;

use serde::Serialize;
use tauri::{AppHandle, State};
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::aerocrypt::overlay::{self, OverlayConfig};
use crate::aerocrypt::{names, KEY_SIZE};
use crate::filesystem::validate_path;
use crate::provider_commands::ProviderState;
use crate::{join_remote_path, sanitize_local_name, validate_single_remote_name};

/// Config file written at the root of an overlay; holds the version + salt the
/// master key is derived from. Skipped from every listing and never decrypted.
const CONFIG_NAME: &str = ".aeroftp-crypt.json";

/// Hard cap on the overlay config read from the (untrusted) remote, so a hostile
/// remote cannot serve a multi-GB "config" and OOM the backend before parsing.
const CONFIG_MAX_BYTES: u64 = 1024 * 1024;

/// Derive the overlay master key off the async executor (Argon2id 128 MiB / t4
/// is CPU/memory heavy and must not block a Tokio worker).
async fn derive_master_key_async(
    config: &OverlayConfig,
    password: &str,
) -> Result<[u8; KEY_SIZE], String> {
    let cfg = config.clone();
    let pw = Zeroizing::new(password.to_string());
    tokio::task::spawn_blocking(move || overlay::derive_master_key(&cfg, &pw))
        .await
        .map_err(|e| format!("Key derivation task failed: {e}"))?
}

/// Maximum directory recursion depth for folder transfers. Matches the
/// rclone-crypt overlay (`RCLONE_OVERLAY_MAX_DEPTH`) and the CLI
/// (`CRYPT_OVERLAY_MAX_DEPTH`) so all three refuse pathologically deep trees
/// identically.
const AEROCRYPT_OVERLAY_MAX_DEPTH: usize = 64;

// ── State ────────────────────────────────────────────────────────────────────

/// Derived material for one unlocked native AeroCrypt overlay. The master key is
/// zeroized on drop; the config carries only the (public) version + salt.
pub struct AeroCryptKeys {
    pub master_key: [u8; KEY_SIZE],
    pub config: OverlayConfig,
}

impl Drop for AeroCryptKeys {
    fn drop(&mut self) {
        self.master_key.zeroize();
    }
}

/// Managed state holding every unlocked native AeroCrypt overlay, keyed by the
/// per-session `vault_id`.
pub struct AeroCryptState {
    pub vaults: Mutex<HashMap<String, AeroCryptKeys>>,
}

impl AeroCryptState {
    pub fn new() -> Self {
        Self {
            vaults: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for AeroCryptState {
    fn default() -> Self {
        Self::new()
    }
}

/// Info returned after unlock / create.
#[derive(Debug, Clone, Serialize)]
pub struct AeroCryptVaultInfo {
    pub vault_id: String,
    pub version: u8,
    /// The overlay config JSON. For a fresh overlay (no `config_json` passed to
    /// unlock) this is the newly generated config the caller must persist as
    /// `.aeroftp-crypt.json` at the overlay root.
    pub config_json: String,
}

/// One listing row with the decrypted name resolved.
#[derive(Serialize)]
pub struct AeroCryptBrowserEntry {
    /// Obfuscated (on-the-wire) name.
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Size on the remote (ciphertext: includes the AECR header + per-block
    /// tags). Surfacing the plaintext size is a later refinement.
    pub size: u64,
    pub modified: Option<String>,
    pub permissions: Option<String>,
    /// Decrypted name, or the raw name when it could not be decrypted.
    pub decrypted_name: String,
    pub decrypt_ok: bool,
}

/// Response for a browse listing through the overlay.
#[derive(Serialize)]
pub struct AeroCryptBrowserListResponse {
    /// Current remote path (obfuscated, as the provider sees it).
    pub current_path: String,
    /// Current remote path with each component decrypted, for display.
    pub display_current_path: String,
    pub files: Vec<AeroCryptBrowserEntry>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Fetch the master key + config for an unlocked vault without holding the
/// state lock across provider I/O.
async fn load_keys(
    state: &State<'_, AeroCryptState>,
    vault_id: &str,
) -> Result<([u8; KEY_SIZE], OverlayConfig), String> {
    let vaults = state.vaults.lock().await;
    let keys = vaults
        .get(vault_id)
        .ok_or_else(|| "Vault not unlocked".to_string())?;
    Ok((keys.master_key, keys.config.clone()))
}

/// Encrypt a single plaintext path component to its obfuscated form.
fn encode_name(master_key: &[u8; KEY_SIZE], plain: &str) -> Result<String, String> {
    names::encrypt_filename(master_key, plain)
}

/// Decrypt an obfuscated path component, or `None` if it is not a valid
/// AeroCrypt name (e.g. the config file or a foreign entry).
fn decode_name(master_key: &[u8; KEY_SIZE], encoded: &str) -> Option<String> {
    names::decrypt_filename(master_key, encoded)
}

/// Encrypt a relative plaintext path component-by-component (deterministic, so
/// the same tree always maps to the same remote layout). `.` segments are
/// dropped; `..` is rejected to forbid traversal in crypt paths.
fn encode_rel_path(master_key: &[u8; KEY_SIZE], rel: &str) -> Result<String, String> {
    let absolute = rel.starts_with('/');
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." || comp.contains('\0') {
            return Err("Invalid AeroCrypt path component".to_string());
        }
        parts.push(encode_name(master_key, comp)?);
    }
    let joined = parts.join("/");
    if absolute {
        Ok(format!("/{}", joined))
    } else {
        Ok(joined)
    }
}

/// Normalize a plaintext crypt anchor: `""`/`"/"` mean "the whole remote is the
/// scope"; otherwise an absolute, slash-trimmed `/Foo/Bar`.
fn norm_anchor(scope: Option<&str>) -> String {
    let s = scope.unwrap_or("").trim();
    if s.is_empty() || s == "/" {
        return String::new();
    }
    format!("/{}", s.trim_matches('/'))
}

/// Normalize an absolute path: leading slash, no trailing slash.
fn norm_abs(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix('/') {
        format!("/{}", stripped.trim_start_matches('/'))
    } else {
        format!("/{}", trimmed)
    }
}

/// Encode a plaintext navigation target for `cd`, encrypting ONLY the components
/// BELOW the overlay's anchor. CWP-20B (B3): a relative target is encrypted in
/// full (it is always resolved against the current in-scope dir, as before). An
/// absolute target keeps the cleartext anchor prefix and encrypts only the tail,
/// so jumping to e.g. `/AeroCryptTest/test` from OUTSIDE the scope (path bar)
/// yields `/AeroCryptTest/<enc(test)>` instead of wrongly encrypting the anchor
/// segment too (which produced an undecryptable cd). FAIL-CLOSED: an absolute
/// target that is not under the anchor is refused, never cd'd blindly.
fn encode_plain_target(
    master_key: &[u8; KEY_SIZE],
    crypt_scope: Option<&str>,
    target: &str,
) -> Result<String, String> {
    if !target.starts_with('/') {
        return encode_rel_path(master_key, target);
    }
    let anchor = norm_anchor(crypt_scope);
    if anchor.is_empty() {
        // Whole-remote scope: every component below root is encrypted.
        return encode_rel_path(master_key, target);
    }
    let t = norm_abs(target);
    if t == anchor {
        return Ok(anchor); // the cleartext anchor root itself, no sub-components
    }
    if let Some(below) = t.strip_prefix(&format!("{}/", anchor)) {
        let enc_below = encode_rel_path(master_key, below)?;
        return Ok(format!("{}/{}", anchor, enc_below));
    }
    Err(format!(
        "crypt navigation target {:?} is outside the overlay scope {:?}",
        target, anchor
    ))
}

/// Decrypt an obfuscated remote path for display, leaving undecryptable
/// components as-is.
fn decode_path(master_key: &[u8; KEY_SIZE], encrypted_path: &str) -> String {
    if encrypted_path.is_empty() || encrypted_path == "." || encrypted_path == "/" {
        return encrypted_path.to_string();
    }
    let absolute = encrypted_path.starts_with('/');
    let parts: Vec<String> = encrypted_path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| decode_name(master_key, part).unwrap_or_else(|| part.to_string()))
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{}", joined)
    } else {
        joined
    }
}

// ── Unlock / lock (pure, no provider I/O) ────────────────────────────────────

/// Unlock a native AeroCrypt overlay for the session.
///
/// `config_json`:
/// - `Some(json)` — an existing overlay; parse it, derive the master key, and
///   verify the key-bound config MAC (v3), so a tampered `version`/`salt` or a
///   wrong password fails closed (the caller reads the config from the remote
///   via [`aerocrypt_provider_read_config`]).
/// - `None` — prepare a fresh overlay: generate a v3 salt + key-bound config and
///   derive the key. The returned `config_json` is what the caller persists on
///   the remote (e.g. via [`aerocrypt_provider_create_remote`]).
#[tauri::command]
pub async fn aerocrypt_unlock(
    state: State<'_, AeroCryptState>,
    password: String,
    config_json: Option<String>,
) -> Result<AeroCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let pw = secrecy::ExposeSecret::expose_secret(&secret_pwd);

    let (config, master_key, config_json_out) = match config_json {
        Some(json) => {
            let config = overlay::parse_config(&json)?;
            let master_key = derive_master_key_async(&config, pw).await?;
            // F1: reject a wrong password or a tampered version/salt before use.
            overlay::verify_config_mac(&config, &master_key)?;
            (config, master_key, json)
        }
        None => {
            let salt = overlay::random_salt_v3();
            let tmp = OverlayConfig::V3 {
                salt,
                mac: [0u8; 32],
            };
            let master_key = derive_master_key_async(&tmp, pw).await?;
            let json = overlay::init_config_v3(&salt, &master_key)?;
            let config = overlay::parse_config(&json)?;
            (config, master_key, json)
        }
    };
    let version = config.version();

    let vault_id = uuid::Uuid::new_v4().to_string();
    let info = AeroCryptVaultInfo {
        vault_id: vault_id.clone(),
        version,
        config_json: config_json_out,
    };

    state
        .vaults
        .lock()
        .await
        .insert(vault_id, AeroCryptKeys { master_key, config });
    Ok(info)
}

/// Lock (forget) an unlocked overlay, zeroizing its master key via `Drop`.
#[tauri::command]
pub async fn aerocrypt_lock(
    state: State<'_, AeroCryptState>,
    vault_id: String,
) -> Result<(), String> {
    let mut vaults = state.vaults.lock().await;
    if vaults.remove(&vault_id).is_none() {
        return Err("Vault not found or already locked".to_string());
    }
    Ok(())
}

// ── Provider commands ────────────────────────────────────────────────────────

/// Read the overlay config (`.aeroftp-crypt.json`) from the provider's current
/// directory. Returns `None` when no overlay is present there, so a GUI can tell
/// "open existing" from "create new". Native-model addition over the rclone set.
#[tauri::command]
pub async fn aerocrypt_provider_read_config(
    provider_state: State<'_, ProviderState>,
    base_path: Option<String>,
) -> Result<Option<String>, String> {
    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    // Anchor the overlay at its configured absolute root, independent of the live
    // pwd. Path-based providers (Filen, etc.) reset current_path to "/" on connect
    // and never cd when they merely *list* a folder, so the overlay would always
    // root at "/". cd into base_path and STAY there, so read_config, the create
    // fallback, and the listing that follows all operate at the overlay root.
    if let Some(bp) = base_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "/")
    {
        match provider.cd(bp).await {
            Ok(()) => log::debug!("[aerocrypt][read_config] anchored at base_path={:?}", bp),
            Err(e) => log::debug!(
                "[aerocrypt][read_config] base_path cd to {:?} failed ({}), staying at pwd",
                bp,
                e
            ),
        }
    }

    let cwd = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    let config_path = join_remote_path(&cwd, CONFIG_NAME);
    log::debug!(
        "[aerocrypt][read_config] provider pwd={:?} -> reading config at {:?}",
        cwd,
        config_path
    );
    // Cap the size before buffering: the config is attacker-controlled (it lives
    // on the remote) and a hostile remote must not be able to OOM us with a giant
    // "config". A real overlay config is well under a kilobyte.
    if let Ok(sz) = provider.size(&config_path).await {
        if sz > CONFIG_MAX_BYTES {
            return Err(format!(
                "crypt config at {} is implausibly large ({} bytes); refusing to read",
                config_path, sz
            ));
        }
    }
    match provider.download_to_bytes(&config_path).await {
        Ok(bytes) => {
            if bytes.len() as u64 > CONFIG_MAX_BYTES {
                return Err("crypt config exceeds the maximum allowed size".to_string());
            }
            log::debug!("[aerocrypt][read_config] FOUND config at {:?}", config_path);
            Ok(Some(String::from_utf8_lossy(&bytes).to_string()))
        }
        Err(_) => {
            log::debug!("[aerocrypt][read_config] NO config at {:?}", config_path);
            Ok(None)
        }
    }
}

/// List an overlay directory, returning decrypted names. `plain_path` (when the
/// `path` is a plaintext name to descend into) is encoded before the `cd`.
#[tauri::command]
pub async fn aerocrypt_provider_list(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    path: Option<String>,
    plain_path: Option<bool>,
    crypt_scope: Option<String>,
) -> Result<AeroCryptBrowserListResponse, String> {
    let (master_key, _config) = load_keys(&aerocrypt_state, &vault_id).await?;

    // Crypt-capability flag: a crypt overlay is in play on this session via the
    // legacy `*_provider_*` command layer (kept until Phase 4). The Phase 3 agent
    // guard (`guard_no_raw_crypt_write`) refuses the raw `gui_tools` paths while
    // the session is crypt-capable but the live provider is NOT wrapped; the
    // on-demand `provider_apply_crypt_overlay` path sets this too. Cleared on
    // connect/disconnect.
    provider_state
        .active_crypt_overlay
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if let Some(target) = path.as_deref() {
        if target == ".." {
            provider
                .cd_up()
                .await
                .map_err(|e| format!("Failed to go up: {}", e))?;
        } else if !target.is_empty() && target != "." {
            let target_path = if plain_path.unwrap_or(false) {
                encode_plain_target(&master_key, crypt_scope.as_deref(), target)?
            } else {
                target.to_string()
            };
            provider
                .cd(&target_path)
                .await
                .map_err(|e| format!("Failed to change directory: {}", e))?;
        }
    }

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    let display_current_path = decode_path(&master_key, &current_path);
    let files = provider
        .list(".")
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let mut out = Vec::new();
    for entry in files {
        if entry.name == CONFIG_NAME {
            continue;
        }
        let (decrypted_name, decrypt_ok) = match decode_name(&master_key, &entry.name) {
            Some(name) => (name, true),
            None => (entry.name.clone(), false),
        };
        out.push(AeroCryptBrowserEntry {
            name: entry.name,
            path: entry.path,
            is_dir: entry.is_dir,
            size: entry.size,
            modified: entry.modified,
            permissions: entry.permissions,
            decrypted_name,
            decrypt_ok,
        });
    }

    Ok(AeroCryptBrowserListResponse {
        current_path,
        display_current_path,
        files: out,
    })
}

/// Create an encrypted-named subdirectory in the current overlay directory.
#[tauri::command]
pub async fn aerocrypt_provider_mkdir(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    plain_name: String,
) -> Result<String, String> {
    validate_single_remote_name(&plain_name)?;
    let (master_key, _config) = load_keys(&aerocrypt_state, &vault_id).await?;
    let encrypted_name = encode_name(&master_key, &plain_name)?;

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    let encrypted_path = join_remote_path(&current_path, &encrypted_name);
    provider
        .mkdir(&encrypted_path)
        .await
        .map_err(|e| format!("Failed to create encrypted folder: {}", e))?;
    Ok(encrypted_path)
}

/// Rename an overlay entry to a new plaintext name, keeping it in place.
#[tauri::command]
pub async fn aerocrypt_provider_rename(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    from_encrypted_path: String,
    new_plain_name: String,
) -> Result<String, String> {
    validate_single_remote_name(&new_plain_name)?;
    let (master_key, _config) = load_keys(&aerocrypt_state, &vault_id).await?;
    let encrypted_name = encode_name(&master_key, &new_plain_name)?;

    let parent = from_encrypted_path
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/");
    let to_encrypted_path = join_remote_path(parent, &encrypted_name);

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .rename(&from_encrypted_path, &to_encrypted_path)
        .await
        .map_err(|e| format!("Failed to rename encrypted entry: {}", e))?;
    Ok(to_encrypted_path)
}

/// Download one encrypted object and decrypt it to `output_path`.
#[tauri::command]
pub async fn aerocrypt_provider_download_file(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    remote_encrypted_path: String,
    output_path: String,
    // AeroSync / cross-profile transfer address a crypt file by its DECRYPTED
    // path; map it to the real encrypted remote path (anchor cleartext, tail
    // encrypted) so the overlay download serves both the browser and the sync
    // engines. Crypt-aware transfer.
    remote_plain_path: Option<String>,
    crypt_scope: Option<String>,
) -> Result<String, String> {
    let (master_key, _config) = load_keys(&aerocrypt_state, &vault_id).await?;

    let remote_encrypted_path = match remote_plain_path.as_deref() {
        Some(p) if !p.trim().is_empty() => {
            encode_plain_target(&master_key, crypt_scope.as_deref(), p)?
        }
        _ => remote_encrypted_path,
    };

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let encrypted = provider
        .download_to_bytes(&remote_encrypted_path)
        .await
        .map_err(|e| format!("Failed to download encrypted file: {}", e))?;

    let plaintext = overlay::decrypt_data(&master_key, &encrypted)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    validate_path(&output_path)?;
    if let Some(parent) = std::path::Path::new(&output_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    write_plaintext_atomic(&output_path, &plaintext).await?;

    Ok(output_path)
}

/// Write decrypted plaintext atomically: stage to a sibling `.aerotmp` file then
/// rename over the target, so an interrupted decrypt never leaves a partial or
/// 0-byte plaintext file (matches the app's atomic-download convention).
async fn write_plaintext_atomic(output_path: &str, plaintext: &[u8]) -> Result<(), String> {
    let tmp = format!("{}.aerotmp", output_path);
    tokio::fs::write(&tmp, plaintext)
        .await
        .map_err(|e| format!("Failed to write output file: {}", e))?;
    if let Err(e) = tokio::fs::rename(&tmp, output_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(format!("Failed to finalize output file: {}", e));
    }
    Ok(())
}

/// Encrypt a local file and upload it under an obfuscated name in the current
/// overlay directory.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn aerocrypt_provider_upload_file(
    app: AppHandle,
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    local_plaintext_path: String,
    remote_plain_name: Option<String>,
    overwrite: Option<bool>,
    // AeroSync / cross-profile transfer to a crypt remote: `remote_plain_path` is
    // the DECRYPTED destination path (possibly nested), within the bound
    // `crypt_scope`. When set, the full path is encrypted and the encrypted
    // parent dirs created, so a selective sync lands nested files in the right
    // encrypted subtree. Crypt-aware transfer.
    remote_plain_path: Option<String>,
    crypt_scope: Option<String>,
) -> Result<String, String> {
    let overwrite = overwrite.unwrap_or(false);
    validate_path(&local_plaintext_path)?;
    let local_meta = std::fs::symlink_metadata(std::path::Path::new(&local_plaintext_path))
        .map_err(|e| format!("Failed to inspect local file: {}", e))?;
    if local_meta.file_type().is_symlink() {
        return Err("Local plaintext path cannot be a symlink".to_string());
    }
    if !local_meta.is_file() {
        return Err("Local plaintext path must be a regular file".to_string());
    }

    let (master_key, config) = load_keys(&aerocrypt_state, &vault_id).await?;

    let plaintext = tokio::fs::read(&local_plaintext_path)
        .await
        .map_err(|e| format!("Failed to read local file: {}", e))?;
    let encrypted_payload = overlay::encrypt_data(&config, &master_key, &plaintext)
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let encoded_target = match remote_plain_path.as_deref() {
        Some(p) if !p.trim().is_empty() => {
            Some(encode_plain_target(&master_key, crypt_scope.as_deref(), p)?)
        }
        _ => None,
    };

    let plain_name = match remote_plain_path.as_deref() {
        Some(p) if !p.trim().is_empty() => std::path::Path::new(p.trim_end_matches('/'))
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or_else(|| "Cannot determine destination filename".to_string())?,
        _ => remote_plain_name
            .and_then(|s| {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
            .or_else(|| {
                std::path::Path::new(&local_plaintext_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            })
            .ok_or_else(|| "Cannot determine destination filename".to_string())?,
    };

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    let remote_encrypted_path = match encoded_target {
        Some(target) => {
            // Create the encrypted parent directories below the cleartext anchor
            // before placing a nested file (best-effort: mkdir on an existing dir
            // is ignored).
            let anchor = norm_anchor(crypt_scope.as_deref());
            let anchor_depth = if anchor.is_empty() {
                0
            } else {
                anchor.trim_matches('/').split('/').count()
            };
            let segs: Vec<&str> = target
                .trim_start_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            if segs.len() > 1 {
                let mut cur = String::new();
                for (i, seg) in segs[..segs.len() - 1].iter().enumerate() {
                    cur = format!("{}/{}", cur, seg);
                    if i >= anchor_depth {
                        let _ = provider.mkdir(&cur).await;
                    }
                }
            }
            target
        }
        None => {
            let encrypted_name = encode_name(&master_key, &plain_name)?;
            join_remote_path(&current_path, &encrypted_name)
        }
    };
    if !overwrite && provider.stat(&remote_encrypted_path).await.is_ok() {
        return Err(format!(
            "Encrypted target already exists: {}",
            remote_encrypted_path
        ));
    }
    let temp_path = std::env::temp_dir().join(format!(
        "aeroftp_aerocrypt_upload_{}_{}.bin",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp_path, &encrypted_payload)
        .await
        .map_err(|e| format!("Failed to write encrypted temp file: {}", e))?;

    // #364: drive the Transfer Queue per-item bar for the AeroCrypt overlay
    // upload (parity with the rclone-crypt path). Adopted by plaintext name; the
    // id must not contain `folder` so it routes through the per-file path.
    let upload_total = encrypted_payload.len() as u64;
    let transfer_id = format!(
        "aerocrypt-file-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    );
    crate::emit_crypt_file_event(
        &app,
        "start",
        &transfer_id,
        &plain_name,
        &remote_encrypted_path,
        0,
        upload_total,
        0,
        None,
    );
    let progress_app = app.clone();
    let progress_id = transfer_id.clone();
    let progress_name = plain_name.clone();
    let progress_remote = remote_encrypted_path.clone();
    let started = std::time::Instant::now();
    let on_progress: Box<dyn Fn(u64, u64) + Send> = Box::new(move |transferred, total| {
        let elapsed = started.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            (transferred as f64 / elapsed) as u64
        } else {
            0
        };
        crate::emit_crypt_file_event(
            &progress_app,
            "progress",
            &progress_id,
            &progress_name,
            &progress_remote,
            transferred,
            total,
            speed,
            None,
        );
    });

    let upload_result = provider
        .upload(
            &temp_path.to_string_lossy(),
            &remote_encrypted_path,
            Some(on_progress),
        )
        .await
        .map_err(|e| format!("Failed to upload encrypted file: {}", e));

    let _ = tokio::fs::remove_file(&temp_path).await;
    match &upload_result {
        Ok(()) => crate::emit_crypt_file_event(
            &app,
            "complete",
            &transfer_id,
            &plain_name,
            &remote_encrypted_path,
            upload_total,
            upload_total,
            0,
            None,
        ),
        Err(e) => crate::emit_crypt_file_event(
            &app,
            "error",
            &transfer_id,
            &plain_name,
            &remote_encrypted_path,
            0,
            upload_total,
            0,
            Some(e.clone()),
        ),
    }
    upload_result?;
    Ok(remote_encrypted_path)
}

/// Recursively download an encrypted overlay subtree, rebuilding the plaintext
/// tree under `local_dest_root`. Undecryptable entries are skipped.
#[tauri::command]
pub async fn aerocrypt_provider_download_folder(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    remote_encrypted_path: String,
    local_dest_root: String,
) -> Result<String, String> {
    validate_path(&local_dest_root)?;
    tokio::fs::create_dir_all(&local_dest_root)
        .await
        .map_err(|e| format!("Failed to create local destination: {}", e))?;

    let (master_key, _config) = load_keys(&aerocrypt_state, &vault_id).await?;

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let saved_pwd = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    let mut files_done: u64 = 0;

    let mut stack: Vec<(String, String, usize)> =
        vec![(remote_encrypted_path.clone(), local_dest_root.clone(), 0)];
    let walk_result: Result<(), String> = async {
        while let Some((remote_dir, local_dir, depth)) = stack.pop() {
            if depth >= AEROCRYPT_OVERLAY_MAX_DEPTH {
                return Err(format!(
                    "AeroCrypt overlay: directory depth {} exceeds {}",
                    depth, AEROCRYPT_OVERLAY_MAX_DEPTH
                ));
            }
            tokio::fs::create_dir_all(&local_dir)
                .await
                .map_err(|e| format!("Failed to create {}: {}", local_dir, e))?;

            provider
                .cd(&remote_dir)
                .await
                .map_err(|e| format!("Failed to cd into {}: {}", remote_dir, e))?;
            let resolved_dir = provider.pwd().await.unwrap_or_else(|_| remote_dir.clone());

            let entries = provider
                .list(".")
                .await
                .map_err(|e| format!("Failed to list {}: {}", resolved_dir, e))?;

            for entry in entries {
                if entry.name == CONFIG_NAME {
                    continue;
                }
                let plain_name = match decode_name(&master_key, &entry.name) {
                    Some(n) => n,
                    None => continue, // skip undecryptable entries
                };
                let safe_name = sanitize_local_name(&plain_name);
                let local_target = std::path::Path::new(&local_dir)
                    .join(&safe_name)
                    .to_string_lossy()
                    .to_string();
                let remote_child = join_remote_path(&resolved_dir, &entry.name);

                if entry.is_dir {
                    stack.push((remote_child, local_target, depth + 1));
                    continue;
                }

                let encrypted_blob = provider
                    .download_to_bytes(&remote_child)
                    .await
                    .map_err(|e| format!("Failed to download {}: {}", remote_child, e))?;
                let plaintext = overlay::decrypt_data(&master_key, &encrypted_blob)
                    .map_err(|e| format!("Decrypt failed for {}: {}", plain_name, e))?;
                write_plaintext_atomic(&local_target, &plaintext).await?;
                files_done += 1;
            }
        }
        Ok(())
    }
    .await;

    let _ = provider.cd(&saved_pwd).await;
    walk_result?;

    Ok(format!(
        "Downloaded {} files into {}",
        files_done, local_dest_root
    ))
}

/// Recursively encrypt and upload a local folder into the overlay, recreating it
/// as an encrypted-named subtree under `remote_parent_path` (or the current dir).
#[tauri::command]
pub async fn aerocrypt_provider_upload_folder(
    app: AppHandle,
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    vault_id: String,
    local_path: String,
    remote_parent_path: Option<String>,
    overwrite: Option<bool>,
) -> Result<String, String> {
    let overwrite = overwrite.unwrap_or(false);
    validate_path(&local_path)?;
    let local_meta = std::fs::symlink_metadata(std::path::Path::new(&local_path))
        .map_err(|e| format!("Failed to inspect local folder: {}", e))?;
    if local_meta.file_type().is_symlink() {
        return Err("Local folder cannot be a symlink".to_string());
    }
    if !local_meta.is_dir() {
        return Err("Local path must be a directory".to_string());
    }

    let (master_key, config) = load_keys(&aerocrypt_state, &vault_id).await?;

    let mut provider_lock = provider_state.provider.lock().await;
    let provider = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let saved_pwd = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

    let parent_remote = match remote_parent_path.as_deref() {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => saved_pwd.clone(),
    };
    let local_root_name = std::path::Path::new(&local_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| "Cannot determine root folder name".to_string())?;

    // #364: surface Transfer Queue folder progress (parity with rclone-crypt).
    let total_files = crate::rclone_overlay_count_local_files(std::path::Path::new(&local_path));
    let transfer_id = format!(
        "aerocrypt-folder-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    );
    let started = std::time::Instant::now();
    crate::emit_crypt_folder_event(
        &app,
        "start",
        &transfer_id,
        &local_root_name,
        &local_path,
        0,
        total_files,
        0,
        None,
    );

    let mut files_uploaded: u64 = 0;

    let walk_result: Result<(), String> = async {
        provider
            .cd(&parent_remote)
            .await
            .map_err(|e| format!("Failed to cd into {}: {}", parent_remote, e))?;
        let resolved_parent = provider
            .pwd()
            .await
            .unwrap_or_else(|_| parent_remote.clone());
        let root_enc_name = encode_name(&master_key, &local_root_name)?;
        let root_remote = join_remote_path(&resolved_parent, &root_enc_name);
        if !overwrite && provider.stat(&root_remote).await.is_ok() {
            return Err(format!("Encrypted target already exists: {}", root_remote));
        }
        let _ = provider.mkdir(&root_remote).await; // best-effort: may already exist

        let mut stack: Vec<(std::path::PathBuf, String, usize)> =
            vec![(std::path::PathBuf::from(&local_path), root_remote, 0)];

        while let Some((local_dir, remote_dir, depth)) = stack.pop() {
            if depth >= AEROCRYPT_OVERLAY_MAX_DEPTH {
                return Err(format!(
                    "AeroCrypt overlay upload: depth {} exceeds {}",
                    depth, AEROCRYPT_OVERLAY_MAX_DEPTH
                ));
            }

            provider
                .cd(&remote_dir)
                .await
                .map_err(|e| format!("Failed to cd into {}: {}", remote_dir, e))?;
            let resolved_dir = provider.pwd().await.unwrap_or_else(|_| remote_dir.clone());

            let mut rd = tokio::fs::read_dir(&local_dir)
                .await
                .map_err(|e| format!("Failed to read {}: {}", local_dir.display(), e))?;
            while let Some(entry) = rd
                .next_entry()
                .await
                .map_err(|e| format!("Failed to walk {}: {}", local_dir.display(), e))?
            {
                let entry_path = entry.path();
                let entry_meta = match entry.metadata().await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if entry_meta.file_type().is_symlink() {
                    continue;
                }
                let entry_name = entry.file_name().to_string_lossy().to_string();
                if entry_name.is_empty() {
                    continue;
                }
                let encoded = encode_name(&master_key, &entry_name)?;
                let encoded_remote = join_remote_path(&resolved_dir, &encoded);

                if entry_meta.is_dir() {
                    let _ = provider.mkdir(&encoded_remote).await;
                    stack.push((entry_path, encoded_remote, depth + 1));
                } else if entry_meta.is_file() {
                    if !overwrite && provider.stat(&encoded_remote).await.is_ok() {
                        return Err(format!(
                            "Encrypted target already exists: {}",
                            encoded_remote
                        ));
                    }
                    let plaintext = tokio::fs::read(&entry_path)
                        .await
                        .map_err(|e| format!("Failed to read {}: {}", entry_path.display(), e))?;
                    let cipher = overlay::encrypt_data(&config, &master_key, &plaintext)
                        .map_err(|e| format!("Encrypt failed for {}: {}", entry_name, e))?;

                    let temp = std::env::temp_dir().join(format!(
                        "aeroftp_aerocrypt_upfolder_{}_{}.bin",
                        chrono::Utc::now().timestamp_millis(),
                        uuid::Uuid::new_v4()
                    ));
                    tokio::fs::write(&temp, &cipher)
                        .await
                        .map_err(|e| format!("Failed to stage encrypted blob: {}", e))?;
                    let up = provider
                        .upload(&temp.to_string_lossy(), &encoded_remote, None)
                        .await
                        .map_err(|e| format!("Failed to upload {}: {}", entry_name, e));
                    let _ = tokio::fs::remove_file(&temp).await;
                    up?;
                    files_uploaded += 1;
                    let elapsed = started.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        (cipher.len() as f64 / elapsed) as u64
                    } else {
                        0
                    };
                    crate::emit_crypt_folder_event(
                        &app,
                        "progress",
                        &transfer_id,
                        &local_root_name,
                        &local_path,
                        files_uploaded,
                        total_files.max(files_uploaded),
                        speed,
                        None,
                    );
                }
            }
        }
        Ok(())
    }
    .await;

    let _ = provider.cd(&saved_pwd).await;
    match &walk_result {
        Ok(()) => crate::emit_crypt_folder_event(
            &app,
            "complete",
            &transfer_id,
            &local_root_name,
            &local_path,
            files_uploaded,
            total_files.max(files_uploaded),
            0,
            None,
        ),
        Err(e) => crate::emit_crypt_folder_event(
            &app,
            "error",
            &transfer_id,
            &local_root_name,
            &local_path,
            files_uploaded,
            total_files.max(files_uploaded),
            0,
            Some(e.clone()),
        ),
    }
    walk_result?;

    Ok(format!(
        "Uploaded {} files from {} into encrypted overlay",
        files_uploaded, local_path
    ))
}

/// Bootstrap a brand-new native AeroCrypt overlay on the connected provider:
/// generate a fresh v3 (key-bound) config, derive the key, write
/// `.aeroftp-crypt.json` at the (optional) sub-path, and register the unlocked
/// vault. Refuses to overwrite an existing overlay unless `force` is set, because
/// re-initializing rotates the salt and would orphan every existing file.
#[tauri::command]
pub async fn aerocrypt_provider_create_remote(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    password: String,
    target_subpath: Option<String>,
    force: Option<bool>,
) -> Result<AeroCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let force = force.unwrap_or(false);
    let salt = overlay::random_salt_v3();
    let tmp_cfg = OverlayConfig::V3 {
        salt,
        mac: [0u8; 32],
    };
    let master_key =
        derive_master_key_async(&tmp_cfg, secrecy::ExposeSecret::expose_secret(&secret_pwd))
            .await?;
    let config_json = overlay::init_config_v3(&salt, &master_key)?;
    let config = overlay::parse_config(&config_json)?;

    {
        let mut provider_lock = provider_state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        let saved_pwd = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

        let target = target_subpath
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        log::debug!(
            "[aerocrypt][create_remote] saved_pwd={:?} target_subpath={:?}",
            saved_pwd,
            target
        );

        let init_result: Result<(), String> = async {
            if let Some(sub) = target {
                let _ = provider.mkdir(sub).await; // idempotent
                provider
                    .cd(sub)
                    .await
                    .map_err(|e| format!("Failed to cd into {}: {}", sub, e))?;
            }

            let base = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
            let config_path = join_remote_path(&base, CONFIG_NAME);
            // C4: never silently clobber an existing overlay. Re-init rotates the
            // salt, which permanently orphans every file already encrypted here.
            if !force && provider.size(&config_path).await.is_ok() {
                return Err(format!(
                    "an AeroCrypt overlay already exists at {}; refusing to re-initialize \
                     (existing files would become permanently undecryptable). \
                     Pass force=true to overwrite.",
                    base
                ));
            }
            log::debug!(
                "[aerocrypt][create_remote] after cd: base(pwd)={:?} writing config to {:?}",
                base,
                config_path
            );
            let temp = std::env::temp_dir().join(format!(
                "aeroftp_aerocrypt_config_{}_{}.json",
                chrono::Utc::now().timestamp_millis(),
                uuid::Uuid::new_v4()
            ));
            tokio::fs::write(&temp, &config_json)
                .await
                .map_err(|e| format!("Failed to stage overlay config: {}", e))?;
            let up = provider
                .upload(&temp.to_string_lossy(), &config_path, None)
                .await
                .map_err(|e| format!("Failed to write overlay config: {}", e));
            let _ = tokio::fs::remove_file(&temp).await;
            up
        }
        .await;

        let _ = provider.cd(&saved_pwd).await;
        init_result?;
    }

    let version = config.version();
    let vault_id = uuid::Uuid::new_v4().to_string();
    let info = AeroCryptVaultInfo {
        vault_id: vault_id.clone(),
        version,
        config_json,
    };
    aerocrypt_state
        .vaults
        .lock()
        .await
        .insert(vault_id, AeroCryptKeys { master_key, config });
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_master_key() -> [u8; KEY_SIZE] {
        let cfg = OverlayConfig::V2 { salt: [7u8; 32] };
        overlay::derive_master_key(&cfg, "correct horse battery staple").unwrap()
    }

    #[test]
    fn name_encode_decode_round_trip() {
        let master = test_master_key();
        let enc = encode_name(&master, "report 2026.pdf").unwrap();
        assert_ne!(enc, "report 2026.pdf");
        assert_eq!(
            decode_name(&master, &enc).as_deref(),
            Some("report 2026.pdf")
        );
        // A foreign / config entry must not decode.
        assert_eq!(decode_name(&master, CONFIG_NAME), None);
    }

    #[test]
    fn encode_rel_path_encodes_each_component_and_rejects_dotdot() {
        let master = test_master_key();
        let encoded = encode_rel_path(&master, "docs/2026/report.pdf").unwrap();
        let parts: Vec<&str> = encoded.split('/').collect();
        assert_eq!(parts.len(), 3);
        // Each component round-trips back to the plaintext name.
        let decoded: Vec<String> = parts
            .iter()
            .map(|c| decode_name(&master, c).expect("component decodes"))
            .collect();
        assert_eq!(decoded, vec!["docs", "2026", "report.pdf"]);
        // Empty / `.` segments drop; `..` is rejected.
        assert_eq!(
            encode_rel_path(&master, "a/./b")
                .unwrap()
                .split('/')
                .count(),
            2
        );
        assert!(encode_rel_path(&master, "a/../b").is_err());
    }

    #[test]
    fn encode_plain_target_relative_encodes_full() {
        let master = test_master_key();
        // A relative target is encrypted in full (resolved against the in-scope cwd).
        let enc = encode_plain_target(&master, Some("/AeroCryptTest"), "test").unwrap();
        assert!(!enc.starts_with('/'));
        assert_eq!(decode_name(&master, &enc).as_deref(), Some("test"));
    }

    #[test]
    fn encode_plain_target_absolute_keeps_anchor_cleartext() {
        let master = test_master_key();
        // CWP-20B (B3): an absolute in-scope target keeps the cleartext anchor and
        // encrypts ONLY the components below it.
        let enc =
            encode_plain_target(&master, Some("/AeroCryptTest"), "/AeroCryptTest/test").unwrap();
        assert!(enc.starts_with("/AeroCryptTest/"));
        let tail = enc.strip_prefix("/AeroCryptTest/").unwrap();
        assert!(!tail.contains('/'));
        assert_eq!(decode_name(&master, tail).as_deref(), Some("test"));

        // Deeper tail: every below-anchor component is encrypted, anchor stays clear.
        let deep =
            encode_plain_target(&master, Some("/AeroCryptTest"), "/AeroCryptTest/a/b").unwrap();
        let deep_tail = deep.strip_prefix("/AeroCryptTest/").unwrap();
        let comps: Vec<String> = deep_tail
            .split('/')
            .map(|c| decode_name(&master, c).expect("component decodes"))
            .collect();
        assert_eq!(comps, vec!["a", "b"]);
    }

    #[test]
    fn encode_plain_target_anchor_root_is_cleartext() {
        let master = test_master_key();
        // The anchor root itself has no sub-components: cleartext, no encryption.
        assert_eq!(
            encode_plain_target(&master, Some("/AeroCryptTest"), "/AeroCryptTest").unwrap(),
            "/AeroCryptTest"
        );
        // Trailing slash tolerated.
        assert_eq!(
            encode_plain_target(&master, Some("/AeroCryptTest/"), "/AeroCryptTest/").unwrap(),
            "/AeroCryptTest"
        );
    }

    #[test]
    fn encode_plain_target_outside_anchor_is_refused() {
        let master = test_master_key();
        // FAIL-CLOSED: an absolute target not under the anchor is rejected, never
        // cd'd blindly.
        assert!(encode_plain_target(&master, Some("/AeroCryptTest"), "/elsewhere/x").is_err());
    }

    #[test]
    fn encode_plain_target_whole_remote_encrypts_all() {
        let master = test_master_key();
        // Whole-remote scope ('' / '/'): every component below root is encrypted.
        for scope in [None, Some(""), Some("/")] {
            let enc = encode_plain_target(&master, scope, "/a/b").unwrap();
            let comps: Vec<String> = enc
                .trim_start_matches('/')
                .split('/')
                .map(|c| decode_name(&master, c).expect("component decodes"))
                .collect();
            assert_eq!(comps, vec!["a", "b"]);
        }
    }

    #[test]
    fn decode_path_leaves_undecryptable_components_intact() {
        let master = test_master_key();
        let enc_a = encode_name(&master, "alpha").unwrap();
        let path = format!("/{}/not-a-valid-name", enc_a);
        assert_eq!(decode_path(&master, &path), "/alpha/not-a-valid-name");
        assert_eq!(decode_path(&master, "/"), "/");
    }

    #[test]
    fn file_round_trip_through_codec() {
        let salt = [9u8; 32];
        let master = overlay::derive_master_key(
            &OverlayConfig::V3 {
                salt,
                mac: [0u8; 32],
            },
            "pw",
        )
        .unwrap();
        let json = overlay::init_config_v3(&salt, &master).unwrap();
        let cfg = overlay::parse_config(&json).unwrap();
        let plaintext = b"AeroCrypt provider round trip".repeat(4096);
        let blob = overlay::encrypt_data(&cfg, &master, &plaintext).unwrap();
        assert_eq!(overlay::decrypt_data(&master, &blob).unwrap(), plaintext);
    }
}
