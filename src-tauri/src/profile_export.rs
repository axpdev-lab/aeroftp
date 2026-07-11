// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP Server Profile Export/Import
// Encrypted backup/restore using AES-256-GCM + Argon2id
// File format: .aeroftp (JSON envelope with encrypted payload)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const FILE_VERSION: u32 = 1;

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("Invalid password")]
    InvalidPassword,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("Unsupported file version: {0}")]
    UnsupportedVersion(u32),
}

// ============ File Format ============

#[derive(Serialize, Deserialize)]
struct ExportFile {
    version: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_payload: Vec<u8>,
    metadata: ExportMetadata,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetadata {
    pub export_date: String,
    pub aeroftp_version: String,
    pub server_count: u32,
    pub has_credentials: bool,
}

#[derive(Serialize, Deserialize)]
struct ExportPayload {
    servers: Vec<ServerProfileExport>,
    /// Map of `profile_id` → encrypted provider tokens bound to that profile.
    /// Currently carries OAuth2 blobs (Google, Dropbox, OneDrive, Box, pCloud,
    /// Zoho, Yandex, Google Photos) and the Jottacloud OIDC refresh token, so
    /// importing a profile on a fresh device reconnects without a re-auth
    /// round-trip. `#[serde(default)]` keeps legacy `.aeroftp` files (file
    /// version 1, exported before issue #214) importable without changes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    provider_secrets: HashMap<String, ProviderSecrets>,
}

/// Per-profile bundle of provider-specific encrypted tokens. Each field is
/// the JSON string the vault stored under its respective key, copied verbatim
/// so the destination device can write it back without re-parsing. Issue
/// #214.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSecrets {
    /// Serialized `StoredTokens` JSON for OAuth2 providers (`oauth_<provider>`
    /// vault format). Absent when the profile is not OAuth-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    /// Serialized refresh-token JSON for Jottacloud (`jottacloud_refresh`
    /// vault format). Absent when the profile is not a Jotta profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jotta_refresh: Option<String>,
    /// CWP-20B: the crypt-overlay password (`aerocrypt_overlay_pw_<id>`) so a
    /// Crypt profile reconnects on a fresh device without re-entering it. Shared
    /// by BOTH overlay kinds (native AeroCrypt and interop rclone-crypt). Absent
    /// for non-crypt profiles or when credentials are excluded from the export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aerocrypt_overlay_pw: Option<String>,
    /// CWP-20B: the crypt-overlay salt (`aerocrypt_overlay_salt_<id>`). For
    /// rclone-crypt this is the second password (password2); native AeroCrypt
    /// leaves it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aerocrypt_overlay_salt: Option<String>,
    /// AeroCrypt keyfile PATH (`aerocrypt_overlay_keyfile_path_<id>`) for a v3
    /// overlay created with a keyfile second factor. This is a filesystem
    /// reference, NOT the keyfile contents (the keyfile itself, the extra factor
    /// you hold, is never exported). On import the path is restored verbatim but
    /// may not exist on the destination device, so the UI must validate it and
    /// prompt the user to re-point. A keyfile vault fails closed if the keyfile is
    /// missing (never a silent password-only fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aerocrypt_overlay_keyfile_path: Option<String>,
    /// Headerless AeroCrypt public config JSON
    /// (`aerocrypt_overlay_config_<id>`). This is the same public OverlayConfig
    /// JSON a headed vault writes to `.aeroftp-crypt.json`, including its local
    /// config MAC, copied verbatim so headed and headerless unlock use the same
    /// parser and verifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aerocrypt_overlay_config: Option<String>,
    /// Issue #215 Caveat A: per-protocol credential snapshots
    /// (`server_modes_<id>` vault JSON) so a one-account-many-protocols profile
    /// keeps each mode's saved credentials across export/import. Single-use
    /// secrets (TOTP/STS) are already stripped at write time by
    /// `modeCredentialStore.stripSingleUseSecrets`, so this blob never carries
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_credentials: Option<String>,
    /// #128-D: the BYO OAuth app `client_id` recovered from an imported rclone
    /// remote (`oauth_<provider>_client_id` vault singleton). AeroFTP mints its
    /// OAuth tokens with the user's own app, so rclone can only refresh them
    /// when it carries the same client_id/secret; recovering them on import
    /// lets a fresh device reconnect without re-entering the app credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    /// #128-D: the BYO OAuth app `client_secret` recovered from an imported
    /// rclone remote (`oauth_<provider>_client_secret` vault singleton).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_secret: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfileExport {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub initial_path: Option<String>,
    pub local_initial_path: Option<String>,
    pub color: Option<String>,
    pub last_connected: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub credential: Option<String>,
    pub has_stored_credential: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url_base: Option<String>,
    /// CWP-20B round-trip: the crypt-overlay binding (`AeroCryptOverlayBinding`:
    /// kind + remoteScope + filename/dir settings). Without this an exported
    /// Crypt profile silently re-imported as plaintext (the binding lived only in
    /// a frontend field this struct dropped). Carried verbatim as JSON;
    /// `#[serde(default)]` keeps pre-CWP-20B `.aeroftp` files importable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aero_crypt_overlay: Option<serde_json::Value>,
    /// Mirrors `ServerProfile.hasStoredAeroCryptPassword` /
    /// `hasStoredAeroCryptSalt`; on import these are corrected to reflect whether
    /// the secret was actually restored (see `import_server_profiles`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_stored_aero_crypt_password: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_stored_aero_crypt_salt: Option<bool>,
    /// Mirrors whether a keyfile PATH is on file for this AeroCrypt overlay; on
    /// import corrected to reflect whether the path was actually restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_stored_aero_crypt_keyfile_path: Option<bool>,
    /// Issue #215 Caveat A: round-trip the opt-in so an imported profile
    /// re-hydrates its per-mode snapshots (ConnectionScreen reads
    /// `profile.persistModeCredentials` before loading `server_modes_<id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_mode_credentials: Option<bool>,
}

