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
//! - The overlay's salt lives in a remote marker (`.aerocrypt.tsv` for new
//!   vaults, legacy `.aeroftp-crypt.json` still readable). rclone keeps its
//!   config locally. Unlocking an existing overlay therefore needs to read that
//!   marker first, which is why this set adds [`aerocrypt_provider_read_config`]
//!   on top of the eight rclone-parallel commands.

use std::collections::HashMap;

use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

use crate::aerocrypt::keyslots::{
    derive_slot_key, Argon2Params, SlotBinding, SlotFactor, SlotType,
};
use crate::aerocrypt::overlay::{self, OverlayConfig, SlotKeyMaterial};
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
    /// The overlay config text. For a fresh overlay (no `config_json` passed to
    /// unlock) this is the newly generated config the caller must persist as the
    /// `.aerocrypt.tsv` marker at the overlay root.
    pub config_json: String,
}

/// Lightweight marker probe for the unlock modal. It carries no config bytes and
/// is safe to expose before the user enters credentials.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroCryptMarkerStatus {
    pub has_current_marker: bool,
    pub has_legacy_marker: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroCryptMarkerMigrationResult {
    pub changed: bool,
    pub legacy_deleted: bool,
    pub warning: Option<String>,
}

// ── Unlock / lock (pure, no provider I/O) ────────────────────────────────────

/// Unlock a native AeroCrypt overlay for the session.
///
/// `config_json`:
/// - `Some(json)` - an existing overlay; parse it, derive the master key, and
///   verify the key-bound config MAC (v3), so a tampered `version`/`salt` or a
///   wrong password fails closed (the caller reads the config from the remote
///   via [`aerocrypt_provider_read_config`]).
/// - `None` - prepare a fresh overlay: generate a v3 salt + key-bound config and
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

/// Read the overlay config marker from the provider's current directory. Returns
/// `None` when no overlay is present there, so a GUI can tell "open existing"
/// from "create new". Native-model addition over the rclone set.
#[tauri::command]
pub async fn aerocrypt_provider_read_config(
    provider_state: State<'_, ProviderState>,
    base_path: Option<String>,
) -> Result<Option<String>, String> {
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    // Markers live as cleartext names on the wire. When the live slot is wrapped
    // by CryptOverlayProvider, path mapping would encrypt `.aerocrypt.tsv` /
    // `.aeroftp-crypt.json` and miss them. Always peel to the concrete transport.
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());

    // Anchor the overlay at its configured absolute root, independent of the live
    // pwd. Path-based providers (Filen, etc.) reset current_path to "/" on connect
    // and never cd when they merely *list* a folder, so the overlay would always
    // root at "/". Prefer an absolute marker path under base_path when given.
    let cwd = if let Some(bp) = base_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "/")
    {
        bp.to_string()
    } else {
        provider.pwd().await.unwrap_or_else(|_| "/".to_string())
    };
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
        "[aerocrypt][read_config] provider root={:?} -> reading config at {:?} (read-both, raw transport)",
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

async fn read_marker_text(
    provider: &mut dyn crate::providers::StorageProvider,
    path: &str,
) -> Result<String, String> {
    if let Ok(sz) = provider.size(path).await {
        if sz > CONFIG_MAX_BYTES {
            return Err(format!(
                "crypt config at {} is implausibly large ({} bytes); refusing to read",
                path, sz
            ));
        }
    }
    let bytes = provider
        .download_to_bytes(path)
        .await
        .map_err(|e| format!("Cannot read AeroCrypt marker: {e}"))?;
    if bytes.len() as u64 > CONFIG_MAX_BYTES {
        return Err("crypt config exceeds the maximum allowed size".to_string());
    }
    String::from_utf8(bytes).map_err(|e| format!("AeroCrypt marker is not valid UTF-8: {e}"))
}

async fn validate_marker_for_migration(
    config_text: &str,
    password: &str,
    keyfile_digest: Option<[u8; KEY_SIZE]>,
) -> Result<(OverlayConfig, [u8; KEY_SIZE]), String> {
    let config = overlay::parse_config(config_text)?;
    if config.is_read_only() {
        return Err(format!(
            "AeroCrypt v{} metadata migration is not supported; only v3 vaults can migrate",
            config.version()
        ));
    }
    let keyfile_digest = match (config.requires_keyfile(), keyfile_digest) {
        (true, None) => {
            return Err("this AeroCrypt overlay requires a keyfile (none was provided)".to_string())
        }
        (false, Some(_)) => return Err(
            "this AeroCrypt overlay was not created with a keyfile (remove the keyfile to migrate)"
                .to_string(),
        ),
        (true, kd) => kd,
        (false, _) => None,
    };
    let master_key = derive_master_key_async(&config, password, keyfile_digest).await?;
    overlay::verify_config_mac(&config, &master_key)?;
    Ok((config, master_key))
}

/// Probe for a legacy marker at the current AeroCrypt root. The modal uses this
/// to show the opt-in conversion action only when it is relevant.
#[tauri::command]
pub async fn aerocrypt_provider_marker_status(
    provider_state: State<'_, ProviderState>,
    base_path: Option<String>,
) -> Result<AeroCryptMarkerStatus, String> {
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());

    let cwd = if let Some(bp) = base_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "/")
    {
        bp.to_string()
    } else {
        provider.pwd().await.unwrap_or_else(|_| "/".to_string())
    };
    let current_path = join_remote_path(&cwd, overlay::CRYPT_CONFIG_WRITE_NAME);
    let legacy_path = join_remote_path(&cwd, overlay::CRYPT_CONFIG_LEGACY_NAME);
    Ok(AeroCryptMarkerStatus {
        has_current_marker: provider.exists(&current_path).await.unwrap_or(false),
        has_legacy_marker: provider.exists(&legacy_path).await.unwrap_or(false),
    })
}

