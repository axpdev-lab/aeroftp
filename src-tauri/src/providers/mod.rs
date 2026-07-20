//! Storage Providers Module
//!
//! This module provides a unified abstraction layer for different storage backends.
//! All providers implement the `StorageProvider` trait, allowing the application
//! to work with FTP, WebDAV, S3, and other storage systems through a common interface.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │              StorageProvider Trait                       │
//! │    connect, list, upload, download, mkdir, etc.          │
//! └─────────────────────────────────────────────────────────┘
//!                           │
//!    ┌──────┬───────┬───────┼───────┬────────┬────────┐
//!    ▼      ▼       ▼       ▼       ▼        ▼        ▼
//! ┌─────┐┌──────┐┌──────┐┌─────┐┌────────┐┌────────┐┌──────┐
//! │ FTP ││ SFTP ││WebDAV││ S3  ││ GDrive ││Dropbox ││ MEGA │
//! └─────┘└──────┘└──────┘└─────┘└────────┘└────────┘└──────┘
//! ```

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod atomic_write;
pub mod azure;
pub mod b2;
pub mod box_provider;
pub mod cloudinary;
pub mod drime_cloud;
pub mod dropbox;
pub mod filelu;
pub mod filen;
pub mod fourshared;
pub mod ftp;
pub mod github;
pub mod gitlab;
pub mod google_drive;
pub mod google_photos;
pub mod http_retry;
pub mod imagekit;
pub mod immich;
pub mod internxt;
pub mod jottacloud;
pub mod kdrive;
pub mod koofr;
pub mod mega;
pub mod mega_crypto;
pub mod mega_df;
pub mod mega_native;
pub mod mtp;
pub mod multi_thread;
pub mod oauth1;
pub mod oauth2;
pub mod onedrive;
pub mod opendrive;
pub mod pcloud;
pub mod peer;
pub mod retry_after;
pub mod s3;
pub mod sftp;
pub mod sts;
pub mod swift;
pub mod totp_helper;
pub mod tpslimit;
pub mod types;
pub mod uploadcare;
pub mod webdav;
pub mod xml_text;
pub mod yandex_disk;
pub mod zoho_workdrive;

/// User-Agent sent with every HTTP request (auto-derived from Cargo.toml version).
/// Used by all providers that make HTTP calls (S3, OAuth, REST APIs).
pub const AEROFTP_USER_AGENT: &str = concat!("AeroFTP/", env!("CARGO_PKG_VERSION"));

/// User-Agent sent by the shared WebDAV client (major version only).
/// pCloud and other WebDAV servers fingerprint the User-Agent as a device
/// identifier: a full version string (`AeroFTP/3.8.1`) makes every app update
/// register as a new device and forces a fresh email re-approval. Pinning to
/// the major version (`AeroFTP/3`) keeps the device stable across patch and
/// minor releases. `CARGO_PKG_VERSION_MAJOR` is provided by cargo at compile time.
pub const AEROFTP_WEBDAV_USER_AGENT: &str = concat!("AeroFTP/", env!("CARGO_PKG_VERSION_MAJOR"));

pub use types::*;
// GAP-A01: retry infrastructure ready: integration into providers deferred to v2.5.0
pub use azure::AzureProvider;
pub use b2::B2Provider;
pub use box_provider::BoxProvider;
pub use cloudinary::CloudinaryProvider;
pub use drime_cloud::DrimeCloudProvider;
pub use dropbox::DropboxProvider;
pub use filelu::FileLuProvider;
pub use filen::FilenProvider;
pub use fourshared::FourSharedProvider;
pub use ftp::FtpProvider;
pub use github::GitHubProvider;
pub use gitlab::GitLabProvider;
pub use google_drive::GoogleDriveProvider;
pub use google_photos::GooglePhotosProvider;
#[allow(unused_imports)]
pub use http_retry::{send_with_retry, HttpRetryConfig};
pub use imagekit::ImageKitProvider;
pub use immich::ImmichProvider;
pub use internxt::InternxtProvider;
pub use jottacloud::JottacloudProvider;
pub use kdrive::KDriveProvider;
pub use koofr::KoofrProvider;
pub use mega::{MegaCmdProvider, MegaProvider};
pub use mega_native::MegaNativeProvider;
pub use mtp::{
    fingerprint_equal, list_mtp_devices, match_live_device_id, mtp_device_fingerprint, MtpProvider,
};
pub use oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider};
pub use onedrive::OneDriveProvider;
pub use opendrive::OpenDriveProvider;
pub use pcloud::PCloudProvider;
pub use peer::PeerProvider;
pub use s3::S3Provider;
pub use sftp::SftpProvider;
pub use swift::SwiftProvider;
pub use uploadcare::UploadcareProvider;
pub use webdav::WebDavProvider;
pub use yandex_disk::YandexDiskProvider;
pub use zoho_workdrive::ZohoWorkdriveProvider;

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;

/// Match a filename against a find pattern.
/// Supports glob patterns (`*`, `?`, `[`) via globset, falls back to
/// case-insensitive substring match for plain strings like "report".
pub fn matches_find_pattern(name: &str, pattern: &str) -> bool {
    let is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
    if is_glob {
        if let Ok(glob) = globset::Glob::new(pattern) {
            glob.compile_matcher().is_match(name)
        } else {
            // Invalid glob: fall back to substring
            name.to_lowercase().contains(&pattern.to_lowercase())
        }
    } else {
        name.to_lowercase().contains(&pattern.to_lowercase())
    }
}

/// H2: Maximum size for download_to_bytes operations (500 MB).
/// Prevents OOM when a remote file is unexpectedly large.
/// For larger files, use the streaming download() method instead.
pub const MAX_DOWNLOAD_TO_BYTES: u64 = 500 * 1024 * 1024;

/// Defense-in-depth helper for the rare case where a server hands the full
/// `Content-Length` body to the TLS layer and *then* closes the connection
/// without sending a `close_notify` alert.
///
/// rustls (and Go `crypto/tls`, and schannel) report this as
/// `io::ErrorKind::UnexpectedEof` on the read that follows the body, while
/// permissive backends (OpenSSL with the default `SSL_OP_IGNORE_UNEXPECTED_EOF`)
/// silently accept what they got. This helper lets us match the permissive
/// behavior **only** when the count matches: no truncation-attack risk.
///
/// **Known case where this helper does NOT help**: the Filen Desktop WebDAV
/// bridge on Windows tears down the TLS connection so early that rustls
/// drops the body bytes before they reach the streaming reader, so `received`
/// stays at 0 and the helper returns false. That bug is server-side,
/// reproduces identically on rclone (Go) and curl (schannel), and is tracked
/// upstream at FilenCloudDienste/filen-desktop. See report 006 of the
/// 2026-05-09 Windows debug session for the full diagnostic trace.
///
/// Returns true only when ALL of:
/// 1. the response advertised a Content-Length (`expected > 0`)
/// 2. the streamed bytes already meet or exceed it (`received >= expected`)
/// 3. the reqwest error is classified as body/decode
/// 4. the underlying io::Error in the cause chain is `UnexpectedEof`
pub fn is_unexpected_eof_after_full_body(e: &reqwest::Error, received: u64, expected: u64) -> bool {
    use std::error::Error as _;
    if expected == 0 || received < expected {
        return false;
    }
    if !e.is_body() && !e.is_decode() {
        return false;
    }
    let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(s) = src {
        if let Some(io_err) = s.downcast_ref::<std::io::Error>() {
            if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                return true;
            }
        }
        src = s.source();
    }
    false
}

