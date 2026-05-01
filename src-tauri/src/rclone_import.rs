// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from rclone configuration files.
//!
//! Parses `rclone.conf` (INI format), maps rclone backend types to AeroFTP
//! ProviderType, and de-obfuscates rclone "obscured" passwords (AES-256-CTR
//! with a well-known key — NOT real encryption).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from rclone's reversible obfuscation to proper authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ rclone obscure: AES-256-CTR with published key ============
// Source: https://github.com/rclone/rclone/blob/master/fs/config/obscure/obscure.go
// This is NOT encryption — the key is public. We reveal it to store in our vault.

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

struct MappedProfile {
    protocol: String,
    provider_id: Option<String>,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
}

/// Map an rclone S3 `provider` field to our providerId.
fn map_s3_provider(provider: &str) -> &'static str {
    match provider.to_lowercase().as_str() {
        "aws" | "amazon" => "amazon-s3",
        "cloudflare" | "r2" => "cloudflare-r2",
        "digitalocean" | "digitaloceanspaces" => "digitalocean-spaces",
        "wasabi" => "wasabi",
        "backblaze" | "b2" => "backblaze-b2",
        "linode" | "linodeobjectstorage" => "linode-object-storage",
        "scaleway" => "scaleway",
        "stackpath" => "stackpath",
        "storj" => "storj",
        "idrive" | "idrivee2" => "idrive-e2",
        "minio" => "minio",
        "ceph" => "custom-s3",
        "arvancloud" => "custom-s3",
        "huaweiobs" | "obs" => "custom-s3",
        "tencentcos" | "cos" => "custom-s3",
        "alicloud" | "oss" => "custom-s3",
        "ibmcos" => "custom-s3",
        "ionos" => "ionos-s3",
        "petabox" => "custom-s3",
        "seaweedfs" => "custom-s3",
        "netease" => "custom-s3",
        "qiniu" => "custom-s3",
        _ => "custom-s3",
    }
}

/// Map an rclone WebDAV `vendor` field to our providerId.
fn map_webdav_vendor(vendor: &str) -> &'static str {
    match vendor.to_lowercase().as_str() {
        "nextcloud" => "nextcloud",
        "owncloud" => "owncloud",
        "sharepoint" | "sharepoint-ntlm" => "custom-webdav",
        "fastmail" => "custom-webdav",
        _ => "custom-webdav",
    }
}

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
            })
        }

        // ---- S3 ----
        "s3" => {
            let s3_provider = get_str("provider").unwrap_or("Other");
            let provider_id = map_s3_provider(s3_provider);
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
                // Generic S3 — need endpoint
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
            })
        }

        // ---- WebDAV ----
        "webdav" => {
            let url = get_str("url").unwrap_or("").to_string();
            if url.is_empty() {
                return None;
            }
            let vendor = get_str("vendor").unwrap_or("other");
            let provider_id = map_webdav_vendor(vendor);

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
            })
        }

        // ---- Google Drive ----
        "drive" => Some(MappedProfile {
            protocol: "googledrive".to_string(),
            provider_id: Some("googledrive".to_string()),
            host: "www.googleapis.com".to_string(),
            port: 443,
            username: name.to_string(), // rclone doesn't store email for drive
            password: None,             // OAuth — token not importable
            options: None,
            initial_path: get_str("root_folder_id").map(|s| s.to_string()),
        }),

        // ---- Dropbox ----
        "dropbox" => Some(MappedProfile {
            protocol: "dropbox".to_string(),
            provider_id: Some("dropbox".to_string()),
            host: "api.dropboxapi.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
        }),

        // ---- OneDrive ----
        "onedrive" => Some(MappedProfile {
            protocol: "onedrive".to_string(),
            provider_id: Some("onedrive".to_string()),
            host: "graph.microsoft.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
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
        }),

        // ---- Box ----
        "box" => Some(MappedProfile {
            protocol: "box".to_string(),
            provider_id: Some("box".to_string()),
            host: "api.box.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
        }),

        // ---- pCloud ----
        "pcloud" => Some(MappedProfile {
            protocol: "pcloud".to_string(),
            provider_id: Some("pcloud".to_string()),
            host: get_str("hostname").unwrap_or("eapi.pcloud.com").to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
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
            })
        }

        // ---- Yandex Disk ----
        "yandexdisk" => Some(MappedProfile {
            protocol: "yandexdisk".to_string(),
            provider_id: Some("yandex-disk".to_string()),
            host: "webdav.yandex.ru".to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
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
        }),

        // ---- Jottacloud ----
        "jottacloud" => Some(MappedProfile {
            protocol: "jottacloud".to_string(),
            provider_id: Some("jottacloud".to_string()),
            host: "jottacloud.com".to_string(),
            port: 443,
            username: name.to_string(),
            password: None, // OAuth
            options: None,
            initial_path: None,
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
        }),

        // Unsupported rclone types — skip gracefully
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
        options.insert("rcloneCryptPassword2".into(), serde_json::Value::String(pw2));
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
                    &uuid_v4()[..8]
                );

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
    })
}

/// Simple UUID v4 generator (avoid pulling uuid crate just for this).
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let random_bytes = crate::crypto::random_bytes(16);

    // Set version (4) and variant (10xx) bits
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&random_bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx

    // Mix in time for extra entropy
    let time_bytes = (seed as u64).to_le_bytes();
    for (i, &b) in time_bytes.iter().enumerate() {
        bytes[i] ^= b;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // re-set version after XOR
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // re-set variant after XOR

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

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

        // Sanitize remote name: rclone uses [name] as INI section, no special chars
        let remote_name = server
            .name
            .replace(['[', ']', '\n', '\r'], "-")
            .trim()
            .to_string();
        if remote_name.is_empty() {
            continue;
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
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "pass = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
            }
            "s3" => {
                output.push_str("type = s3\n");
                let provider_id = server.provider_id.as_deref().unwrap_or("custom-s3");
                let rclone_provider = match provider_id {
                    "amazon-s3" => "AWS",
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
                    _ => "Other",
                };
                output.push_str(&format!("provider = {}\n", rclone_provider));
                output.push_str(&format!("access_key_id = {}\n", server.username));
                if let Some(pw) = password {
                    output.push_str(&format!(
                        "secret_access_key = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
                }
                if let Some(opts) = options {
                    if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                        output.push_str(&format!("region = {}\n", region));
                    }
                    if let Some(endpoint) = opts.get("endpoint").and_then(|v| v.as_str()) {
                        output.push_str(&format!("endpoint = {}\n", endpoint));
                    }
                    if let Some(bucket) = opts.get("bucket").and_then(|v| v.as_str()) {
                        output.push_str(&format!("bucket = {}\n", bucket));
                    }
                }
            }
            "webdav" => {
                output.push_str("type = webdav\n");
                let vendor = match server.provider_id.as_deref() {
                    Some("nextcloud") => "nextcloud",
                    Some("owncloud") => "owncloud",
                    _ => "other",
                };
                let base_path = options
                    .and_then(|o| o.get("basePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let scheme = if server.port == 80 { "http" } else { "https" };
                let port_str = if server.port == 443 || server.port == 80 {
                    String::new()
                } else {
                    format!(":{}", server.port)
                };
                output.push_str(&format!(
                    "url = {}://{}{}{}\n",
                    scheme, server.host, port_str, base_path
                ));
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
                    output.push_str(&format!(
                        "key = {}\n",
                        obscure_password(pw).unwrap_or_default()
                    ));
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
            // Protocols without rclone equivalent — skip
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
        assert_eq!(obj.get("rcloneCryptEnabled").and_then(|v| v.as_bool()), Some(true));
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
}