/// Convert an explicitly detected legacy headed marker to the new TSV marker.
/// This is never automatic: callers must pass the unlock factors, we verify the
/// legacy config MAC, write and verify the new marker, then delete the legacy one.
#[tauri::command]
pub async fn aerocrypt_provider_migrate_legacy_marker(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
) -> Result<AeroCryptMarkerMigrationResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to migrate the AeroCrypt marker".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    // Same peel as read_config/marker_status: migrate must see cleartext marker names.
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());

    let cwd = if let Some(bp) = base_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "/")
    {
        bp.to_string()
    } else {
        provider.pwd().await.unwrap_or_else(|_| "/".to_string())
    };
    let current_path = join_remote_path(&cwd, overlay::CRYPT_CONFIG_WRITE_NAME);
    let legacy_path = join_remote_path(&cwd, overlay::CRYPT_CONFIG_LEGACY_NAME);
    let legacy_exists = provider
        .exists(&legacy_path)
        .await
        .map_err(|e| format!("Failed to probe legacy AeroCrypt marker: {e}"))?;
    if !legacy_exists {
        return Ok(AeroCryptMarkerMigrationResult {
            changed: false,
            legacy_deleted: false,
            warning: None,
        });
    }

    let legacy_text = read_marker_text(provider, &legacy_path).await?;
    let (legacy_config, legacy_master) =
        validate_marker_for_migration(&legacy_text, &password, keyfile_digest).await?;
    let rebuilt_marker = overlay::rebuild_config_v3(&legacy_config, &legacy_master)?;
    validate_marker_for_migration(&rebuilt_marker, &password, keyfile_digest).await?;

    let current_exists = provider
        .exists(&current_path)
        .await
        .map_err(|e| format!("Failed to probe current AeroCrypt marker: {e}"))?;
    if current_exists {
        let current_text = read_marker_text(provider, &current_path).await?;
        validate_marker_for_migration(&current_text, &password, keyfile_digest).await?;
    } else {
        let temp = std::env::temp_dir().join(format!(
            "aeroftp_aerocrypt_marker_{}_{}.tsv",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::write(&temp, rebuilt_marker.as_bytes())
            .await
            .map_err(|e| format!("Failed to stage AeroCrypt marker: {e}"))?;
        let remote_tmp = format!("{current_path}.aerotmp-{}", uuid::Uuid::new_v4());
        let upload = provider
            .upload(&temp.to_string_lossy(), &remote_tmp, None)
            .await
            .map_err(|e| format!("Failed to upload staged AeroCrypt marker: {e}"));
        let _ = tokio::fs::remove_file(&temp).await;
        upload?;

        let staged = match provider.download_to_bytes(&remote_tmp).await {
            Ok(bytes) => bytes,
            Err(e) => {
                let _ = provider.delete(&remote_tmp).await;
                return Err(format!("Failed to verify staged AeroCrypt marker: {e}"));
            }
        };
        if staged != rebuilt_marker.as_bytes() {
            let _ = provider.delete(&remote_tmp).await;
            return Err(
                "staged AeroCrypt marker failed byte-for-byte verification; legacy marker retained"
                    .to_string(),
            );
        }
        let staged_text = String::from_utf8(staged)
            .map_err(|e| format!("Staged AeroCrypt marker is not valid UTF-8: {e}"))?;
        validate_marker_for_migration(&staged_text, &password, keyfile_digest).await?;
        if let Err(e) = provider.rename(&remote_tmp, &current_path).await {
            let _ = provider.delete(&remote_tmp).await;
            return Err(format!("Failed to publish verified AeroCrypt marker: {e}"));
        }
    }

    let current_text = read_marker_text(provider, &current_path).await?;
    validate_marker_for_migration(&current_text, &password, keyfile_digest).await?;
    let warning = match provider.delete(&legacy_path).await {
        Ok(()) => None,
        Err(e) => Some(format!(
            "current marker verified, but legacy marker could not be deleted: {e}"
        )),
    };
    Ok(AeroCryptMarkerMigrationResult {
        changed: warning.is_none(),
        legacy_deleted: warning.is_none(),
        warning,
    })
}

/// Bootstrap a brand-new native AeroCrypt overlay on the connected provider:
/// generate a fresh v3 (key-bound) config, derive the key, write `.aerocrypt.tsv`
/// at the (optional) sub-path, and register the unlocked vault. Refuses to
/// overwrite an existing overlay unless `force` is set, because re-initializing
/// rotates the salt and would orphan every existing file.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn aerocrypt_provider_create_remote(
    provider_state: State<'_, ProviderState>,
    aerocrypt_state: State<'_, AeroCryptState>,
    password: String,
    target_subpath: Option<String>,
    force: Option<bool>,
    keyfile_path: Option<String>,
    use_default_salt: Option<bool>,
    salt_strength: Option<String>,
) -> Result<AeroCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let force = force.unwrap_or(false);
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let use_default = use_default_salt.unwrap_or(false);
    // Entropy gate at the crypto boundary (not UI-only): reject a weak password
    // for a public-constant-salt vault. The UI already gates this, but a direct
    // invoke must not bypass it. Defaults to the 128 floor when the tier is absent.
    if use_default {
        crate::aerocrypt::enforce_default_salt_entropy(
            secrecy::ExposeSecret::expose_secret(&secret_pwd),
            salt_strength.as_deref().unwrap_or("128"),
        )?;
    }
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

