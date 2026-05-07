//! Shared types for storage providers
//!
//! This module contains all shared types used across different storage providers,
//! including configuration structs, file entry representations, and error types.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Supported storage provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    /// Standard FTP (File Transfer Protocol)
    Ftp,
    /// FTP over TLS/SSL
    Ftps,
    /// SSH File Transfer Protocol
    Sftp,
    /// WebDAV (Web Distributed Authoring and Versioning)
    WebDav,
    /// Amazon S3 and S3-compatible storage
    S3,
    /// AeroCloud - Personal multi-protocol cloud sync
    AeroCloud,
    /// Google Drive (OAuth2)
    GoogleDrive,
    /// Dropbox (OAuth2)
    Dropbox,
    /// Microsoft OneDrive (OAuth2)
    OneDrive,
    /// MEGA.nz Cloud Storage
    Mega,
    /// Box Cloud Storage (OAuth2)
    Box,
    /// pCloud (OAuth2)
    PCloud,
    /// Azure Blob Storage
    Azure,
    /// Filen.io (E2E Encrypted)
    Filen,
    /// 4shared (OAuth 1.0)
    FourShared,
    /// Zoho WorkDrive (OAuth2)
    ZohoWorkdrive,
    /// Internxt Drive (E2E Encrypted)
    Internxt,
    /// Infomaniak kDrive (Swiss Cloud)
    KDrive,
    /// Jottacloud (Norwegian Secure Cloud)
    Jottacloud,
    /// Drime Cloud (20GB Secure Cloud)
    DrimeCloud,
    /// FileLu Cloud Storage (API key authentication)
    FileLu,
    /// Koofr Cloud Storage (European, 10 GB free)
    Koofr,
    /// OpenDrive Cloud Storage
    OpenDrive,
    /// Yandex Disk (Russian cloud, 5 GB free)
    YandexDisk,
    /// GitHub (Repository & Releases browser)
    GitHub,
    /// GitLab (Repository browser, REST API v4)
    GitLab,
    /// OpenStack Swift Object Storage (Blomp, OVH, Rackspace)
    Swift,
    /// Google Photos (OAuth2, Photos Library API v1)
    GooglePhotos,
    /// Immich (Self-hosted photo/video management, API key auth)
    Immich,
    /// ImageKit (media CDN + DAM filesystem API, private key auth)
    ImageKit,
    /// Uploadcare (EU media management, public key + secret key auth)
    Uploadcare,
    /// Backblaze B2 Cloud Storage (native API, applicationKeyId + applicationKey)
    Backblaze,
    /// Cloudinary (media management CDN, REST API, api_key + api_secret)
    /// Free tier: 25 monthly credits (1 credit = 1 GB storage OR 1 GB bandwidth
    /// OR 1000 transformations).
    Cloudinary,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::Ftp => write!(f, "FTP"),
            ProviderType::Ftps => write!(f, "FTPS"),
            ProviderType::Sftp => write!(f, "SFTP"),
            ProviderType::WebDav => write!(f, "WebDAV"),
            ProviderType::S3 => write!(f, "S3"),
            ProviderType::AeroCloud => write!(f, "AeroCloud"),
            ProviderType::GoogleDrive => write!(f, "Google Drive"),
            ProviderType::Dropbox => write!(f, "Dropbox"),
            ProviderType::OneDrive => write!(f, "OneDrive"),
            ProviderType::Mega => write!(f, "MEGA"),
            ProviderType::Box => write!(f, "Box"),
            ProviderType::PCloud => write!(f, "pCloud"),
            ProviderType::Azure => write!(f, "Azure Blob"),
            ProviderType::Filen => write!(f, "Filen"),
            ProviderType::FourShared => write!(f, "4shared"),
            ProviderType::ZohoWorkdrive => write!(f, "Zoho WorkDrive"),
            ProviderType::Internxt => write!(f, "Internxt Drive"),
            ProviderType::KDrive => write!(f, "kDrive"),
            ProviderType::Jottacloud => write!(f, "Jottacloud"),
            ProviderType::DrimeCloud => write!(f, "Drime Cloud"),
            ProviderType::FileLu => write!(f, "FileLu"),
            ProviderType::Koofr => write!(f, "Koofr"),
            ProviderType::OpenDrive => write!(f, "OpenDrive"),
            ProviderType::YandexDisk => write!(f, "Yandex Disk"),
            ProviderType::GitHub => write!(f, "GitHub"),
            ProviderType::GitLab => write!(f, "GitLab"),
            ProviderType::Swift => write!(f, "Swift"),
            ProviderType::GooglePhotos => write!(f, "Google Photos"),
            ProviderType::Immich => write!(f, "Immich"),
            ProviderType::ImageKit => write!(f, "ImageKit"),
            ProviderType::Uploadcare => write!(f, "Uploadcare"),
            ProviderType::Backblaze => write!(f, "Backblaze B2"),
            ProviderType::Cloudinary => write!(f, "Cloudinary"),
        }
    }
}

impl ProviderType {
    /// Get default port for this provider type
    pub fn default_port(&self) -> u16 {
        match self {
            ProviderType::Ftp => 21,
            ProviderType::Ftps => 990,
            ProviderType::Sftp => 22,
            ProviderType::WebDav => 443,
            ProviderType::S3 => 443,
            ProviderType::AeroCloud => 21, // Uses FTP in background
            ProviderType::GoogleDrive => 443,
            ProviderType::Dropbox => 443,
            ProviderType::OneDrive => 443,
            ProviderType::Mega => 443,
            ProviderType::Box => 443,
            ProviderType::PCloud => 443,
            ProviderType::Azure => 443,
            ProviderType::Filen => 443,
            ProviderType::FourShared => 443,
            ProviderType::ZohoWorkdrive => 443,
            ProviderType::Internxt => 443,
            ProviderType::KDrive => 443,
            ProviderType::Jottacloud => 443,
            ProviderType::DrimeCloud => 443,
            ProviderType::FileLu => 443,
            ProviderType::Koofr => 443,
            ProviderType::OpenDrive => 443,
            ProviderType::YandexDisk => 443,
            ProviderType::GitHub => 443,
            ProviderType::GitLab => 443,
            ProviderType::Swift => 443,
            ProviderType::GooglePhotos => 443,
            ProviderType::Immich => 2283,
            ProviderType::ImageKit => 443,
            ProviderType::Uploadcare => 443,
            ProviderType::Backblaze => 443,
            ProviderType::Cloudinary => 443,
        }
    }