/// H2: Read a reqwest Response into Vec<u8> with a size cap.
/// Checks Content-Length first; if absent, reads up to `limit` bytes via streaming.
///
/// Tolerates a missing TLS close_notify when the full Content-Length has
/// already been received (see [`is_unexpected_eof_after_full_body`]).
pub async fn response_bytes_with_limit(
    resp: reqwest::Response,
    limit: u64,
) -> Result<Vec<u8>, ProviderError> {
    // Check Content-Length header if present
    let expected = resp.content_length();
    if let Some(cl) = expected {
        if cl > limit {
            return Err(ProviderError::TransferFailed(format!(
                "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                cl as f64 / 1_048_576.0,
                limit as f64 / 1_048_576.0,
            )));
        }
    }
    let expected = expected.unwrap_or(0);

    // Stream the body with a size guard
    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    loop {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if bytes.len() as u64 + chunk.len() as u64 > limit {
                    return Err(ProviderError::TransferFailed(format!(
                        "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                        limit as f64 / 1_048_576.0,
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Err(e)) => {
                if is_unexpected_eof_after_full_body(&e, bytes.len() as u64, expected) {
                    tracing::warn!(
                        "[HTTP] Server closed connection without TLS close_notify but full body received ({}/{} bytes); accepting",
                        bytes.len(),
                        expected
                    );
                    break;
                }
                return Err(ProviderError::TransferFailed(e.to_string()));
            }
            None => break,
        }
    }

    Ok(bytes)
}

/// F-011: Unescape the five standard XML entities in an error string.
/// S3, WebDAV, and Azure return XML error bodies, so a `<Message>` extracted
/// verbatim keeps `&apos;`/`&amp;`/`&lt;` etc. That is correct for an XML sink
/// but garbles a plain-text or JSON error field (an agent pattern-matching the
/// message sees `&apos;` instead of `'`). Plain-text/JSON error bodies never
/// contain these entity sequences, so unescaping them is a no-op there and a
/// fix for the XML providers. `&amp;` is decoded last so a double-escaped
/// `&amp;lt;` does not collapse in one pass.
fn unescape_xml_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// GAP-A10: Sanitize API error response bodies to prevent leaking sensitive data.
/// Truncates to first line (max 200 chars), strips potential tokens/keys.
pub fn sanitize_api_error(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("unknown error");
    let truncated = if first_line.len() > 200 {
        let boundary = first_line
            .char_indices()
            .take_while(|&(i, _)| i <= 200)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(200);
        format!("{}...", &first_line[..boundary])
    } else {
        first_line.to_string()
    };
    // Apply the same regex-based sanitization used by the AI pipeline
    // (covers sk-*, Bearer tokens, x-api-key, Google key= params)
    let sanitized = crate::ai::sanitize_error_message(&truncated);
    // F-011: emit raw UTF-8 for the JSON/plain-text error sink.
    unescape_xml_entities(&sanitized)
}

/// Transfer optimization hints: per-provider capability advertisement
#[derive(Debug, Clone, Serialize)]
pub struct TransferOptimizationHints {
    pub supports_multipart: bool,
    pub multipart_threshold: u64,
    pub multipart_part_size: u64,
    pub multipart_max_parallel: u8,
    pub supports_resume_download: bool,
    pub supports_resume_upload: bool,
    pub supports_range_download: bool,
    pub supports_server_checksum: bool,
    pub preferred_checksum_algo: Option<String>,
    pub supports_compression: bool,
    pub supports_delta_sync: bool,
    pub delta_sync_eligible: bool,
    pub delta_sync_active: bool,
    pub delta_sync_note: Option<String>,
}

impl Default for TransferOptimizationHints {
    fn default() -> Self {
        Self {
            supports_multipart: false,
            multipart_threshold: 0,
            multipart_part_size: 0,
            multipart_max_parallel: 1,
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_range_download: false,
            supports_server_checksum: false,
            preferred_checksum_algo: None,
            supports_compression: false,
            supports_delta_sync: false,
            delta_sync_eligible: false,
            delta_sync_active: false,
            delta_sync_note: None,
        }
    }
}

/// Execution model the Core DAG provider executor may use for file transfers.
///
/// `LockedSingle` is the conservative default for providers whose trait object is
/// protected by the GUI session mutex for the whole file. Providers should only
/// return `HttpClonePool` when `clone_for_transfer()` produces an independent
/// transfer-capable instance backed by a cloneable HTTP client or equivalent
/// real resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransferExecutorKind {
    LockedSingle,
    HttpClonePool,
    /// SFTP file-level parallelism via N **independent** SSH connections
    /// re-dialled from a retained secure connection spec (PD-SFTP-1, same
    /// model as the FTP pool). Distinct from `HttpClonePool` so the
    /// session pool / metrics label the transport as SFTP, never HTTP.
    SftpConnectionPool,
    /// FTP file-level + intra-file parallelism via N **independent** FTP
    /// control+data connections re-dialled from a retained connection spec
    /// (PD-FTP-1, the FTP mirror of `SftpConnectionPool`). Distinct so the
    /// session pool / metrics label the transport as FTP, never HTTP.
    FtpConnectionPool,
}

/// Execution model the Core DAG scanner may use for remote list/checker work.
///
/// This is intentionally separate from file transfer execution: a provider may
/// support clone-backed transfers before its list/stat/checksum path has been
/// audited for concurrent use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderListExecutorKind {
    LockedSingle,
    HttpClonePool,
}

/// Options for creating a share link - provider-specific fields are optional.
/// Providers that don't support a given option simply ignore it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ShareLinkOptions {
    /// Link expiration in seconds from now (None = permanent/provider default)
    pub expires_in_secs: Option<u64>,
    /// Password protection (None = no password)
    pub password: Option<String>,
    /// Permission level: "view", "edit", "comment" (None = provider default)
    pub permissions: Option<String>,
}

/// Result of creating a share link - contains the URL and optional metadata
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ShareLinkResult {
    /// The share link URL
    pub url: String,
    /// Password set on the link (for auto-generated passwords, e.g. Nextcloud)
    pub password: Option<String>,
    /// When the link expires (ISO 8601), if applicable
    pub expires_at: Option<String>,
}