// ── v4 keyslot manager (T6 GUI parity with CLI crypt migrate-v4 / *-slot) ─────

/// Public, no-secret slot row for the keyslot manager UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroCryptSlotSummary {
    pub id: u32,
    pub kind: String,
    pub salt_len: usize,
}

/// Snapshot returned by [`aerocrypt_list_slots`] after a successful unlock.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroCryptSlotListResult {
    pub version: u8,
    pub epoch: u32,
    /// Short hex prefix of vault_id (public); empty when absent.
    pub vault_id_short: String,
    /// Slot that authenticated this unlock (v4 only).
    pub opened_slot_id: Option<u32>,
    pub slots: Vec<AeroCryptSlotSummary>,
}

/// Result of a marker-mutating keyslot action (migrate / add / remove / rotate).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AeroCryptSlotMutateResult {
    pub version: u8,
    pub epoch: u32,
    pub vault_id_short: String,
    pub opened_slot_id: Option<u32>,
    pub slots: Vec<AeroCryptSlotSummary>,
    pub action: String,
    /// Fresh recovery code when a recovery slot was created (show once).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_code: Option<String>,
    /// True when recovery was auto-offered for a keyfile/hardware-only vault.
    #[serde(default)]
    pub auto_offered_recovery: bool,
}

fn slot_kind_label(kind: SlotType) -> String {
    match kind {
        SlotType::Passphrase => "passphrase".to_string(),
        SlotType::Keyfile => "keyfile".to_string(),
        SlotType::Recovery => "recovery".to_string(),
        SlotType::Fido2Hmac => "fido2-hmac".to_string(),
        SlotType::And => "and".to_string(),
        SlotType::Threshold => "threshold".to_string(),
    }
}

fn vault_id_short_hex(cfg: &OverlayConfig) -> String {
    match cfg.vault_id() {
        Some(vid) => {
            // Short hex of first 4 bytes for UI diagnostics (public).
            vid[..4.min(vid.len())]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        None => String::new(),
    }
}

fn summarize_v4_slots(cfg: &OverlayConfig) -> Result<(u32, Vec<AeroCryptSlotSummary>), String> {
    let OverlayConfig::V4 { epoch, slots, .. } = cfg else {
        return Err(format!(
            "keyslot manager requires a v4 vault; got v{}",
            cfg.version()
        ));
    };
    let summaries = slots
        .iter()
        .map(|s| AeroCryptSlotSummary {
            id: s.id,
            kind: slot_kind_label(s.kind),
            salt_len: s.salt.len(),
        })
        .collect();
    Ok((*epoch, summaries))
}

fn next_slot_id(slots: &[crate::aerocrypt::keyslots::Slot]) -> u32 {
    slots.iter().map(|s| s.id).max().map(|m| m + 1).unwrap_or(0)
}

/// Resolve the overlay root: absolute `base_path` when given, else provider pwd.
async fn resolve_overlay_cwd(
    provider: &mut dyn crate::providers::StorageProvider,
    base_path: Option<&str>,
) -> String {
    if let Some(bp) = base_path
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "/")
    {
        bp.to_string()
    } else {
        provider.pwd().await.unwrap_or_else(|_| "/".to_string())
    }
}

/// Read the headed marker (prefer `.aerocrypt.tsv`, fall back to legacy JSON).
async fn load_headed_marker(
    provider: &mut dyn crate::providers::StorageProvider,
    cwd: &str,
) -> Result<(String, String), String> {
    let current_path = join_remote_path(cwd, overlay::CRYPT_CONFIG_WRITE_NAME);
    let legacy_path = join_remote_path(cwd, overlay::CRYPT_CONFIG_LEGACY_NAME);
    if provider.exists(&current_path).await.unwrap_or(false) {
        let text = read_marker_text(provider, &current_path).await?;
        return Ok((text, current_path));
    }
    if provider.exists(&legacy_path).await.unwrap_or(false) {
        let text = read_marker_text(provider, &legacy_path).await?;
        return Ok((text, legacy_path));
    }
    Err(format!(
        "No AeroCrypt marker found under {cwd} (looked for {} and {})",
        overlay::CRYPT_CONFIG_WRITE_NAME,
        overlay::CRYPT_CONFIG_LEGACY_NAME
    ))
}