    /// Check if this provider uses encryption by default
    #[allow(dead_code)]
    pub fn uses_encryption(&self) -> bool {
        matches!(
            self,
            ProviderType::Ftps |
            ProviderType::Sftp |
            ProviderType::WebDav |
            ProviderType::S3 |
            ProviderType::AeroCloud |  // AeroCloud recommends FTPS
            ProviderType::GoogleDrive |
            ProviderType::Dropbox |
            ProviderType::OneDrive |
            ProviderType::Mega |
            ProviderType::Box |
            ProviderType::PCloud |
            ProviderType::Azure |
            ProviderType::Filen |
            ProviderType::FourShared |
            ProviderType::ZohoWorkdrive |
            ProviderType::Internxt |
            ProviderType::KDrive |
            ProviderType::Jottacloud |
            ProviderType::DrimeCloud |
            ProviderType::FileLu |
            ProviderType::Koofr |
            ProviderType::OpenDrive |
            ProviderType::YandexDisk |
            ProviderType::GitHub |
            ProviderType::GitLab |
            ProviderType::Swift |
            ProviderType::GooglePhotos |
            ProviderType::Immich |
            ProviderType::ImageKit |
            ProviderType::Uploadcare |
            ProviderType::Backblaze |
            ProviderType::Cloudinary
        )
    }

    /// Check if this provider requires OAuth2 authentication
    #[allow(dead_code)]
    pub fn requires_oauth2(&self) -> bool {
        matches!(
            self,
            ProviderType::GoogleDrive
                | ProviderType::Dropbox
                | ProviderType::OneDrive
                | ProviderType::Box
                | ProviderType::PCloud
                | ProviderType::ZohoWorkdrive
                | ProviderType::GooglePhotos
        )
    }

    /// Check if this is an AeroCloud provider (uses FTP backend with sync)
    #[allow(dead_code)]
    pub fn is_aerocloud(&self) -> bool {
        matches!(self, ProviderType::AeroCloud)
    }
}

/// Generic provider configuration
///
/// This struct can be used to configure any provider type.
/// Provider-specific fields are stored in the `extra` HashMap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Display name for this connection
    pub name: String,
    /// Provider type
    pub provider_type: ProviderType,
    /// Host/endpoint URL
    pub host: String,
    /// Port number (uses default if None)
    pub port: Option<u16>,
    /// Username for authentication
    pub username: Option<String>,
    /// Password for authentication
    pub password: Option<String>,
    /// Initial path to navigate to after connection
    pub initial_path: Option<String>,
    /// Extra provider-specific options
    #[serde(default)]
    pub extra: std::collections::HashMap<String, String>,
}

impl ProviderConfig {
    /// Get the effective port (default or specified)
    pub fn effective_port(&self) -> u16 {
        self.port
            .unwrap_or_else(|| self.provider_type.default_port())
    }
}

impl ProviderConfig {
    /// A3-05: Explicitly zeroize the password field to reduce credential exposure in memory.
    /// Call this after the password has been consumed (e.g., converted to SecretString).
    /// Cannot use `impl Drop` because ProviderConfig derives Clone and uses partial moves
    /// across 20+ provider modules: Drop trait would break `..config.clone()` patterns.
    /// TODO: migrate `password: Option<String>` to `Option<SecretString>` for automatic zeroization.
    pub fn zeroize_password(&mut self) {
        use zeroize::Zeroize;
        if let Some(ref mut pwd) = self.password {
            pwd.zeroize();
        }
    }
}

/// TLS mode for FTP connections
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FtpTlsMode {
    /// Plain FTP (no encryption)
    #[default]
    None,
    /// Explicit TLS (AUTH TLS on port 21) - required
    Explicit,
    /// Implicit TLS (direct TLS on port 990)
    Implicit,
    /// Try explicit TLS, fall back to plain if unsupported
    ExplicitIfAvailable,
}

/// FTP-specific configuration
#[derive(Debug, Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: secrecy::SecretString,
    pub tls_mode: FtpTlsMode,
    pub verify_cert: bool,
    pub initial_path: Option<String>,
}