/// Advertised capabilities for share link advanced options
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShareLinkCapabilities {
    pub supports_expiration: bool,
    pub supports_password: bool,
    pub supports_permissions: bool,
    pub available_permissions: Vec<String>,
    /// Whether this provider supports listing existing share links
    pub supports_list_links: bool,
    /// Whether this provider supports revoking share links
    pub supports_revoke: bool,
}

/// Information about an existing share link
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ShareLinkInfo {
    /// Provider-specific link identifier (for revocation)
    pub id: String,
    /// The share link URL
    pub url: String,
    /// When the link was created (ISO 8601)
    pub created_at: Option<String>,
    /// When the link expires (ISO 8601), None = permanent
    pub expires_at: Option<String>,
    /// Whether the link is password protected
    pub password_protected: bool,
    /// Permission level (e.g. "view", "edit")
    pub permissions: Option<String>,
}

/// Opaque handle to an in-progress multipart upload.
///
/// Returned by `StorageProvider::begin_multipart_upload` and consumed by
/// `upload_part`, `complete_multipart_upload`, and `abort_multipart_upload`.
/// The shape is intentionally provider-agnostic: `upload_id` holds whatever
/// the backend uses to identify a multipart session (S3 UploadId, B2
/// fileId, etc.), and `remote_path` echoes the destination key so the
/// runner can correlate handles with shaped-graph nodes.
#[derive(Clone, PartialEq, Eq)]
pub struct MultipartHandle {
    /// Provider-assigned multipart upload identifier (opaque string).
    pub upload_id: String,
    /// Destination remote path or key the handle is bound to.
    pub remote_path: String,
}

impl std::fmt::Debug for MultipartHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Global redaction: Filen (and other providers) embed crypto/session
        // material in `upload_id`. Never print it in logs, panics, or Debug.
        f.debug_struct("MultipartHandle")
            .field("upload_id", &"<redacted>")
            .field("remote_path", &self.remote_path)
            .finish()
    }
}

/// Receipt for a single uploaded part of a multipart session.
///
/// Returned by `StorageProvider::upload_part` and passed back to
/// `complete_multipart_upload` so the backend can finalize the object.
/// `etag` carries whatever per-part token the protocol uses to verify the
/// part on completion (S3 ETag, B2 SHA-1, WebDAV ETag, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedPart {
    /// 1-based part number, as required by S3 and most multipart protocols.
    pub part_number: u32,
    /// Provider-issued verification tag for the uploaded part.
    pub etag: String,
}

/// Unified storage provider trait
///
/// All storage backends must implement this trait to be used with AeroFTP.
/// This enables protocol-agnostic file operations and makes it easy to add
/// new storage providers in the future.
///
/// Note: Some trait methods are not yet used but are part of the planned API
/// for future features (Properties dialog, chmod support, etc.)
#[async_trait]
#[allow(dead_code)]
pub trait StorageProvider: Send + Sync {
    /// Downcast to concrete provider type for provider-specific operations
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Get the provider type identifier
    fn provider_type(&self) -> ProviderType;

    /// Get display name for this provider instance
    fn display_name(&self) -> String;

    /// Get the authenticated account email/username (if available after connect)
    fn account_email(&self) -> Option<String> {
        None
    }

    /// Stable identity for the process-global transfer governor. Direct
    /// providers normally expose a `user@host` display name; cloud providers
    /// expose their account or tenant label. Implementations with richer
    /// authority data may override this default.
    fn endpoint_identity(&self) -> crate::transfer_dag::EndpointIdentity {
        crate::transfer_dag::EndpointIdentity::new(
            self.provider_type().to_string(),
            self.display_name(),
            self.account_email().unwrap_or_default(),
        )
    }

    /// Connect to the storage backend
    async fn connect(&mut self) -> Result<(), ProviderError>;

    /// Disconnect from the storage backend
    async fn disconnect(&mut self) -> Result<(), ProviderError>;

    /// Check if currently connected
    fn is_connected(&self) -> bool;

    /// List files and directories in the given path
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError>;

    /// Get current working directory
    async fn pwd(&mut self) -> Result<String, ProviderError>;

    /// Change current directory
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Go to parent directory
    async fn cd_up(&mut self) -> Result<(), ProviderError>;

    /// Download a file to local path
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError>;

    /// Download a file to memory (returns bytes)
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError>;

    /// Download a file to memory, refusing before the in-memory buffer would
    /// exceed `max_bytes`. Small-cap callers (the MCP/CLI `edit` tool caps at
    /// 10 MB) use this so a server that under-reports its size cannot force a
    /// full materialization up to the general 500 MB `download_to_bytes` limit.
    ///
    /// The default here is a safe fallback: it performs the full read and then
    /// enforces the cap. Streaming-capable providers (SFTP, HTTP) override it to
    /// stop reading as soon as the accumulated bytes would exceed `max_bytes`,
    /// so the oversized body is never fully held in memory.
    async fn download_to_bytes_capped(
        &mut self,
        remote_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        let data = self.download_to_bytes(remote_path).await?;
        if data.len() as u64 > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded the {:.0} MB cap.",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        Ok(data)
    }

    /// Upload a file from local path
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError>;

    /// Create a directory
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Delete a file
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Delete a directory (must be empty for most providers)
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Delete a directory recursively (with all contents)
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Rename/move a file or directory
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError>;

    /// Get file/directory info
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError>;

    /// Get file size
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError>;

    /// Check if path exists
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError>;

    /// Keep connection alive (send heartbeat/noop)
    async fn keep_alive(&mut self) -> Result<(), ProviderError>;

    /// Get server/service info
    async fn server_info(&mut self) -> Result<String, ProviderError>;

    // Optional capabilities - providers can override these

    /// Hard-delete a path bypassing the provider's recycle bin.
    ///
    /// `delete()` on consumer-cloud providers (Google Drive, Dropbox, OneDrive,
    /// Box, MEGA, Yandex, FileLu, Internxt, kDrive, Zoho WorkDrive, pCloud,
    /// Jottacloud, OpenDrive, Backblaze B2 with versioning) is a soft delete:
    /// the item ends up in the provider's trash and still consumes quota.
    ///
    /// Implementations should override this to perform the actual purge so
    /// trash does not silently fill up during long-running benchmarks or sync
    /// operations. The path is the same one passed to `delete()`; ID-based
    /// providers must resolve it via their trash listing.
    ///
    /// Returns:
    /// - `Ok(true)` when a real purge was performed.
    /// - `Ok(false)` when the provider has no trash concept (FTP, FTPS, SFTP,
    ///   most WebDAV, plain S3) and the call was a no-op. This is the safe
    ///   default for unmigrated providers.
    /// - `Err(_)` only when the provider has trash but the purge call failed.
    async fn delete_permanent(&mut self, _path: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }

    /// Check if provider supports chmod
    fn supports_chmod(&self) -> bool {
        false
    }