// ============ Export/Import ============

pub fn export_profiles(
    servers: Vec<ServerProfileExport>,
    provider_secrets: HashMap<String, ProviderSecrets>,
    password: &str,
    file_path: &Path,
) -> Result<ExportMetadata, ExportError> {
    // A2-06: Use strong KDF (128 MiB): same strength as vault
    let salt = crate::crypto::random_bytes(32);
    let key = crate::crypto::derive_key_strong(password, &salt).map_err(ExportError::Encryption)?;

    // Issue #214/#215: the "has credentials" badge must reflect EVERY secret a
    // profile can carry, not only OAuth/Jotta tokens. A profile whose only
    // saved secret is a crypt-overlay password (CWP-20B) or a per-protocol
    // snapshot (#215 `mode_credentials`) still contains credentials, so the
    // import preview should say so.
    let any_provider_secret = provider_secrets.values().any(|s| {
        s.oauth.is_some()
            || s.jotta_refresh.is_some()
            || s.aerocrypt_overlay_pw.is_some()
            || s.aerocrypt_overlay_salt.is_some()
            || s.aerocrypt_overlay_keyfile_path.is_some()
            || s.aerocrypt_overlay_config.is_some()
            || s.mode_credentials.is_some()
    });
    let metadata = ExportMetadata {
        export_date: chrono::Utc::now().to_rfc3339(),
        aeroftp_version: env!("CARGO_PKG_VERSION").to_string(),
        server_count: servers.len() as u32,
        // Issue #214: the "has credentials" badge in the import dialog now
        // also reflects OAuth / Jotta refresh tokens, not only password blobs.
        has_credentials: servers.iter().any(|s| s.credential.is_some()) || any_provider_secret,
    };

    let payload = ExportPayload {
        servers,
        provider_secrets,
    };
    let payload_json = serde_json::to_vec(&payload)?;

    let nonce = crate::crypto::random_bytes(12);
    let encrypted = crate::crypto::encrypt_aes_gcm(&key, &nonce, &payload_json)
        .map_err(ExportError::Encryption)?;

    let export_file = ExportFile {
        version: FILE_VERSION,
        salt,
        nonce,
        encrypted_payload: encrypted,
        metadata: metadata.clone(),
    };

    let file_data = serde_json::to_vec_pretty(&export_file)?;
    // A2-08: Atomic write (temp+rename) + secure permissions
    let tmp_path = file_path.with_extension("tmp");
    std::fs::write(&tmp_path, &file_data)?;
    std::fs::rename(&tmp_path, file_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(metadata)
}

/// Triple returned by `import_profiles`: the decrypted server list, the
/// per-profile provider tokens (OAuth and Jotta refresh, may be empty for
/// legacy v1 exports) and the public envelope metadata.
pub type ImportedProfiles = (
    Vec<ServerProfileExport>,
    HashMap<String, ProviderSecrets>,
    ExportMetadata,
);

pub fn import_profiles(file_path: &Path, password: &str) -> Result<ImportedProfiles, ExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: ExportFile = serde_json::from_slice(&file_data)?;

    if export_file.version > FILE_VERSION {
        return Err(ExportError::UnsupportedVersion(export_file.version));
    }

    // A2-06: Try strong KDF first (128 MiB, new exports), fall back to legacy (64 MiB) for old files
    let key_strong = crate::crypto::derive_key_strong(password, &export_file.salt)
        .map_err(ExportError::Encryption)?;
    let payload_json = match crate::crypto::decrypt_aes_gcm(
        &key_strong,
        &export_file.nonce,
        &export_file.encrypted_payload,
    ) {
        Ok(data) => data,
        Err(_) => {
            let key_legacy = crate::crypto::derive_key(password, &export_file.salt)
                .map_err(ExportError::Encryption)?;
            crate::crypto::decrypt_aes_gcm(
                &key_legacy,
                &export_file.nonce,
                &export_file.encrypted_payload,
            )
            .map_err(|_| ExportError::InvalidPassword)?
        }
    };

    let payload: ExportPayload = serde_json::from_slice(&payload_json)?;

    Ok((
        payload.servers,
        payload.provider_secrets,
        export_file.metadata,
    ))
}