impl FtpConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let tls_mode = config
            .extra
            .get("tls_mode")
            .map(|v| match v.as_str() {
                "explicit" => FtpTlsMode::Explicit,
                "implicit" => FtpTlsMode::Implicit,
                "explicit_if_available" => FtpTlsMode::ExplicitIfAvailable,
                _ => FtpTlsMode::None,
            })
            .unwrap_or_else(|| {
                if config.provider_type == ProviderType::Ftps {
                    FtpTlsMode::Implicit
                } else {
                    FtpTlsMode::None
                }
            });

        let verify_cert = config
            .extra
            .get("verify_cert")
            .map(|v| v != "false")
            .unwrap_or(true);

        Ok(Self {
            host: config.host.clone(),
            port: config.effective_port(),
            username: config
                .username
                .clone()
                .unwrap_or_else(|| "anonymous".to_string()),
            password: secrecy::SecretString::from(config.password.clone().unwrap_or_default()),
            tls_mode,
            verify_cert,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// WebDAV-specific configuration
#[derive(Debug, Clone)]
pub struct WebDavConfig {
    /// Full URL to WebDAV endpoint (e.g., https://cloud.example.com/remote.php/dav/files/user/)
    pub url: String,
    pub username: String,
    pub password: secrecy::SecretString,
    pub initial_path: Option<String>,
    /// Whether to verify TLS certificates (default: true). Set to false for self-signed certs.
    pub verify_cert: bool,
    /// Whether requests should omit Authorization headers.
    pub anonymous: bool,
}

impl WebDavConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        // Build WebDAV URL from host. Three knobs decide the scheme, in order:
        //   1. Explicit scheme prefix on `host` (`http://...` / `https://...`)
        //      always wins.
        //   2. `extra["tls_mode"]` ("http", "https", "auto"). Default "auto".
        //   3. Auto rules: port 443 → https, port 80 → http, otherwise infer
        //      from the host: localhost / 127.0.0.1 / RFC1918 / `*.local` /
        //      Filen Desktop hostnames default to http (their local servers
        //      don't speak TLS by default), everything else falls back to
        //      https. This keeps the public-internet default safe but unblocks
        //      local network drives such as Filen Desktop's WebDAV bridge
        //      (port 1900) and MEGAcmd (port 4443).
        let port = config.effective_port();
        let tls_mode = config
            .extra
            .get("tls_mode")
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(|| "auto".to_string());

        let resolved_scheme = match tls_mode.as_str() {
            "http" => "http",
            "https" => "https",
            _ => match port {
                443 => "https",
                80 => "http",
                _ => {
                    if Self::is_local_or_filen_host(&config.host) {
                        "http"
                    } else {
                        "https"
                    }
                }
            },
        };

        // Suppress port suffix when it matches the implied default for the
        // resolved scheme so URLs stay clean for traditional WebDAV servers.
        let port_suffix = match (resolved_scheme, port) {
            ("https", 443) | ("http", 80) => String::new(),
            _ => format!(":{}", port),
        };

        let raw_url = if config.host.starts_with("http://") || config.host.starts_with("https://") {
            config.host.clone()
        } else {
            format!("{}://{}{}", resolved_scheme, config.host, port_suffix)
        };

        // Resolve {username} template in URL (used by CloudMe, Nextcloud presets)
        let username = config.username.clone().unwrap_or_default();
        let url = raw_url.replace("{username}", &username);

        let verify_cert = config
            .extra
            .get("verify_cert")
            .map(|v| v != "false")
            .unwrap_or(true);
        let anonymous = config
            .extra
            .get("anonymous")
            .map(|v| v == "true")
            .unwrap_or(false);

        Ok(Self {
            url,
            username,
            password: secrecy::SecretString::from(config.password.clone().unwrap_or_default()),
            initial_path: config.initial_path.clone(),
            verify_cert,
            anonymous,
        })
    }

    /// Returns true when the host targets a local-only WebDAV bridge (Filen
    /// Desktop, MEGAcmd, OpenDrive Desktop, custom localhost gateways) so we
    /// know to default to HTTP on non-standard ports. The decision is based
    /// purely on the host string: no DNS lookup, no IP stack heuristic.
    pub(crate) fn is_local_or_filen_host(host: &str) -> bool {
        let h = host
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches('[');
        // Strip user info, brackets, and port. We intentionally do not use
        // `url::Url::parse` because raw `host:port` isn't a valid URL.
        let host_only = h
            .split('@')
            .next_back()
            .unwrap_or(h)
            .split('/')
            .next()
            .unwrap_or(h);
        let host_only = host_only.trim_end_matches(']');
        let host_only = match host_only.rsplit_once(':') {
            Some((before, after)) if after.chars().all(|c| c.is_ascii_digit()) => before,
            _ => host_only,
        };
        let lower = host_only.to_ascii_lowercase();
        if lower == "localhost"
            || lower == "127.0.0.1"
            || lower == "::1"
            || lower.ends_with(".localhost")
            || lower.ends_with(".local")
            || lower == "local.webdav.filen.io"
            || lower == "local.s3.filen.io"
        {
            return true;
        }
        // RFC 1918 / link-local IPv4: 10/8, 172.16/12, 192.168/16, 169.254/16
        if let Some(first) = lower.split('.').next().and_then(|s| s.parse::<u8>().ok()) {
            if first == 10 {
                return true;
            }
            let parts: Vec<u8> = lower
                .split('.')
                .filter_map(|s| s.parse::<u8>().ok())
                .collect();
            if parts.len() == 4 {
                if parts[0] == 192 && parts[1] == 168 {
                    return true;
                }
                if parts[0] == 169 && parts[1] == 254 {
                    return true;
                }
                if parts[0] == 172 && (16..=31).contains(&parts[1]) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod webdav_config_tests {
    use super::*;

    fn build(host: &str, port: Option<u16>, tls_mode: Option<&str>) -> WebDavConfig {
        let mut extra = std::collections::HashMap::new();
        if let Some(m) = tls_mode {
            extra.insert("tls_mode".to_string(), m.to_string());
        }
        let cfg = ProviderConfig {
            name: "test".to_string(),
            provider_type: ProviderType::WebDav,
            host: host.to_string(),
            port,
            username: Some("u".to_string()),
            password: Some("p".to_string()),
            initial_path: None,
            extra,
        };
        WebDavConfig::from_provider_config(&cfg).unwrap()
    }

    #[test]
    fn auto_picks_http_for_filen_local_hostname() {
        let cfg = build("local.webdav.filen.io", Some(1900), None);
        assert_eq!(cfg.url, "http://local.webdav.filen.io:1900");
    }

    #[test]
    fn auto_picks_http_for_localhost_with_custom_port() {
        let cfg = build("127.0.0.1", Some(1900), None);
        assert_eq!(cfg.url, "http://127.0.0.1:1900");
        let cfg = build("localhost", Some(4443), None);
        assert_eq!(cfg.url, "http://localhost:4443");
    }

    #[test]
    fn auto_picks_http_for_rfc1918_with_custom_port() {
        let cfg = build("192.168.1.50", Some(8080), None);
        assert_eq!(cfg.url, "http://192.168.1.50:8080");
        let cfg = build("10.0.0.5", Some(1900), None);
        assert_eq!(cfg.url, "http://10.0.0.5:1900");
        let cfg = build("172.20.10.1", Some(8443), None);
        assert_eq!(cfg.url, "http://172.20.10.1:8443");
    }

    #[test]
    fn auto_picks_https_for_public_host_with_custom_port() {
        let cfg = build("cloud.example.com", Some(8443), None);
        assert_eq!(cfg.url, "https://cloud.example.com:8443");
    }

    #[test]
    fn explicit_tls_mode_overrides_auto() {
        let cfg = build("cloud.example.com", Some(80), Some("https"));
        assert_eq!(cfg.url, "https://cloud.example.com:80");
        let cfg = build("local.webdav.filen.io", Some(1900), Some("https"));
        assert_eq!(cfg.url, "https://local.webdav.filen.io:1900");
        let cfg = build("cloud.example.com", Some(443), Some("http"));
        assert_eq!(cfg.url, "http://cloud.example.com:443");
    }

    #[test]
    fn explicit_scheme_in_host_wins_over_everything() {
        let cfg = build("https://cloud.example.com:1234", Some(80), Some("http"));
        assert_eq!(cfg.url, "https://cloud.example.com:1234");
    }

    #[test]
    fn standard_ports_omit_port_suffix() {
        let cfg = build("cloud.example.com", Some(443), None);
        assert_eq!(cfg.url, "https://cloud.example.com");
        let cfg = build("intranet.local", Some(80), None);
        assert_eq!(cfg.url, "http://intranet.local");
    }
}

/// S3-specific configuration
#[derive(Debug, Clone)]
pub struct S3Config {
    /// S3-compatible endpoint URL (empty for AWS S3)
    pub endpoint: Option<String>,
    /// AWS region (e.g., us-east-1)
    pub region: String,
    /// Access key ID
    pub access_key_id: String,
    /// Secret access key (SecretString for memory zeroization)
    pub secret_access_key: secrecy::SecretString,
    /// Bucket name
    pub bucket: String,
    /// Path prefix within bucket
    pub prefix: Option<String>,
    /// Use path-style addressing (for MinIO, etc.)
    pub path_style: bool,
    /// Default storage class for uploads (e.g., STANDARD, STANDARD_IA, GLACIER_IR)
    pub storage_class: Option<String>,
    /// Server-side encryption mode (AES256 = SSE-S3, aws:kms = SSE-KMS)
    pub sse_mode: Option<String>,
    /// KMS key ID for SSE-KMS (optional, uses default AWS-managed key if absent)
    pub sse_kms_key_id: Option<String>,
}

impl S3Config {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let bucket = config
            .extra
            .get("bucket")
            .ok_or_else(|| ProviderError::InvalidConfig("S3 bucket name is required".to_string()))?
            .trim()
            .to_string();

        let region = config
            .extra
            .get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string())
            .trim()
            .to_string();

        let explicit_endpoint = config
            .extra
            .get("endpoint")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let endpoint_raw = explicit_endpoint.or_else(|| {
            if !config.host.is_empty() && config.host != "s3.amazonaws.com" {
                Some(config.host.trim().to_string())
            } else {
                None
            }
        });
        tracing::debug!(
            "S3Config: host={:?}, port={:?}, extra_endpoint={:?}",
            config.host,
            config.port,
            config.extra.get("endpoint")
        );
        let endpoint = endpoint_raw.map(|host| normalize_s3_endpoint(&host, config.port));
        if endpoint
            .as_deref()
            .map(|ep| ep.to_ascii_lowercase().contains("s4.mega.io"))
            .unwrap_or(false)
        {
            const MEGA_S4_REGIONS: &[&str] =
                &["eu-central-1", "eu-central-2", "ca-central-1", "ca-west-1"];
            if !MEGA_S4_REGIONS.contains(&region.as_str()) {
                return Err(ProviderError::InvalidConfig(format!(
                    "Invalid MEGA S4 region '{}'. Supported regions: {}",
                    region,
                    MEGA_S4_REGIONS.join(", ")
                )));
            }
        }

        let path_style = config
            .extra
            .get("path_style")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(endpoint.is_some()); // Default to path style for custom endpoints

        let storage_class = config
            .extra
            .get("storage_class")
            .cloned()
            .filter(|s| !s.is_empty());
        let sse_mode = config
            .extra
            .get("sse_mode")
            .cloned()
            .filter(|s| !s.is_empty());
        let sse_kms_key_id = config
            .extra
            .get("sse_kms_key_id")
            .cloned()
            .filter(|s| !s.is_empty());

        Ok(Self {
            endpoint,
            region,
            access_key_id: config.username.clone().unwrap_or_default(),
            secret_access_key: secrecy::SecretString::from(
                config.password.clone().unwrap_or_default(),
            ),
            bucket,
            prefix: config.initial_path.clone(),
            path_style,
            storage_class,
            sse_mode,
            sse_kms_key_id,
        })
    }
}

fn normalize_s3_endpoint(endpoint: &str, configured_port: Option<u16>) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if let Ok(mut url) = url::Url::parse(trimmed) {
            if url.port().is_none() {
                if let Some(port) = configured_port {
                    let default_port = match url.scheme() {
                        "http" => Some(80),
                        "https" => Some(443),
                        _ => None,
                    };
                    if Some(port) != default_port {
                        let _ = url.set_port(Some(port));
                    }
                }
            }
            return url.to_string().trim_end_matches('/').to_string();
        }

        return trimmed.to_string();
    }

    let parsed_authority = url::Url::parse(&format!("http://{trimmed}")).ok();
    let explicit_port = parsed_authority.as_ref().and_then(|url| url.port());
    let host = parsed_authority
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or(trimmed);
    let port = explicit_port.or(configured_port);
    let scheme = infer_s3_endpoint_scheme(host, port);
    let default_port = match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };

    match configured_port {
        Some(port) if explicit_port.is_none() && Some(port) != default_port => {
            format!("{scheme}://{trimmed}:{port}")
        }
        _ => format!("{scheme}://{trimmed}"),
    }
}