    /// Change file permissions (Unix-style)
    async fn chmod(&mut self, _path: &str, _mode: u32) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("chmod".to_string()))
    }

    /// Check if provider supports symlinks
    fn supports_symlinks(&self) -> bool {
        false
    }

    /// Check if provider supports server-side copy
    fn supports_server_copy(&self) -> bool {
        false
    }

    /// Capability gate for the DAG shaped-graph `ServerSideCopy` node.
    ///
    /// Default delegates to `supports_server_copy()` so all existing
    /// provider overrides keep advertising the capability without
    /// duplication. New code (capability builder, runner dispatch) should
    /// reach for `supports_server_side_copy` because it matches the
    /// `TransferCapabilities::server_side_copy` slot one-to-one.
    fn supports_server_side_copy(&self) -> bool {
        self.supports_server_copy()
    }

    /// Copy file on server side (without download/upload)
    async fn server_copy(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("server_copy".to_string()))
    }

    // Multipart upload + server-side copy (DAG shaped-graph wiring).
    //
    // These methods carry the upload of a single object as a series of
    // independent parts that the shaped-graph runner can dispatch in
    // parallel. The default implementations return `NotSupported`; providers
    // that have a native multipart API (S3, B2) and a native copy primitive
    // (S3, B2, WebDAV, ImageKit, GDrive, Dropbox, OneDrive, ...) override
    // them. Runner code must guard each call with the matching capability
    // (`multipart_upload` / `server_side_copy`) before dispatch.

    /// Begin a multipart upload session for `remote_path`.
    ///
    /// `total_size` and `content_type` are passed in so backends that need
    /// to declare them upfront (S3 sets Content-Type on initiation; B2
    /// validates the file_info hash later) can do so without a second
    /// round trip. `local_source_path` is the on-disk source file the
    /// runner is uploading, threaded through for backends whose commit
    /// step requires a whole-file checksum (Box's `Digest: sha=...` on
    /// `/upload_sessions/<id>/commit`) so the provider can stream the
    /// file once at commit time. Backends that do not need it ignore it.
    ///
    /// Returns an opaque `MultipartHandle` that callers must thread
    /// through subsequent `upload_part` / `complete_multipart_upload` /
    /// `abort_multipart_upload` calls.
    async fn begin_multipart_upload(
        &mut self,
        _remote_path: &str,
        _total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        Err(ProviderError::NotSupported(
            "begin_multipart_upload".to_string(),
        ))
    }

    /// Upload a single part of a multipart session.
    ///
    /// `part_number` is 1-based to match the S3 contract that B2/WebDAV/Azure
    /// all happen to follow. `data` is the raw bytes for this part; the
    /// runner is responsible for slicing the source file into chunks of the
    /// provider's preferred size (`TransferOptimizationHints::multipart_part_size`).
    /// Returns an `UploadedPart` receipt that must be passed to
    /// `complete_multipart_upload` in part-number order.
    async fn upload_part(
        &mut self,
        _handle: &MultipartHandle,
        _part_number: u32,
        _data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        Err(ProviderError::NotSupported("upload_part".to_string()))
    }

    /// Upload one part from a [`PartBody`] (DAG-P2-05).
    ///
    /// The runner hands part data as a `PartBody` so providers that can honestly
    /// stream a bounded window (single-`send` PUT/POST with a known length) avoid
    /// keeping the whole part in memory. The default materializes the body and
    /// delegates to [`upload_part`](StorageProvider::upload_part), so providers
    /// that must own the whole part to hash/encrypt/sign it (or that have not
    /// been migrated) keep their behaviour byte for byte. Streaming providers
    /// override this and return `true` from
    /// [`multipart_streams_part_body`](StorageProvider::multipart_streams_part_body).
    async fn upload_part_body(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<UploadedPart, ProviderError> {
        let data = body.into_owned_bytes().await?;
        self.upload_part(handle, part_number, data).await
    }

    /// Whether [`upload_part_body`](StorageProvider::upload_part_body) streams a
    /// `PartBody::DiskSlice` without holding the whole part in memory. When
    /// `true`, the runner reserves only a bounded streaming window of
    /// `buffer_bytes` per part instead of the full part size.
    fn multipart_streams_part_body(&self) -> bool {
        false
    }

    /// Finalize a multipart upload by submitting the ordered list of parts.
    ///
    /// The `handle` is consumed because the session is no longer valid after
    /// the call (success or failure). `parts` must be sorted by
    /// `part_number` ascending; backends may enforce this and the runner
    /// guarantees it by collecting receipts in DAG topological order.
    async fn complete_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
        _parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "complete_multipart_upload".to_string(),
        ))
    }

    /// Abort a multipart upload session, releasing any provider-side state.
    ///
    /// Called by the runner when one of the `upload_part` nodes fails and
    /// the DAG decides not to retry the whole upload. Best-effort: backends
    /// that cannot abort (or where abort is implicit on TTL) may return
    /// `Ok(())`.
    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "abort_multipart_upload".to_string(),
        ))
    }

    /// Copy an object on the server side without touching the network round
    /// trip through the local host.
    ///
    /// Default delegates to the legacy `server_copy` method so all existing
    /// provider overrides (Azure, Box, Dropbox, FileLu, Google Drive,
    /// ImageKit, kDrive, Koofr, MEGA, OneDrive, pCloud, S3, Swift, WebDAV,
    /// Yandex Disk, Zoho WorkDrive, drime_cloud) keep working untouched.
    /// New code, runner wiring, and capability checks should reach for
    /// `server_side_copy` because it matches the
    /// `TransferCapabilities::server_side_copy` slot.
    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.server_copy(from, to).await
    }

    /// Check if provider supports share links
    fn supports_share_links(&self) -> bool {
        false
    }

    /// Advertise which share link options this provider supports
    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities::default()
    }

    /// Generate a share link for a file
    async fn create_share_link(
        &mut self,
        _path: &str,
        _options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        Err(ProviderError::NotSupported("share_link".to_string()))
    }

    /// Check if provider supports importing from public links
    fn supports_import_link(&self) -> bool {
        false
    }

    /// Import a file/folder from a public link into the account
    async fn import_link(&mut self, _link: &str, _dest: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("import_link".to_string()))
    }

    /// Remove a previously created share/export link
    async fn remove_share_link(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remove_share_link".to_string()))
    }

    /// List existing share links for a file or folder.
    /// Returns all active share links for the given path.
    async fn list_share_links(&mut self, _path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        Err(ProviderError::NotSupported("list_share_links".to_string()))
    }

    /// Get storage quota information (used/total/free)
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        Err(ProviderError::NotSupported("storage_info".to_string()))
    }

    /// Get disk usage for a specific path in bytes
    async fn disk_usage(&mut self, _path: &str) -> Result<u64, ProviderError> {
        Err(ProviderError::NotSupported("disk_usage".to_string()))
    }

    /// Check if provider supports remote search
    fn supports_find(&self) -> bool {
        false
    }

    /// Search for files matching a pattern under the given path
    async fn find(
        &mut self,
        _path: &str,
        _pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        Err(ProviderError::NotSupported("find".to_string()))
    }

    /// Set transfer speed limits (in KB/s, 0 = unlimited)
    async fn set_speed_limit(
        &mut self,
        _upload_kb: u64,
        _download_kb: u64,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("set_speed_limit".to_string()))
    }

    /// Get current transfer speed limits (upload_kb, download_kb) in KB/s
    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Err(ProviderError::NotSupported("get_speed_limit".to_string()))
    }

    /// Check if provider supports resume (REST command)
    fn supports_resume(&self) -> bool {
        false
    }

    /// Whether this provider can resume an interrupted upload by appending
    /// bytes from a given offset (the GUI "Resume" action on the overwrite
    /// dialog). Kept deliberately separate from the DAG `transfer_capabilities`
    /// hints so enabling append-resume never reshapes DAG upload planning.
    /// Default false; only providers with a real append path (currently SFTP)
    /// override it. Crypt overlay providers MUST leave it false: a partial
    /// ciphertext is not byte-resumable (per-file nonce / AEAD framing), so a
    /// crypt-bound resume falls back to a full re-encrypt instead.
    fn supports_resume_upload_append(&self) -> bool {
        false
    }

    /// Resume a download from a given byte offset
    async fn resume_download(
        &mut self,
        _remote_path: &str,
        _local_path: &str,
        _offset: u64,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("resume_download".to_string()))
    }

    /// Resume an upload from a given byte offset
    async fn resume_upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _offset: u64,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("resume_upload".to_string()))
    }

    /// Check if provider supports file versions
    fn supports_versions(&self) -> bool {
        false
    }

    /// List versions of a file
    async fn list_versions(&mut self, _path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        Err(ProviderError::NotSupported("list_versions".to_string()))
    }

    /// List every version AND delete marker under a prefix (powers the trash browse).
    ///
    /// When `include_noncurrent` is false, emit only delete markers and the
    /// latest version of each key; when true, emit all non-current versions too.
    async fn list_object_versions(
        &mut self,
        _prefix: &str,
        _include_noncurrent: bool,
    ) -> Result<Vec<TrashEntry>, ProviderError> {
        Err(ProviderError::NotSupported(
            "list_object_versions".to_string(),
        ))
    }

    /// Download a specific version of a file
    async fn download_version(
        &mut self,
        _path: &str,
        _version_id: &str,
        _local_path: &str,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("download_version".to_string()))
    }

    /// Restore a file to a specific version
    async fn restore_version(
        &mut self,
        _path: &str,
        _version_id: &str,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("restore_version".to_string()))
    }

    /// Permanently delete a single version or delete marker (hard-delete).
    async fn delete_version(
        &mut self,
        _path: &str,
        _version_id: &str,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("delete_version".to_string()))
    }

    /// Empty the trash under `prefix`: enumerate every version and delete marker
    /// via [`list_object_versions`](Self::list_object_versions) and hard-delete
    /// each one. Returns `(count, bytes)` of what was (or, with `dry_run`, would
    /// be) purged.
    ///
    /// The default implementation loops [`delete_version`](Self::delete_version)
    /// one object at a time; a provider with a batch delete API (S3) overrides
    /// this to purge in chunks. `dry_run` only enumerates and totals, deleting
    /// nothing. Irreversible when `dry_run` is false.
    async fn empty_object_versions(
        &mut self,
        prefix: &str,
        include_noncurrent: bool,
        dry_run: bool,
    ) -> Result<(u64, u64), ProviderError> {
        let entries = self
            .list_object_versions(prefix, include_noncurrent)
            .await?;
        let count = entries.len() as u64;
        let bytes: u64 = entries.iter().map(|e| e.size).sum();
        if !dry_run {
            for entry in &entries {
                self.delete_version(&entry.key, &entry.version_id).await?;
            }
        }
        Ok((count, bytes))
    }

    /// Check if provider supports file locking
    fn supports_locking(&self) -> bool {
        false
    }

    /// Lock a file
    async fn lock_file(&mut self, _path: &str, _timeout: u64) -> Result<LockInfo, ProviderError> {
        Err(ProviderError::NotSupported("lock_file".to_string()))
    }

    /// Unlock a file
    async fn unlock_file(&mut self, _path: &str, _lock_token: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("unlock_file".to_string()))
    }

    /// Check if provider supports thumbnails
    fn supports_thumbnails(&self) -> bool {
        false
    }

    /// Get a thumbnail URL or base64-encoded data for a file
    async fn get_thumbnail(&mut self, _path: &str) -> Result<String, ProviderError> {
        Err(ProviderError::NotSupported("get_thumbnail".to_string()))
    }

    /// Check if provider supports advanced sharing (per-user permissions)
    fn supports_permissions(&self) -> bool {
        false
    }

    /// List current permissions/shares on a file
    async fn list_permissions(
        &mut self,
        _path: &str,
    ) -> Result<Vec<SharePermission>, ProviderError> {
        Err(ProviderError::NotSupported("list_permissions".to_string()))
    }

    /// Add a permission/share to a file
    async fn add_permission(
        &mut self,
        _path: &str,
        _permission: &SharePermission,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("add_permission".to_string()))
    }

    /// Remove a permission/share from a file
    async fn remove_permission(&mut self, _path: &str, _target: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remove_permission".to_string()))
    }

    /// Whether this provider supports file checksums
    fn supports_checksum(&self) -> bool {
        false
    }

    /// Whether `size`/`stat` report the EXACT logical (plaintext) size. True for
    /// every normal provider. A crypt overlay whose size mapping is deferred
    /// (legacy AeroCrypt v1/v2) returns false so size-based sync comparison is
    /// skipped for it: the local plaintext size never equals the on-wire
    /// ciphertext size, so comparing them would flag every unchanged file as
    /// different and churn every cycle. Such a provider is compared on timestamp
    /// + the AEAD tag instead. rclone-crypt and AeroCrypt v3 report exact sizes
    /// (deterministic overhead map) and keep this true.
    fn reports_exact_size(&self) -> bool {
        true
    }

    /// Get checksum(s) for a file. Returns HashMap with algorithm → hex digest.
    async fn checksum(&mut self, _path: &str) -> Result<HashMap<String, String>, ProviderError> {
        Err(ProviderError::NotSupported("checksum".to_string()))
    }

    /// Whether this provider supports remote/URL upload (server fetches a URL)
    fn supports_remote_upload(&self) -> bool {
        false
    }

    /// Tell the server to download a file from a URL into the given path
    async fn remote_upload(&mut self, _url: &str, _dest_path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remote_upload".to_string()))
    }

    /// Whether this provider supports change tracking (delta sync)
    fn supports_change_tracking(&self) -> bool {
        false
    }

    /// Get a start page token for change tracking
    async fn get_change_token(&mut self) -> Result<String, ProviderError> {
        Err(ProviderError::NotSupported("get_change_token".to_string()))
    }

    /// List changes since the given page token, returns (changes, new_token)
    async fn list_changes(
        &mut self,
        _page_token: &str,
    ) -> Result<(Vec<ChangeEntry>, String), ProviderError> {
        Err(ProviderError::NotSupported("list_changes".to_string()))
    }

    /// Get transfer optimization hints for this provider
    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints::default()
    }

    /// Scheduler-facing provider executor model.
    ///
    /// Defaults to a single locked session. Override only when this provider can
    /// create independent transfer workers via `clone_for_transfer()`.
    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::LockedSingle
    }

    /// Maximum file-level leases for the provider executor when clone-backed.
    fn transfer_executor_max_sessions(&self) -> u16 {
        1
    }

    /// Create an independent transfer worker for clone-backed provider DAG execution.
    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Err(ProviderError::NotSupported(
            "clone_for_transfer".to_string(),
        ))
    }

    /// Scheduler-facing provider scanner model.
    ///
    /// Defaults to a single locked session. Override only when list/stat/checksum
    /// work can run on independent workers without taking the GUI provider mutex
    /// for the duration of the remote scan.
    fn list_executor_kind(&self) -> ProviderListExecutorKind {
        ProviderListExecutorKind::LockedSingle
    }

    /// Maximum checker/list leases for clone-backed remote scan.
    fn list_executor_max_sessions(&self) -> u16 {
        1
    }

    /// Create an independent list/checker worker for clone-backed scan.
    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Err(ProviderError::NotSupported("clone_for_list".to_string()))
    }

    /// Routing hint for the [`crate::transfer_router`] data-driven engine
    /// selector. Default implementation maps the [`ProviderType`] to a
    /// [`crate::transfer_router::ProviderHint`] without inspecting any
    /// server URL, which is the right answer for every provider except
    /// WebDAV (Nextcloud vs gateway vs vanilla discrimination needs the
    /// URL). The WebDAV provider overrides this method.
    fn router_hint(&self) -> crate::transfer_router::ProviderHint {
        crate::transfer_router::hints::from_provider_type(self.provider_type(), None, None)
    }

    /// Get scheduler-facing transfer capabilities for the Core DAG engine.
    ///
    /// This is deliberately stricter than the legacy hint surface: a provider
    /// can support `read_range()` for delta sync while still not being safe for
    /// concurrent Range downloads or file-level parallelism under the current
    /// executor. Defaults derive from existing hints and keep legacy providers
    /// on a single lease unless they advertise a real pool-backed path.
    fn transfer_capabilities(&self) -> crate::transfer_dag::TransferCapabilities {
        let mut caps = crate::transfer_dag::TransferCapabilities::from_provider_hints(
            self.provider_type(),
            &self.transfer_optimization_hints(),
            self.supports_server_side_copy(),
        );

        // DAG-P2-05: single source of truth for streaming part bodies. The
        // shaping profile reads this to reserve a streaming window (not the whole
        // part) of `buffer_bytes`, so it must reflect whether this provider
        // actually streams (i.e. overrides `upload_part_body`).
        caps.multipart_streaming_body = self.multipart_streams_part_body();

        if matches!(
            self.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
                | ProviderTransferExecutorKind::SftpConnectionPool
                | ProviderTransferExecutorKind::FtpConnectionPool
        ) {
            caps.file_parallel = crate::transfer_dag::Capability::Supported;
            caps.session_pool = crate::transfer_dag::Capability::Supported;
            caps.max_file_slots = Some(self.transfer_executor_max_sessions().max(1));
        }

        // PD-SFTP-2 / PD-FTP-1: intra-file concurrent range is real once a
        // connection pool exists (N independent SSH/FTP connections +
        // seek/read), runtime-gated by the multi-thread cutoff in
        // `download()`. Flip honestly only behind a real pool kind, never
        // as a protocol claim. HTTP providers keep their own
        // `from_provider_hints` derivation untouched (this branch is
        // SFTP/FTP-only).
        if matches!(
            self.transfer_executor_kind(),
            ProviderTransferExecutorKind::SftpConnectionPool
                | ProviderTransferExecutorKind::FtpConnectionPool
        ) {
            caps.strict_concurrent_range_download = crate::transfer_dag::Capability::Supported;
        }

        if self.list_executor_kind() == ProviderListExecutorKind::HttpClonePool
            && self.clone_for_list().is_ok()
        {
            caps.list_parallel = crate::transfer_dag::Capability::Supported;
            caps.max_checker_slots = Some(self.list_executor_max_sessions().max(1));
        } else {
            caps.list_parallel = crate::transfer_dag::Capability::Unsupported;
            caps.max_checker_slots = Some(1);
        }

        caps
    }

    /// Override upload chunk size and download buffer size.
    /// Providers that support dynamic sizing should override this.
    fn set_chunk_sizes(&mut self, _upload: Option<u64>, _download: Option<u64>) {}

    /// Configure multi-thread download (rclone `--multi-thread-streams`).
    /// `streams = 1` disables the feature; otherwise files larger than `cutoff_bytes`
    /// are downloaded by splitting them into N concurrent Range requests.
    /// Providers that support concurrent Range downloads should override this.
    fn set_multi_thread_download(&mut self, _streams: usize, _cutoff_bytes: u64) {}

    /// Whether this provider supports delta sync (rsync-style block transfer)
    fn supports_delta_sync(&self) -> bool {
        false
    }

    /// Read a byte range from a remote file (needed for delta sync)
    async fn read_range(
        &mut self,
        _path: &str,
        _offset: u64,
        _len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::NotSupported("read_range".to_string()))
    }
}