/// Stage-upload-verify-rename a headed marker to the current write name, then
/// best-effort drop a leftover legacy JSON name (same pattern as CLI
/// `publish_crypt_marker` and legacy-marker migration).
async fn publish_headed_marker(
    provider: &mut dyn crate::providers::StorageProvider,
    cwd: &str,
    marker_text: &str,
    password: &str,
    keyfile_digest: Option<[u8; KEY_SIZE]>,
) -> Result<(), String> {
    let current_path = join_remote_path(cwd, overlay::CRYPT_CONFIG_WRITE_NAME);
    let temp = std::env::temp_dir().join(format!(
        "aeroftp_aerocrypt_keyslot_{}_{}.json",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp, marker_text.as_bytes())
        .await
        .map_err(|e| format!("Failed to stage AeroCrypt marker: {e}"))?;
    let remote_tmp = format!("{current_path}.aerotmp-{}", uuid::Uuid::new_v4());
    let upload = provider
        .upload(&temp.to_string_lossy(), &remote_tmp, None)
        .await
        .map_err(|e| format!("Failed to upload staged AeroCrypt marker: {e}"));
    let _ = tokio::fs::remove_file(&temp).await;
    upload?;

    let staged = match provider.download_to_bytes(&remote_tmp).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = provider.delete(&remote_tmp).await;
            return Err(format!("Failed to verify staged AeroCrypt marker: {e}"));
        }
    };
    if staged != marker_text.as_bytes() {
        let _ = provider.delete(&remote_tmp).await;
        return Err(
            "staged AeroCrypt marker failed byte-for-byte verification; original marker retained"
                .to_string(),
        );
    }
    let staged_text = String::from_utf8_lossy(&staged).to_string();
    // Fail-closed unlock check before rename (parity with CLI publish).
    if let Err(e) =
        overlay::unlock_overlay_from_config(&staged_text, password, keyfile_digest.as_ref())
    {
        let _ = provider.delete(&remote_tmp).await;
        return Err(format!(
            "staged AeroCrypt marker failed unlock verification: {e}"
        ));
    }
    if let Err(e) = provider.rename(&remote_tmp, &current_path).await {
        let _ = provider.delete(&remote_tmp).await;
        return Err(format!("Failed to publish verified AeroCrypt marker: {e}"));
    }
    let legacy_path = join_remote_path(cwd, overlay::CRYPT_CONFIG_LEGACY_NAME);
    if provider.exists(&legacy_path).await.unwrap_or(false) {
        let _ = provider.delete(&legacy_path).await;
    }
    Ok(())
}

/// Best-effort: refresh the local public config cache so Recovery Kit / next
/// connect see the post-mutation header. Never fails the remote write path.
fn best_effort_refresh_keystore_config(profile_id: Option<&str>, marker_text: &str) {
    let Some(id) = profile_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
        return;
    };
    let config_key = format!("aerocrypt_overlay_config_{id}");
    if let Err(e) =
        crate::user_partitions::store_active_credential_dual(&store, &config_key, marker_text)
    {
        log::warn!("[aerocrypt] keyslot keystore config refresh skipped for {id}: {e}");
    }
}

struct MigratePureResult {
    marker: String,
    recovery_code: Option<String>,
}

fn crypt_v4_migrate_header_full_pure(
    raw_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
) -> Result<MigratePureResult, String> {
    let (cfg, master) = overlay::unlock_overlay_from_config(raw_header, password, keyfile_digest)?;
    if cfg.version() != overlay::VERSION_V3 {
        return Err(format!(
            "migrate-v4 requires a v3 headed vault; got v{}",
            cfg.version()
        ));
    }
    let mut marker = overlay::migrate_v3_to_v4(&cfg, &master)?;
    let mut recovery_code = None;
    // Spec 08 §4: keyfile-only vault must auto-offer a recovery slot.
    let v4_cfg = overlay::parse_config(&marker)?;
    let material = overlay::unlock_v4_for_management(&marker, password, keyfile_digest)?;
    if let Some((with_rec, code)) =
        overlay::ensure_recovery_slot_if_needed(&v4_cfg, &material.vk, &material.epoch_key)?
    {
        marker = with_rec;
        recovery_code = Some(code.formatted);
    }
    Ok(MigratePureResult {
        marker,
        recovery_code,
    })
}

fn crypt_v4_add_slot_header_pure(
    raw_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    slot_type: &str,
    new_password: &str,
    new_keyfile_digest: Option<&[u8; KEY_SIZE]>,
) -> Result<AddSlotPureResult, String> {
    let material = overlay::unlock_v4_for_management(raw_header, password, keyfile_digest)?;
    let OverlayConfig::V4 {
        vault_id, slots, ..
    } = &material.config
    else {
        return Err("add-slot requires a v4 vault".into());
    };
    let kind = match slot_type {
        "passphrase" => SlotType::Passphrase,
        "keyfile" => SlotType::Keyfile,
        "recovery" => SlotType::Recovery,
        other => {
            return Err(format!(
                "unsupported slot type '{other}' (use passphrase|keyfile|recovery)"
            ))
        }
    };
    let new_id = next_slot_id(slots);
    let mut recovery_code_out: Option<String> = None;
    let sk = match kind {
        SlotType::Passphrase => {
            if new_password.is_empty() {
                return Err("add-slot passphrase requires a new password".into());
            }
            if new_keyfile_digest.is_some() {
                return Err("add-slot passphrase does not take a new keyfile".into());
            }
            let salt = overlay::random_salt_v3();
            let kdf = Argon2Params::v3_profile();
            let slot_key = derive_slot_key(
                kind,
                &SlotFactor::Passphrase(new_password),
                &salt,
                Some(&kdf),
            )?;
            SlotKeyMaterial {
                id: new_id,
                kind,
                salt: salt.to_vec(),
                kdf: Some(kdf),
                binding: SlotBinding::None,
                slot_key,
            }
        }
        SlotType::Keyfile => {
            let Some(digest) = new_keyfile_digest else {
                return Err("add-slot keyfile requires a new keyfile".into());
            };
            let salt = overlay::random_salt_v3();
            let kdf = Argon2Params::v3_profile();
            let slot_key = derive_slot_key(
                kind,
                &SlotFactor::KeyfileDigest {
                    password: new_password,
                    digest,
                },
                &salt,
                Some(&kdf),
            )?;
            SlotKeyMaterial {
                id: new_id,
                kind,
                salt: salt.to_vec(),
                kdf: Some(kdf),
                binding: SlotBinding::None,
                slot_key,
            }
        }
        SlotType::Recovery => {
            if new_keyfile_digest.is_some() {
                return Err("add-slot recovery does not take a new keyfile".into());
            }
            let supplied = if new_password.is_empty() {
                None
            } else {
                Some(crate::aerocrypt::recovery::parse_recovery_code(
                    new_password,
                    Some(vault_id),
                )?)
            };
            let (mat, code) =
                overlay::build_recovery_slot_material(vault_id, new_id, supplied.as_ref())?;
            recovery_code_out = Some(code.formatted);
            mat
        }
        _ => return Err("unsupported slot type".into()),
    };
    let mut marker = overlay::add_slot(&material.config, &material.vk, &material.epoch_key, sk)?;
    let mut auto_offered = false;

    // Spec 08 §4: auto-offer recovery when the vault would be keyfile-only.
    if kind == SlotType::Keyfile {
        let cfg = overlay::parse_config(&marker)?;
        if let Some((with_rec, code)) =
            overlay::ensure_recovery_slot_if_needed(&cfg, &material.vk, &material.epoch_key)?
        {
            marker = with_rec;
            recovery_code_out = Some(code.formatted);
            auto_offered = true;
        }
    }

    Ok(AddSlotPureResult {
        marker,
        recovery_code: recovery_code_out,
        auto_offered_recovery: auto_offered,
    })
}