fn infer_s3_endpoint_scheme(host: &str, port: Option<u16>) -> &'static str {
    match port {
        Some(80) | Some(3900) | Some(9000) | Some(9001) | Some(9002) => "http",
        Some(443) => "https",
        Some(_) if is_loopback_host(host) => "http",
        _ => "https",
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase();
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// SFTP-specific configuration
#[derive(Debug, Clone)]
pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Password authentication (optional if using key)
    pub password: Option<secrecy::SecretString>,
    /// Path to private key file (e.g., ~/.ssh/id_rsa)
    pub private_key_path: Option<String>,
    /// Passphrase for encrypted private key
    pub key_passphrase: Option<secrecy::SecretString>,
    /// Initial directory to navigate to
    pub initial_path: Option<String>,
    /// Connection timeout in seconds
    pub timeout_secs: u64,
    /// CLI mode: auto-accept unknown host keys and save to known_hosts
    pub trust_unknown_hosts: bool,
}

impl SftpConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let username = config.username.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Username required for SFTP".to_string())
        })?;

        let private_key_path = config.extra.get("private_key_path").cloned();
        let key_passphrase = config
            .extra
            .get("key_passphrase")
            .map(|v| secrecy::SecretString::from(v.clone()));

        let timeout_secs = config
            .extra
            .get("timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        let trust_unknown_hosts = config
            .extra
            .get("trust_unknown_hosts")
            .map(|v| v == "true")
            .unwrap_or(false);

        Ok(Self {
            host: config.host.clone(),
            port: config.effective_port(),
            username,
            password: config.password.clone().map(secrecy::SecretString::from),
            private_key_path,
            key_passphrase,
            initial_path: config.initial_path.clone(),
            timeout_secs,
            trust_unknown_hosts,
        })
    }
}