/// Provider factory for creating provider instances
pub struct ProviderFactory;

impl ProviderFactory {
    /// Create a new provider instance based on configuration
    pub fn create(config: &ProviderConfig) -> Result<Box<dyn StorageProvider>, ProviderError> {
        match config.provider_type {
            ProviderType::Ftp | ProviderType::Ftps => {
                let ftp_config = FtpConfig::from_provider_config(config)?;
                Ok(Box::new(FtpProvider::new(ftp_config)))
            }
            ProviderType::WebDav => {
                let webdav_config = WebDavConfig::from_provider_config(config)?;
                Ok(Box::new(WebDavProvider::new(webdav_config)?))
            }
            ProviderType::S3 => {
                let s3_config = S3Config::from_provider_config(config)?;
                Ok(Box::new(S3Provider::new(s3_config)?))
            }
            ProviderType::Sftp => {
                let sftp_config = SftpConfig::from_provider_config(config)?;
                Ok(Box::new(SftpProvider::new(sftp_config)))
            }
            ProviderType::AeroCloud => {
                // AeroCloud uses FTP internally but is configured via CloudPanel
                Err(ProviderError::NotSupported(
                    "AeroCloud must be configured via the AeroCloud panel (click AeroCloud in status bar)".to_string()
                ))
            }
            ProviderType::GoogleDrive
            | ProviderType::GooglePhotos
            | ProviderType::Dropbox
            | ProviderType::OneDrive
            | ProviderType::Box
            | ProviderType::PCloud
            | ProviderType::ZohoWorkdrive => {
                // OAuth2 providers require a different initialization flow
                // Use oauth2_connect command instead
                Err(ProviderError::NotSupported(
                    "OAuth2 providers must be connected using oauth2_start_auth and oauth2_connect commands".to_string()
                ))
            }
            ProviderType::FourShared => {
                // OAuth1 provider: use fourshared_connect command
                Err(ProviderError::NotSupported(
                    "4shared must be connected using fourshared_start_auth and fourshared_connect commands".to_string()
                ))
            }
            ProviderType::Mega => {
                let mega_config = MegaConfig::from_provider_config(config)?;
                match mega_config.connection_mode {
                    MegaConnectionMode::Native => {
                        Ok(Box::new(MegaNativeProvider::new(mega_config)))
                    }
                    MegaConnectionMode::MegaCmd => Ok(Box::new(MegaCmdProvider::new(mega_config))),
                }
            }
            ProviderType::Azure => {
                let azure_config = AzureConfig::from_provider_config(config)?;
                Ok(Box::new(AzureProvider::new(azure_config)))
            }
            ProviderType::Filen => {
                let filen_config = FilenConfig::from_provider_config(config)?;
                Ok(Box::new(FilenProvider::new(filen_config)))
            }
            ProviderType::Internxt => {
                let internxt_config = InternxtConfig::from_provider_config(config)?;
                Ok(Box::new(InternxtProvider::new(internxt_config)))
            }
            ProviderType::KDrive => {
                let kdrive_config = KDriveConfig::from_provider_config(config)?;
                Ok(Box::new(KDriveProvider::new(kdrive_config)))
            }
            ProviderType::Jottacloud => {
                let jotta_config = JottacloudConfig::from_provider_config(config)?;
                let mut provider = JottacloudProvider::new(jotta_config);
                // Issue #214: when the caller threaded a profile id through
                // `ProviderConfig.extra["profile_id"]` we bind the refresh
                // token to a per-profile vault key. Without it the legacy
                // singleton key is used, preserving historic behaviour.
                if let Some(pid) = config.extra.get("profile_id") {
                    if !pid.is_empty() {
                        provider = provider.with_profile_id(pid);
                    }
                }
                Ok(Box::new(provider))
            }
            ProviderType::DrimeCloud => {
                let drime_config = DrimeCloudConfig::from_provider_config(config)?;
                Ok(Box::new(DrimeCloudProvider::new(drime_config)))
            }
            ProviderType::FileLu => {
                let filelu_config = FileLuConfig::from_provider_config(config)?;
                Ok(Box::new(FileLuProvider::new(filelu_config)))
            }
            ProviderType::Koofr => {
                let koofr_config = koofr::KoofrConfig::from_provider_config(config)?;
                Ok(Box::new(KoofrProvider::new(koofr_config)))
            }
            ProviderType::OpenDrive => {
                let opendrive_config = opendrive::OpenDriveConfig::from_provider_config(config)?;
                Ok(Box::new(OpenDriveProvider::new(opendrive_config)))
            }
            ProviderType::YandexDisk => {
                let token = config.password.clone().unwrap_or_default();
                let initial_path = config.initial_path.clone();
                Ok(Box::new(YandexDiskProvider::new(token, initial_path)))
            }
            ProviderType::GitHub => {
                let gh_config = github::GitHubConfig::from_provider_config(config)?;
                Ok(Box::new(GitHubProvider::new(gh_config)?))
            }
            ProviderType::GitLab => {
                let gl_config = gitlab::GitLabConfig::from_provider_config(config)?;
                Ok(Box::new(GitLabProvider::new(gl_config)?))
            }
            ProviderType::Swift => {
                let swift_config = swift::SwiftConfig::from_provider_config(config)?;
                Ok(Box::new(SwiftProvider::new(swift_config)))
            }
            ProviderType::Immich => {
                let immich_config = immich::ImmichConfig::from_provider_config(config)?;
                Ok(Box::new(ImmichProvider::new(immich_config)))
            }
            ProviderType::ImageKit => {
                let imagekit_config = imagekit::ImageKitConfig::from_provider_config(config)?;
                Ok(Box::new(ImageKitProvider::new(imagekit_config)))
            }
            ProviderType::Uploadcare => {
                let uploadcare_config = uploadcare::UploadcareConfig::from_provider_config(config)?;
                Ok(Box::new(UploadcareProvider::new(uploadcare_config)))
            }
            ProviderType::Backblaze => {
                let b2_config = b2::B2Config::from_provider_config(config)?;
                Ok(Box::new(B2Provider::new(b2_config)))
            }
            ProviderType::Cloudinary => {
                let cloudinary_config = cloudinary::CloudinaryConfig::from_provider_config(config)?;
                Ok(Box::new(CloudinaryProvider::new(cloudinary_config)))
            }
            // AeroMount's unlocked-vault provider is constructed directly from an
            // in-memory `ReadableVault`, never from a persisted profile config.
            ProviderType::AeroVaultMount => Err(ProviderError::InvalidConfig(
                "AeroVaultMount is not created through the provider factory".to_string(),
            )),
            ProviderType::Peer => {
                let peer_config = peer::PeerProviderConfig::from_provider_config(config)?;
                Ok(Box::new(PeerProvider::new(peer_config)))
            }
            // MTP connect is fingerprint match → mtp_open_device (PLACES or a
            // saved device profile), not ProviderFactory host+password create.
            // APPENDIX-MTP / APPENDIX-DEVICE-PROFILES.
            ProviderType::Mtp => Err(ProviderError::InvalidConfig(
                "MTP is opened via mtp_open_device / device profile match, not ProviderFactory host connect"
                    .to_string(),
            )),
        }
    }