fn crypt_v4_remove_slot_header_pure(
    raw_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    slot_id: u32,
) -> Result<String, String> {
    let material = overlay::unlock_v4_for_management(raw_header, password, keyfile_digest)?;
    let OverlayConfig::V4 { slots, .. } = &material.config else {
        return Err("remove-slot requires a v4 vault".into());
    };
    if material.slot_id == slot_id {
        return Err(
            "cannot remove the slot used to unlock; unlock with a surviving slot's credential"
                .into(),
        );
    }
    let surviving: Vec<_> = slots.iter().filter(|s| s.id != slot_id).collect();
    if surviving.is_empty() {
        return Err("cannot revoke the last slot".into());
    }
    // T5/T6 limit: only re-wrap keys we hold (the authenticating slot).
    if surviving.len() != 1 || surviving[0].id != material.slot_id {
        return Err(format!(
            "remove-slot currently supports only the single-survivor case (vault has {} slots after remove would keep {}; unlock must match the sole survivor). Multi-survivor revoke needs every surviving slot key (later task).",
            slots.len(),
            surviving.len()
        ));
    }
    overlay::revoke_slot(
        &material.config,
        &material.vk,
        slot_id,
        &[(material.slot_id, material.slot_key)],
    )
}

fn crypt_v4_rotate_slot_header_pure(
    raw_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    slot_id: u32,
    new_password: &str,
    new_keyfile_digest: Option<&[u8; KEY_SIZE]>,
) -> Result<String, String> {
    let material = overlay::unlock_v4_for_management(raw_header, password, keyfile_digest)?;
    if material.slot_id != slot_id {
        return Err(format!(
            "rotate-slot: unlock authenticated slot {}, but slot-id is {slot_id}; unlock with the slot you are rotating",
            material.slot_id
        ));
    }
    let OverlayConfig::V4 { slots, .. } = &material.config else {
        return Err("rotate-slot requires a v4 vault".into());
    };
    let slot = slots
        .iter()
        .find(|s| s.id == slot_id)
        .ok_or_else(|| format!("unknown slot id {slot_id}"))?;
    let kind = slot.kind;
    let salt = overlay::random_salt_v3();
    let kdf = Argon2Params::v3_profile();
    let factor = match kind {
        SlotType::Passphrase => {
            if new_password.is_empty() {
                return Err("rotate-slot for a passphrase slot requires a new password".into());
            }
            if new_keyfile_digest.is_some() {
                return Err("rotate-slot passphrase slot does not take a new keyfile".into());
            }
            SlotFactor::Passphrase(new_password)
        }
        SlotType::Keyfile => {
            let Some(digest) = new_keyfile_digest else {
                return Err("rotate-slot for a keyfile slot requires a new keyfile".into());
            };
            SlotFactor::KeyfileDigest {
                password: new_password,
                digest,
            }
        }
        other => {
            return Err(format!(
                "rotate-slot does not yet support slot type {other:?}"
            ));
        }
    };
    let slot_key = derive_slot_key(kind, &factor, &salt, Some(&kdf))?;
    let sk = SlotKeyMaterial {
        id: slot_id,
        kind,
        salt: salt.to_vec(),
        kdf: Some(kdf),
        binding: SlotBinding::None,
        slot_key,
    };
    overlay::rotate_slot(&material.config, &material.vk, &material.epoch_key, sk)
}

fn list_result_from_v4(
    raw_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
) -> Result<AeroCryptSlotListResult, String> {
    let material = overlay::unlock_v4_for_management(raw_header, password, keyfile_digest)?;
    let (epoch, slots) = summarize_v4_slots(&material.config)?;
    Ok(AeroCryptSlotListResult {
        version: material.config.version(),
        epoch,
        vault_id_short: vault_id_short_hex(&material.config),
        opened_slot_id: Some(material.slot_id),
        slots,
    })
}