/// MEGA configuration
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MegaConnectionMode {
    Native,
    #[default]
    MegaCmd,
}

#[derive(Debug, Clone)]
pub struct MegaConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// 6-digit TOTP for accounts with two-factor authentication enabled.
    /// MEGA's `us` (login) command accepts an optional `mfa` field; we forward
    /// it when set and let the server reject with E_MFAREQUIRED / E_FAILED if
    /// the code is missing or wrong. Single-use, never persisted in profile
    /// options after a successful connect.
    pub two_factor_code: Option<String>,
    /// Whether to save session for reconnection (used in future session persistence)
    #[allow(dead_code)]
    pub save_session: bool,
    pub logout_on_disconnect: Option<bool>,
    pub connection_mode: MegaConnectionMode,
}

impl MegaConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email required for MEGA".to_string()))?;

        let password = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Password required for MEGA".to_string())
        })?;

        let save_session = config
            .extra
            .get("save_session")
            .map(|v| v == "true")
            .unwrap_or(true);

        let logout_on_disconnect = config
            .extra
            .get("logout_on_disconnect")
            .map(|v| v == "true");

        // Compatibility fallback: profiles created before the MEGA backend selector
        // should continue using MEGAcmd until the native path is fully implemented.
        let connection_mode = match config
            .extra
            .get("mega_mode")
            .or_else(|| config.extra.get("connection_mode"))
            .map(String::as_str)
        {
            Some("native") => MegaConnectionMode::Native,
            Some("megacmd") => MegaConnectionMode::MegaCmd,
            Some(other) => {
                return Err(ProviderError::InvalidConfig(format!(
                    "Invalid MEGA connection mode: {}",
                    other
                )));
            }
            None => MegaConnectionMode::MegaCmd,
        };

        let two_factor_code = config
            .extra
            .get("two_factor_code")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(Self {
            email,
            password: password.into(),
            two_factor_code,
            save_session,
            logout_on_disconnect,
            connection_mode,
        })
    }
}

/// Box configuration
#[derive(Debug, Clone)]
pub struct BoxConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl BoxConfig {
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config
            .extra
            .get("client_id")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing client_id for Box".to_string()))?;
        let client_secret = config.extra.get("client_secret").ok_or_else(|| {
            ProviderError::InvalidConfig("Missing client_secret for Box".to_string())
        })?;
        Ok(Self::new(client_id, client_secret))
    }
}

/// pCloud configuration
#[derive(Debug, Clone)]
pub struct PCloudConfig {
    pub client_id: String,
    pub client_secret: String,
    /// API region: "us" or "eu"
    pub region: String,
}

impl PCloudConfig {
    pub fn new(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            region: region.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config.extra.get("client_id").ok_or_else(|| {
            ProviderError::InvalidConfig("Missing client_id for pCloud".to_string())
        })?;
        let client_secret = config.extra.get("client_secret").ok_or_else(|| {
            ProviderError::InvalidConfig("Missing client_secret for pCloud".to_string())
        })?;
        let region = config
            .extra
            .get("region")
            .cloned()
            .unwrap_or_else(|| "us".to_string());
        Ok(Self::new(client_id, client_secret, &region))
    }

    /// Get the API base URL for this region
    pub fn api_base(&self) -> &str {
        if self.region == "eu" {
            "https://eapi.pcloud.com"
        } else {
            "https://api.pcloud.com"
        }
    }
}