pub fn read_metadata(file_path: &Path) -> Result<ExportMetadata, ExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: ExportFile = serde_json::from_slice(&file_data)?;
    Ok(export_file.metadata)
}

// Crypto primitives shared via crate::crypto module

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server(id: &str, protocol: &str) -> ServerProfileExport {
        ServerProfileExport {
            id: id.to_string(),
            name: format!("Test {}", protocol),
            host: format!("{}.example.com", protocol),
            port: 443,
            username: "user@example.com".to_string(),
            protocol: Some(protocol.to_string()),
            initial_path: None,
            local_initial_path: None,
            color: None,
            last_connected: None,
            options: None,
            provider_id: None,
            credential: None,
            has_stored_credential: None,
            public_url_base: None,
            aero_crypt_overlay: None,
            has_stored_aero_crypt_password: None,
            has_stored_aero_crypt_salt: None,
            has_stored_aero_crypt_keyfile_path: None,
            persist_mode_credentials: None,
        }
    }

    /// Issue #214: an export bundle carries `provider_secrets` keyed by
    /// profile id; the round-trip must surface them intact so the destination
    /// device can write the OAuth / Jotta blobs back into the per-profile
    /// vault keys without re-parsing.
    #[test]
    fn round_trip_preserves_provider_secrets() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_export_secrets_{}_{}.aeroftp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        let server_a = sample_server("server-a", "googledrive");
        let server_b = sample_server("server-b", "jottacloud");

        let mut secrets = HashMap::new();
        secrets.insert(
            "server-a".to_string(),
            ProviderSecrets {
                oauth: Some(
                    r#"{"access_token":"AT","refresh_token":"RT","expires_at":null,"token_type":"Bearer","scopes":[]}"#
                        .to_string(),
                ),
                jotta_refresh: None,
                // BYO OAuth app credentials must survive the round-trip so an
                // imported OAuth profile reconnects without re-entering them.
                oauth_client_id: Some("client-id-abc".to_string()),
                oauth_client_secret: Some("client-secret-xyz".to_string()),
                ..Default::default()
            },
        );
        secrets.insert(
            "server-b".to_string(),
            ProviderSecrets {
                oauth: None,
                jotta_refresh: Some(
                    r#"{"refresh_token":"jotta-rt","token_endpoint":"https://example/token","username":"alice"}"#
                        .to_string(),
                ),
                ..Default::default()
            },
        );

        let metadata = export_profiles(
            vec![server_a, server_b],
            secrets.clone(),
            "pw-12345678",
            &tmp,
        )
        .expect("export should succeed");
        assert!(
            metadata.has_credentials,
            "metadata.has_credentials must include provider tokens"
        );

        let (servers, restored_secrets, _meta) =
            import_profiles(&tmp, "pw-12345678").expect("import should succeed");
        assert_eq!(servers.len(), 2);
        assert_eq!(restored_secrets.len(), 2);
        assert_eq!(
            restored_secrets
                .get("server-a")
                .and_then(|s| s.oauth.clone()),
            secrets.get("server-a").and_then(|s| s.oauth.clone())
        );
        assert_eq!(
            restored_secrets
                .get("server-a")
                .and_then(|s| s.oauth_client_id.clone()),
            Some("client-id-abc".to_string()),
            "BYO OAuth client_id must survive export -> import"
        );
        assert_eq!(
            restored_secrets
                .get("server-a")
                .and_then(|s| s.oauth_client_secret.clone()),
            Some("client-secret-xyz".to_string()),
            "BYO OAuth client_secret must survive export -> import"
        );
        assert_eq!(
            restored_secrets
                .get("server-b")
                .and_then(|s| s.jotta_refresh.clone()),
            secrets
                .get("server-b")
                .and_then(|s| s.jotta_refresh.clone())
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// CWP-20B: a Crypt profile (either overlay kind) must re-import AS a Crypt
    /// profile. The `aeroCryptOverlay` binding (kind + remoteScope) and the
    /// overlay password/salt secrets must survive the export -> import round-trip;
    /// before the fix the binding lived in a frontend-only field that
    /// `ServerProfileExport` silently dropped, so the import degraded to plaintext.
    #[test]
    fn round_trip_preserves_crypt_overlay_binding_both_kinds() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_export_crypt_{}_{}.aeroftp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        // Native AeroCrypt on a subfolder scope.
        let mut native = sample_server("crypt-native", "filen");
        native.aero_crypt_overlay = Some(serde_json::json!({
            "enabled": true,
            "kind": "aerocrypt",
            "remoteScope": "/AeroCryptTest",
        }));
        native.has_stored_aero_crypt_password = Some(true);

        // Interop rclone-crypt with a second password (salt).
        let mut rclone = sample_server("crypt-rclone", "sftp");
        rclone.aero_crypt_overlay = Some(serde_json::json!({
            "enabled": true,
            "kind": "rclone-crypt",
            "remoteScope": "/enc",
            "directoryNameEncryption": true,
        }));
        rclone.has_stored_aero_crypt_password = Some(true);
        rclone.has_stored_aero_crypt_salt = Some(true);

        let mut secrets = HashMap::new();
        secrets.insert(
            "crypt-native".to_string(),
            ProviderSecrets {
                aerocrypt_overlay_pw: Some("native-overlay-pw".to_string()),
                aerocrypt_overlay_keyfile_path: Some("/home/user/.aeroftp/vault.key".to_string()),
                ..Default::default()
            },
        );
        secrets.insert(
            "crypt-rclone".to_string(),
            ProviderSecrets {
                aerocrypt_overlay_pw: Some("rclone-pw1".to_string()),
                aerocrypt_overlay_salt: Some("rclone-pw2-salt".to_string()),
                ..Default::default()
            },
        );

        export_profiles(vec![native, rclone], secrets.clone(), "pw-12345678", &tmp)
            .expect("export should succeed");

        let (servers, restored_secrets, _meta) =
            import_profiles(&tmp, "pw-12345678").expect("import should succeed");

        let native_out = servers.iter().find(|s| s.id == "crypt-native").unwrap();
        assert_eq!(
            native_out
                .aero_crypt_overlay
                .as_ref()
                .and_then(|b| b.get("kind"))
                .and_then(|k| k.as_str()),
            Some("aerocrypt"),
            "native crypt binding kind must survive"
        );
        assert_eq!(
            native_out
                .aero_crypt_overlay
                .as_ref()
                .and_then(|b| b.get("remoteScope"))
                .and_then(|s| s.as_str()),
            Some("/AeroCryptTest"),
            "native crypt remoteScope must survive"
        );

        let rclone_out = servers.iter().find(|s| s.id == "crypt-rclone").unwrap();
        assert_eq!(
            rclone_out
                .aero_crypt_overlay
                .as_ref()
                .and_then(|b| b.get("kind"))
                .and_then(|k| k.as_str()),
            Some("rclone-crypt"),
            "rclone-crypt binding kind must survive"
        );

        assert_eq!(
            restored_secrets
                .get("crypt-native")
                .and_then(|s| s.aerocrypt_overlay_pw.clone()),
            Some("native-overlay-pw".to_string()),
        );
        // The AeroCrypt keyfile PATH round-trips (the keyfile contents never do).
        assert_eq!(
            restored_secrets
                .get("crypt-native")
                .and_then(|s| s.aerocrypt_overlay_keyfile_path.clone()),
            Some("/home/user/.aeroftp/vault.key".to_string()),
        );
        assert_eq!(
            restored_secrets
                .get("crypt-rclone")
                .and_then(|s| s.aerocrypt_overlay_salt.clone()),
            Some("rclone-pw2-salt".to_string()),
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Issue #215 Caveat A: a multi-protocol profile that opted into
    /// `persistModeCredentials` must re-import with that opt-in intact AND with
    /// its per-mode credential snapshots (`server_modes_<id>`) restored, so the
    /// user keeps each protocol's saved credentials on a fresh device. The same
    /// round-trip also exercises the OAuth Remote Path field (`initial_path`)
    /// the harmonized OAuth pages added: verify it survives export -> import too.
    #[test]
    fn round_trip_preserves_mode_credentials_and_oauth_remote_path() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_export_modecreds_{}_{}.aeroftp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        // A Koofr profile saved in two protocol modes, with the per-protocol
        // "remember credentials" opt-in ON and an OAuth Remote Path set.
        let mut server = sample_server("koofr-multi", "koofr");
        server.persist_mode_credentials = Some(true);
        server.initial_path = Some("/Backups/AeroFTP".to_string());

        // The vault blob modeCredentialStore persists under `server_modes_<id>`:
        // one entry per protocol mode, single-use secrets already stripped.
        let modes_json = r#"{"koofr":{"username":"u@example.com","password":"native-pw","server":"app.koofr.net"},"webdav":{"username":"u@example.com","password":"dav-pw","server":"app.koofr.net/dav/Koofr"}}"#;

        let mut secrets = HashMap::new();
        secrets.insert(
            "koofr-multi".to_string(),
            ProviderSecrets {
                mode_credentials: Some(modes_json.to_string()),
                ..Default::default()
            },
        );

        let metadata = export_profiles(vec![server], secrets.clone(), "pw-12345678", &tmp)
            .expect("export should succeed");
        assert!(
            metadata.has_credentials,
            "a profile carrying only per-protocol snapshots must still flip has_credentials"
        );

        let (servers, restored_secrets, _meta) =
            import_profiles(&tmp, "pw-12345678").expect("import should succeed");
        assert_eq!(servers.len(), 1);

        let out = &servers[0];
        assert_eq!(
            out.persist_mode_credentials,
            Some(true),
            "the persistModeCredentials opt-in must survive the round-trip"
        );
        assert_eq!(
            out.initial_path.as_deref(),
            Some("/Backups/AeroFTP"),
            "the OAuth Remote Path (initial_path) must survive the round-trip"
        );
        assert_eq!(
            restored_secrets
                .get("koofr-multi")
                .and_then(|s| s.mode_credentials.clone()),
            Some(modes_json.to_string()),
            "each protocol mode's saved credentials must survive the round-trip"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Issue #214: existing `.aeroftp` files (file version 1, exported before
    /// the refactor) do not carry the `provider_secrets` section. Importing
    /// them must succeed, returning an empty map instead of erroring on the
    /// missing field.
    #[test]
    fn import_legacy_v1_without_provider_secrets() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_export_legacy_{}_{}.aeroftp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        let server = sample_server("server-a", "ftp");
        export_profiles(vec![server], HashMap::new(), "pw-12345678", &tmp)
            .expect("export should succeed");

        let (servers, secrets, _meta) =
            import_profiles(&tmp, "pw-12345678").expect("legacy import should succeed");
        assert_eq!(servers.len(), 1);
        assert!(
            secrets.is_empty(),
            "files exported without provider_secrets must yield an empty map"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Empty `ProviderSecrets` entries should not flip `has_credentials` on
    /// when no actual password or token is present, so the import dialog
    /// keeps showing the unobtrusive "no credentials" path.
    #[test]
    fn empty_provider_secrets_do_not_force_has_credentials() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_export_empty_{}_{}.aeroftp",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let server = sample_server("server-a", "ftp");
        let metadata = export_profiles(vec![server], HashMap::new(), "pw-12345678", &tmp).unwrap();
        assert!(!metadata.has_credentials);
        let _ = std::fs::remove_file(&tmp);
    }
}