fn mutate_result_from_header(
    new_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    action: &str,
) -> Result<AeroCryptSlotMutateResult, String> {
    mutate_result_from_header_with_recovery(
        new_header,
        password,
        keyfile_digest,
        action,
        None,
        false,
    )
}

fn mutate_result_from_header_with_recovery(
    new_header: &str,
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    action: &str,
    recovery_code: Option<String>,
    auto_offered_recovery: bool,
) -> Result<AeroCryptSlotMutateResult, String> {
    let list = list_result_from_v4(new_header, password, keyfile_digest)?;
    Ok(AeroCryptSlotMutateResult {
        version: list.version,
        epoch: list.epoch,
        vault_id_short: list.vault_id_short,
        opened_slot_id: list.opened_slot_id,
        slots: list.slots,
        action: action.to_string(),
        recovery_code,
        auto_offered_recovery,
    })
}

/// Pure add-slot result (marker + optional one-time recovery code).
struct AddSlotPureResult {
    marker: String,
    recovery_code: Option<String>,
    auto_offered_recovery: bool,
}

/// List keyslots on the connected AeroCrypt vault (v4). Requires unlock factors.
/// On a v3 vault returns version=3 with an empty slot list so the GUI can offer
/// "Convert to keyslot vault (v4)" without a separate probe.
#[tauri::command]
pub async fn aerocrypt_list_slots(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
) -> Result<AeroCryptSlotListResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to list AeroCrypt slots".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());
    let cwd = resolve_overlay_cwd(provider, base_path.as_deref()).await;
    let (raw, _) = load_headed_marker(provider, &cwd).await?;
    let kd = keyfile_digest;
    let pw = password;
    tokio::task::spawn_blocking(move || {
        let (cfg, _) = overlay::unlock_overlay_from_config(&raw, &pw, kd.as_ref())?;
        match cfg.version() {
            4 => list_result_from_v4(&raw, &pw, kd.as_ref()),
            overlay::VERSION_V3 => Ok(AeroCryptSlotListResult {
                version: 3,
                epoch: 0,
                vault_id_short: vault_id_short_hex(&cfg),
                opened_slot_id: None,
                slots: vec![],
            }),
            other => Err(format!(
                "keyslot manager does not support AeroCrypt v{other}"
            )),
        }
    })
    .await
    .map_err(|e| format!("list-slots task failed: {e}"))?
}

/// Convert a connected v3 headed vault to v4 keyslots and publish the new marker.
#[tauri::command]
pub async fn aerocrypt_migrate_v4(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
    profile_id: Option<String>,
) -> Result<AeroCryptSlotMutateResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to migrate to v4".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());
    let cwd = resolve_overlay_cwd(provider, base_path.as_deref()).await;
    let (raw, _) = load_headed_marker(provider, &cwd).await?;
    let kd = keyfile_digest;
    let pw = password.clone();
    let migrate = tokio::task::spawn_blocking(move || {
        let result = crypt_v4_migrate_header_full_pure(&raw, &pw, kd.as_ref())?;
        // Verify unlock recovers OMK before any remote write.
        overlay::unlock_overlay_from_config(&result.marker, &pw, kd.as_ref())?;
        Ok::<_, String>(result)
    })
    .await
    .map_err(|e| format!("migrate-v4 task failed: {e}"))??;
    let v4_marker = migrate.marker;
    let recovery_code = migrate.recovery_code;
    let auto_offered = recovery_code.is_some();

    publish_headed_marker(provider, &cwd, &v4_marker, &password, keyfile_digest).await?;
    best_effort_refresh_keystore_config(profile_id.as_deref(), &v4_marker);

    let kd = keyfile_digest;
    let pw = password;
    let marker = v4_marker;
    tokio::task::spawn_blocking(move || {
        mutate_result_from_header_with_recovery(
            &marker,
            &pw,
            kd.as_ref(),
            "migrate-v4",
            recovery_code,
            auto_offered,
        )
    })
    .await
    .map_err(|e| format!("migrate-v4 verify task failed: {e}"))?
}