    /// Get list of all supported provider types
    #[allow(dead_code)]
    pub fn supported_types() -> Vec<ProviderType> {
        vec![
            ProviderType::Ftp,
            ProviderType::Ftps,
            ProviderType::Sftp,
            ProviderType::WebDav,
            ProviderType::S3,
            ProviderType::AeroCloud,
            ProviderType::GoogleDrive,
            ProviderType::Dropbox,
            ProviderType::OneDrive,
            ProviderType::Mega,
            ProviderType::Box,
            ProviderType::PCloud,
            ProviderType::Azure,
            ProviderType::Filen,
            ProviderType::FourShared,
            ProviderType::ZohoWorkdrive,
            ProviderType::Internxt,
            ProviderType::Jottacloud,
            ProviderType::KDrive,
            ProviderType::DrimeCloud,
            ProviderType::FileLu,
            ProviderType::Koofr,
            ProviderType::OpenDrive,
            ProviderType::YandexDisk,
            ProviderType::GitHub,
            ProviderType::GitLab,
            ProviderType::Swift,
            ProviderType::GooglePhotos,
            ProviderType::Immich,
            ProviderType::ImageKit,
            ProviderType::Uploadcare,
            ProviderType::Backblaze,
            ProviderType::Cloudinary,
            ProviderType::Peer,
        ]
    }
}