/// Azure Blob Storage configuration
#[derive(Debug, Clone)]
pub struct AzureConfig {
    /// Storage account name
    pub account_name: String,
    /// Shared Key for HMAC signing (SecretString for memory zeroization)
    pub access_key: secrecy::SecretString,
    /// Container name
    pub container: String,
    /// Optional SAS token (alternative to access_key)
    pub sas_token: Option<secrecy::SecretString>,
    /// Custom endpoint (for Azure Stack, Azurite emulator, etc.)
    pub endpoint: Option<String>,
}

impl AzureConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let account_name = config
            .extra
            .get("account_name")
            .or(config.username.as_ref())
            .ok_or_else(|| {
                ProviderError::InvalidConfig("Account name required for Azure".to_string())
            })?
            .clone();
        let access_key: secrecy::SecretString = config
            .extra
            .get("access_key")
            .or(config.password.as_ref())
            .ok_or_else(|| {
                ProviderError::InvalidConfig("Access key required for Azure".to_string())
            })?
            .clone()
            .into();
        let container = config
            .extra
            .get("container")
            .ok_or_else(|| {
                ProviderError::InvalidConfig("Container name required for Azure".to_string())
            })?
            .clone();
        let sas_token: Option<secrecy::SecretString> =
            config.extra.get("sas_token").map(|s| s.clone().into());
        // Host may arrive as ":443" when the endpoint field is empty but port is set
        let clean_host = config.host.split(':').next().unwrap_or("").trim();
        let endpoint = if clean_host.is_empty() || clean_host == "blob.core.windows.net" {
            None
        } else {
            Some(config.host.clone())
        };
        Ok(Self {
            account_name,
            access_key,
            container,
            sas_token,
            endpoint,
        })
    }

    /// Get the blob service endpoint URL
    pub fn blob_endpoint(&self) -> String {
        if let Some(ref ep) = self.endpoint {
            // Ensure custom endpoint has a scheme
            if ep.starts_with("http://") || ep.starts_with("https://") {
                ep.clone()
            } else {
                format!("https://{}", ep)
            }
        } else {
            format!("https://{}.blob.core.windows.net", self.account_name)
        }
    }
}

/// Filen configuration
#[derive(Debug, Clone)]
pub struct FilenConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// Optional TOTP code for accounts with 2FA enabled
    pub two_factor_code: Option<String>,
}

impl FilenConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email required for Filen".to_string()))?;
        let password = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Password required for Filen".to_string())
        })?;
        let two_factor_code = config
            .extra
            .get("two_factor_code")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            email,
            password: password.into(),
            two_factor_code,
        })
    }
}

/// Internxt Drive configuration
#[derive(Debug, Clone)]
pub struct InternxtConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// Optional TOTP code for accounts with 2FA enabled
    pub two_factor_code: Option<String>,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl InternxtConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config.username.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Email required for Internxt".to_string())
        })?;
        let password = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Password required for Internxt".to_string())
        })?;
        let two_factor_code = config
            .extra
            .get("two_factor_code")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Self {
            email,
            password: password.into(),
            two_factor_code,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Infomaniak kDrive configuration (API Token)
#[derive(Debug, Clone)]
pub struct KDriveConfig {
    /// Bearer API token from Infomaniak dashboard
    pub api_token: secrecy::SecretString,
    /// kDrive ID (numeric)
    pub drive_id: String,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl KDriveConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("API token required for kDrive".to_string())
        })?;
        let drive_id = config.extra.get("drive_id").cloned().ok_or_else(|| {
            ProviderError::InvalidConfig("Drive ID required for kDrive".to_string())
        })?;
        // F6: Validate drive_id is numeric to prevent URL path traversal
        if !drive_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(ProviderError::InvalidConfig(
                "Drive ID must be numeric".to_string(),
            ));
        }
        Ok(Self {
            api_token: token.into(),
            drive_id,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Jottacloud configuration (Personal Login Token)
#[derive(Debug, Clone)]
pub struct JottacloudConfig {
    /// Base64-encoded Personal Login Token from Jottacloud settings
    pub login_token: secrecy::SecretString,
    /// Device name (default "Jotta")
    pub device: String,
    /// Mountpoint name (default "Archive")
    pub mountpoint: String,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl JottacloudConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Login token required for Jottacloud".to_string())
        })?;
        let device = config
            .extra
            .get("device")
            .cloned()
            .unwrap_or_else(|| "Jotta".to_string());
        let mountpoint = config
            .extra
            .get("mountpoint")
            .cloned()
            .unwrap_or_else(|| "Archive".to_string());
        // Validate device/mountpoint don't contain path traversal
        if device.contains("..") || device.contains('/') {
            return Err(ProviderError::InvalidConfig(
                "Invalid device name".to_string(),
            ));
        }
        if mountpoint.contains("..") || mountpoint.contains('/') {
            return Err(ProviderError::InvalidConfig(
                "Invalid mountpoint name".to_string(),
            ));
        }
        Ok(Self {
            login_token: token.into(),
            device,
            mountpoint,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Drime Cloud configuration (API Token)
#[derive(Debug, Clone)]
pub struct DrimeCloudConfig {
    /// Bearer API token from Drime Cloud dashboard
    pub api_token: secrecy::SecretString,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl DrimeCloudConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("API token required for Drime Cloud".to_string())
        })?;
        Ok(Self {
            api_token: token.into(),
            initial_path: config.initial_path.clone(),
        })
    }
}

/// FileLu configuration (API Key)
#[derive(Debug, Clone)]
pub struct FileLuConfig {
    /// API key from FileLu account settings
    pub api_key: secrecy::SecretString,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl FileLuConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let api_key = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("API key required for FileLu".to_string())
        })?;
        Ok(Self {
            api_key: api_key.into(),
            initial_path: config.initial_path.clone(),
        })
    }
}