/// Add a passphrase or keyfile slot to a connected v4 vault.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn aerocrypt_add_slot(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
    profile_id: Option<String>,
    slot_type: String,
    new_password: Option<String>,
    new_keyfile_path: Option<String>,
) -> Result<AeroCryptSlotMutateResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to add a slot".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let new_keyfile_digest = resolve_ui_keyfile_digest(new_keyfile_path.as_deref())?;
    let new_pw_owned = new_password.unwrap_or_default();
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());
    let cwd = resolve_overlay_cwd(provider, base_path.as_deref()).await;
    let (raw, _) = load_headed_marker(provider, &cwd).await?;
    let kd = keyfile_digest;
    let nkd = new_keyfile_digest;
    let pw = password.clone();
    let st = slot_type.clone();
    let new_pw_for_build = new_pw_owned.clone();
    let add_result = tokio::task::spawn_blocking(move || {
        crypt_v4_add_slot_header_pure(&raw, &pw, kd.as_ref(), &st, &new_pw_for_build, nkd.as_ref())
    })
    .await
    .map_err(|e| format!("add-slot task failed: {e}"))??;
    let new_header = add_result.marker;
    let recovery_code = add_result.recovery_code;
    let auto_offered = add_result.auto_offered_recovery;

    // Verify with the NEW factor when present; otherwise with unlock factor.
    let verify_pw = if slot_type == "passphrase" {
        if new_pw_owned.is_empty() {
            password.clone()
        } else {
            new_pw_owned.clone()
        }
    } else if slot_type == "recovery" {
        recovery_code
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| password.clone())
    } else if new_pw_owned.is_empty() {
        password.clone()
    } else {
        new_pw_owned.clone()
    };
    let verify_kd = if slot_type == "keyfile" {
        new_keyfile_digest
    } else {
        None
    };
    {
        let marker = new_header.clone();
        let vpw = verify_pw.clone();
        let vkd = verify_kd;
        tokio::task::spawn_blocking(move || {
            overlay::unlock_overlay_from_config(&marker, &vpw, vkd.as_ref())
        })
        .await
        .map_err(|e| format!("add-slot verify task failed: {e}"))?
        .map_err(|e| format!("add-slot produced a marker that fails unlock: {e}"))?;
    }
    // Also verify the recovery code when auto-offered (keyfile path).
    if let Some(ref code) = recovery_code {
        if slot_type != "recovery" {
            let marker = new_header.clone();
            let vpw = code.clone();
            tokio::task::spawn_blocking(move || {
                overlay::unlock_overlay_from_config(&marker, &vpw, None)
            })
            .await
            .map_err(|e| format!("add-slot recovery verify task failed: {e}"))?
            .map_err(|e| format!("auto-offered recovery code fails unlock: {e}"))?;
        }
    }

    // Publish with the original unlock factor (still valid; add does not revoke).
    publish_headed_marker(provider, &cwd, &new_header, &password, keyfile_digest).await?;
    best_effort_refresh_keystore_config(profile_id.as_deref(), &new_header);

    let kd = keyfile_digest;
    let pw = password;
    let marker = new_header;
    let rec = recovery_code;
    let auto = auto_offered;
    tokio::task::spawn_blocking(move || {
        mutate_result_from_header_with_recovery(&marker, &pw, kd.as_ref(), "add-slot", rec, auto)
    })
    .await
    .map_err(|e| format!("add-slot list task failed: {e}"))?
}

/// Remove (revoke) a slot. T6 mirrors T5 single-survivor limit. Carries F6
/// honesty in the GUI; this command only enforces crypto/API rules.
#[tauri::command]
pub async fn aerocrypt_remove_slot(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
    profile_id: Option<String>,
    slot_id: u32,
) -> Result<AeroCryptSlotMutateResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to remove a slot".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());
    let cwd = resolve_overlay_cwd(provider, base_path.as_deref()).await;
    let (raw, _) = load_headed_marker(provider, &cwd).await?;
    let kd = keyfile_digest;
    let pw = password.clone();
    let new_header = tokio::task::spawn_blocking(move || {
        crypt_v4_remove_slot_header_pure(&raw, &pw, kd.as_ref(), slot_id)
    })
    .await
    .map_err(|e| format!("remove-slot task failed: {e}"))??;

    {
        let marker = new_header.clone();
        let vpw = password.clone();
        let vkd = keyfile_digest;
        tokio::task::spawn_blocking(move || {
            overlay::unlock_overlay_from_config(&marker, &vpw, vkd.as_ref())
        })
        .await
        .map_err(|e| format!("remove-slot verify task failed: {e}"))?
        .map_err(|e| format!("remove-slot produced a marker that fails unlock: {e}"))?;
    }

    publish_headed_marker(provider, &cwd, &new_header, &password, keyfile_digest).await?;
    best_effort_refresh_keystore_config(profile_id.as_deref(), &new_header);

    let kd = keyfile_digest;
    let pw = password;
    let marker = new_header;
    tokio::task::spawn_blocking(move || {
        mutate_result_from_header(&marker, &pw, kd.as_ref(), "remove-slot")
    })
    .await
    .map_err(|e| format!("remove-slot list task failed: {e}"))?
}

