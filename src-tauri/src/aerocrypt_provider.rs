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
use tauri::State;
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::aerocrypt::overlay::{self, OverlayConfig};
use crate::aerocrypt::{emergency_kit, KEY_SIZE};
use crate::join_remote_path;
use crate::provider_commands::ProviderState;

/// Config file written at the root of an overlay; holds the version + salt the
/// master key is derived from. Skipped from every listing and never decrypted.
const CONFIG_NAME: &str = crate::aerocrypt::overlay::CRYPT_CONFIG_WRITE_NAME; // write new; read-both handled at call sites

/// Hard cap on the overlay config read from the (untrusted) remote, so a hostile
/// remote cannot serve a multi-GB "config" and OOM the backend before parsing.
const CONFIG_MAX_BYTES: u64 = 1024 * 1024;

/// Derive the overlay master key off the async executor (Argon2id 128 MiB / t4
/// is CPU/memory heavy and must not block a Tokio worker). `keyfile_digest` is
/// the OPTIONAL AeroCrypt Tier 1 second factor, mixed into the KDF secret.
async fn derive_master_key_async(
    config: &OverlayConfig,
    password: &str,
    keyfile_digest: Option<[u8; KEY_SIZE]>,
) -> Result<[u8; KEY_SIZE], String> {
    let cfg = config.clone();
    let pw = Zeroizing::new(password.to_string());
    tokio::task::spawn_blocking(move || {
        overlay::derive_master_key_with_keyfile(&cfg, &pw, keyfile_digest.as_ref())
    })
    .await
    .map_err(|e| format!("Key derivation task failed: {e}"))?
}

/// Resolve an optional keyfile path from the unlock UI to its digest,
/// fail-closed (an unreadable path is an error, never a silent password-only
/// derivation). Empty/whitespace paths mean "no keyfile".
fn resolve_ui_keyfile_digest(keyfile_path: Option<&str>) -> Result<Option<[u8; KEY_SIZE]>, String> {
    match keyfile_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => crate::crypt_overlay_provider::keyfile_digest_from_path(p).map(Some),
        None => Ok(None),
    }
}

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
    keyfile_path: Option<String>,
) -> Result<AeroCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let pw = secrecy::ExposeSecret::expose_secret(&secret_pwd);
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;

    let (config, master_key, config_json_out) = match config_json {
        Some(json) => {
            let config = overlay::parse_config(&json)?;
            // Tier 1: reconcile the supplied keyfile against what the config
            // requires BEFORE the expensive KDF, so a missing or spurious
            // keyfile is a clear error instead of a confusing "wrong password".
            let keyfile_digest = match (config.requires_keyfile(), keyfile_digest) {
                (true, None) => {
                    return Err(
                        "this AeroCrypt overlay requires a keyfile (none was provided)".to_string(),
                    )
                }
                (false, Some(_)) => {
                    return Err(
                        "this AeroCrypt overlay was not created with a keyfile (remove the keyfile to unlock)"
                            .to_string(),
                    )
                }
                (true, kd) => kd,
                (false, _) => None,
            };
            let master_key = derive_master_key_async(&config, pw, keyfile_digest).await?;
            // F1: reject a wrong password or a tampered version/salt before use.
            overlay::verify_config_mac(&config, &master_key)?;
            (config, master_key, json)
        }
        None => {
            let salt = overlay::random_salt_v3();
            let tmp = OverlayConfig::v3_bootstrap(salt);
            let master_key = derive_master_key_async(&tmp, pw, keyfile_digest).await?;
            // With a keyfile the config records kdf_inputs + a fresh vault_id
            // and omits keyfile_hint by default (F5), mirroring the CLI init.
            let json = if keyfile_digest.is_some() {
                overlay::init_config_v3_with_keyfile(
                    &salt,
                    &master_key,
                    &overlay::random_vault_id(),
                    None,
                    overlay::SaltMode::PerVault,
                )?
            } else {
                overlay::init_config_v3(&salt, &master_key)?
            };
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
    let new_name = overlay::CRYPT_CONFIG_WRITE_NAME;
    let legacy_name = overlay::CRYPT_CONFIG_LEGACY_NAME;
    let new_path = join_remote_path(&cwd, new_name);
    let legacy_path = join_remote_path(&cwd, legacy_name);

    // Read-both: prefer the new .aerocrypt.tsv, fall back to legacy .aeroftp-crypt.json
    let config_path = if provider.exists(&new_path).await.unwrap_or(false) {
        new_path
    } else if provider.exists(&legacy_path).await.unwrap_or(false) {
        legacy_path
    } else {
        new_path // for "not found" case below
    };

    log::debug!(
        "[aerocrypt][read_config] provider pwd={:?} -> reading config at {:?} (read-both)",
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
    keyfile_path: Option<String>,
    use_default_salt: Option<bool>,
) -> Result<AeroCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let force = force.unwrap_or(false);
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let use_default = use_default_salt.unwrap_or(false);
    let salt = if use_default {
        crate::aerocrypt::AEROCRYPT_DEFAULT_SALT_V1
    } else {
        overlay::random_salt_v3()
    };
    let salt_mode = if use_default {
        overlay::SaltMode::DefaultV1
    } else {
        overlay::SaltMode::PerVault
    };
    let tmp_cfg = OverlayConfig::v3_bootstrap(salt);
    let master_key = derive_master_key_async(
        &tmp_cfg,
        secrecy::ExposeSecret::expose_secret(&secret_pwd),
        keyfile_digest,
    )
    .await?;
    // Keyfile vaults record kdf_inputs + a fresh vault_id (no keyfile_hint by
    // default, F5), so any client knows the second factor is required.
    let config_json = if keyfile_digest.is_some() {
        overlay::init_config_v3_with_keyfile(
            &salt,
            &master_key,
            &overlay::random_vault_id(),
            None,
            salt_mode,
        )?
    } else {
        overlay::init_config_v3_with_vault_id(
            &salt,
            &master_key,
            &overlay::random_vault_id(),
            salt_mode,
        )?
    };
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

/// Build and return the mandatory Emergency Kit from a (persisted) config JSON.
/// This is the single builder implementation also used by the CLI.
/// The caller (GUI create flow) is expected to have already persisted the
/// config (via headerless keystore store or remote marker) and now reads it
/// back before calling, exactly as the CLI does.
#[tauri::command]
pub fn aerocrypt_build_emergency_kit(
    config_json: String,
) -> Result<emergency_kit::EmergencyKit, String> {
    emergency_kit::build_from_config_json(&config_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip_through_codec() {
        let salt = [9u8; 32];
        let master = overlay::derive_master_key(&OverlayConfig::v3_bootstrap(salt), "pw").unwrap();
        let json = overlay::init_config_v3(&salt, &master).unwrap();
        let cfg = overlay::parse_config(&json).unwrap();
        let plaintext = b"AeroCrypt provider round trip".repeat(4096);
        let blob = overlay::encrypt_data(&cfg, &master, &plaintext).unwrap();
        assert_eq!(overlay::decrypt_data(&master, &blob).unwrap(), plaintext);
    }
}
