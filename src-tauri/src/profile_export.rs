// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP Server Profile Export/Import
// Encrypted backup/restore using AES-256-GCM + Argon2id
// File format: .aeroftp (JSON envelope with encrypted payload)

use serde::{Deserialize, Serialize};
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
    password: &str,
    file_path: &Path,
) -> Result<ExportMetadata, ExportError> {
    // A2-06: Use strong KDF (128 MiB) — same strength as vault
    let salt = crate::crypto::random_bytes(32);
    let key = crate::crypto::derive_key_strong(password, &salt)
        .map_err(ExportError::Encryption)?;

    let metadata = ExportMetadata {
        export_date: chrono::Utc::now().to_rfc3339(),
        aeroftp_version: env!("CARGO_PKG_VERSION").to_string(),
        server_count: servers.len() as u32,
        has_credentials: servers.iter().any(|s| s.credential.is_some()),
    };

    let payload = ExportPayload { servers };
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

pub fn import_profiles(
    file_path: &Path,
    password: &str,
) -> Result<(Vec<ServerProfileExport>, ExportMetadata), ExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: ExportFile = serde_json::from_slice(&file_data)?;

    if export_file.version > FILE_VERSION {
        return Err(ExportError::UnsupportedVersion(export_file.version));
    }

    // A2-06: Try strong KDF first (128 MiB, new exports), fall back to legacy (64 MiB) for old files
    let key_strong = crate::crypto::derive_key_strong(password, &export_file.salt)
        .map_err(ExportError::Encryption)?;
    let payload_json = match crate::crypto::decrypt_aes_gcm(&key_strong, &export_file.nonce, &export_file.encrypted_payload) {
        Ok(data) => data,
        Err(_) => {
            let key_legacy = crate::crypto::derive_key(password, &export_file.salt)
                .map_err(ExportError::Encryption)?;
            crate::crypto::decrypt_aes_gcm(&key_legacy, &export_file.nonce, &export_file.encrypted_payload)
                .map_err(|_| ExportError::InvalidPassword)?
        }
    };

    let payload: ExportPayload = serde_json::from_slice(&payload_json)?;

    Ok((payload.servers, export_file.metadata))
}

pub fn read_metadata(file_path: &Path) -> Result<ExportMetadata, ExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: ExportFile = serde_json::from_slice(&file_data)?;
    Ok(export_file.metadata)
}

// Crypto primitives shared via crate::crypto module