/// Rotate the factor of the authenticating slot (new password or keyfile).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn aerocrypt_rotate_slot(
    provider_state: State<'_, ProviderState>,
    password: String,
    keyfile_path: Option<String>,
    base_path: Option<String>,
    profile_id: Option<String>,
    slot_id: u32,
    new_password: Option<String>,
    new_keyfile_path: Option<String>,
) -> Result<AeroCryptSlotMutateResult, String> {
    if password.is_empty() && keyfile_path.as_deref().unwrap_or("").trim().is_empty() {
        return Err("password or keyfile required to rotate a slot".to_string());
    }
    let keyfile_digest = resolve_ui_keyfile_digest(keyfile_path.as_deref())?;
    let new_keyfile_digest = resolve_ui_keyfile_digest(new_keyfile_path.as_deref())?;
    let new_pw = new_password.clone().unwrap_or_default();
    let mut provider_lock = provider_state.provider.lock().await;
    let slot = provider_lock
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    let provider = crate::crypt_overlay_provider::concrete_provider_mut(slot.as_mut());
    let cwd = resolve_overlay_cwd(provider, base_path.as_deref()).await;
    let (raw, _) = load_headed_marker(provider, &cwd).await?;
    let kd = keyfile_digest;
    let nkd = new_keyfile_digest;
    let pw = password.clone();
    let new_header = tokio::task::spawn_blocking(move || {
        crypt_v4_rotate_slot_header_pure(&raw, &pw, kd.as_ref(), slot_id, &new_pw, nkd.as_ref())
    })
    .await
    .map_err(|e| format!("rotate-slot task failed: {e}"))??;

    // After rotate, only the NEW factor unlocks the rotated slot.
    // Prefer new keyfile when provided; else new password.
    let (vpw, vkd): (String, Option<[u8; KEY_SIZE]>) = if new_keyfile_digest.is_some() {
        (new_password.clone().unwrap_or_default(), new_keyfile_digest)
    } else {
        (new_password.clone().unwrap_or_default(), None)
    };
    if vpw.is_empty() && vkd.is_none() {
        return Err("rotate-slot requires a new password or new keyfile".to_string());
    }
    {
        let marker = new_header.clone();
        let vpw2 = vpw.clone();
        let vkd2 = vkd;
        tokio::task::spawn_blocking(move || {
            overlay::unlock_overlay_from_config(&marker, &vpw2, vkd2.as_ref())
        })
        .await
        .map_err(|e| format!("rotate-slot verify task failed: {e}"))?
        .map_err(|e| {
            format!("rotate-slot produced a marker that fails unlock with new factor: {e}")
        })?;
    }

    // Publish: verify path above already used the new factor; use the same for
    // the staged unlock check inside publish.
    publish_headed_marker(provider, &cwd, &new_header, &vpw, vkd).await?;
    best_effort_refresh_keystore_config(profile_id.as_deref(), &new_header);

    let marker = new_header;
    tokio::task::spawn_blocking(move || {
        mutate_result_from_header(&marker, &vpw, vkd.as_ref(), "rotate-slot")
    })
    .await
    .map_err(|e| format!("rotate-slot list task failed: {e}"))?
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

    /// T6 pure path: migrate v3 → list → add → rotate → remove recovers OMK.
    #[test]
    fn keyslot_gui_pure_migrate_add_rotate_remove() {
        let salt = overlay::random_salt_v3();
        let master =
            overlay::derive_master_key(&OverlayConfig::v3_bootstrap(salt), "owner-pw").unwrap();
        let v3 = overlay::init_config_v3_with_vault_id(
            &salt,
            &master,
            &overlay::random_vault_id(),
            overlay::SaltMode::PerVault,
        )
        .unwrap();

        let v4 = crypt_v4_migrate_header_full_pure(&v3, "owner-pw", None)
            .unwrap()
            .marker;
        let list = list_result_from_v4(&v4, "owner-pw", None).unwrap();
        assert_eq!(list.version, 4);
        assert_eq!(list.slots.len(), 1);
        assert_eq!(list.opened_slot_id, Some(0));

        let with_extra =
            crypt_v4_add_slot_header_pure(&v4, "owner-pw", None, "passphrase", "second-pw", None)
                .unwrap();
        // New factor opens; original still opens.
        overlay::unlock_overlay_from_config(&with_extra.marker, "second-pw", None).unwrap();
        overlay::unlock_overlay_from_config(&with_extra.marker, "owner-pw", None).unwrap();

        let rotated = crypt_v4_rotate_slot_header_pure(
            &with_extra.marker,
            "second-pw",
            None,
            1,
            "second-pw-rotated",
            None,
        )
        .unwrap();
        assert!(
            overlay::unlock_overlay_from_config(&rotated, "second-pw", None).is_err(),
            "old factor of rotated slot must fail closed"
        );
        overlay::unlock_overlay_from_config(&rotated, "second-pw-rotated", None).unwrap();

        // Remove slot 1 while unlocked as sole survivor (slot 0).
        let removed = crypt_v4_remove_slot_header_pure(&rotated, "owner-pw", None, 1).unwrap();
        assert!(
            overlay::unlock_overlay_from_config(&removed, "second-pw-rotated", None).is_err(),
            "revoked factor must fail closed"
        );
        let (cfg, omk) = overlay::unlock_overlay_from_config(&removed, "owner-pw", None).unwrap();
        assert_eq!(cfg.version(), 4);
        assert_eq!(omk, master, "OMK must stay stable through manage ops");
        let list2 = list_result_from_v4(&removed, "owner-pw", None).unwrap();
        assert_eq!(list2.slots.len(), 1);
        assert_eq!(list2.slots[0].id, 0);
    }

    /// T7: recovery slot recovers the same OMK; bad checksum fails closed.
    #[test]
    fn keyslot_gui_pure_recovery_slot_same_omk() {
        let salt = overlay::random_salt_v3();
        let master =
            overlay::derive_master_key(&OverlayConfig::v3_bootstrap(salt), "owner-pw").unwrap();
        let v3 = overlay::init_config_v3_with_vault_id(
            &salt,
            &master,
            &overlay::random_vault_id(),
            overlay::SaltMode::PerVault,
        )
        .unwrap();
        let v4 = crypt_v4_migrate_header_full_pure(&v3, "owner-pw", None)
            .unwrap()
            .marker;
        let added =
            crypt_v4_add_slot_header_pure(&v4, "owner-pw", None, "recovery", "", None).unwrap();
        let code = added.recovery_code.expect("recovery code once");
        let (_cfg, omk) = overlay::unlock_overlay_from_config(&added.marker, &code, None).unwrap();
        assert_eq!(omk, master);
        let mut chars: Vec<char> = code.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == '0' { '1' } else { '0' };
        let bad: String = chars.into_iter().collect();
        assert!(
            overlay::unlock_overlay_from_config(&added.marker, &bad, None).is_err(),
            "corrupted recovery must fail closed"
        );
    }
}
