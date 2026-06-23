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

/// Inverse of [`rclone_token_to_aeroftp`]: convert AeroFTP's stored
/// `StoredTokens` JSON (Unix `expires_at`) into the rclone `token = {...}`
/// blob (RFC 3339 `expiry`) the OAuth backends expect. Returns `None` when the
/// stored value lacks a usable `access_token`. `token_type` defaults to
/// `Bearer`; `refresh_token` and `expiry` are emitted only when present (rclone
/// treats a missing expiry as "refresh on first use", the safe default for an
/// exported token). Issue #128-D.
fn aeroftp_token_to_rclone(blob: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(blob).ok()?;
    let access_token = value.get("access_token")?.as_str()?.to_string();
    if access_token.is_empty() {
        return None;
    }
    let token_type = value
        .get("token_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let mut out = serde_json::Map::new();
    out.insert(
        "access_token".into(),
        serde_json::Value::String(access_token),
    );
    out.insert("token_type".into(), serde_json::Value::String(token_type));
    if let Some(refresh) = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.insert(
            "refresh_token".into(),
            serde_json::Value::String(refresh.to_string()),
        );
    }
    if let Some(expiry) = value
        .get("expires_at")
        .and_then(|v| v.as_i64())
        .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
    {
        out.insert(
            "expiry".into(),
            serde_json::Value::String(expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(out)).ok()
}

/// Append the rclone OAuth credential block for an OAuth-token backend.
///
/// AeroFTP mints its OAuth tokens with the user's own (BYO) OAuth app, so
/// rclone can refresh them only when the config carries the SAME
/// `client_id`/`client_secret`; rclone's built-in app cannot. The export path
/// injects all three from the vault into the profile `options` under private
/// `__aeroftp_oauth_*` keys. When every piece is present we emit a usable,
/// refreshable remote; otherwise we emit a guidance comment instead of a
/// silently broken half-remote. Issue #128-D.
fn push_oauth_credentials(output: &mut String, options: Option<&serde_json::Value>) {
    let get = |k: &str| {
        options
            .and_then(|o| o.get(k))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let token = get("__aeroftp_oauth_token").and_then(aeroftp_token_to_rclone);
    let client_id = get("__aeroftp_oauth_client_id");
    let client_secret = get("__aeroftp_oauth_client_secret");
    match (token, client_id, client_secret) {
        (Some(tok), Some(cid), Some(csec)) => {
            output.push_str(&format!("client_id = {}\n", cid));
            output.push_str(&format!("client_secret = {}\n", csec));
            output.push_str(&format!("token = {}\n", tok));
        }
        _ => {
            output.push_str(
                "# OAuth credentials not exported (client_id/client_secret/token\n\
                 # unavailable in the vault). Run `rclone config reconnect <remote>:`\n\
                 # to authorize this remote before use.\n",
            );
        }
    }
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

        // ---- Filen ----
        // rclone's `filen` backend stores `email` + obscured `password` +
        // obscured `api_key` (all three required), plus advanced keys it derives
        // during `rclone config` (`master_keys`, `auth_version`, ...). AeroFTP
        // needs the account password (it re-derives the master keys / E2E
        // material from it on connect) and can optionally take the CLI api_key,
        // which skips the /v3/login 2FA window (issue #230). We import both; the
        // advanced rclone keys are ignored because AeroFTP re-derives them. The
        // api_key lands in `options.filen_api_key`, which the save path relocates
        // to the vault under `filen_api_key_<id>`.
        "filen" => {
            let email = get_str("email").unwrap_or("").to_string();
            if email.is_empty() {
                return None;
            }
            let mut options = serde_json::Map::new();
            if let Some(api_key) = get_password("api_key").filter(|k| !k.is_empty()) {
                options.insert("filen_api_key".into(), serde_json::Value::String(api_key));
            }
            Some(MappedProfile {
                protocol: "filen".to_string(),
                provider_id: Some("filen".to_string()),
                host: "filen.io".to_string(),
                port: 443,
                username: email,
                password: get_password("password"),
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
        // rclone's pcloud `hostname` defaults to api.pcloud.com (US); only EU
        // accounts carry eapi.pcloud.com. Derive the region so the imported
        // profile dials the right API instead of silently defaulting to US.
        "pcloud" => {
            let hostname = get_str("hostname").unwrap_or("api.pcloud.com");
            let region = if hostname.to_ascii_lowercase().contains("eapi") {
                "eu"
            } else {
                "us"
            };
            let mut options = serde_json::Map::new();
            options.insert(
                "region".into(),
                serde_json::Value::String(region.to_string()),
            );
            Some(MappedProfile {
                protocol: "pcloud".to_string(),
                provider_id: Some("pcloud".to_string()),
                host: hostname.to_string(),
                port: 443,
                username: name.to_string(),
                password: None,
                options: Some(serde_json::Value::Object(options)),
                initial_path: None,
                oauth_token: get_str("token").and_then(rclone_token_to_aeroftp),
                jotta_refresh: None,
            })
        }

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
        // rclone's backend type is `yandex`; older configs and some forks used
        // `yandexdisk`. Accept both so a real `rclone config` remote imports.
        "yandex" | "yandexdisk" => Some(MappedProfile {
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

    // Iterate remotes in a stable, alphabetical order. parse_rclone_conf
    // returns a HashMap whose iteration order is randomized per run, which
    // surfaced the imported servers (and the skipped list) in a different
    // order every time. Sorting the section names by their lexicographic
    // order mirrors `rclone listremotes`, so the import list matches what
    // the user sees in rclone itself.
    let mut section_names: Vec<&String> = sections.keys().collect();
    section_names.sort();

    for name in section_names {
        let remote = &sections[name];
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
                // #128-D: also recover the BYO OAuth app client_id/secret that
                // rclone stores in plain in the remote section, so a fresh
                // device can refresh the imported token (rclone refreshes an
                // AeroFTP-minted token only with the same OAuth app).
                if mapped.oauth_token.is_some() || mapped.jotta_refresh.is_some() {
                    let (oauth_client_id, oauth_client_secret) = if mapped.oauth_token.is_some() {
                        let pick = |k: &str| {
                            remote
                                .get(k)
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                        };
                        (pick("client_id"), pick("client_secret"))
                    } else {
                        (None, None)
                    };
                    provider_secrets.insert(
                        id.clone(),
                        crate::profile_export::ProviderSecrets {
                            oauth: mapped.oauth_token,
                            jotta_refresh: mapped.jotta_refresh,
                            oauth_client_id,
                            oauth_client_secret,
                            ..Default::default()
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
                    ..Default::default()
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

/// Whether an endpoint URL points at the local loopback interface
/// (127.0.0.0/8, ::1, or `localhost`). Self-signed local bridges such as the
/// Filen Desktop S3 gateway present a certificate without IP SANs, which rclone
/// rejects unless `no_check_certificate` is set; AeroFTP itself accepts invalid
/// certs on loopback, so the export mirrors that.
fn endpoint_is_loopback(endpoint: &str) -> bool {
    let without_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    // Drop any userinfo (`user:pass@host`).
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literals are bracketed: `[::1]:1800`.
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
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
                    "backblaze" | "backblaze-b2" => "Backblaze",
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
                let mut pinned_bucket: Option<String> = None;
                let mut endpoint_from_opts: Option<String> = None;
                let mut verify_cert_off = false;
                if let Some(opts) = options {
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        output.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(endpoint) = opts
                        .get("endpoint")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        endpoint_from_opts = Some(endpoint.to_string());
                    }
                    if let Some(b) = opts.get("bucket").and_then(|v| v.as_str()) {
                        if !b.trim().is_empty() {
                            pinned_bucket = Some(b.trim().to_string());
                        }
                    }
                    // verify_cert is stored as a bool by the GUI but may arrive
                    // as the string "false" through normalization; honour both.
                    if let Some(vc) = opts.get("verify_cert").or_else(|| opts.get("verifyCert")) {
                        verify_cert_off =
                            vc.as_bool() == Some(false) || vc.as_str() == Some("false");
                    }
                }

                // Resolve the effective endpoint in precedence order:
                //   1. explicit `options.endpoint`
                //   2. the profile host field (Storj S3 gateway, Cloudflare R2
                //      via the legacy URL field, generic S3 remotes embed the
                //      endpoint URL directly in the host)
                //   3. the provider-registry preset (Google Cloud Storage,
                //      FileLu S5, ...). Without this last fallback, registry-
                //      default presets exported with no endpoint at all and
                //      rclone silently fell back to the AWS auto-region host
                //      `<bucket>.s3.auto.amazonaws.com`, which is unreachable
                //      (observed on Google S3-interop: `dial 127.0.0.1:443`).
                //      Mirrors the connect path's apply_s3_profile_defaults.
                let resolved_endpoint = endpoint_from_opts
                    .or_else(|| {
                        let h = server.host.trim();
                        if h.is_empty() {
                            None
                        } else {
                            Some(h.to_string())
                        }
                    })
                    .or_else(|| {
                        let mut probe: HashMap<String, String> = HashMap::new();
                        if let Some(opts) = options.and_then(|v| v.as_object()) {
                            for (k, v) in opts {
                                crate::profile_loader::insert_profile_option(&mut probe, k, v);
                            }
                        }
                        crate::profile_loader::apply_s3_profile_defaults(
                            &mut probe,
                            Some(provider_id),
                        )
                    });

                if let Some(endpoint) = resolved_endpoint {
                    let endpoint = endpoint.trim_end_matches('/');
                    let endpoint =
                        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                            endpoint.to_string()
                        } else {
                            format!("https://{}", endpoint)
                        };
                    output.push_str(&format!("endpoint = {}\n", endpoint));

                    // Local self-signed bridges (Filen Desktop S3 at
                    // https://127.0.0.1:1800) present a certificate without IP
                    // SANs that rclone rejects by default; AeroFTP itself
                    // accepts invalid certs on loopback. Honour an explicit
                    // verify_cert=false from the profile too.
                    if verify_cert_off || endpoint_is_loopback(&endpoint) {
                        output.push_str("no_check_certificate = true\n");
                    }
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
                // rclone's `nextcloud`/`owncloud` vendors do not auto-discover
                // the DAV collection root the way AeroFTP does at connect time
                // (PROPFIND probe -> `/remote.php/dav/files/<user>/`). They
                // require the full collection URL up front, otherwise the
                // exported remote cannot list or transfer. AeroFTP profiles
                // usually persist a bare host (empty basePath), so synthesise
                // the path here when it is missing. Generic WebDAV (vendor
                // `other`: Koofr, SharePoint, Fastmail) has no such convention
                // and is left untouched. Any explicit `/remote.php/` segment
                // already present is honoured verbatim.
                let url = if matches!(vendor, "nextcloud" | "owncloud")
                    && !url.contains("/remote.php/")
                {
                    format!(
                        "{}/remote.php/dav/files/{}/",
                        url.trim_end_matches('/'),
                        server.username
                    )
                } else {
                    url
                };
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
                push_oauth_credentials(&mut output, options);
            }
            "dropbox" => {
                output.push_str("type = dropbox\n");
                push_oauth_credentials(&mut output, options);
            }
            "onedrive" => {
                output.push_str("type = onedrive\n");
                // region/drive hints when AeroFTP captured them. rclone marks
                // none of these `Required`, and AeroFTP operates on the default
                // `/me/drive`, so their absence still yields a working remote.
                if let Some(opts) = options {
                    if let Some(region) = opts
                        .get("region")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        output.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(drive_id) = opts
                        .get("drive_id")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        output.push_str(&format!("drive_id = {}\n", drive_id));
                    }
                    if let Some(drive_type) = opts
                        .get("drive_type")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        output.push_str(&format!("drive_type = {}\n", drive_type));
                    }
                }
                push_oauth_credentials(&mut output, options);
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
            "filen" => {
                // rclone's `filen` backend needs `email` + obscured `password`;
                // from those it logs in and derives the api_key and master keys
                // itself (prompting for the 2FA code on first use if the account
                // has it enabled). The account password is what unlocks the E2E
                // material, so it is the field that makes the remote usable.
                output.push_str("type = filen\n");
                output.push_str(&format!("email = {}\n", server.username));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    output.push_str(&format!(
                        "password = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                // An inline api_key (a pre-#230 profile, or one the caller
                // injected) lets rclone skip its own login + 2FA. The api_key is
                // an rclone `IsPassword` field, so it is emitted obscured like
                // `password`. The separately vaulted api_key is not exported
                // here; rclone re-derives it from email + password on first use.
                if let Some(api_key) = options
                    .and_then(|o| o.get("filen_api_key"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    output.push_str(&format!(
                        "api_key = {}\n",
                        obscure_password(api_key).unwrap_or_default()
                    ));
                }
            }
            "box" => {
                output.push_str("type = box\n");
                push_oauth_credentials(&mut output, options);
            }
            "pcloud" => {
                output.push_str("type = pcloud\n");
                // rclone's pcloud `hostname` must be a real API host:
                // api.pcloud.com (US, the default) or eapi.pcloud.com (EU).
                // AeroFTP stores the region in options.region ("us"/"eu") or, on
                // OAuth profiles, only as a display label in `host`
                // ("pCloud (US)"/"pCloud (EU)"), never a hostname. Emitting that
                // label verbatim produced an unusable remote. Map it: emit the
                // EU host only for EU; omit for US (rclone defaults to it).
                let is_eu = options
                    .and_then(|o| o.get("region"))
                    .and_then(|v| v.as_str())
                    .map(|r| r.eq_ignore_ascii_case("eu"))
                    .unwrap_or_else(|| {
                        let h = server.host.to_ascii_lowercase();
                        h.contains("eu") || h.contains("eapi")
                    });
                if is_eu {
                    output.push_str("hostname = eapi.pcloud.com\n");
                }
                push_oauth_credentials(&mut output, options);
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
                    // Swift `key` is the API key / OS_PASSWORD. rclone's swift
                    // backend does NOT mark this field as `IsPassword: true`, so
                    // it is used verbatim and must be emitted plain (same
                    // reasoning as S3 secret_access_key and Azure key). Emitting
                    // an obscured value makes rclone send the obscured string as
                    // the password, which fails authentication.
                    output.push_str(&format!("key = {}\n", pw));
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
                // rclone's backend type is `yandex` (not `yandexdisk`); the old
                // value produced a config rclone refused to load.
                output.push_str("type = yandex\n");
                push_oauth_credentials(&mut output, options);
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
            "backblaze" => {
                // AeroFTP's native Backblaze protocol maps to rclone's `b2`
                // backend: `account` (the key ID) + `key` (the application key).
                // `key` is NOT an rclone IsPassword field, so it is emitted plain
                // (same as S3 secret_access_key / Swift key). The bucket is part
                // of the remote path in rclone (`remote:bucket`), not a config
                // key, so it is intentionally not emitted here.
                output.push_str("type = b2\n");
                output.push_str(&format!("account = {}\n", server.username));
                if let Some(pw) = password.filter(|p| !p.is_empty()) {
                    output.push_str(&format!("key = {}\n", pw));
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

    /// Write `conf` to a uniquely-named temp file and return its path.
    fn tmp_write(conf: &str, name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, conf).expect("write temp conf");
        path
    }

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
    fn test_import_rclone_orders_remotes_alphabetically() {
        use std::io::Write;
        // Remotes deliberately listed out of order. parse_rclone_conf stores
        // them in a HashMap, so without an explicit sort the import surfaced
        // them in a randomized order. import_rclone must emit them sorted by
        // name, mirroring `rclone listremotes`.
        let conf = r#"
[zulu]
type = ftp
host = zulu.example.com
user = z

[mike]
type = ftp
host = mike.example.com
user = m

[alpha]
type = ftp
host = alpha.example.com
user = a

[tango]
type = ftp
host = tango.example.com
user = t
"#;
        let tmp = std::env::temp_dir().join("aeroftp-test-rclone-order.conf");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(conf.as_bytes()).unwrap();
        }
        let result = import_rclone(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "tango", "zulu"]);
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
    fn test_export_rclone_ftps_uses_explicit_tls_only() {
        // rclone treats `tls = true` as implicit FTPS and rejects it when
        // `explicit_tls = true` is also present. AeroFTP's `ftps` profiles are
        // explicit TLS on port 21, so export must set only `explicit_tls`.
        let servers = vec![RcloneExportServer {
            name: "secure-ftp".to_string(),
            host: "ftp.example.com".to_string(),
            port: 21,
            username: "alice".to_string(),
            protocol: Some("ftps".to_string()),
            options: None,
            provider_id: None,
        }];
        let mut passwords = HashMap::new();
        passwords.insert("secure-ftp".to_string(), "secret123".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-ftps.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("explicit_tls = true"),
            "explicit FTPS flag must be present:\n{conf}"
        );
        assert!(
            !conf.contains("\ntls = true\n"),
            "must not also enable implicit FTPS:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_s3_bucket_alias() {
        // F1 regression: a profile-pinned bucket must NOT be emitted as the
        // inert `bucket =` s3 key (silently ignored by rclone), but as an
        // `alias` remote that is bucket-relative, matching how the AeroFTP
        // S3 client writes keys.
        let servers = vec![RcloneExportServer {
            name: "minio".to_string(),
            host: "s3.lab.example.test".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "region": "us-east-1",
                "endpoint": "https://s3.lab.example.test",
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

    #[test]
    fn test_export_rclone_swift_key_is_plaintext() {
        // Bug (2026-06-07): swift `key` was emitted obscured, but rclone's
        // swift backend does NOT mark `key` as `IsPassword: true`, so it uses
        // the value verbatim. An obscured key was sent as the literal password
        // and authentication failed (HTTP 401). It must be emitted plain, like
        // S3 `secret_access_key` and Azure `key`. Verified against
        // `rclone config providers` (swift.key IsPassword = false).
        let servers = vec![RcloneExportServer {
            name: "blomp".to_string(),
            host: "authenticate.blomp.com".to_string(),
            port: 443,
            username: "user@example.com".to_string(),
            protocol: Some("swift".to_string()),
            options: Some(serde_json::json!({
                "endpoint": "https://authenticate.blomp.com",
                "tenant": "storage"
            })),
            provider_id: Some("blomp".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("blomp".to_string(), "148%BlomPass".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-swift.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("key = 148%BlomPass"),
            "swift key must be emitted plain, not obscured:\n{conf}"
        );
        // Round-trips back to the same plaintext on import.
        let p = std::env::temp_dir().join("aeroftp-test-export-swift2.conf");
        export_rclone(&servers, &passwords, &p).unwrap();
        let result = import_rclone(&p).unwrap();
        std::fs::remove_file(&p).ok();
        let swift = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("swift"))
            .expect("swift server present");
        assert_eq!(
            swift.credential.as_deref(),
            Some("148%BlomPass"),
            "swift key must round-trip as plaintext"
        );
    }

    #[test]
    fn test_export_rclone_filen_roundtrip() {
        // #128: a Filen profile exports to rclone's `filen` backend as
        // `email` + obscured `password` + obscured `api_key`, and imports back
        // to the same account (password as the credential, api_key relocated
        // into options.filen_api_key for the vault).
        let servers = vec![RcloneExportServer {
            name: "filen-acct".to_string(),
            host: "filen.io".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("filen".to_string()),
            options: Some(serde_json::json!({ "filen_api_key": "secret-cli-key" })),
            provider_id: Some("filen".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("filen-acct".to_string(), "S3cr3tPass!".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-filen.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = filen"), "missing type:\n{conf}");
        assert!(
            conf.contains("email = me@example.com"),
            "missing email:\n{conf}"
        );
        assert!(
            conf.contains("password = ") && !conf.contains("password = S3cr3tPass!"),
            "password must be present and obscured:\n{conf}"
        );
        assert!(
            conf.contains("api_key = ") && !conf.contains("api_key = secret-cli-key"),
            "api_key must be present and obscured:\n{conf}"
        );

        let result = import_rclone(&tmp_write(&conf, "aeroftp-test-import-filen.conf")).unwrap();
        let filen = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("filen"))
            .expect("filen server present");
        assert_eq!(filen.username, "me@example.com");
        assert_eq!(
            filen.credential.as_deref(),
            Some("S3cr3tPass!"),
            "password must round-trip"
        );
        assert_eq!(
            filen
                .options
                .as_ref()
                .and_then(|o| o.get("filen_api_key"))
                .and_then(|v| v.as_str()),
            Some("secret-cli-key"),
            "api_key must round-trip into options.filen_api_key"
        );
    }

    #[test]
    fn test_import_rclone_filen_reveals_real_rclone_obscured() {
        // The obscured values below were produced by the real rclone binary
        // (`rclone obscure`), so this pins our reveal codec to rclone's actual
        // output for the `filen` backend's IsPassword fields.
        let conf = "\
[filen-real]
type = filen
email = real@example.com
password = MohwAn6swmCQmBhRD-iaWciNBrBXM-ph2axM
api_key = 4BVmu-SCRQai2-0-hucKgbeyzH6-uqexma-skpRs4Kk
";
        let path = tmp_write(conf, "aeroftp-test-import-filen-real.conf");
        let result = import_rclone(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let filen = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("filen"))
            .expect("filen server present");
        assert_eq!(filen.username, "real@example.com");
        assert_eq!(filen.credential.as_deref(), Some("TestPass123"));
        assert_eq!(
            filen
                .options
                .as_ref()
                .and_then(|o| o.get("filen_api_key"))
                .and_then(|v| v.as_str()),
            Some("fake-api-key-abc"),
        );
    }

    #[test]
    fn test_export_rclone_backblaze_key_is_plaintext() {
        // AeroFTP's native Backblaze protocol exports to rclone's `b2` backend.
        // `key` is NOT an rclone IsPassword field, so it must be emitted plain
        // (verified against `rclone config providers`: b2.key IsPassword=false).
        let servers = vec![RcloneExportServer {
            name: "b2-acct".to_string(),
            host: "api.backblazeb2.com".to_string(),
            port: 443,
            username: "0011deadbeef".to_string(),
            protocol: Some("backblaze".to_string()),
            options: Some(serde_json::json!({ "bucket": "my-bucket" })),
            provider_id: Some("backblaze-native".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("b2-acct".to_string(), "K001abcdEFG키".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-b2.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = b2"), "missing type:\n{conf}");
        assert!(
            conf.contains("account = 0011deadbeef"),
            "missing account:\n{conf}"
        );
        assert!(
            conf.contains("key = K001abcdEFG키"),
            "b2 key must be emitted plain, not obscured:\n{conf}"
        );
    }

    /// #128-D: `aeroftp_token_to_rclone` is the exact inverse of
    /// `rclone_token_to_aeroftp`. An rclone token blob converted into AeroFTP's
    /// `StoredTokens` shape and back must preserve every field, so an exported
    /// remote carries the same credentials rclone produced.
    #[test]
    fn test_token_conversion_roundtrips_both_ways() {
        let rclone_blob = r#"{
            "access_token": "ya29.aShortLived",
            "token_type": "Bearer",
            "refresh_token": "1//04longLivedRefresh",
            "expiry": "2030-06-01T08:30:00Z"
        }"#;
        let aero = rclone_token_to_aeroftp(rclone_blob).expect("rclone -> aero");
        let back = aeroftp_token_to_rclone(&aero).expect("aero -> rclone");
        let parsed: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(parsed["access_token"], "ya29.aShortLived");
        assert_eq!(parsed["token_type"], "Bearer");
        assert_eq!(parsed["refresh_token"], "1//04longLivedRefresh");
        // 2030-06-01T08:30:00Z survives the Unix round-trip.
        assert_eq!(parsed["expiry"], "2030-06-01T08:30:00Z");
    }

    /// A `StoredTokens` value with no usable access token must not produce a
    /// token line (the export arm then emits the reconnect guidance instead).
    #[test]
    fn test_aeroftp_token_to_rclone_rejects_empty_access() {
        assert!(aeroftp_token_to_rclone(r#"{"access_token":""}"#).is_none());
        assert!(aeroftp_token_to_rclone(r#"{"refresh_token":"r"}"#).is_none());
    }

    /// #128-D: every OAuth-token provider exports a USABLE rclone remote when
    /// the export path injected the token + BYO client_id/secret, and the token
    /// re-imports to the same AeroFTP profile. Drive/Dropbox/OneDrive/Box/
    /// pCloud/Yandex share the same code path; the table pins each backend type.
    #[test]
    fn test_export_rclone_oauth_providers_roundtrip() {
        let cases = [
            ("googledrive", "drive"),
            ("dropbox", "dropbox"),
            ("onedrive", "onedrive"),
            ("box", "box"),
            ("pcloud", "pcloud"),
            ("yandexdisk", "yandex"),
        ];
        // The injected token is AeroFTP's StoredTokens shape (Unix expires_at).
        let stored_tokens = r#"{"access_token":"acc-123","refresh_token":"ref-456","expires_at":1893499200,"token_type":"Bearer","scopes":[]}"#;
        for (protocol, rclone_type) in cases {
            let servers = vec![RcloneExportServer {
                name: format!("{}-acct", protocol),
                host: "example.com".to_string(),
                port: 443,
                username: "me@example.com".to_string(),
                protocol: Some(protocol.to_string()),
                options: Some(serde_json::json!({
                    "__aeroftp_oauth_token": stored_tokens,
                    "__aeroftp_oauth_client_id": "client-id-xyz",
                    "__aeroftp_oauth_client_secret": "client-secret-xyz",
                })),
                provider_id: Some(protocol.to_string()),
            }];
            let passwords = HashMap::new();
            let tmp = std::env::temp_dir().join(format!("aeroftp-test-oauth-{}.conf", protocol));
            export_rclone(&servers, &passwords, &tmp).expect("should export");
            let conf = std::fs::read_to_string(&tmp).expect("read conf");
            std::fs::remove_file(&tmp).ok();

            assert!(
                conf.contains(&format!("type = {}", rclone_type)),
                "{protocol}: expected rclone type {rclone_type}:\n{conf}"
            );
            assert!(
                conf.contains("client_id = client-id-xyz"),
                "{protocol}: client_id must be emitted:\n{conf}"
            );
            assert!(
                conf.contains("client_secret = client-secret-xyz"),
                "{protocol}: client_secret must be emitted:\n{conf}"
            );
            assert!(
                conf.contains("token = ") && conf.contains("acc-123"),
                "{protocol}: token blob must be emitted:\n{conf}"
            );
            // The private injection keys must never leak into the config.
            assert!(
                !conf.contains("__aeroftp_oauth"),
                "{protocol}: private injection keys must not leak:\n{conf}"
            );

            // Re-import recovers the token onto the same protocol.
            let result = import_rclone(&tmp_write(
                &conf,
                &format!("aeroftp-reimport-{}.conf", protocol),
            ))
            .unwrap();
            let server = result
                .servers
                .iter()
                .find(|s| s.protocol.as_deref() == Some(protocol))
                .unwrap_or_else(|| panic!("{protocol}: server must re-import:\n{conf}"));
            let secrets = result
                .provider_secrets
                .get(&server.id)
                .unwrap_or_else(|| panic!("{protocol}: provider_secrets must carry the token"));
            let oauth = secrets.oauth.as_deref().expect("oauth blob present");
            let parsed: serde_json::Value = serde_json::from_str(oauth).unwrap();
            assert_eq!(parsed["access_token"], "acc-123", "{protocol}");
            assert_eq!(parsed["refresh_token"], "ref-456", "{protocol}");
            // BYO app credentials recovered for a fresh-device reconnect.
            assert_eq!(
                secrets.oauth_client_id.as_deref(),
                Some("client-id-xyz"),
                "{protocol}: client_id must round-trip"
            );
            assert_eq!(
                secrets.oauth_client_secret.as_deref(),
                Some("client-secret-xyz"),
                "{protocol}: client_secret must round-trip"
            );
        }
    }

    /// #128-D: pCloud's rclone `hostname` must be a real API host, not the
    /// display label AeroFTP stores in `host`. US omits it (rclone default
    /// api.pcloud.com); EU emits eapi.pcloud.com. Region may arrive via the
    /// host label or `options.region`.
    #[test]
    fn test_export_rclone_pcloud_region_hostname() {
        // US profile: host is a display label, no options.region -> no hostname.
        let us = vec![RcloneExportServer {
            name: "pcloud-us".to_string(),
            host: "pCloud (US)".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: None,
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-us.conf");
        export_rclone(&us, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("type = pcloud"), "type:\n{conf}");
        assert!(
            !conf.contains("hostname ="),
            "US must omit hostname (rclone defaults to api.pcloud.com):\n{conf}"
        );
        assert!(
            !conf.contains("pCloud (US)"),
            "the display label must never be emitted as a hostname:\n{conf}"
        );

        // EU via the host label.
        let eu = vec![RcloneExportServer {
            name: "pcloud-eu".to_string(),
            host: "pCloud (EU)".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: None,
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-eu.conf");
        export_rclone(&eu, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(
            conf.contains("hostname = eapi.pcloud.com"),
            "EU must emit eapi.pcloud.com:\n{conf}"
        );

        // EU via options.region, and the import maps the hostname back to region.
        let eu2 = vec![RcloneExportServer {
            name: "pcloud-eu2".to_string(),
            host: "anything".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("pcloud".to_string()),
            options: Some(serde_json::json!({ "region": "EU" })),
            provider_id: Some("pcloud".to_string()),
        }];
        let tmp = std::env::temp_dir().join("aeroftp-test-pcloud-eu2.conf");
        export_rclone(&eu2, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();
        assert!(conf.contains("hostname = eapi.pcloud.com"), "EU2:\n{conf}");
        let result = import_rclone(&tmp_write(&conf, "aeroftp-reimport-pcloud-eu.conf")).unwrap();
        let p = result
            .servers
            .iter()
            .find(|s| s.protocol.as_deref() == Some("pcloud"))
            .expect("pcloud server");
        assert_eq!(p.host, "eapi.pcloud.com", "host maps to EU API");
        assert_eq!(
            p.options
                .as_ref()
                .and_then(|o| o.get("region"))
                .and_then(|v| v.as_str()),
            Some("eu"),
            "imported region must be EU"
        );
    }

    /// Without injected secrets the OAuth arm must emit a `reconnect` guidance
    /// comment, never a half-formed `token =` line: no silently broken remote.
    #[test]
    fn test_export_rclone_oauth_without_secrets_emits_reconnect_comment() {
        let servers = vec![RcloneExportServer {
            name: "drive-noauth".to_string(),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: "me@example.com".to_string(),
            protocol: Some("googledrive".to_string()),
            options: None,
            provider_id: Some("googledrive".to_string()),
        }];
        let passwords = HashMap::new();
        let tmp = std::env::temp_dir().join("aeroftp-test-oauth-noauth.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(conf.contains("type = drive"), "type present:\n{conf}");
        assert!(
            !conf.contains("token = "),
            "must not emit a token line without credentials:\n{conf}"
        );
        assert!(
            conf.contains("rclone config reconnect"),
            "must emit reconnect guidance:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_nextcloud_appends_dav_path() {
        // Bug (2026-06-03): exporting a Nextcloud profile with a bare host
        // wrote `url = https://host` without the `/remote.php/dav/files/<user>/`
        // collection root rclone's nextcloud vendor requires, so the exported
        // remote could not list or transfer.
        let servers = vec![RcloneExportServer {
            name: "ncloud".to_string(),
            host: "https://cloud.lab.example.test".to_string(),
            port: 443,
            username: "alice".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("nextcloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ncloud".to_string(), "ncsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-nc.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = nextcloud"),
            "expected nextcloud vendor:\n{conf}"
        );
        assert!(
            conf.contains("url = https://cloud.lab.example.test/remote.php/dav/files/alice/"),
            "must synthesise the DAV collection root:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_owncloud_appends_dav_path() {
        let servers = vec![RcloneExportServer {
            name: "ocloud".to_string(),
            host: "https://oc.example.com".to_string(),
            port: 443,
            username: "bob".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("owncloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ocloud".to_string(), "ocsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-oc.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = owncloud"),
            "expected owncloud vendor:\n{conf}"
        );
        assert!(
            conf.contains("url = https://oc.example.com/remote.php/dav/files/bob/"),
            "must synthesise the DAV collection root:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_nextcloud_existing_dav_path_not_duplicated() {
        // A profile that already carries a `/remote.php/...` path (basePath or
        // explicit URL) must be honoured verbatim, never double-appended.
        let servers = vec![RcloneExportServer {
            name: "ncloud".to_string(),
            host: "https://cloud.lab.example.test".to_string(),
            port: 443,
            username: "alice".to_string(),
            protocol: Some("webdav".to_string()),
            options: Some(serde_json::json!({
                "basePath": "/remote.php/dav/files/alice/"
            })),
            provider_id: Some("nextcloud".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("ncloud".to_string(), "ncsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-nc-dup.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("url = https://cloud.lab.example.test/remote.php/dav/files/alice/"),
            "existing DAV path must be kept as-is:\n{conf}"
        );
        assert!(
            !conf.contains("dav/files/alice/remote.php"),
            "must not double-append the DAV path:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_webdav_generic_vendor_untouched() {
        // Generic WebDAV (vendor `other`: Koofr, SharePoint, Fastmail) has no
        // Nextcloud collection convention and must be left exactly as-is.
        let servers = vec![RcloneExportServer {
            name: "generic".to_string(),
            host: "https://dav.example.com".to_string(),
            port: 443,
            username: "carol".to_string(),
            protocol: Some("webdav".to_string()),
            options: None,
            provider_id: Some("webdav".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("generic".to_string(), "gsecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-webdav-generic.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("vendor = other"),
            "expected generic vendor=other:\n{conf}"
        );
        assert!(
            conf.contains("url = https://dav.example.com\n"),
            "generic WebDAV url must be untouched:\n{conf}"
        );
        assert!(
            !conf.contains("/remote.php/"),
            "must not inject a Nextcloud DAV path on generic WebDAV:\n{conf}"
        );
    }

    #[test]
    fn test_endpoint_is_loopback() {
        assert!(endpoint_is_loopback("https://127.0.0.1:1800"));
        assert!(endpoint_is_loopback("http://localhost:9000"));
        assert!(endpoint_is_loopback("https://127.0.0.1"));
        assert!(endpoint_is_loopback("https://[::1]:443"));
        assert!(endpoint_is_loopback("https://127.5.6.7:8000/path"));
        assert!(!endpoint_is_loopback("https://storage.googleapis.com"));
        assert!(!endpoint_is_loopback(
            "https://s3.eu-central-003.backblazeb2.com"
        ));
        // A bucket literally named "localhost" must not be misread: the host
        // here is the real endpoint, not the path segment.
        assert!(!endpoint_is_loopback("https://s3.example.com/localhost"));
    }

    #[test]
    fn test_export_rclone_s3_registry_endpoint_fallback() {
        // #3 regression: Google Cloud Storage S3-interop profiles store the
        // bucket but not the endpoint (it comes from the provider registry).
        // Before the fix the export emitted no endpoint and rclone fell back
        // to the unreachable AWS auto-region host. The export must now resolve
        // the preset endpoint just like the connect path.
        let servers = vec![RcloneExportServer {
            name: "gcs".to_string(),
            host: String::new(),
            port: 443,
            username: "GOOGTESTKEY".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "bucket": "gcsbucket" })),
            provider_id: Some("google-cloud-storage".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("gcs".to_string(), "gcssecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-gcs.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://storage.googleapis.com"),
            "must resolve the GCS preset endpoint:\n{conf}"
        );
        assert!(
            !conf.contains("amazonaws.com"),
            "must not leave rclone to fall back to the AWS auto-region:\n{conf}"
        );
        // Loopback guard must not trip for a public endpoint.
        assert!(
            !conf.contains("no_check_certificate"),
            "public endpoint must keep cert verification:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_s3_loopback_no_check_certificate() {
        // #2 regression: the Filen Desktop S3 bridge runs on a self-signed
        // loopback endpoint without IP SANs. rclone rejects it unless
        // `no_check_certificate` is set, mirroring AeroFTP's accept-invalid-
        // certs-on-loopback behaviour.
        let servers = vec![RcloneExportServer {
            name: "filen-s3".to_string(),
            host: String::new(),
            port: 1800,
            username: "FILENKEY".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "endpoint": "https://127.0.0.1:1800",
                "bucket": "filen",
                "verifyCert": false
            })),
            provider_id: Some("filen-desktop-s3".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("filen-s3".to_string(), "filensecret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-filen.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://127.0.0.1:1800"),
            "loopback endpoint must be preserved:\n{conf}"
        );
        assert!(
            conf.contains("no_check_certificate = true"),
            "loopback self-signed bridge must disable cert verification:\n{conf}"
        );
    }

    #[test]
    fn test_export_rclone_s3_public_endpoint_keeps_cert_check() {
        // Regression guard: a normal public S3 endpoint must keep certificate
        // verification and must not be rewritten.
        let servers = vec![RcloneExportServer {
            name: "aws".to_string(),
            host: "s3.amazonaws.com".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": "eu-west-1", "bucket": "mybucket" })),
            provider_id: Some("amazon-s3".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("aws".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join("aeroftp-test-export-s3-aws-public.conf");
        export_rclone(&servers, &passwords, &tmp).expect("should export");
        let conf = std::fs::read_to_string(&tmp).expect("read conf");
        std::fs::remove_file(&tmp).ok();

        assert!(
            conf.contains("endpoint = https://s3.amazonaws.com"),
            "host endpoint must be preserved:\n{conf}"
        );
        assert!(
            !conf.contains("no_check_certificate"),
            "public endpoint must keep cert verification:\n{conf}"
        );
    }
}