/// 4shared configuration (OAuth 1.0)
#[derive(Debug, Clone)]
pub struct FourSharedConfig {
    pub consumer_key: String,
    pub consumer_secret: secrecy::SecretString,
    pub access_token: secrecy::SecretString,
    pub access_token_secret: secrecy::SecretString,
}

/// Remote file/directory entry
///
/// Unified representation of a file or directory across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// File or directory name
    pub name: String,
    /// Full path from root
    pub path: String,
    /// Whether this is a directory
    pub is_dir: bool,
    /// File size in bytes (0 for directories)
    pub size: u64,
    /// Last modification time (ISO 8601 string)
    pub modified: Option<String>,
    /// Permission string (Unix-style, e.g., "rwxr-xr-x")
    pub permissions: Option<String>,
    /// Owner name
    pub owner: Option<String>,
    /// Group name
    pub group: Option<String>,
    /// Whether this is a symbolic link
    pub is_symlink: bool,
    /// Link target (if symlink)
    pub link_target: Option<String>,
    /// MIME type (if known)
    pub mime_type: Option<String>,
    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

impl RemoteEntry {
    /// Create a new directory entry
    pub fn directory(name: String, path: String) -> Self {
        Self {
            name,
            path,
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Create a new file entry (used in tests and future provider implementations)
    #[allow(dead_code)]
    pub fn file(name: String, path: String, size: u64) -> Self {
        Self {
            name,
            path,
            is_dir: false,
            size,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Get file extension (used in tests and MIME type detection)
    #[allow(dead_code)]
    pub fn extension(&self) -> Option<&str> {
        if self.is_dir {
            return None;
        }
        self.name
            .rsplit('.')
            .next()
            .filter(|ext| ext.len() < self.name.len())
    }
}

/// Provider error type
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum ProviderError {
    #[error("Not connected to server")]
    NotConnected,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Path not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Path already exists: {0}")]
    AlreadyExists(String),

    #[error("Directory not empty: {0}")]
    DirectoryNotEmpty(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Transfer cancelled")]
    Cancelled,

    #[error("Transfer failed: {0}")]
    TransferFailed(String),

    #[error("Timeout")]
    Timeout,

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// The transport (TCP/SSH/TLS/HTTP keep-alive) was torn down by the peer
    /// after a successful connect+auth. Distinct from `ConnectionFailed`
    /// (which is a connect-time failure) and `NotConnected` (which is the
    /// pre-connect state). Surfaced when the next user action hits a dead
    /// session, typically because the server's idle reaper closed it.
    /// Carries enough context for the UI to offer a silent reconnect.
    #[error("Connection lost: {0}")]
    ConnectionLost(String),

    #[error("Unknown error: {0}")]
    Unknown(String),

    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Check if this error is recoverable (can retry)
    #[allow(dead_code)]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ProviderError::Timeout
                | ProviderError::NetworkError(_)
                | ProviderError::NotConnected
                | ProviderError::ConnectionLost(_)
        )
    }

    /// True if this error indicates the live session was torn down by the
    /// peer mid-flight. Caller can attempt a silent reconnect + replay.
    #[allow(dead_code)]
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, ProviderError::ConnectionLost(_))
    }
}

/// Heuristic check for transport-level errors that indicate the session
/// was closed by the peer (server idle timeout, NAT eviction, network
/// blip), as opposed to a logical "path not found" or permission error.
///
/// Used to upgrade misclassified errors (e.g. russh returning a generic
/// error string when the SFTP channel is dead) into [`ProviderError::ConnectionLost`]
/// so the command layer can attempt a silent reconnect.
pub fn is_session_closed_error_message(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "session closed",
        "channel closed",
        "channel is closed",
        "stream closed",
        "broken pipe",
        "connection reset",
        "connection aborted",
        "connection closed",
        "unexpected eof",
        "early eof",
        "transport endpoint is not connected",
        "epipe",
        "econnreset",
        "econnaborted",
        "the remote host closed the connection",
        "operation timed out",
        "ssh disconnect",
        "remote disconnected",
    ]
    .iter()
    .any(|p| m.contains(p))
}

/// Storage quota information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    /// Bytes used
    pub used: u64,
    /// Total bytes available
    pub total: u64,
    /// Bytes free
    pub free: u64,
}

/// File version metadata (for versioned providers like Google Drive, Dropbox, OneDrive)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    /// Version identifier
    pub id: String,
    /// Modification timestamp
    pub modified: Option<String>,
    /// Size in bytes
    pub size: u64,
    /// User who modified (if available)
    pub modified_by: Option<String>,
}

/// Lock information for WebDAV locking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    /// Lock token
    pub token: String,
    /// Lock owner
    pub owner: Option<String>,
    /// Lock timeout in seconds (0 = infinite)
    pub timeout: u64,
    /// Whether this is an exclusive lock
    pub exclusive: bool,
}

/// Share permission for advanced sharing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePermission {
    /// Permission role: "reader", "writer", "commenter", "owner"
    pub role: String,
    /// Target type: "user", "group", "domain", "anyone"
    pub target_type: String,
    /// Target email or identifier (empty for "anyone")
    pub target: String,
}

/// Change tracking entry (for delta sync)
#[derive(Debug, Clone, Serialize)]
pub struct ChangeEntry {
    /// File/folder path or ID
    pub file_id: String,
    /// File name
    pub name: String,
    /// Change type: "created", "modified", "deleted", "renamed"
    pub change_type: String,
    /// MIME type of the changed file
    pub mime_type: Option<String>,
    /// Timestamp of the change
    pub timestamp: Option<String>,
    /// Whether the file was trashed/deleted
    pub removed: bool,
}

