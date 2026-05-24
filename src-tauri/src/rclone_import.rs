// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from rclone configuration files.
//!
//! Parses `rclone.conf` (INI format), maps rclone backend types to AeroFTP
//! ProviderType, and de-obfuscates rclone "obscured" passwords (AES-256-CTR
//! with a well-known key: NOT real encryption).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from rclone's reversible obfuscation to proper authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ rclone obscure: AES-256-CTR with published key ============
// Source: https://github.com/rclone/rclone/blob/master/fs/config/obscure/obscure.go
// This is NOT encryption: the key is public. We reveal it to store in our vault.

const RCLONE_CRYPT_KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];

/// Reveal an rclone-obscured password.
/// Format: base64url(IV_16bytes || AES-256-CTR(plaintext))
fn reveal_obscured(obscured: &str) -> Result<String, String> {
    use aes::cipher::{KeyIvInit, StreamCipher};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    // rclone uses raw URL-safe base64 (no padding)
    let ciphertext = URL_SAFE_NO_PAD
        .decode(obscured)
        .or_else(|_| {
            // Some rclone versions may use standard base64
            use base64::engine::general_purpose::STANDARD;
            STANDARD.decode(obscured)
        })
        .map_err(|e| format!("base64 decode: {}", e))?;

    if ciphertext.len() < 16 {
        return Err("obscured value too short (need at least 16-byte IV)".into());
    }

    let iv = &ciphertext[..16];
    let mut buf = ciphertext[16..].to_vec();

    type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;
    let mut cipher = Aes256Ctr::new((&RCLONE_CRYPT_KEY).into(), iv.into());
    cipher.apply_keystream(&mut buf);

    String::from_utf8(buf).map_err(|e| format!("UTF-8 decode after reveal: {}", e))
}

/// Obscure a plaintext password using rclone's AES-256-CTR scheme.
/// Output: base64url(random_IV_16 || AES-256-CTR(plaintext))
fn obscure_password(plaintext: &str) -> Result<String, String> {
    use aes::cipher::{KeyIvInit, StreamCipher};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let iv = crate::crypto::random_bytes(16);
    let mut buf = plaintext.as_bytes().to_vec();

    type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;
    let mut cipher = Aes256Ctr::new((&RCLONE_CRYPT_KEY).into(), iv.as_slice().into());
    cipher.apply_keystream(&mut buf);

    let mut output = Vec::with_capacity(16 + buf.len());
    output.extend_from_slice(&iv);
    output.extend_from_slice(&buf);

    Ok(URL_SAFE_NO_PAD.encode(&output))
}

// ============ INI Parser ============

/// A parsed rclone remote: section name → key/value pairs.
type RcloneRemote = HashMap<String, String>;

/// Parse rclone.conf INI format into named sections.
fn parse_rclone_conf(content: &str) -> HashMap<String, RcloneRemote> {
    let mut sections: HashMap<String, RcloneRemote> = HashMap::new();
    let mut current_section: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header: [name]
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_string();
            if !name.is_empty() {
                sections.entry(name.clone()).or_default();
                current_section = Some(name);
            }
            continue;
        }

        // Key = value
        if let Some(ref section) = current_section {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if let Some(sec) = sections.get_mut(section) {
                    sec.insert(key, value);
                }
            }
        }
    }

    sections
}

// ============ Type Mapping ============

#[derive(Default)]
struct MappedProfile {
    protocol: String,
    provider_id: Option<String>,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
    /// AeroFTP-format `StoredTokens` JSON converted from the rclone
    /// `token = {...}` blob. The vault writer pairs this with the new
    /// per-profile id once the import is materialised. Issue #214.
    oauth_token: Option<String>,
    /// Jottacloud refresh-token JSON in the shape `JottacloudProvider`
    /// persists (`refresh_token`/`token_endpoint`/`username`), bound to the
    /// new per-profile vault key on import. Issue #214.
    jotta_refresh: Option<String>,
}

/// Convert an rclone `token = {...}` blob to the JSON shape AeroFTP's vault
/// stores under `oauth_<provider>` keys (`StoredTokens`). rclone uses
/// RFC 3339 `"expiry"` while AeroFTP uses a Unix `expires_at`; the
/// conversion is otherwise field-for-field. Returns `None` when the blob
/// lacks `access_token` or is not valid JSON. Issue #214.
fn rclone_token_to_aeroftp(blob: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(blob).ok()?;
    let access_token = value.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    let refresh_token = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let token_type = value
        .get("token_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let expires_at = value
        .get("expiry")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp());
    let aeroftp = serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "token_type": token_type,
        "scopes": serde_json::Value::Array(Vec::new()),
    });
    serde_json::to_string_pretty(&aeroftp).ok()
}

/// Build the Jotta refresh-token blob in the shape `JottacloudProvider`
/// persists locally (issue #214). rclone stores `username`, `token = {...}`
/// (with the same fields as OAuth2 providers) and a `client_id`. We extract
/// the refresh token and rebuild the persistence shape so reconnecting on
/// the destination device skips the single-use login token.
fn rclone_jotta_to_aeroftp(remote: &RcloneRemote) -> Option<String> {
    let token_blob = remote.get("token")?;
    let token: serde_json::Value = serde_json::from_str(token_blob).ok()?;
    let refresh_token = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let username = remote.get("user").map(|s| s.as_str()).unwrap_or("");
    // rclone's jottacloud backend uses the JAR token endpoint by default;
    // surface it the same way `JottacloudProvider` does after a successful
    // reconnect, but leave it empty when rclone did not store it so the
    // provider re-discovers OIDC on first use.
    let token_endpoint = remote
        .get("token_endpoint")
        .map(|s| s.as_str())
        .unwrap_or("");
    let blob = serde_json::json!({
        "refresh_token": refresh_token,
        "token_endpoint": token_endpoint,
        "username": username,
    });
    serde_json::to_string(&blob).ok()
}

// Provider tables now live in `crate::bridge_shared` (Refactor 6): a single
// source for every importer instead of per-module duplicates.

