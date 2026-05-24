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
}

#[derive(Serialize, Deserialize, Clone)]
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

    let any_provider_secret = provider_secrets
        .values()
        .any(|s| s.oauth.is_some() || s.jotta_refresh.is_some());
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
                .get("server-b")
                .and_then(|s| s.jotta_refresh.clone()),
            secrets
                .get("server-b")
                .and_then(|s| s.jotta_refresh.clone())
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