/// Transfer progress information (for future progress events)
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct TransferProgressInfo {
    /// Bytes transferred so far
    pub bytes_transferred: u64,
    /// Total bytes to transfer
    pub total_bytes: u64,
    /// Progress percentage (0-100)
    pub percentage: f64,
    /// Current transfer speed in bytes/second
    pub speed_bps: u64,
    /// Estimated time remaining in seconds
    pub eta_seconds: Option<u64>,
}

#[allow(dead_code)]
impl TransferProgressInfo {
    pub fn new(bytes_transferred: u64, total_bytes: u64) -> Self {
        let percentage = if total_bytes > 0 {
            (bytes_transferred as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };

        Self {
            bytes_transferred,
            total_bytes,
            percentage,
            speed_bps: 0,
            eta_seconds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_provider_type_default_port() {
        assert_eq!(ProviderType::Ftp.default_port(), 21);
        assert_eq!(ProviderType::Ftps.default_port(), 990);
        assert_eq!(ProviderType::Sftp.default_port(), 22);
        assert_eq!(ProviderType::WebDav.default_port(), 443);
        assert_eq!(ProviderType::S3.default_port(), 443);
    }

    #[test]
    fn test_remote_entry_extension() {
        let file = RemoteEntry::file(
            "document.pdf".to_string(),
            "/path/document.pdf".to_string(),
            1000,
        );
        assert_eq!(file.extension(), Some("pdf"));

        let dir = RemoteEntry::directory("folder".to_string(), "/path/folder".to_string());
        assert_eq!(dir.extension(), None);

        let no_ext = RemoteEntry::file("Makefile".to_string(), "/path/Makefile".to_string(), 500);
        assert_eq!(no_ext.extension(), None);
    }

    #[test]
    fn test_s3_custom_http_endpoint_keeps_configured_port_and_path_style_default() {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "garage-bucket".to_string());
        extra.insert("region".to_string(), "garage".to_string());

        let config = ProviderConfig {
            name: "Garage".to_string(),
            provider_type: ProviderType::S3,
            host: "http://localhost".to_string(),
            port: Some(3900),
            username: Some("access".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let s3_config = S3Config::from_provider_config(&config).unwrap();
        assert_eq!(s3_config.endpoint.as_deref(), Some("http://localhost:3900"));
        assert!(s3_config.path_style);
    }

    #[test]
    fn test_s3_explicit_virtual_host_style_is_preserved() {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "bucket".to_string());
        extra.insert("region".to_string(), "us-east-1".to_string());
        extra.insert("path_style".to_string(), "false".to_string());

        let config = ProviderConfig {
            name: "Virtual Hosted".to_string(),
            provider_type: ProviderType::S3,
            host: "s3.example.com".to_string(),
            port: Some(443),
            username: Some("access".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let s3_config = S3Config::from_provider_config(&config).unwrap();
        assert_eq!(
            s3_config.endpoint.as_deref(),
            Some("https://s3.example.com")
        );
        assert!(!s3_config.path_style);
    }

    #[test]
    fn test_s3_explicit_endpoint_overrides_gui_host() {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "test".to_string());
        extra.insert("region".to_string(), "garage".to_string());
        extra.insert("endpoint".to_string(), "s3.garage.localhost".to_string());
        extra.insert("path_style".to_string(), "false".to_string());

        let config = ProviderConfig {
            name: "Garage".to_string(),
            provider_type: ProviderType::S3,
            host: "http://localhost".to_string(),
            port: Some(3900),
            username: Some("access".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let s3_config = S3Config::from_provider_config(&config).unwrap();
        assert_eq!(
            s3_config.endpoint.as_deref(),
            Some("http://s3.garage.localhost:3900")
        );
        assert!(!s3_config.path_style);
    }

    #[test]
    fn test_mega_s4_endpoint_rejects_unknown_region() {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "bucket".to_string());
        extra.insert("region".to_string(), "us-east-1".to_string());
        extra.insert(
            "endpoint".to_string(),
            "s3.us-east-1.s4.mega.io".to_string(),
        );

        let config = ProviderConfig {
            name: "MEGA S4".to_string(),
            provider_type: ProviderType::S3,
            host: String::new(),
            port: Some(443),
            username: Some("access".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let err = S3Config::from_provider_config(&config).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig(_)));
        assert!(err.to_string().contains("Invalid MEGA S4 region"));
    }

    #[test]
    fn test_mega_s4_endpoint_accepts_supported_region() {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "bucket".to_string());
        extra.insert("region".to_string(), "eu-central-1".to_string());
        extra.insert(
            "endpoint".to_string(),
            "s3.eu-central-1.s4.mega.io".to_string(),
        );

        let config = ProviderConfig {
            name: "MEGA S4".to_string(),
            provider_type: ProviderType::S3,
            host: String::new(),
            port: Some(443),
            username: Some("access".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let s3_config = S3Config::from_provider_config(&config).unwrap();
        assert_eq!(s3_config.region, "eu-central-1");
    }

    #[test]
    fn test_mega_config_defaults_to_megacmd_for_legacy_profiles() {
        let config = ProviderConfig {
            name: "MEGA".to_string(),
            provider_type: ProviderType::Mega,
            host: "mega.nz".to_string(),
            port: None,
            username: Some("user@example.com".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra: HashMap::new(),
        };

        let mega_config = MegaConfig::from_provider_config(&config).unwrap();
        assert_eq!(mega_config.connection_mode, MegaConnectionMode::MegaCmd);
    }

    #[test]
    fn test_mega_config_parses_explicit_native_mode() {
        let mut extra = HashMap::new();
        extra.insert("mega_mode".to_string(), "native".to_string());

        let config = ProviderConfig {
            name: "MEGA".to_string(),
            provider_type: ProviderType::Mega,
            host: "mega.nz".to_string(),
            port: None,
            username: Some("user@example.com".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };

        let mega_config = MegaConfig::from_provider_config(&config).unwrap();
        assert_eq!(mega_config.connection_mode, MegaConnectionMode::Native);
    }
}