/// Convert a single rclone remote to an AeroFTP profile.
fn map_remote(name: &str, remote: &RcloneRemote) -> Option<MappedProfile> {
    let rclone_type = remote.get("type")?.to_lowercase();

    // Helper to get and optionally reveal password
    let get_password = |key: &str| -> Option<String> {
        remote.get(key).and_then(|v| {
            if v.is_empty() {
                return None;
            }
            // Try to reveal obscured password; fall back to plaintext
            match reveal_obscured(v) {
                Ok(revealed) if !revealed.is_empty() => Some(revealed),
                _ => Some(v.clone()),
            }
        })
    };

    let get_str = |key: &str| remote.get(key).map(|s| s.as_str());
    let get_port = |key: &str, default: u32| -> u32 {
        remote
            .get(key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
    };

    match rclone_type.as_str() {
        // ---- FTP ----
        "ftp" => {
            let host = get_str("host").unwrap_or("").to_string();
            if host.is_empty() {
                return None;
            }
            let tls = get_str("tls")
                .or(get_str("explicit_tls"))
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            let protocol = if tls { "ftps" } else { "ftp" };
            let default_port = if tls { 990 } else { 21 };

            Some(MappedProfile {
                protocol: protocol.to_string(),
                provider_id: None,
                host,
                port: get_port("port", default_port),
                username: get_str("user").unwrap_or("anonymous").to_string(),
                password: get_password("pass"),
                options: None,
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- SFTP ----
        "sftp" => {
            let host = get_str("host").unwrap_or("").to_string();
            if host.is_empty() {
                return None;
            }
            Some(MappedProfile {
                protocol: "sftp".to_string(),
                provider_id: None,
                host,
                port: get_port("port", 22),
                username: get_str("user").unwrap_or("root").to_string(),
                password: get_password("pass"),
                options: None,
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- S3 ----
        "s3" => {
            let s3_provider = get_str("provider").unwrap_or("Other");
            let provider_id = crate::bridge_shared::map_s3_provider(s3_provider);
            let region = get_str("region").unwrap_or("us-east-1").to_string();
            let endpoint = get_str("endpoint").unwrap_or("").to_string();

            // Build S3 endpoint host
            let host = if !endpoint.is_empty() {
                endpoint
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string()
            } else if provider_id == "amazon-s3" {
                format!("s3.{}.amazonaws.com", region)
            } else {
                // Generic S3: need endpoint
                return None;
            };

            let mut options = serde_json::Map::new();
            if let Some(bucket) = get_str("bucket") {
                if !bucket.is_empty() {
                    options.insert(
                        "bucket".into(),
                        serde_json::Value::String(bucket.to_string()),
                    );
                }
            }
            options.insert("region".into(), serde_json::Value::String(region));
            if let Some(ep) = get_str("endpoint") {
                if !ep.is_empty() {
                    options.insert("endpoint".into(), serde_json::Value::String(ep.to_string()));
                }
            }
            // Path-style access (common for non-AWS)
            let path_style = get_str("force_path_style")
                .or(get_str("use_path_style"))
                .map(|v| v == "true" || v == "1")
                .unwrap_or(provider_id != "amazon-s3");
            options.insert("pathStyle".into(), serde_json::Value::Bool(path_style));

            Some(MappedProfile {
                protocol: "s3".to_string(),
                provider_id: Some(provider_id.to_string()),
                host,
                port: 443,
                username: get_str("access_key_id").unwrap_or("").to_string(),
                password: get_password("secret_access_key"),
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- WebDAV ----
        "webdav" => {
            let url = get_str("url").unwrap_or("").to_string();
            if url.is_empty() {
                return None;
            }
            let vendor = get_str("vendor").unwrap_or("other");
            let provider_id = crate::bridge_shared::map_webdav_vendor(vendor);

            // Parse URL to extract host and base path
            let (host, base_path, port) = parse_webdav_url(&url);

            let mut options = serde_json::Map::new();
            if !base_path.is_empty() {
                options.insert("basePath".into(), serde_json::Value::String(base_path));
            }

            Some(MappedProfile {
                protocol: "webdav".to_string(),
                provider_id: Some(provider_id.to_string()),
                host,
                port,
                username: get_str("user").unwrap_or("").to_string(),
                password: get_password("pass"),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- Google Drive ----
        "drive" => Some(MappedProfile {
            protocol: "googledrive".to_string(),
            provider_id: Some("googledrive".to_string()),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: name.to_string(), // rclone doesn't store email for drive
            password: None,
            options: None,
            initial_path: get_str("root_folder_id").map(|s| s.to_string()),
            // Issue #214: import the OAuth token blob so the destination
            // device reconnects without a re-auth round-trip, symmetrically
            // with how `.aeroftp` exports carry credentials.
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- Google Photos ----
        "gphotos" | "googlephotos" => Some(MappedProfile {
            protocol: "googlephotos".to_string(),
            provider_id: Some("googlephotos".to_string()),
            host: "photoslibrary.googleapis.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- Dropbox ----
        "dropbox" => Some(MappedProfile {
            protocol: "dropbox".to_string(),
            provider_id: Some("dropbox".to_string()),
            host: "api.dropboxapi.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- OneDrive ----
        "onedrive" => Some(MappedProfile {
            protocol: "onedrive".to_string(),
            provider_id: Some("onedrive".to_string()),
            host: "graph.microsoft.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- MEGA ----
        "mega" => Some(MappedProfile {
            protocol: "mega".to_string(),
            provider_id: Some("mega".to_string()),
            host: "mega.nz".to_string(),
            port: 443,
            username: get_str("user").unwrap_or("").to_string(),
            password: get_password("pass"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
        }),

        // ---- Box ----
        "box" => Some(MappedProfile {
            protocol: "box".to_string(),
            provider_id: Some("box".to_string()),
            host: "api.box.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- pCloud ----
        "pcloud" => Some(MappedProfile {
            protocol: "pcloud".to_string(),
            provider_id: Some("pcloud".to_string()),
            host: get_str("hostname").unwrap_or("eapi.pcloud.com").to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- Azure Blob Storage ----
        "azureblob" => {
            let account = get_str("account").unwrap_or("").to_string();
            if account.is_empty() {
                return None;
            }
            let mut options = serde_json::Map::new();
            if let Some(container) = get_str("container") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(container.to_string()),
                );
            }

            Some(MappedProfile {
                protocol: "azure".to_string(),
                provider_id: Some("azure-blob".to_string()),
                host: format!("{}.blob.core.windows.net", account),
                port: 443,
                username: account,
                password: get_password("key"),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- OpenStack Swift ----
        "swift" => {
            let auth_url = get_str("auth").unwrap_or("").to_string();
            let host = auth_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            if host.is_empty() {
                return None;
            }

            let mut options = serde_json::Map::new();
            if let Some(container) = get_str("container") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(container.to_string()),
                );
            }
            if !auth_url.is_empty() {
                options.insert("endpoint".into(), serde_json::Value::String(auth_url));
            }
            if let Some(region) = get_str("region") {
                options.insert(
                    "region".into(),
                    serde_json::Value::String(region.to_string()),
                );
            }
            if let Some(tenant) = get_str("tenant").or(get_str("tenant_id")) {
                options.insert(
                    "tenant".into(),
                    serde_json::Value::String(tenant.to_string()),
                );
            }

            Some(MappedProfile {
                protocol: "swift".to_string(),
                provider_id: Some("custom-swift".to_string()),
                host,
                port: 443,
                username: get_str("user").unwrap_or("").to_string(),
                password: get_password("key"),
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- Yandex Disk ----
        "yandexdisk" => Some(MappedProfile {
            protocol: "yandexdisk".to_string(),
            provider_id: Some("yandex-disk".to_string()),
            host: "webdav.yandex.ru".to_string(),
            port: 443,
            username: name.to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
            jotta_refresh: None,
        }),

        // ---- Koofr ----
        "koofr" => Some(MappedProfile {
            protocol: "koofr".to_string(),
            provider_id: Some("koofr".to_string()),
            host: get_str("endpoint")
                .unwrap_or("https://app.koofr.net")
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string(),
            port: 443,
            username: get_str("user").unwrap_or("").to_string(),
            password: get_password("password"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
        }),

        // ---- Jottacloud ----
        "jottacloud" => Some(MappedProfile {
            protocol: "jottacloud".to_string(),
            provider_id: Some("jottacloud".to_string()),
            host: "jottacloud.com".to_string(),
            port: 443,
            username: get_str("user").unwrap_or(name).to_string(),
            password: None,
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: rclone_jotta_to_aeroftp(remote),
        }),

        // ---- Backblaze B2 (native API v4) ----
        // rclone stores `account` as the applicationKeyId and `key` as the
        // applicationKey. We route them to the native AeroFTP B2 provider so
        // users get large-file workflow + server-side copy + version history
        // (the previous mapping went through the S3-compatible endpoint).
        "b2" => {
            let account = get_str("account").unwrap_or("").to_string();
            if account.is_empty() {
                return None;
            }

            let mut options = serde_json::Map::new();
            if let Some(bucket) = get_str("bucket") {
                options.insert(
                    "bucket".into(),
                    serde_json::Value::String(bucket.to_string()),
                );
            }

            Some(MappedProfile {
                protocol: "backblaze".to_string(),
                provider_id: Some("backblaze-native".to_string()),
                host: "api.backblazeb2.com".to_string(),
                port: 443,
                username: account,
                password: get_password("key"),
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: None,
                jotta_refresh: None,
            })
        }

        // ---- OpenDrive ----
        "opendrive" => Some(MappedProfile {
            protocol: "opendrive".to_string(),
            provider_id: Some("opendrive".to_string()),
            host: "od.lk".to_string(),
            port: 443,
            username: get_str("username").unwrap_or("").to_string(),
            password: get_password("password"),
            options: None,
            initial_path: None,
            oauth_token: None,
            jotta_refresh: None,
        }),

        // Unsupported rclone types: skip gracefully
        _ => None,
    }
}

fn parse_crypt_remote_target(remote_target: &str) -> (String, Option<String>) {
    if let Some((base, subpath)) = remote_target.split_once(':') {
        let normalized = subpath.trim().trim_start_matches('/');
        let initial_path = if normalized.is_empty() {
            None
        } else {
            Some(format!("/{}", normalized))
        };
        (base.trim().to_string(), initial_path)
    } else {
        (remote_target.trim().to_string(), None)
    }
}

fn map_crypt_remote(
    name: &str,
    remote: &RcloneRemote,
    sections: &HashMap<String, RcloneRemote>,
) -> Option<MappedProfile> {
    let remote_target = remote.get("remote")?.trim().to_string();
    if remote_target.is_empty() {
        return None;
    }

    let (base_remote_name, crypt_subpath) = parse_crypt_remote_target(&remote_target);
    let base_remote = sections.get(&base_remote_name)?;
    let mut mapped = map_remote(&base_remote_name, base_remote)?;

    let get_str = |k: &str| remote.get(k).map(|s| s.as_str());
    let get_password = |k: &str| {
        get_str(k).and_then(|v| {
            if v.is_empty() {
                None
            } else {
                reveal_obscured(v).ok().or_else(|| Some(v.to_string()))
            }
        })
    };

    let mut options = match mapped.options.take() {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };

    options.insert("rcloneCryptEnabled".into(), serde_json::Value::Bool(true));
    options.insert(
        "rcloneCryptRemote".into(),
        serde_json::Value::String(remote_target),
    );
    options.insert(
        "rcloneCryptOverlayName".into(),
        serde_json::Value::String(name.to_string()),
    );

    if let Some(pw) = get_password("password") {
        options.insert("rcloneCryptPassword".into(), serde_json::Value::String(pw));
    }
    if let Some(pw2) = get_password("password2") {
        options.insert(
            "rcloneCryptPassword2".into(),
            serde_json::Value::String(pw2),
        );
    }
    if let Some(mode) = get_str("filename_encryption") {
        options.insert(
            "rcloneCryptFilenameEncryption".into(),
            serde_json::Value::String(mode.to_string()),
        );
    }
    if let Some(v) = get_str("directory_name_encryption") {
        let dir_enc = v.eq_ignore_ascii_case("true") || v == "1";
        options.insert(
            "rcloneCryptDirectoryNameEncryption".into(),
            serde_json::Value::Bool(dir_enc),
        );
    }

    mapped.options = Some(serde_json::Value::Object(options));

    if crypt_subpath.is_some() {
        mapped.initial_path = crypt_subpath;
    }

    Some(mapped)
}

/// Parse a WebDAV URL into (host, basePath, port).
fn parse_webdav_url(url: &str) -> (String, String, u32) {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let is_https = url.starts_with("https://");

    let (host_port, path) = match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], without_scheme[i..].to_string()),
        None => (without_scheme, String::new()),
    };

    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port = p.parse::<u32>().unwrap_or(if is_https { 443 } else { 80 });
            (h.to_string(), port)
        }
        None => (host_port.to_string(), if is_https { 443 } else { 80 }),
    };

    (host, path, port)
}

// ============ Default config path detection ============

/// Returns the default rclone.conf path for the current platform.
pub fn default_rclone_config_path() -> Option<PathBuf> {
    // rclone uses $RCLONE_CONFIG env var first
    if let Ok(path) = std::env::var("RCLONE_CONFIG") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try `rclone config file` command output (most reliable)
    if let Ok(output) = std::process::Command::new("rclone")
        .args(["config", "file"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Output is like: "Configuration file is stored at:\n/path/to/rclone.conf\n"
            for line in stdout.lines() {
                let line = line.trim();
                if line.ends_with("rclone.conf") || line.ends_with("rclone.conf\"") {
                    let path = PathBuf::from(line.trim_matches('"'));
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
    }

    // Platform-specific defaults
    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let path = PathBuf::from(xdg).join("rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = PathBuf::from(appdata).join("rclone/rclone.conf");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Public API ============

/// Result of importing rclone config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<RcloneSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
    /// Per-profile OAuth / Jotta token blobs imported from the rclone
    /// `[remote]` sections, keyed by the freshly minted AeroFTP profile id.
    /// `#[serde(skip)]` keeps them out of the renderer-bound response: the
    /// vault writer in `lib.rs` consumes them server-side and only the
    /// `hasStoredCredential` boolean reaches the frontend. Issue #214.
    #[serde(skip)]
    pub provider_secrets: std::collections::HashMap<String, crate::profile_export::ProviderSecrets>,
}

/// A remote that was skipped (unsupported type).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneSkippedRemote {
    pub name: String,
    pub rclone_type: String,
    pub reason: String,
}

/// Import all supported remotes from an rclone.conf file.
pub fn import_rclone(config_path: &Path) -> Result<RcloneImportResult, String> {
    let content =
        std::fs::read_to_string(config_path).map_err(|e| format!("Read rclone.conf: {}", e))?;

    let sections = parse_rclone_conf(&content);
    let total_remotes = sections.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut provider_secrets: std::collections::HashMap<
        String,
        crate::profile_export::ProviderSecrets,
    > = std::collections::HashMap::new();

    for (name, remote) in &sections {
        let rclone_type = remote.get("type").map(|s| s.as_str()).unwrap_or("unknown");

        let mapped = if rclone_type == "crypt" {
            map_crypt_remote(name, remote, &sections)
        } else {
            map_remote(name, remote)
        };

        match mapped {
            Some(mapped) => {
                let id = format!(
                    "rclone-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                // Issue #214: capture per-profile OAuth / Jotta blobs so the
                // vault writer can store them under the new per-profile key.
                if mapped.oauth_token.is_some() || mapped.jotta_refresh.is_some() {
                    provider_secrets.insert(
                        id.clone(),
                        crate::profile_export::ProviderSecrets {
                            oauth: mapped.oauth_token,
                            jotta_refresh: mapped.jotta_refresh,
                        },
                    );
                }

                servers.push(ServerProfileExport {
                    id,
                    name: name.clone(),
                    host: mapped.host,
                    port: mapped.port,
                    username: mapped.username,
                    protocol: Some(mapped.protocol),
                    initial_path: mapped.initial_path,
                    local_initial_path: None,
                    color: None,
                    last_connected: None,
                    options: mapped.options,
                    provider_id: mapped.provider_id,
                    credential: mapped.password,
                    has_stored_credential: None,
                    public_url_base: None,
                });
            }
            None => {
                let reason = if rclone_type == "unknown" {
                    "missing type field".to_string()
                } else {
                    format!("unsupported rclone type: {}", rclone_type)
                };
                skipped.push(RcloneSkippedRemote {
                    name: name.clone(),
                    rclone_type: rclone_type.to_string(),
                    reason,
                });
            }
        }
    }

    Ok(RcloneImportResult {
        servers,
        skipped,
        source_path: config_path.display().to_string(),
        total_remotes,
        provider_secrets,
    })
}

// uuid_v4 now lives in `crate::bridge_shared` (Refactor 6).

// ============ Export to rclone.conf ============

/// A server profile to export as rclone remote.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RcloneExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    // Password is fetched from vault separately and passed in
}

/// Sanitize a saved-profile name into a valid rclone remote name.
///
/// rclone validates remote names against `^[\w.+@ -]+$` and additionally
/// rejects a leading `-`/space and a trailing space
/// (`fspath.CheckConfigName`, `\w` being ASCII-only in Go's regexp).
/// AeroFTP profile names are free-form and routinely contain characters
/// rclone refuses, e.g. `axpbuntu-remote (admin)` (parentheses). The
/// previous export only stripped INI-breaking characters (`[ ] CR LF`),
/// so such profiles produced a config rclone could not even load. This
/// maps every disallowed character to `-`, collapses runs of separators
/// to a single `-`, and trims the edges. Names that already worked
/// (single internal spaces, e.g. `axpbuntu lab MinIO`) are left intact.
/// Returns an empty string only if nothing usable remains; the caller
/// skips those.
fn sanitize_rclone_remote_name(name: &str) -> String {
    let mapped = name.chars().map(|c| {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '@' | ' ' | '-') {
            c
        } else {
            '-'
        }
    });

    // Collapse runs of 2+ separators ([space-]) into a single '-' so a
    // replaced bracket adjacent to an existing space/dash does not leave
    // "remote -admin-". A lone space is a run of length 1 and survives.
    let mut collapsed = String::with_capacity(name.len());
    let mut sep_run = 0usize;
    for c in mapped {
        if c == ' ' || c == '-' {
            sep_run += 1;
            match sep_run {
                1 => collapsed.push(c),
                2 => {
                    collapsed.pop();
                    collapsed.push('-');
                }
                _ => {}
            }
        } else {
            sep_run = 0;
            collapsed.push(c);
        }
    }

    collapsed.trim_matches(|c| c == ' ' || c == '-').to_string()
}

/// Export server profiles to rclone.conf INI format.
/// Passwords are obscured using rclone's AES-256-CTR scheme for compatibility.
pub fn export_rclone(
    servers: &[RcloneExportServer],
    passwords: &HashMap<String, String>,
    file_path: &Path,
) -> Result<usize, String> {
    let mut output = String::new();
    output.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    output.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut exported = 0;

    for server in servers {
        let proto = server.protocol.as_deref().unwrap_or("ftp");
        let options = server.options.as_ref();
        let password = passwords.get(&server.name);

        // Sanitize remote name to rclone's own naming rules, not just INI
        // safety: rclone refuses names outside `^[\w.+@ -]+$`, so profiles
        // like "axpbuntu-remote (admin)" previously produced a config
        // rclone could not load at all.
        let remote_name = sanitize_rclone_remote_name(&server.name);
        if remote_name.is_empty() {
            continue;
        }
        if remote_name != server.name {
            tracing::warn!(
                "[rclone export] profile '{}' renamed to '{}' to satisfy rclone remote-name rules",
                server.name,
                remote_name
            );
            output.push_str(&format!(
                "# renamed from AeroFTP profile \"{}\" (rclone remote-name rules)\n",
                server.name.replace(['\n', '\r'], " ")
            ));
        }

        output.push_str(&format!("[{}]\n", remote_name));

        match proto {
            "ftp" => {
                output.push_str("type = ftp\n");
                output.push_str(&format!("host = {}\n", server.host));
                output.push_str(&format!("port = {}\n", server.port));
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "ftps" => {
                output.push_str("type = ftp\n");
                output.push_str(&format!("host = {}\n", server.host));
                output.push_str(&format!("port = {}\n", server.port));
                output.push_str(&format!("user = {}\n", server.username));
                output.push_str("tls = true\n");
                output.push_str("explicit_tls = true\n");
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "sftp" => {
                output.push_str("type = sftp\n");
                output.push_str(&format!("host = {}\n", server.host));
                output.push_str(&format!("port = {}\n", server.port));
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                // SFTP key auth: AeroFTP stores the private key as a file
                // path in `options.private_key_path` (+ optional
                // `key_passphrase`). Exporting these key-auth profiles as
                // password-only made rclone fail SSH auth even though
                // `aeroftp-cli connect` succeeded with the key. We emit a
                // `key_file` reference (no private-key bytes are copied
                // into the plaintext config).
                if let Some(opts) = options {
                    if let Some(key_file) = opts
                        .get("private_key_path")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        output.push_str(&format!("key_file = {}\n", key_file));
                    }
                    if let Some(key_pass) = opts
                        .get("key_passphrase")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        // rclone requires key_file_pass obscured, like `pass`.
                        output.push_str(&format!(
                            "key_file_pass = {}\n",
                            obscure_password(key_pass).unwrap_or_default()
                        ));
                    }
                }
            }
            "s3" => {
                output.push_str("type = s3\n");
                let provider_id = server.provider_id.as_deref().unwrap_or("custom-s3");
                // rclone S3 backend providers: see `rclone help backend s3`.
                // Names are case-sensitive; "Other" forces the generic
                // signature path which works for unknown S3-compatible
                // endpoints but skips provider-specific quirks.
                let rclone_provider = match provider_id {
                    "amazon-s3" | "aws-s3" => "AWS",
                    "cloudflare-r2" => "Cloudflare",
                    "digitalocean-spaces" => "DigitalOcean",
                    "wasabi" => "Wasabi",
                    "backblaze-b2" => "Backblaze",
                    "linode-object-storage" => "Linode",
                    "scaleway" => "Scaleway",
                    "storj" => "Storj",
                    "idrive-e2" => "IDrive",
                    "minio" => "Minio",
                    "ionos-s3" => "IONOS",
                    "alibaba-oss" => "Alibaba",
                    "tencent-cos" => "TencentCOS",
                    "qiniu-kodo" => "Qiniu",
                    "google-cloud-storage" => "GCS",
                    _ => "Other",
                };
                output.push_str(&format!("provider = {}\n", rclone_provider));
                output.push_str(&format!("access_key_id = {}\n", server.username));
                if let Some(pw) = password {
                    // S3 `secret_access_key` is NOT a `IsPassword: true`
                    // field in rclone's backend definition: it must be
                    // emitted in plain form. Obscuring it produces an
                    // `AWS4-HMAC-SHA256` signature mismatch on every
                    // request because rclone hashes the obscured string
                    // verbatim instead of reversing it first.
                    output.push_str(&format!("secret_access_key = {}\n", pw));
                }
                let mut emitted_endpoint = false;
                let mut pinned_bucket: Option<String> = None;
                if let Some(opts) = options {
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        output.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(endpoint) = opts.get("endpoint").and_then(|v| v.as_str()) {
                        output.push_str(&format!("endpoint = {}\n", endpoint));
                        emitted_endpoint = true;
                    }
                    if let Some(b) = opts.get("bucket").and_then(|v| v.as_str()) {
                        if !b.trim().is_empty() {
                            pinned_bucket = Some(b.trim().to_string());
                        }
                    }
                }
                // Fallback: some profiles (Storj S3 gateway, Cloudflare R2
                // when configured via the legacy URL field, generic S3
                // remotes) embed the endpoint URL directly in the host
                // field instead of the options map. Reuse it so the
                // rclone section is functional out of the box.
                if !emitted_endpoint && !server.host.trim().is_empty() {
                    let endpoint = server.host.trim_end_matches('/');
                    let endpoint =
                        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                            endpoint.to_string()
                        } else {
                            format!("https://{}", endpoint)
                        };
                    output.push_str(&format!("endpoint = {}\n", endpoint));
                }

                // Bucket reconciliation. The s3 backend has no `bucket` config
                // key: it is silently ignored, so a profile-pinned bucket used
                // to round-trip wrong (objects written by AeroFTP at key
                // `<obj>` in bucket `<b>` were only reachable as
                // `<remote>:<b>/<obj>`, while `<remote>:` gave "directory not
                // found"). When the profile pins a bucket we emit an `alias`
                // remote whose path is exactly `<remote>:<bucket>`, so the
                // alias addresses objects bucket-relative, identical to how
                // the AeroFTP S3 client writes keys (drop-in for crypt/sync).
                if let Some(bucket) = pinned_bucket {
                    let alias_name =
                        sanitize_rclone_remote_name(&format!("{}-{}", remote_name, bucket));
                    if !alias_name.is_empty() && alias_name != remote_name {
                        output.push_str(&format!(
                            "\n# Bucket-relative view of '{}' (bucket '{}').\n\
                             # AeroFTP writes keys at bucket root; address them\n\
                             # as `{}:` or wrap a crypt remote with `remote = {}:`.\n",
                            remote_name, bucket, alias_name, alias_name
                        ));
                        output.push_str(&format!("[{}]\n", alias_name));
                        output.push_str("type = alias\n");
                        output.push_str(&format!("remote = {}:{}\n", remote_name, bucket));
                    }
                }
            }
            "webdav" => {
                output.push_str("type = webdav\n");
                // Nextcloud-derived presets (Tab.digital, FeliCloud, generic
                // Nextcloud) speak the same WebDAV dialect as upstream
                // Nextcloud. Mapping them to vendor=nextcloud lets rclone
                // use the correct chunked-upload + checksum behaviour.
                let vendor = match server.provider_id.as_deref() {
                    Some("nextcloud")
                    | Some("nextcloud-webdav")
                    | Some("tabdigital")
                    | Some("tabdigital-webdav")
                    | Some("felicloud")
                    | Some("felicloud-webdav") => "nextcloud",
                    Some("owncloud") | Some("owncloud-webdav") => "owncloud",
                    _ => "other",
                };
                let base_path = options
                    .and_then(|o| o.get("basePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Some host strings already carry the scheme (`https://...`)
                // because the GUI persists the full URL as the host on
                // certain WebDAV presets. In that case we MUST NOT prepend
                // another scheme or the URL becomes `https://https://...`
                // and rclone refuses to dial.
                let host = server.host.trim_end_matches('/');
                let url = if host.starts_with("http://") || host.starts_with("https://") {
                    format!("{}{}", host, base_path)
                } else {
                    let scheme = if server.port == 80 { "http" } else { "https" };
                    let port_str = if server.port == 443 || server.port == 80 {
                        String::new()
                    } else {
                        format!(":{}", server.port)
                    };
                    format!("{}://{}{}{}", scheme, host, port_str, base_path)
                };
                // Resolve {username} template that Nextcloud-derived
                // presets store verbatim in the URL or initial path
                // (Tab.digital, FeliCloud).
                let url = url.replace("{username}", &server.username);
                output.push_str(&format!("url = {}\n", url));
                output.push_str(&format!("vendor = {}\n", vendor));
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "googledrive" => {
                output.push_str("type = drive\n");
            }
            "dropbox" => {
                output.push_str("type = dropbox\n");
            }
            "onedrive" => {
                output.push_str("type = onedrive\n");
            }
            "mega" => {
                output.push_str("type = mega\n");
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "box" => {
                output.push_str("type = box\n");
            }
            "pcloud" => {
                output.push_str("type = pcloud\n");
                if server.host != "eapi.pcloud.com" {
                    output.push_str(&format!("hostname = {}\n", server.host));
                }
            }
            "azure" => {
                output.push_str("type = azureblob\n");
                output.push_str(&format!("account = {}\n", server.username));
                if let Some(pw) = password {
                    // Azure Blob `key` is the storage account access key
                    // (base64). rclone's azureblob backend does NOT mark
                    // this field as `IsPassword: true`, so it must be
                    // emitted plain (same reasoning as S3 secret_access_key).
                    output.push_str(&format!("key = {}\n", pw));
                }
                if let Some(opts) = options {
                    if let Some(container) = opts.get("bucket").and_then(|v| v.as_str()) {
                        output.push_str(&format!("container = {}\n", container));
                    }
                }
            }
            "swift" => {
                output.push_str("type = swift\n");
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "key = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                if let Some(opts) = options {
                    if let Some(endpoint) = opts.get("endpoint").and_then(|v| v.as_str()) {
                        output.push_str(&format!("auth = {}\n", endpoint));
                    }
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        output.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(tenant) = opts.get("tenant").and_then(|v| v.as_str()) {
                        output.push_str(&format!("tenant = {}\n", tenant));
                    }
                    if let Some(container) = opts.get("bucket").and_then(|v| v.as_str()) {
                        output.push_str(&format!("container = {}\n", container));
                    }
                }
            }
            "yandexdisk" => {
                output.push_str("type = yandexdisk\n");
            }
            "koofr" => {
                output.push_str("type = koofr\n");
                output.push_str(&format!("endpoint = https://{}\n", server.host));
                output.push_str(&format!("user = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "jottacloud" => {
                output.push_str("type = jottacloud\n");
            }
            "opendrive" => {
                output.push_str("type = opendrive\n");
                output.push_str(&format!("username = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            // Protocols without rclone equivalent: skip
            _ => {
                continue;
            }
        }

        output.push('\n');
        exported += 1;
    }

    // Atomic write + secure permissions
    let tmp_path = file_path.with_extension("tmp");
    std::fs::write(&tmp_path, output.as_bytes())
        .map_err(|e| format!("Write rclone.conf: {}", e))?;
    std::fs::rename(&tmp_path, file_path).map_err(|e| format!("Rename temp file: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ini() {
        let conf = r#"
[mynas]
type = sftp
host = 192.168.1.100
port = 22
user = admin
pass = some_obscured_value

[backup-s3]
type = s3
provider = AWS
access_key_id = AKIAIOSFODNN7EXAMPLE
secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
region = eu-west-1
"#;
        let sections = parse_rclone_conf(conf);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections["mynas"]["type"], "sftp");
        assert_eq!(sections["mynas"]["host"], "192.168.1.100");
        assert_eq!(sections["backup-s3"]["provider"], "AWS");
    }

    #[test]
    fn test_parse_webdav_url() {
        let (host, path, port) =
            parse_webdav_url("https://cloud.example.com/remote.php/dav/files/user/");
        assert_eq!(host, "cloud.example.com");
        assert_eq!(path, "/remote.php/dav/files/user/");
        assert_eq!(port, 443);

        let (host, path, port) = parse_webdav_url("http://localhost:8080/webdav");
        assert_eq!(host, "localhost");
        assert_eq!(path, "/webdav");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_map_ftp() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "ftp".into());
        remote.insert("host".into(), "ftp.example.com".into());
        remote.insert("user".into(), "admin".into());
        remote.insert("port".into(), "21".into());

        let mapped = map_remote("test-ftp", &remote).expect("should map FTP");
        assert_eq!(mapped.protocol, "ftp");
        assert_eq!(mapped.host, "ftp.example.com");
        assert_eq!(mapped.port, 21);
        assert_eq!(mapped.username, "admin");
    }

    #[test]
    fn test_map_ftps() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "ftp".into());
        remote.insert("host".into(), "ftps.example.com".into());
        remote.insert("user".into(), "secure".into());
        remote.insert("tls".into(), "true".into());

        let mapped = map_remote("test-ftps", &remote).expect("should map FTPS");
        assert_eq!(mapped.protocol, "ftps");
    }

    #[test]
    fn test_map_unsupported() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "fichier".into());

        assert!(map_remote("unsupported", &remote).is_none());
    }

    /// Issue #214: rclone stores `token = {"access_token", "refresh_token",
    /// "expiry"}` per `[remote]`. The importer converts the blob to the
    /// AeroFTP `StoredTokens` shape (Unix `expires_at` instead of RFC 3339
    /// `expiry`) so the destination vault writes it back verbatim.
    #[test]
    fn test_rclone_token_to_aeroftp_full_conversion() {
        let blob = r#"{
            "access_token": "ya29.a0AfH6SM",
            "token_type": "Bearer",
            "refresh_token": "1//04abcdef",
            "expiry": "2030-01-01T12:00:00Z"
        }"#;
        let aero = rclone_token_to_aeroftp(blob).expect("blob should convert");
        let parsed: serde_json::Value = serde_json::from_str(&aero).unwrap();
        assert_eq!(parsed["access_token"], "ya29.a0AfH6SM");
        assert_eq!(parsed["refresh_token"], "1//04abcdef");
        assert_eq!(parsed["token_type"], "Bearer");
        // 2030-01-01T12:00:00Z == 1893499200
        assert_eq!(parsed["expires_at"].as_i64().unwrap(), 1893499200);
        assert!(parsed["scopes"].is_array());
    }

    /// A blob without `access_token` is unusable: the importer must skip it
    /// rather than write a half-formed record into the destination vault.
    #[test]
    fn test_rclone_token_to_aeroftp_rejects_missing_access_token() {
        let blob = r#"{"refresh_token": "r", "expiry": "2030-01-01T00:00:00Z"}"#;
        assert!(rclone_token_to_aeroftp(blob).is_none());
    }

    /// A blob with an empty refresh_token serialises with `refresh_token:
    /// null` so the destination vault treats the entry as a refreshless
    /// session rather than trying to use the empty string as a refresh
    /// token.
    #[test]
    fn test_rclone_token_to_aeroftp_normalises_empty_refresh() {
        let blob =
            r#"{"access_token": "a", "refresh_token": "", "expiry": "2030-01-01T00:00:00Z"}"#;
        let aero = rclone_token_to_aeroftp(blob).expect("blob should convert");
        let parsed: serde_json::Value = serde_json::from_str(&aero).unwrap();
        assert!(parsed["refresh_token"].is_null());
    }

    /// Issue #214: an rclone Google Drive remote that carries `token = {...}`
    /// produces a `MappedProfile` with the converted OAuth blob ready for
    /// vault-side storage under `oauth_google_<profile_id>`.
    #[test]
    fn test_map_drive_extracts_oauth_token() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "drive".into());
        remote.insert(
            "token".into(),
            r#"{"access_token":"abc","token_type":"Bearer","refresh_token":"r","expiry":"2030-01-01T00:00:00Z"}"#
                .into(),
        );
        let mapped = map_remote("my-drive", &remote).expect("should map drive");
        assert_eq!(mapped.protocol, "googledrive");
        let oauth_json = mapped.oauth_token.expect("token must be propagated");
        let parsed: serde_json::Value = serde_json::from_str(&oauth_json).unwrap();
        assert_eq!(parsed["access_token"], "abc");
        assert_eq!(parsed["refresh_token"], "r");
    }

    /// Issue #214: rclone Jotta remotes encode the refresh token inside the
    /// same `token = {...}` shape. The importer reshapes it to the local
    /// persistence layout (refresh_token / token_endpoint / username) so the
    /// destination provider reconnects without burning the single-use login
    /// token.
    #[test]
    fn test_map_jottacloud_extracts_refresh_token() {
        let mut remote = HashMap::new();
        remote.insert("type".into(), "jottacloud".into());
        remote.insert("user".into(), "alice@example.com".into());
        remote.insert(
            "token".into(),
            r#"{"access_token":"a","refresh_token":"rotated"}"#.into(),
        );
        let mapped = map_remote("jotta", &remote).expect("should map jotta");
        assert_eq!(mapped.protocol, "jottacloud");
        assert_eq!(mapped.username, "alice@example.com");
        let refresh_json = mapped.jotta_refresh.expect("refresh must be propagated");
        let parsed: serde_json::Value = serde_json::from_str(&refresh_json).unwrap();
        assert_eq!(parsed["refresh_token"], "rotated");
        assert_eq!(parsed["username"], "alice@example.com");
    }

    #[test]
    fn test_map_crypt_overlay_on_base_remote() {
        let mut sections: HashMap<String, RcloneRemote> = HashMap::new();

        let mut base = HashMap::new();
        base.insert("type".into(), "sftp".into());
        base.insert("host".into(), "192.168.1.10".into());
        base.insert("port".into(), "22".into());
        base.insert("user".into(), "admin".into());
        sections.insert("mynas".into(), base);

        let mut crypt = HashMap::new();
        crypt.insert("type".into(), "crypt".into());
        crypt.insert("remote".into(), "mynas:/encrypted".into());
        crypt.insert("password".into(), "topsecret".into());
        crypt.insert("password2".into(), "saltsecret".into());
        crypt.insert("filename_encryption".into(), "standard".into());
        crypt.insert("directory_name_encryption".into(), "true".into());

        let mapped = map_crypt_remote("mycrypt", &crypt, &sections).expect("should map crypt");
        assert_eq!(mapped.protocol, "sftp");
        assert_eq!(mapped.host, "192.168.1.10");
        assert_eq!(mapped.initial_path.as_deref(), Some("/encrypted"));

        let opts = mapped.options.expect("options should exist");
        let obj = opts.as_object().expect("options must be object");
        assert_eq!(
            obj.get("rcloneCryptEnabled").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            obj.get("rcloneCryptRemote").and_then(|v| v.as_str()),
            Some("mynas:/encrypted")
        );
        assert_eq!(
            obj.get("rcloneCryptPassword").and_then(|v| v.as_str()),
            Some("topsecret")
        );
        assert_eq!(
            obj.get("rcloneCryptPassword2").and_then(|v| v.as_str()),
            Some("saltsecret")
        );
        assert_eq!(
            obj.get("rcloneCryptFilenameEncryption")
                .and_then(|v| v.as_str()),
            Some("standard")
        );
        assert_eq!(
            obj.get("rcloneCryptDirectoryNameEncryption")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_reveal_obscured_password() {
        // Generated with: rclone obscure "testpassword123"
        let obscured = "LZ9RxVK9L7SryViTF1LcFaIhT4Pe_wQkOD3Gud9FnQ";
        let revealed = reveal_obscured(obscured).expect("should reveal password");
        assert_eq!(revealed, "testpassword123");
    }

    #[test]
    fn test_reveal_empty_returns_error() {
        assert!(reveal_obscured("").is_err() || reveal_obscured("short").is_err());
    }

    #[test]
    fn test_full_import() {
        use std::io::Write;
        let conf = r#"
[my-nas]
type = sftp
host = 192.168.1.100
port = 22
user = admin
pass = LZ9RxVK9L7SryViTF1LcFaIhT4Pe_wQkOD3Gud9FnQ

[gdrive]
type = drive
token = {"access_token":"fake"}

[unsupported-thing]
type = fichier
"#;
        let tmp = std::env::temp_dir().join("aeroftp-test-rclone.conf");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(conf.as_bytes()).unwrap();
        }
        let result = import_rclone(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 3);
        assert_eq!(result.servers.len(), 2); // sftp + drive
        assert_eq!(result.skipped.len(), 1); // fichier

        // Verify SFTP mapping
        let sftp = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("sftp"))
            .unwrap();
        assert_eq!(sftp.host, "192.168.1.100");
        assert_eq!(sftp.port, 22);
        assert_eq!(sftp.username, "admin");
        assert_eq!(sftp.credential.as_deref(), Some("testpassword123"));

        // Verify Google Drive mapping (no credential, OAuth)
        let gdrive = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("googledrive"))
            .unwrap();
        assert!(gdrive.credential.is_none());

        // Verify skipped
        assert_eq!(result.skipped[0].rclone_type, "fichier");
    }

    #[test]
    fn test_obscure_reveal_roundtrip() {
        let passwords = [
            "hello",
            "p@ssw0rd!",
            "with spaces",
            "unicode: \u{00e9}\u{00f1}",
            "",
        ];
        for pw in &passwords {
            if pw.is_empty() {
                continue; // empty password has no meaningful obscure
            }
            let obscured = obscure_password(pw).expect("should obscure");
            let revealed = reveal_obscured(&obscured).expect("should reveal");
            assert_eq!(&revealed, pw, "roundtrip failed for: {}", pw);
        }
    }

    #[test]
    fn test_export_rclone() {
        let servers = vec![
            RcloneExportServer {
                name: "test-sftp".to_string(),
                host: "192.168.1.1".to_string(),
                port: 22,
                username: "admin".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
            },
            RcloneExportServer {
                name: "my-s3".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "AKIAEXAMPLE".to_string(),
                protocol: Some("s3".to_string()),
                options: Some(serde_json::json!({"region": "eu-west-1", "bucket": "mybucket"})),
                provider_id: Some("amazon-s3".to_string()),
            },
        ];
        let mut passwords = HashMap::new();
        passwords.insert("test-sftp".to_string(), "secret123".to_string());
        passwords.insert("my-s3".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-rclone.conf");
        let exported = export_rclone(&servers, &passwords, &tmp).expect("should export");
        assert_eq!(exported, 2);

        // Verify the exported file can be re-imported
        let result = import_rclone(&tmp).expect("should reimport");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.servers.len(), 2);

        let sftp = result
            .servers
            .iter()
            .find(|s| s.name == "test-sftp")
            .unwrap();
        assert_eq!(sftp.protocol.as_deref(), Some("sftp"));
        assert_eq!(sftp.credential.as_deref(), Some("secret123"));

        let s3 = result.servers.iter().find(|s| s.name == "my-s3").unwrap();
        assert_eq!(s3.protocol.as_deref(), Some("s3"));
        assert_eq!(s3.credential.as_deref(), Some("s3secret"));
    }

    #[test]
    fn test_export_rclone_s3_bucket_alias() {
        // F1 regression: a profile-pinned bucket must NOT be emitted as the
        // inert `bucket =` s3 key (silently ignored by rclone), but as an
        // `alias` remote that is bucket-relative, matching how the AeroFTP
        // S3 client writes keys.
        let servers = vec![RcloneExportServer {
            name: "minio".to_string(),
            host: "s3.lab.axpdev.it".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "region": "us-east-1",
                "endpoint": "https://s3.lab.axpdev.it",
                "bucket": "aeroftp-test"
            })),
            provider_id: Some("minio".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("minio".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-alias.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        // No inert bucket key in the s3 backend section.
        assert!(
            !conf.contains("\nbucket = "),
            "must not emit the ignored s3 `bucket =` key:\n{conf}"
        );
        // Bucket-relative alias remote present and correct.
        assert!(
            conf.contains("[minio-aeroftp-test]"),
            "expected bucket alias section:\n{conf}"
        );
        assert!(
            conf.contains("type = alias"),
            "alias remote must be type=alias:\n{conf}"
        );
        assert!(
            conf.contains("remote = minio:aeroftp-test"),
            "alias must point at <remote>:<bucket>:\n{conf}"
        );

        // The alias section is not re-imported as a phantom server.
        let result = import_rclone(&tmp).unwrap_or_else(|_| {
            // file already removed; re-export to a fresh path for re-import
            let p = std::env::temp_dir().join("aeroftp-test-export-s3-alias2.conf");
            export_rclone(&servers, &passwords, &p).unwrap();
            let r = import_rclone(&p).unwrap();
            std::fs::remove_file(&p).ok();
            r
        });
        assert_eq!(result.servers.len(), 1, "alias must not become a server");
    }
}