// =========================================================================
// Resume Download Helpers
// =========================================================================

/// Stream an HTTP response body into a `ResumableFile`, tracking progress.
///
/// This is the shared implementation used by all HTTP-based providers for resume
/// downloads. The caller is responsible for:
/// 1. Detecting the `.aerotmp` offset via `ResumableFile::open()`
/// 2. Sending the HTTP request with `Range: bytes=<offset>-` header
/// 3. Handling 200 (restart) vs 206 (resume) vs 416 (range error)
/// 4. Passing the response and the `ResumableFile` to this function
///
/// Progress reports `(transferred_total, total_size)` where `transferred_total`
/// includes the pre-existing offset bytes.
pub async fn stream_response_to_resumable(
    response: reqwest::Response,
    resumable: &mut atomic_write::ResumableFile,
    total_size: u64,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    use futures_util::StreamExt;

    let mut stream = response.bytes_stream();
    let mut transferred = resumable.offset();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        resumable
            .write_all(&chunk)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        transferred += chunk.len() as u64;
        if let Some(ref progress) = on_progress {
            progress(transferred, total_size);
        }
    }

    Ok(())
}

/// Perform a resumable HTTP GET download for any provider that uses reqwest.
///
/// This handles the full lifecycle:
/// 1. Check for existing `.aerotmp` partial file
/// 2. Send GET with Range header if partial data exists
/// 3. Handle 200 (fresh) / 206 (resume) / 416 (completed or range error)
/// 4. Stream to `ResumableFile` and commit on success
///
/// `build_request` is a closure that takes an optional `Range` header value
/// (e.g. `"bytes=12345-"`) and returns a configured `reqwest::RequestBuilder`.
/// This allows each provider to add its own auth headers.
pub async fn http_resumable_download<F>(
    local_path: &str,
    build_request: F,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError>
where
    F: FnOnce(Option<&str>) -> reqwest::RequestBuilder,
{
    use reqwest::StatusCode;

    let mut resumable = atomic_write::ResumableFile::open(local_path)
        .await
        .map_err(ProviderError::IoError)?;

    let offset = resumable.offset();
    let range_header = if offset > 0 {
        Some(format!("bytes={}-", offset))
    } else {
        None
    };

    let response = build_request(range_header.as_deref())
        .send()
        .await
        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

    match response.status() {
        StatusCode::PARTIAL_CONTENT => {
            // 206: server honored our Range: append to existing data
            let content_len = response.content_length().unwrap_or(0);
            let total_size = offset + content_len;
            stream_response_to_resumable(response, &mut resumable, total_size, on_progress).await?;
            resumable.commit().await.map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
            })?;
            Ok(())
        }
        StatusCode::OK => {
            // 200: server ignored Range or fresh download: restart from scratch
            if offset > 0 {
                // We had partial data but server sent full content: discard and restart
                let _ = resumable.discard().await;
                let mut fresh = atomic_write::ResumableFile::open_fresh(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let total_size = response.content_length().unwrap_or(0);
                stream_response_to_resumable(response, &mut fresh, total_size, on_progress).await?;
                fresh.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
            } else {
                let total_size = response.content_length().unwrap_or(0);
                stream_response_to_resumable(response, &mut resumable, total_size, on_progress)
                    .await?;
                resumable.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
            }
            Ok(())
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            // 416: offset past end: file may already be complete
            // Discard partial and restart
            let _ = resumable.discard().await;
            Err(ProviderError::TransferFailed(
                "Range not satisfiable: file may have changed on server".to_string(),
            ))
        }
        StatusCode::NOT_FOUND => {
            let _ = resumable.discard().await;
            Err(ProviderError::NotFound(local_path.to_string()))
        }
        status => {
            // Keep partial data for retry
            Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_factory_supported_types() {
        let types = ProviderFactory::supported_types();
        assert!(types.contains(&ProviderType::Ftp));
        assert!(types.contains(&ProviderType::WebDav));
        assert!(types.contains(&ProviderType::S3));
    }

    #[test]
    fn unescape_xml_entities_decodes_standard_entities() {
        assert_eq!(
            unescape_xml_entities("The key &apos;abc123&apos; is not valid"),
            "The key 'abc123' is not valid"
        );
        assert_eq!(unescape_xml_entities("a &amp; b"), "a & b");
        assert_eq!(unescape_xml_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(unescape_xml_entities("say &quot;hi&quot;"), "say \"hi\"");
        assert_eq!(unescape_xml_entities("&#39;x&#39;"), "'x'");
        // No ampersand: returns input unchanged (no-op for plain/JSON errors).
        assert_eq!(unescape_xml_entities("plain error"), "plain error");
    }

    #[test]
    fn sanitize_api_error_unescapes_s3_message() {
        // F-011: an S3 <Message> with XML entities lands in a JSON error field
        // as raw UTF-8, not &apos;.
        let out = sanitize_api_error("The key &apos;003d90ca&apos; is not valid");
        assert_eq!(out, "The key '003d90ca' is not valid");
        assert!(!out.contains("&apos;"));
    }

    /// Row 4: an over-long first line is clipped near the 200-char cap with an
    /// ellipsis (defence against multi-KB provider error bodies in the UI/log).
    #[test]
    fn sanitize_api_error_truncates_long_first_line() {
        let long = format!("ERR {}", "x".repeat(300));
        let out = sanitize_api_error(&long);
        assert!(out.starts_with("ERR "));
        assert!(out.ends_with("..."), "over-long line gets an ellipsis");
        assert!(
            out.len() <= 210,
            "clipped near the 200-char cap, got {}",
            out.len()
        );
    }

    /// Row 4: only the first line survives a multi-line body, and an embedded
    /// API key is redacted (defence-in-depth via ai::sanitize_error_message).
    #[test]
    fn sanitize_api_error_keeps_first_line_and_redacts_secrets() {
        let body = "Auth failed: sk-ant-1234567890abcdefghij rejected\nstack trace line 2";
        let out = sanitize_api_error(body);
        assert!(
            !out.contains("sk-ant-1234567890abcdefghij"),
            "the API key must be redacted"
        );
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("stack trace"), "only the first line is kept");
    }

    /// Row 4: an empty body degrades to a stable placeholder, never panics.
    #[test]
    fn sanitize_api_error_empty_body_falls_back() {
        assert_eq!(sanitize_api_error(""), "unknown error");
    }
}
