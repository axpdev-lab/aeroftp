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
pub mod bulk;
pub mod checksum_matrix;
pub mod cloudinary;
pub mod directory;
pub mod drime_cloud;
pub mod dropbox;
pub mod filelu;
pub mod filen;
pub mod fourshared;
pub mod ftp;
pub mod ftp_listing;
pub mod ftp_site;
pub mod ftp_site_template;
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
#[cfg(test)]
mod path_resolution_guard;
pub mod pcloud;
pub mod peer;
#[cfg(test)]
pub(crate) mod probe_fake;
pub mod proton;
#[cfg(feature = "proton-native")]
pub mod proton_native;
pub mod redirect_policy;
pub mod retry_after;
pub mod s3;
pub(crate) mod s3_delta;
pub mod s3_delta_baseline;
pub mod s3_delta_plan;
pub mod served_file;
pub mod sftp;
#[cfg(test)]
mod share_link_capability_guard;
pub mod sts;
pub mod swift;
pub mod totp_helper;
pub mod tpslimit;
pub mod twake;
pub mod types;
pub mod upload_progress;
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

/// How a profile signs in to Proton Drive's native client, in the words of
/// every message that asks for it: the app's button and the CLI command, both,
/// so a message never sends a CLI user to a GUI they may not have, or the
/// reverse. A macro, so constant texts can `concat!` it. It lives here, not in
/// `proton_native`, because code built without the `proton-native` feature
/// (the rclone import) compiles texts that name it too.
macro_rules! proton_sign_in_hint {
    () => {
        "sign in with Proton in the AeroFTP app, or run `aeroftp-cli proton sign-in <profile>`"
    };
}
pub(crate) use proton_sign_in_hint;

/// [`proton_sign_in_hint!`] for texts built at run time.
pub const PROTON_SIGN_IN_HINT: &str = proton_sign_in_hint!();

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
pub use http_retry::{send_with_retry, send_with_retry_replayable, HttpRetryConfig};
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
pub use proton::{ProtonCliProvider, ProtonConfig, ProtonConnectionMode};
pub use s3::S3Provider;
pub use sftp::SftpProvider;
pub use swift::SwiftProvider;
pub use twake::TwakeProvider;
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

/// Classify a failed HTTP request the same way in every provider: a request
/// that got no reply inside the client's limits (the connect timeout, or the
/// read timeout while it waited for the answer) is [`ProviderError::Timeout`],
/// whatever the call site calls its other failures. Everything else goes to
/// `otherwise`, which keeps the call site's own variant and wording.
///
/// The call sites map on the text, and reqwest's `Display` for a timeout is
/// the generic "error sending request for url (...)": the timeout is only in
/// the source chain. Measured against a listener that accepts and never
/// writes, `aeroftp-cli stat` on WebDAV waited the 1800 s read timeout and
/// then exited 1, "Connection failed ... verify the service is running",
/// instead of 8, the exit code the CLI documents for a timeout.
pub fn http_send_error(
    e: reqwest::Error,
    otherwise: impl FnOnce(reqwest::Error) -> ProviderError,
) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout
    } else {
        otherwise(e)
    }
}

/// [`http_send_error`] as a `map_err` for a reqwest result, which is how the
/// HTTP providers map their requests: `.map_http_err(|e| ...)` in place of
/// `.map_err(|e| ...)`, with the same closure for every failure that is not
/// a timeout. It exists only for `reqwest::Error`, so it cannot land on a
/// result whose error says nothing about a timeout.
pub trait HttpResultExt<T> {
    fn map_http_err(
        self,
        otherwise: impl FnOnce(reqwest::Error) -> ProviderError,
    ) -> Result<T, ProviderError>;
}

impl<T> HttpResultExt<T> for Result<T, reqwest::Error> {
    fn map_http_err(
        self,
        otherwise: impl FnOnce(reqwest::Error) -> ProviderError,
    ) -> Result<T, ProviderError> {
        self.map_err(|e| http_send_error(e, otherwise))
    }
}

/// The rule of [`http_send_error`] one level up: a caller that wraps an error
/// its own request already classified (a connect that files every failure of
/// a step as "authentication failed", a step that adds its own context)
/// passes a [`ProviderError::Timeout`] through as it is, so the retry, the
/// exit code and the session handling still see a timeout. Every other error
/// goes to `otherwise`.
///
/// Not for a step that leaves an operation half done (the second rename of a
/// move, the delete after a copy): there the error describes where things
/// are now, and a timeout read as "retry" would repeat the first half.
pub fn keep_timeout(
    e: ProviderError,
    otherwise: impl FnOnce(ProviderError) -> ProviderError,
) -> ProviderError {
    match e {
        ProviderError::Timeout => e,
        other => otherwise(other),
    }
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
    body_bytes_with_limit(resp, limit, |e| {
        http_send_error(e, |e| ProviderError::TransferFailed(e.to_string()))
    })
    .await
}

/// [`response_bytes_with_limit`] for the window of a ranged read: a body the
/// connection dropped halfway is a [`ProviderError::NetworkError`]
/// ([`multi_thread::range_read_error`]), so the window is resumed from what it
/// wrote instead of failing the download.
pub(crate) async fn range_window_with_limit(
    resp: reqwest::Response,
    limit: u64,
) -> Result<Vec<u8>, ProviderError> {
    body_bytes_with_limit(resp, limit, multi_thread::range_read_error).await
}

/// The reader behind [`response_bytes_with_limit`], with the mapping of a
/// failed body read left to the caller.
async fn body_bytes_with_limit(
    resp: reqwest::Response,
    limit: u64,
    body_error: impl FnOnce(reqwest::Error) -> ProviderError,
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
                return Err(body_error(e));
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
    /// Largest single file the provider accepts, in bytes, when its own
    /// documentation states one. `None` means no documented limit, never
    /// "unlimited" and never a guess: an invented cap would tell the user a
    /// file cannot be uploaded when it can (#347).
    pub max_file_size: Option<u64>,
    /// Longest file name the provider accepts, in UTF-8 bytes, when documented.
    pub max_name_bytes: Option<u32>,
    /// Longest file name the provider accepts, in characters, when documented.
    pub max_name_chars: Option<u32>,
    /// Longest whole path (or object key) the provider accepts, in UTF-8
    /// bytes, when the documentation states the limit for the path rather
    /// than for one name.
    pub max_path_bytes: Option<u32>,
    /// The same, in characters.
    pub max_path_chars: Option<u32>,
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
            max_file_size: None,
            max_name_bytes: None,
            max_name_chars: None,
            max_path_bytes: None,
            max_path_chars: None,
        }
    }
}

/// Single-file and file-name limits a provider documents itself (#347).
///
/// The Compare tab warns about a file that exceeds them before a sync, so a
/// value here must make that warning TRUE for every user of the provider. The
/// rules that follow from it, applied to the official pages cited below
/// (read 2026-09-24):
///
/// - Only the provider's own documentation counts. No value = no warning.
/// - A plan-dependent limit uses the highest plan: a file above it cannot fit
///   on any plan, whatever the user pays for.
/// - When two official pages disagree, the larger value wins.
/// - "GB" and "TB" are read as GiB and TiB, the larger reading.
/// - A limit stated for the whole path or object key goes in the path fields,
///   checked against the destination path, not against the file name.
/// - Limits an operator can change (OpenStack Swift, self-hosted GitLab) and
///   plans with a "custom" ceiling are left out.
///
/// S3 is not here: the AWS limits hold only for AWS, so the S3 provider sets
/// them itself when its endpoint is AWS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocumentedFileLimits {
    pub max_file_size: Option<u64>,
    pub max_name_bytes: Option<u32>,
    pub max_name_chars: Option<u32>,
    pub max_path_bytes: Option<u32>,
    pub max_path_chars: Option<u32>,
}

const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;

pub fn documented_file_limits(provider: ProviderType) -> DocumentedFileLimits {
    let size = |bytes: u64| DocumentedFileLimits {
        max_file_size: Some(bytes),
        ..Default::default()
    };
    match provider {
        // "Maximum file size: 5,120 GB"
        // https://developers.google.com/workspace/drive/api/reference/rest/v3/files/create
        ProviderType::GoogleDrive => size(5120 * GIB),
        // API: photos 200 MB, videos 20 GB; "The file name, including the file
        // extension, shouldn't be more than 255 characters."
        // https://developers.google.com/photos/library/guides/upload-media
        // https://developers.google.com/photos/library/reference/rest/v1/mediaItems/batchCreate
        ProviderType::GooglePhotos => DocumentedFileLimits {
            max_file_size: Some(20 * GIB),
            max_name_chars: Some(255),
            ..Default::default()
        },
        // Help center: 2 TB (2,199,019,061,248 bytes) per file; the API spec
        // says 350 GB per upload session. The larger one, by the rule above.
        // https://help.dropbox.com/sync/upload-limitations
        ProviderType::Dropbox => size(2_199_019_061_248),
        // "250 GB - File upload limit"; "The entire decoded file path,
        // including the file name, can't exceed 400 characters."
        // https://learn.microsoft.com/en-us/office365/servicedescriptions/sharepoint-online-service-description/sharepoint-online-limits
        // https://support.microsoft.com/en-us/onedrive/what-are-file-path-length-limits
        ProviderType::OneDrive => DocumentedFileLimits {
            max_file_size: Some(250 * GIB),
            max_path_chars: Some(400),
            ..Default::default()
        },
        // Highest plan "Enterprise Advanced: 500 GB"; "Box only supports file
        // or folder names that are 255 characters or less."
        // https://support.box.com/hc/en-us/articles/360043697314
        // https://support.box.com/hc/en-us/articles/360044196773
        ProviderType::Box => DocumentedFileLimits {
            max_file_size: Some(500 * GIB),
            max_name_chars: Some(255),
            ..Default::default()
        },
        // No size limit beyond free storage; "All filenames must be between 1
        // and 255 characters". https://proton.me/support/drive-filenames
        ProviderType::Proton => DocumentedFileLimits {
            max_name_chars: Some(255),
            ..Default::default()
        },
        // Free 2 GB, Premium "up to 100 GB each".
        // https://www.4shared.com/web/helpCenter/ffqSEuqpUce
        ProviderType::FourShared => size(100 * GIB),
        // Highest plan "Business Plan: ... a single file of up to 250 GB".
        // https://help.zoho.com/portal/en/kb/workdrive/manage-files-and-folders/articles/upload-large-files-in-chunks
        ProviderType::ZohoWorkdrive => size(250 * GIB),
        // Highest plan "Ultimate: 100GB per file".
        // https://help.internxt.com/en/articles/6534031
        ProviderType::Internxt => size(100 * GIB),
        // "1000 GB per file sent via the desktop app, the web app, and the API"
        // https://www.infomaniak.com/en/support/faq/2387/manage-kdrive-storage
        ProviderType::KDrive => size(1000 * GIB),
        // No size limit; "The maximum file name length is 255 characters,
        // and the total path length cannot exceed 1023 characters."
        // https://koofr.eu/help/koofr_files/what-is-the-max-file-name-length-on-koofr/
        ProviderType::Koofr => DocumentedFileLimits {
            max_name_chars: Some(255),
            max_path_chars: Some(1023),
            ..Default::default()
        },
        // "With any Yandex 360 plan: 50 GB."
        // https://yandex.com/support/disk/uploading.html
        ProviderType::YandexDisk => size(50 * GIB),
        // "Maximum size of a block blob | 50,000 x 4,000 MiB"; a blob name
        // "cannot be more than 1,024 characters long".
        // https://learn.microsoft.com/en-us/azure/storage/blobs/scalability-targets
        // https://learn.microsoft.com/en-us/rest/api/storageservices/naming-and-referencing-containers--blobs--and-metadata
        ProviderType::Azure => DocumentedFileLimits {
            max_file_size: Some(50_000 * 4_000 * (1 << 20)),
            max_path_chars: Some(1024),
            ..Default::default()
        },
        // "Large files can range in size from 5 MB to 10 TB."; "Names should be
        // a UTF-8 string up to 1024 bytes".
        // https://www.backblaze.com/docs/cloud-storage-large-files
        // https://www.backblaze.com/docs/cloud-storage-files
        ProviderType::Backblaze => DocumentedFileLimits {
            max_file_size: Some(10 * TIB),
            max_path_bytes: Some(1024),
            ..Default::default()
        },
        // Size is plan-dependent up to "Custom"; public_id "Can be up to 255
        // characters". https://cloudinary.com/documentation/image_upload_api_reference
        ProviderType::Cloudinary => DocumentedFileLimits {
            max_name_chars: Some(255),
            ..Default::default()
        },
        // No limit to warn about, each for a stated reason. The match has no
        // wildcard on purpose: a new provider type does not compile until
        // someone answers for it.
        //
        // Set by the provider itself, on AWS endpoints only (AWS_S3_FILE_LIMITS).
        ProviderType::S3 => DocumentedFileLimits::default(),
        // Decided by each server or by the operator, not by a service.
        ProviderType::Ftp
        | ProviderType::Ftps
        | ProviderType::Sftp
        | ProviderType::WebDav
        | ProviderType::Swift
        | ProviderType::GitLab
        | ProviderType::Immich => DocumentedFileLimits::default(),
        // Documented as unlimited, or unlimited on the highest plan.
        ProviderType::Mega | ProviderType::Filen | ProviderType::FileLu => {
            DocumentedFileLimits::default()
        }
        // No official number found (marketing page only, site not readable,
        // or custom plans without a stated ceiling).
        ProviderType::PCloud
        | ProviderType::Jottacloud
        | ProviderType::DrimeCloud
        | ProviderType::OpenDrive
        | ProviderType::ImageKit
        | ProviderType::Uploadcare
        | ProviderType::Twake => DocumentedFileLimits::default(),
        // Two write paths with two limits: a repository file goes through the
        // Contents API (100 MB, refused by the provider itself before the
        // upload, github/mod.rs MAX_CONTENT_SIZE), a release asset has 2 GiB.
        // One number per provider type would warn wrongly on one of them.
        // https://docs.github.com/en/rest/repos/contents#create-or-update-file-contents
        // https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
        ProviderType::GitHub => DocumentedFileLimits::default(),
        // Not a remote a sync uploads to through this path.
        ProviderType::AeroCloud
        | ProviderType::AeroVaultMount
        | ProviderType::Peer
        | ProviderType::Mtp
        | ProviderType::Local => DocumentedFileLimits::default(),
    }
}

/// AWS S3: "Maximum object size | 48.8 TiB", which is 10,000 parts of 5 GiB,
/// 50,000 GiB exactly (the upload page rounds it to "50 TB"); an object key
/// has "a maximum length of 1,024 bytes", prefix included.
/// https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html
/// https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-keys.html
pub const AWS_S3_FILE_LIMITS: DocumentedFileLimits = DocumentedFileLimits {
    max_file_size: Some(50_000 * GIB),
    max_name_bytes: None,
    max_name_chars: None,
    max_path_bytes: Some(1024),
    max_path_chars: None,
};

impl TransferOptimizationHints {
    /// Fill the limits the provider left empty from `limits`.
    pub fn with_documented_limits(mut self, limits: DocumentedFileLimits) -> Self {
        self.max_file_size = self.max_file_size.or(limits.max_file_size);
        self.max_name_bytes = self.max_name_bytes.or(limits.max_name_bytes);
        self.max_name_chars = self.max_name_chars.or(limits.max_name_chars);
        self.max_path_bytes = self.max_path_bytes.or(limits.max_path_bytes);
        self.max_path_chars = self.max_path_chars.or(limits.max_path_chars);
        self
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

/// Whether `id` can be placed in a URL path as a share-link id: the ids the
/// providers hand out are alphanumeric with `-` and `_`, so anything else is
/// refused rather than encoded.
pub(crate) fn is_link_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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

/// The granularity at which each backend's listed modification times can be
/// compared, `None` when they cannot be. The match is exhaustive on purpose: a
/// new backend does not compile until somebody states what its dates are.
///
/// Measured from what each `list` puts in `RemoteEntry.modified` (audit of
/// 2026-09-25). A value is the granularity of a time with a known zone; it
/// says nothing about whether the time is the file's own mtime or its upload
/// time, which the one-way rule (`destination_is_current`) already absorbs.
/// `None` is declared for a date with no zone, a localized display string, a
/// shape nothing reads, or no date at all.
pub fn declared_mtime_precision(provider: ProviderType) -> Option<std::time::Duration> {
    use std::time::Duration;
    const SECOND: Option<Duration> = Some(Duration::from_secs(1));
    const MILLI: Option<Duration> = Some(Duration::from_millis(1));
    const MICRO: Option<Duration> = Some(Duration::from_micros(1));
    const NANO: Option<Duration> = Some(Duration::from_nanos(1));
    match provider {
        // Without a session nothing says whether MLSD (UTC, seconds) or LIST
        // (server-local, no zone) will be read; `FtpProvider` answers per
        // session and the legacy `FtpManager` only reads LIST.
        ProviderType::Ftp | ProviderType::Ftps => None,
        // `attrs.mtime`, Unix seconds.
        ProviderType::Sftp => SECOND,
        // `getlastmodified`, RFC 1123.
        ProviderType::WebDav => SECOND,
        // `LastModified` carries milliseconds, but the ListObjects value is
        // the upload time and whole seconds are what the sync compares.
        ProviderType::S3 => SECOND,
        // A sync configuration, never a storage backend of its own.
        ProviderType::AeroCloud => None,
        // `modifiedTime`, RFC 3339 with milliseconds.
        ProviderType::GoogleDrive => MILLI,
        // `server_modified`, whole seconds.
        ProviderType::Dropbox => SECOND,
        // `lastModifiedDateTime`, RFC 3339.
        ProviderType::OneDrive => SECOND,
        // Native: node `ts`, Unix seconds. `MegaCmdProvider` overrides this:
        // `mega-ls -l` prints a date with no zone.
        ProviderType::Mega => SECOND,
        // `claimedModificationTime`, milliseconds.
        ProviderType::Proton => MILLI,
        // `modified_at`, RFC 3339 with an offset.
        ProviderType::Box => SECOND,
        // `modified`, RFC 2822.
        ProviderType::PCloud => SECOND,
        // `Last-Modified`, RFC 1123.
        ProviderType::Azure => SECOND,
        // `lastModified` in milliseconds, cut to seconds.
        ProviderType::Filen => SECOND,
        // `modified` passed through with a shape nothing in the code pins.
        ProviderType::FourShared => None,
        // `modified_time_i18` is a localized display string.
        ProviderType::ZohoWorkdrive => None,
        // `modificationTime`, ISO 8601 with milliseconds.
        ProviderType::Internxt => MILLI,
        // `last_modified_at`, Unix seconds.
        ProviderType::KDrive => SECOND,
        // `<modified>`, parsed to seconds.
        ProviderType::Jottacloud => SECOND,
        // `updated_at`, parsed to seconds.
        ProviderType::DrimeCloud => SECOND,
        // `uploaded` is `YYYY-MM-DD HH:MM:SS` with no zone.
        ProviderType::FileLu => None,
        // `modified` in milliseconds, cut to seconds.
        ProviderType::Koofr => SECOND,
        // `DateModified`, Unix seconds.
        ProviderType::OpenDrive => SECOND,
        // `modified`, RFC 3339 with an offset.
        ProviderType::YandexDisk => SECOND,
        // The Contents API lists no date; only `stat` looks one up.
        ProviderType::GitHub => None,
        ProviderType::GitLab => None,
        // `last_modified`, microseconds in UTC.
        ProviderType::Swift => MICRO,
        // `creationTime`, RFC 3339.
        ProviderType::GooglePhotos => SECOND,
        // `fileModifiedAt`, milliseconds.
        ProviderType::Immich => MILLI,
        // `updatedAt`, milliseconds.
        ProviderType::ImageKit => MILLI,
        // `datetime_uploaded`, microseconds.
        ProviderType::Uploadcare => MICRO,
        // `uploadTimestamp`, milliseconds.
        ProviderType::Backblaze => MILLI,
        // `created_at`, whole seconds.
        ProviderType::Cloudinary => SECOND,
        // Vault entries carry no date.
        ProviderType::AeroVaultMount => None,
        // The peer's own file mtime, RFC 3339 with nanoseconds.
        ProviderType::Peer => NANO,
        // libmtp reports bare Unix seconds, WPD nothing, gvfs RFC 3339: the
        // backend is chosen at run time and two of the three do not read.
        ProviderType::Mtp => None,
        // The file's own mtime, read with nanoseconds and listed in RFC 3339
        // with them; an upload sets it from the source.
        ProviderType::Local => NANO,
        // `updated_at`, RFC 3339 (Cozy); uploads write the local mtime to
        // the second.
        ProviderType::Twake => SECOND,
    }
}

/// The measurement note of the Filen Desktop presets (S3 and WebDAV): both are
/// `rclone serve` bridges on this machine with a local cache in front of Filen.
pub const FILEN_DESKTOP_BRIDGE_NOTE: &str = "Filen Desktop preset: these figures measure the local Filen Desktop bridge and its cache, not the Filen servers. The bridge answers an upload from its cache and sends it on to Filen afterwards; a write-back wait printed apart is that send.";

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

    /// The provider owns the upload retry budget, including individual parts.
    /// Executors must not wrap it in whole-file retries: those multiply the
    /// budget and may duplicate versions after an ambiguous upload response.
    fn manages_upload_retries(&self) -> bool {
        false
    }

    /// Get display name for this provider instance
    fn display_name(&self) -> String;

    /// Folders a used-storage scan of `root` walks. The default is `root`
    /// itself. A provider whose root is not one tree of the user's own files
    /// (Proton Drive lists shared views and other people's files beside them)
    /// names the folders that are the user's storage instead.
    fn used_scan_roots(&self, root: &str) -> Vec<String> {
        vec![root.to_string()]
    }

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

    /// [`Self::download`] for a caller that already knows the file size (a
    /// listing entry, its own stat). A provider that would otherwise ask the
    /// server for the size before deciding on a multi-stream download uses
    /// the hint instead, and skips that round trip when the hint is below the
    /// multi-thread cutoff; an unknown size (`None`) keeps the probe. The
    /// default is plain `download`.
    async fn download_with_size_hint(
        &mut self,
        remote_path: &str,
        local_path: &str,
        size_hint: Option<u64>,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let _ = size_hint;
        self.download(remote_path, local_path, on_progress).await
    }

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

    /// Optional native multi-file operation. Defaults are unsupported: enabling
    /// a capability is a promise to return positional per-file outcomes and to
    /// stop owned wire work when the future is dropped. Never replay a partially
    /// completed bulk. An item the adapter cannot carry comes back as
    /// `NotAttempted`, and the caller runs its single-file path for it; `Err`
    /// means the whole call was refused before any item reached the wire.
    /// Adapters may expose completion-only progress if the external program
    /// has no trustworthy per-file streaming interface.
    fn supports_bulk_upload(&self) -> bool {
        false
    }

    fn supports_bulk_download(&self) -> bool {
        false
    }

    async fn upload_bulk(
        &mut self,
        _items: &[bulk::BulkTransferItem],
        _progress: Option<bulk::BulkProgress>,
    ) -> Result<Vec<bulk::BulkTransferOutcome>, ProviderError> {
        Err(ProviderError::NotSupported("bulk upload".to_string()))
    }

    async fn download_bulk(
        &mut self,
        _items: &[bulk::BulkTransferItem],
        _progress: Option<bulk::BulkProgress>,
    ) -> Result<Vec<bulk::BulkTransferOutcome>, ProviderError> {
        Err(ProviderError::NotSupported("bulk download".to_string()))
    }

    /// Create a directory
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Delete a file
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Remove an empty directory. A directory that holds anything is refused
    /// with [`ProviderError::DirectoryNotEmpty`] and stays with its content;
    /// a directory is removed with its content only by [`Self::rmdir_recursive`].
    ///
    /// Before the 4.2.1 review most backends removed the directory whole
    /// here, so a caller that had listed it empty could still take along an
    /// object the listing left out, or one written in between. How each
    /// backend keeps the promise now (the round 2 audit of #979):
    ///
    /// | Backend | How `rmdir` refuses a directory that is not empty |
    /// |---|---|
    /// | FTP, FTPS | `RMD`, refused by the server; a reply that says "not empty" is `DirectoryNotEmpty`, a bare 550 stays a server error |
    /// | SFTP | `SSH_FXP_RMDIR`, refused by the server; a reply that says "not empty" is `DirectoryNotEmpty` |
    /// | Local, MTP mounted | `remove_dir`, refused by the operating system (`ENOTEMPTY`) |
    /// | MTP (WPD) | `Delete` with `PORTABLE_DEVICE_DELETE_NO_RECURSION`, refused by the device |
    /// | Box | `DELETE /folders/{id}?recursive=false`, refused with `folder_not_empty` |
    /// | pCloud | `deletefolder` (not `deletefolderrecursive`), refused with result 2006 |
    /// | Cloudinary | `DELETE /folders/{path}`, refused by the API |
    /// | S3 | lists the prefix and refuses when any object but the directory marker is under it; removes the marker only |
    /// | Azure Blob | lists the prefix and refuses when any blob but the markers is under it; removes the `<path>/` marker and the `hdi_isfolder` stub (`If-Match`) only |
    /// | Swift | lists the prefix and refuses; removes the pseudo-folder marker only |
    /// | Backblaze B2 | lists the prefix and refuses; hides the `.bzEmpty` placeholder only |
    /// | OneDrive | reads the folder (`childCount`) and refuses; deletes with `If-Match` on the eTag read |
    /// | Twake Drive | lists every page and refuses; then `DELETE /files/{id}` |
    /// | Google Drive, Dropbox, WebDAV, Filen, Drime, 4shared, FileLu, Jottacloud, kDrive, Koofr, Zoho WorkDrive, Yandex Disk, OpenDrive, Proton Drive, Internxt, ImageKit, MEGA (MEGAcmd and native), GitHub, GitLab, Immich | lists the directory ([`Self::refuse_non_empty_dir`]) and refuses; then the API's own delete, which takes the content along and so is sent only to a directory that listed empty |
    /// | AeroCrypt / rclone-crypt overlay | lists through the overlay and refuses; removes its own sentinels, then the inner backend's `rmdir` |
    /// | Compress overlay | the inner backend's `rmdir` |
    /// | Uploadcare, Google Photos, peer, vault | not supported or read only |
    ///
    /// Where the check is a listing, an object written between the listing
    /// and the delete goes with the directory: the API offers no delete that
    /// refuses on its own, and the window is one round trip.
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError>;

    /// The listing check of [`Self::rmdir`] for a backend whose own delete
    /// takes a directory's content along: lists `path` and refuses it with
    /// [`ProviderError::DirectoryNotEmpty`] when the listing shows anything,
    /// dotfiles included. A listing that fails is that failure, with nothing
    /// removed.
    async fn refuse_non_empty_dir(&mut self, path: &str) -> Result<(), ProviderError> {
        let children = self.list(path).await?;
        if children.is_empty() {
            Ok(())
        } else {
            Err(directory_not_empty(path, children.len()))
        }
    }

    /// Delete a directory recursively (with all contents)
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError>;

    /// Rename/move a file or directory.
    ///
    /// This is the verb behind the user's "rename", and it deliberately does
    /// NOT promise to replace an existing destination. On SFTP the server's
    /// refusal is the only thing standing between `mv a b` and the loss of a
    /// `b` that was already there, because no caller in this tree checks
    /// first: giving this method overwrite semantics would turn a visible
    /// error into a silent deletion. To put a file in place of another on
    /// purpose, use [`StorageProvider::replace`].
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError>;

    /// Put `from` in place of `to`, atomically where the backend can, whether
    /// or not `to` already exists.
    ///
    /// This is the verb behind "publish a staged temporary over the live
    /// file": the destination is expected to be there and replacing it is the
    /// whole point, which is exactly what makes it a different question from
    /// [`rename`]. Keeping the two apart is what lets the replace path gain
    /// overwrite semantics without every `mv` gaining them too.
    ///
    /// The default forwards to `rename`, which is what every caller did
    /// before this method existed. A backend whose rename refuses an occupied
    /// destination must override this, or every replace onto an existing
    /// file fails: SFTP, the copy-based backends (S3, B2, Swift, Azure,
    /// Cloudinary, OpenDrive), FTP, ImageKit, pCloud, Yandex Disk and the
    /// MTP folder overwrite in one server step, MEGAcmd and Jottacloud send
    /// their move without the look their rename makes, OneDrive replaces in
    /// the request that moves, Google Drive uploads the new content as a
    /// revision of the file there, and MEGA, Filen, FileLu, Dropbox, Koofr,
    /// Drime and kDrive, which have neither, set the old item aside first
    /// (see [`set_aside_name`]), as WebDAV does, whose overwriting MOVE
    /// deletes the destination before it moves (RFC 4918 section 9.9.3). A
    /// backend with none of these keeps the default, whose refusal is the
    /// answer, and says so through [`StorageProvider::supports_atomic_replace`].
    ///
    /// A replace puts a file in place of a file or a folder in place of a
    /// folder. Across the two (see [`refuse_replace_across_types`]) it is
    /// refused with AlreadyExists before anything changes.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.rename(from, to).await
    }

    /// Whether [`replace`] can put one file in place of another without a
    /// moment in which neither is there.
    ///
    /// Ask this BEFORE staging a temporary, never after. A caller that
    /// uploads first and asks second has already left a file on the server,
    /// so an error that says "nothing was written" would be a lie, and
    /// [`ensure_atomic_replace`] exists to make asking first the easy path.
    ///
    /// The default answers `true`, which is the assumption every caller
    /// already made. It means "no known obstacle", not "verified": only a
    /// backend that has actually measured its own ground says otherwise:
    /// `SftpProvider`, which asks the server whether it offers
    /// `posix-rename@openssh.com`; the backends whose replace sets the old
    /// item aside (MEGA, Filen, FileLu, Dropbox, Koofr, Drime, kDrive,
    /// WebDAV, see [`StorageProvider::replace_sets_aside`]); those whose move over a file is not
    /// documented as one step (MEGAcmd, Jottacloud); those with no replace
    /// at all, whose rename refuses a taken name or who have no rename (each
    /// says why on its own answer); and ImageKit and OpenDrive, which
    /// overwrite only across folders while every caller stages its temporary
    /// in the target's own folder.
    ///
    /// [`replace`]: StorageProvider::replace
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(true)
    }

    /// What [`transfer_capabilities`] reports as `atomic_rename`: what this
    /// provider has established about an atomic [`replace`] without asking
    /// the server again. The default is its type's declaration,
    /// [`atomic_replace_baseline`]; a provider whose answer depends on its
    /// own state says so here (SFTP from what the server advertised). It
    /// must never claim more than [`supports_atomic_replace`] answers, and
    /// may claim less: that answer can mean "no known obstacle", this one
    /// only what is established.
    ///
    /// [`transfer_capabilities`]: StorageProvider::transfer_capabilities
    /// [`replace`]: StorageProvider::replace
    /// [`supports_atomic_replace`]: StorageProvider::supports_atomic_replace
    /// [`atomic_replace_baseline`]: crate::transfer_dag::capabilities::atomic_replace_baseline
    fn atomic_replace_capability(&self) -> crate::transfer_dag::Capability {
        crate::transfer_dag::capabilities::atomic_replace_baseline(self.provider_type())
    }

    /// Wait until a file this session just uploaded can be read back, and
    /// return how long that took, or `None` when there was nothing to wait
    /// for. A server that writes uploads back to its own storage after a
    /// delay (Filen Desktop's `rclone serve s3`) answers 404 for a few
    /// seconds; a benchmark calls this before it starts the download clock,
    /// so the wait is reported apart and not counted as download time. The
    /// download itself still waits on its own, so skipping this call loses
    /// only the split, never the file.
    async fn wait_until_readable(
        &mut self,
        _path: &str,
    ) -> Result<Option<std::time::Duration>, ProviderError> {
        Ok(None)
    }

    /// A caveat a benchmark or speed test prints next to its figures, once
    /// per run, when they do not measure what the profile's name suggests.
    /// `None` for an ordinary server.
    fn measurement_note(&self) -> Option<&'static str> {
        None
    }

    /// Whether [`replace`] puts a file over an existing one by setting the
    /// previous item aside first: it is renamed to [`set_aside_name`], the
    /// new one moves into its place, and only then is the old one deleted.
    /// The name is empty between the first two steps, so these backends
    /// answer `false` to [`StorageProvider::supports_atomic_replace`], but
    /// their replace does put a file over another and loses neither.
    ///
    /// This is the question behind the edit opt-in (`--allow-non-atomic`,
    /// `allow_non_atomic`), asked through [`ensure_edit_can_replace`] BEFORE
    /// anything is staged. A backend whose replace is its rename, which
    /// refuses a taken name, keeps the default `false`: there the opt-in
    /// would upload a temporary that the replace then refuses to publish.
    ///
    /// `true` on MEGA through the native API, Filen, FileLu, Dropbox, Koofr,
    /// Drime, kDrive and WebDAV (not on a single-file WebDAV profile). Not on
    /// MEGAcmd or Jottacloud: their replace is a server move over the file
    /// whose atomicity is not documented, not a set-aside. The overlays
    /// (crypt, compress) forward the inner answer. Onto a free name the
    /// replace of every one of them is its rename, which is what lets
    /// [`ensure_atomic_replace`] and [`ensure_marker_replace`] pass a publish
    /// where nothing is yet.
    ///
    /// [`replace`]: StorageProvider::replace
    fn replace_sets_aside(&self) -> bool {
        false
    }

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

    /// Whether a successful directory listing is an authoritative statement
    /// that every stored child was returned.
    ///
    /// Sync may use absence from an authoritative source listing to delete an
    /// orphan on the other side. Providers whose upstream listing can omit
    /// successfully stored objects must return `false`; uploads and ordinary
    /// browsing remain available, but a remote-to-local delete pass must fail
    /// closed.
    fn listing_is_authoritative(&self) -> bool {
        true
    }

    /// The shortest time this backend asks to leave between two full
    /// recursive scans of a synced folder, `None` when it asks for none.
    ///
    /// AeroCloud's scheduler runs a pair's full scan at the larger of the
    /// user's interval and this floor; local changes between two scans travel
    /// one path at a time, without a listing. A backend whose terms rule out
    /// frequent recursive traversals declares its floor here. An overlay
    /// forwards its inner provider's, so a crypt or compress layer cannot hide
    /// it.
    fn min_full_scan_interval(&self) -> Option<std::time::Duration> {
        None
    }

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

    /// Remove one of the links `list_share_links` returned for `path`, by the
    /// `id` it carried. A provider that keeps a single link per item has
    /// nothing to choose between and removes that one, which is the default;
    /// a provider that can hold several links on one item overrides this so
    /// that revoking one link does not remove another.
    async fn remove_share_link_by_id(
        &mut self,
        path: &str,
        _link_id: &str,
    ) -> Result<(), ProviderError> {
        self.remove_share_link(path).await
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

    /// Whether this provider can resume an interrupted transfer at all, which
    /// is the gate the CLI's `--partial` consults before calling
    /// [`Self::resume_upload`] or the resumable download path.
    ///
    /// The name of a wire command used to sit in this line, `REST`, which is
    /// FTP's. It has not described the meaning for a long time: nineteen
    /// providers that never speak FTP answer true here, over HTTP ranges,
    /// multipart uploads and SFTP offsets. The stale wording is worth
    /// recording rather than just deleting, because it is what made an
    /// omission invisible: a provider author reading "REST command" has no
    /// reason to think the question is being asked of them.
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

    /// How finely the modification times this backend lists can be compared,
    /// `None` when they cannot be compared at all. The sync reads it through
    /// [`crate::sync_core::mtime::ModifyWindow::resolve`]: the window is the
    /// coarser of this and 2 s, and `None` compares by size only. The default
    /// is the backend's declared value ([`declared_mtime_precision`]); a
    /// provider whose answer depends on the session (FTP with or without
    /// MLSD) overrides it, and an overlay forwards its inner provider's.
    fn mtime_precision(&self) -> Option<std::time::Duration> {
        declared_mtime_precision(self.provider_type())
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
    ///
    /// The digests are of the PLAINTEXT and are safe to compare against a
    /// local hash of the same file, which is what the sync engine does. A
    /// provider that cannot promise that (a crypt overlay) leaves this
    /// unsupported and answers [`stored_checksum`](Self::stored_checksum)
    /// instead.
    async fn checksum(&mut self, _path: &str) -> Result<HashMap<String, String>, ProviderError> {
        Err(ProviderError::NotSupported("checksum".to_string()))
    }

    /// [`checksum`](Self::checksum) for a caller that wants one algorithm,
    /// named by its canonical key (`md5`, `sha1`, `sha256`, ...).
    ///
    /// A backend that answers from digests it already stores returns what it
    /// has, so the default ignores the hint and the caller looks its key up in
    /// the map. A backend that computes the digest on request and lets the
    /// client choose the algorithm overrides it: FTP selects it with
    /// `OPTS HASH` before `HASH`, and would otherwise hash with whatever the
    /// server has selected.
    async fn checksum_for(
        &mut self,
        path: &str,
        _algorithm: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        self.checksum(path).await
    }

    /// Which digests this backend can produce without downloading the file.
    ///
    /// Describes the backend for a surface that has to decide what to offer;
    /// it drives no transfer or comparison decision, so a provider can report
    /// a capability here (a crypt overlay reporting ciphertext digests)
    /// without that leaking into sync, which keeps reading
    /// [`supports_checksum`](Self::supports_checksum).
    ///
    /// The default reads the shared matrix keyed by
    /// [`provider_type`](Self::provider_type), so a backend's stance lives in
    /// one exhaustively-matched table rather than scattered across impls.
    /// Only backends whose answer is genuinely per-connection override it:
    /// FTP narrows it to what `FEAT` advertised, SFTP to whether the SSH
    /// session is up, and the crypt overlay wraps the inner backend's.
    ///
    /// `path` is taken because under a crypt overlay the answer really does
    /// vary by path: a file inside the Overlays Path is stored encrypted and
    /// its digest covers ciphertext, while a file outside it is stored as-is
    /// and its digest is an ordinary plaintext one. Every other backend
    /// ignores the argument.
    fn checksum_capability(&self, _path: &str) -> ChecksumCapability {
        checksum_matrix::capability(self.provider_type())
    }

    /// Digests of the bytes AS STORED on the server.
    ///
    /// Identical to [`checksum`](Self::checksum) for every ordinary backend,
    /// where stored bytes and plaintext are the same thing. A crypt overlay
    /// overrides it to return the inner backend's digest of the ciphertext,
    /// which is a real answer to "did the stored object change" and a wrong
    /// answer to "does this match my local file"; the caller is told which it
    /// is by [`ChecksumCapability::ciphertext`] and must label it.
    async fn stored_checksum(
        &mut self,
        path: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        self.checksum(path).await
    }

    /// [`stored_checksum`](Self::stored_checksum) with the algorithm hint of
    /// [`checksum_for`](Self::checksum_for). A wrapper that overrides
    /// `stored_checksum` overrides this too, or the hint stops at the wrapper.
    async fn stored_checksum_for(
        &mut self,
        path: &str,
        algorithm: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        self.checksum_for(path, algorithm).await
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

    /// Whether a `clone_for_transfer` worker can serve several sequential
    /// transfers over ONE warm connection, so the shared transfer executor may
    /// park it in a reuse pool after a file and hand it to the next file
    /// instead of re-dialling (PD-FTP-2, the connection-reuse rclone amortises).
    ///
    /// Default `false`: the conservative per-file re-dial, unchanged for every
    /// transport that does not override this (SFTP, HTTP clone pools). FTP opts
    /// in because `download`/`upload` are safe to call repeatedly on one
    /// connected provider (`ensure_connected` is a no-op when connected and each
    /// call is a self-contained SIZE/RETR or STOR by absolute path). A worker is
    /// only ever parked after a SUCCESSFUL transfer, so a desynced stream is
    /// dropped rather than recycled.
    fn supports_transfer_worker_reuse(&self) -> bool {
        false
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
        caps.atomic_rename = self.atomic_replace_capability();
        caps.bulk_upload = crate::transfer_dag::Capability::from_bool(self.supports_bulk_upload());
        caps.bulk_download =
            crate::transfer_dag::Capability::from_bool(self.supports_bulk_download());

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

    /// Lower bound this provider enforces on the multi-thread download cutoff.
    /// `set_multi_thread_download` clamps to this value and the shared batch
    /// executor raises its segmented-download gate to it, so single-file and
    /// batch downloads agree. `0` (default) means the provider honors the
    /// caller's cutoff as-is.
    fn multi_thread_cutoff_floor(&self) -> u64 {
        0
    }

    /// Windows the single-file download path plans for `file_size` under the
    /// tuning set via `set_multi_thread_download`: the shared
    /// `plan_segment_count` rule with this provider's stored cutoff, stream
    /// cap and floor. `0`/`1` = single stream. Default: no multi-thread
    /// support. Providers overriding `set_multi_thread_download` should
    /// override this too and use it in their download gate, so the gate is
    /// the testable unit of the convergence table.
    fn planned_download_segments(&self, _file_size: u64) -> usize {
        0
    }

    /// Configure the SFTP read-ahead window for this provider instance.
    /// `None` explicitly disables read-ahead; `Some(n)` requests a bounded
    /// window. Providers other than SFTP ignore this setting.
    fn set_sftp_readahead(&mut self, _window: Option<usize>) {}

    /// Pin every later ranged read to one version of the object.
    ///
    /// A multi-stream download reads its windows in parallel, so an object
    /// replaced while they are in flight is assembled out of two versions and
    /// still has the length it should. Providers whose protocol carries a
    /// validator (an HTTP ETag) send it with each range, and the server
    /// refuses the read instead of serving the new bytes. Providers without
    /// one ignore this, and the caller compares the object before and after
    /// the transfer.
    fn set_range_validator(&mut self, _validator: Option<String>) {}

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

/// A `stat` answer that says the provider cannot describe the path, as
/// opposed to one that failed. S3, Azure, Swift and B2 see no directory
/// behind a path without its trailing slash (NotFound); Box and GitHub fail
/// to parse the answer for a folder (ParseError). A transient failure
/// (network, server, timeout) says nothing about the path, and acting on it
/// as if the path were a directory reached the directory of the same name.
pub fn stat_cannot_describe(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::NotFound(_) | ProviderError::NotSupported(_) | ProviderError::ParseError(_)
    )
}

/// The one trashed item a `delete_permanent` of `path` may purge, among
/// `matches`: the items of the path's exact name trashed from the path's
/// folder. That trash can hold several generations of one name, and nothing
/// in the listing tells which of them was this path, so purging the first one
/// listed could hard-delete an item nobody named. That is refused, as Proton
/// refuses it: the item stays in the trash, which the provider empties on its
/// own schedule.
pub(crate) fn the_one_trashed_item<T>(
    path: &str,
    mut matches: Vec<T>,
) -> Result<Option<T>, ProviderError> {
    if matches.len() > 1 {
        return Err(ProviderError::ServerError(format!(
            "Not purged: {} items in the trash were deleted from the folder of {path} under its name, and the trash cannot tell which one it is",
            matches.len()
        )));
    }
    Ok(matches.pop())
}

/// The refusal of a directory that holds `entries` entries, as every backend
/// whose `rmdir` lists first says it.
pub fn directory_not_empty(path: &str, entries: usize) -> ProviderError {
    ProviderError::DirectoryNotEmpty(format!(
        "{path} holds {entries} entr{}; delete it recursively to remove it with its content",
        if entries == 1 { "y" } else { "ies" }
    ))
}

/// Creates the folder `path` and every missing folder above it, as the
/// CLI's `put -r` does for its target before any upload: `Ok` and
/// `AlreadyExists` both mean the folder is there. `path` itself goes first:
/// when its parent exists, the usual case, that one request settles the
/// chain (a walk from the top asked again for every level that was already
/// there, and on Proton each of those is a refused create). Only when it
/// fails are the levels made from the top down. With `strict`, the first
/// failure of that walk stops it and comes back with the folder it was for;
/// without, failures are skipped, since backends with virtual folders refuse
/// a level that does not need to exist.
pub async fn create_folder_chain(
    provider: &mut dyn StorageProvider,
    path: &str,
    strict: bool,
) -> Result<(), (String, ProviderError)> {
    let ladder = crate::community_benchmark::benchmark_mkdir_ladder(path);
    let Some(deepest) = ladder.last() else {
        return Ok(());
    };
    if let Ok(()) | Err(ProviderError::AlreadyExists(_)) = provider.mkdir(deepest).await {
        return Ok(());
    }
    for folder in ladder {
        match provider.mkdir(&folder).await {
            Ok(()) | Err(ProviderError::AlreadyExists(_)) => {}
            Err(error) if strict => return Err((folder, error)),
            Err(_) => {}
        }
    }
    Ok(())
}

/// Remove `path` only if it is an empty directory: [`StorageProvider::rmdir`],
/// which refuses a directory that holds anything on every backend. The one
/// primitive behind `rm` without `-r`, the served FTP RMD and SFTP RMDIR, the
/// mount's rmdir, MCP and AeroAgent deletes without `recursive`, the CLI's
/// `rmdir` and `sync --delete`, and the GUI sync's empty-folder removal.
pub async fn remove_empty_directory(
    provider: &mut dyn StorageProvider,
    path: &str,
) -> Result<(), ProviderError> {
    provider.rmdir(path).await
}

/// Whether a failed listing of `path` says there is no folder there, so a
/// `delete` of it cannot take a folder's content along: only NotFound. Any
/// other failure (a timeout, a 503, a lost connection, a permission or parse
/// error) says nothing about the path, and acting on it as if the path were
/// a file could remove a folder nobody had looked at.
fn listing_says_no_folder(error: &ProviderError) -> bool {
    matches!(error, ProviderError::NotFound(_))
}

/// Delete `path` without recursing: a file, a link, or an empty directory.
///
/// `delete` of a folder removes it with its content on most backends, so a
/// non-recursive delete asks `stat` first and sends a directory through
/// `rmdir`, which refuses one that holds anything. A directory `stat` cannot
/// describe (see [`stat_cannot_describe`]) is found by listing it: a listing
/// with entries is refused the same way, an empty one is removed with
/// `rmdir` (an object-store directory marker) and, when that fails, `delete`
/// (a file `stat` could not see). A listing that failed goes on to `delete`
/// only when it says there is no folder there ([`listing_says_no_folder`]);
/// any other listing failure, and any other `stat` failure, is returned as
/// it is, with nothing removed.
pub async fn delete_non_recursive(
    provider: &mut dyn StorageProvider,
    path: &str,
) -> Result<(), ProviderError> {
    match provider.stat(path).await {
        Ok(entry) if entry.is_dir && !entry.is_symlink => {
            remove_empty_directory(provider, path).await
        }
        Ok(_) => provider.delete(path).await,
        Err(e) if stat_cannot_describe(&e) => match provider.list(path).await {
            Ok(children) if !children.is_empty() => Err(directory_not_empty(path, children.len())),
            Ok(_) => match provider.rmdir(path).await {
                Ok(()) => Ok(()),
                Err(_) => provider.delete(path).await,
            },
            Err(list_error) if listing_says_no_folder(&list_error) => provider.delete(path).await,
            Err(list_error) => Err(list_error),
        },
        Err(e) => Err(e),
    }
}

/// Recursive removal still uses the file verb for a file or a symlink.
/// Only a confirmed directory (including an object-store prefix discovered
/// by listing) reaches the provider's guarded recursive directory removal.
pub async fn delete_recursive(
    provider: &mut dyn StorageProvider,
    path: &str,
) -> Result<(), ProviderError> {
    let absolute = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{path}", provider.pwd().await?)
    };
    if crate::sync_core::walk_path_key(&absolute.replace('\\', "/"))
        .trim_matches('/')
        .is_empty()
    {
        return Err(ProviderError::InvalidPath(
            "refusing to recursively delete the remote root".into(),
        ));
    }
    match provider.stat(path).await {
        Ok(entry) if entry.is_walkable_dir() => provider.rmdir_recursive(path).await,
        Ok(_) => provider.delete(path).await,
        Err(error) if stat_cannot_describe(&error) => match provider.list(path).await {
            Ok(children) => {
                crate::sync_core::listing_to_remove(path, children)?;
                provider.rmdir_recursive(path).await
            }
            Err(list_error) if listing_says_no_folder(&list_error) => {
                if matches!(error, ProviderError::NotFound(_)) {
                    Err(error)
                } else {
                    provider.delete(path).await
                }
            }
            Err(list_error) => Err(list_error),
        },
        Err(error) => Err(error),
    }
}

fn is_a_directory(path: &str) -> ProviderError {
    ProviderError::InvalidPath(format!(
        "{path} is a directory: this deletes files only; remove a directory with RMD or RMDIR"
    ))
}

/// Delete `path` only if it is not a directory: the served FTP DELE (RFC
/// 959) and SFTP REMOVE, which are file operations. Their directory verbs,
/// RMD and RMDIR, go through [`remove_empty_directory`]. Unlike
/// [`delete_non_recursive`], which `rm` uses, an empty directory is refused
/// too, with InvalidPath, and nothing is removed.
///
/// A `stat` that cannot describe the path is judged by listing it. A
/// listing with entries is a directory. An empty listing is one as well when
/// `stat` failed to parse the path (Box and GitHub fail that way on a
/// folder); after a NotFound it is an object-store name with nothing under
/// it, and the `delete` of the name without its slash leaves a folder's
/// marker alone. A listing that failed goes on to `delete` only when it says
/// there is no folder there ([`listing_says_no_folder`]).
pub async fn delete_file_only(
    provider: &mut dyn StorageProvider,
    path: &str,
) -> Result<(), ProviderError> {
    match provider.stat(path).await {
        Ok(entry) if entry.is_dir && !entry.is_symlink => Err(is_a_directory(path)),
        Ok(_) => provider.delete(path).await,
        Err(e) if stat_cannot_describe(&e) => match provider.list(path).await {
            Ok(children) if !children.is_empty() => Err(is_a_directory(path)),
            Ok(_) if !matches!(e, ProviderError::NotFound(_)) => Err(is_a_directory(path)),
            Ok(_) => provider.delete(path).await,
            Err(list_error) if listing_says_no_folder(&list_error) => provider.delete(path).await,
            Err(list_error) => Err(list_error),
        },
        Err(e) => Err(e),
    }
}

/// Refuse to stage a temporary that could not then be published.
///
/// Every "write a remote file in place" path in this tree has the same shape:
/// upload a temporary beside the target, check it, then put it in the
/// target's place. The last step is the one that can be refused, and asking
/// about it last is what made the refusal expensive: the temporary is already
/// on the server by then, so the error cannot honestly say that nothing was
/// written, and it cannot honestly say what is still lying around either.
///
/// Call this BEFORE the upload. It costs one capability question, which on
/// SFTP is one channel and one init, and it buys an error that names both
/// facts the reader needs: why this server cannot do it, and that their file
/// is untouched.
///
/// A backend whose replace sets the old item aside
/// ([`StorageProvider::replace_sets_aside`]) passes when nothing is at
/// `target`, which costs one more question: there is nothing to set aside,
/// so the publish is the rename onto a free name, one step that leaves no
/// moment without a file (the heal of a missing crypt marker, a legacy
/// marker migration). A backend whose replace is only its rename may have
/// no rename at all, so it is refused either way.
///
/// The answer says how to publish, and why: [`StagedPublish::Create`] when
/// the target was free and that alone let the backend through,
/// [`StagedPublish::Replace`] otherwise.
pub async fn ensure_atomic_replace(
    provider: &mut dyn StorageProvider,
    target: &str,
) -> Result<StagedPublish, ProviderError> {
    match staged_publish(provider, target).await? {
        Some(publish) => Ok(publish),
        None => Err(ProviderError::NotSupported(format!(
            "cannot replace `{target}` atomically: this server offers no way to put one file \
             over another in a single step, and doing it in two would leave a moment with no \
             file at all. Nothing was written and `{target}` is unchanged. To overwrite it \
             anyway, upload over it with `put`, which truncates and rewrites in place: that \
             is not atomic either, but it is your choice and its bad moment is a partial \
             file rather than no file."
        ))),
    }
}

/// [`ensure_atomic_replace`] for a crypt marker (`.aerocrypt.tsv` and the
/// legacy names): the same check, a refusal that fits a marker, and an
/// explicit opt-in for the backends whose replace sets the old file aside.
///
/// The way around a refused replace that [`ensure_atomic_replace`] offers is
/// to upload over the file with `put`. For a marker it is no way at all: the
/// new marker exists only inside the operation that built it (a slot added
/// or revoked, a migration, a header written for a headerless vault), so the
/// user has nothing to upload, and a marker rewritten in place and cut short
/// cannot open the vault. This refusal says what is true instead:
/// nothing was written, the vault is as it was, and the change needs a
/// server that can put one file over another in one step.
///
/// The operations that rewrite a marker already there (`crypt migrate-v4`
/// and the slot commands, in the CLI and in the app's slots dialog) may
/// replace it through a set-aside only with the user's consent
/// (`allow_non_atomic`, owner decision 2026-10-11): with it, a backend that
/// sets the old file aside passes as a replace when a marker is at
/// `target` and as a creation when the name is free (the consent is to
/// replace the marker that is there, not one written after the check), and
/// a backend that neither replaces atomically nor sets aside is refused,
/// before anything is staged. Without it the
/// check is [`ensure_atomic_replace`]'s, and on a set-aside backend the
/// refusal names the caller's opt-in (`opt_in`, its spelling of it) and what
/// it risks ([`marker_set_aside_opt_in_hint`]); `None` where the caller has
/// no opt-in. The publishes that only ever write onto a free name
/// (`crypt to-headed`, `crypt migrate-marker`, the heal of a missing marker,
/// the legacy marker migration) pass `false` and `None`.
pub async fn ensure_marker_replace(
    provider: &mut dyn StorageProvider,
    target: &str,
    allow_non_atomic: bool,
    opt_in: Option<&str>,
) -> Result<StagedPublish, ProviderError> {
    match staged_publish(provider, target).await? {
        Some(publish) => Ok(publish),
        // The consent covers the one case it was asked for: a set-aside
        // backend and a marker at the target. Onto a free name the publish
        // stays the creation above, so a marker another client writes after
        // this check is refused by the rename, never set aside and deleted.
        None if allow_non_atomic && provider.replace_sets_aside() => Ok(StagedPublish::Replace),
        None if allow_non_atomic => Err(ProviderError::NotSupported(marker_refusal(target))),
        None => {
            let refusal = marker_refusal(target);
            Err(ProviderError::NotSupported(
                match opt_in.filter(|_| provider.replace_sets_aside()) {
                    Some(opt_in) => format!("{refusal} {}", marker_set_aside_opt_in_hint(opt_in)),
                    None => refusal,
                },
            ))
        }
    }
}

fn marker_refusal(target: &str) -> String {
    format!(
        "cannot write the crypt marker `{target}` on this server: it offers no way to put one \
         file over another in a single step, and replacing a marker in two would leave the \
         vault a moment without one. Nothing was written and the vault is as it was. \
         Uploading a marker by hand is no way around it: the new one exists only inside this \
         operation, and a marker rewritten in place and cut short cannot open the vault. Run \
         it where the same storage is reached through a server that replaces in one step \
         (SFTP with `posix-rename@openssh.com`, for one)."
    )
}

/// The sentence a marker refusal adds on a backend whose replace sets the
/// previous file aside: the opt-in `opt_in` and the risks the user takes
/// with it, the ones the set-aside has (`replace_by_setting_aside`).
pub fn marker_set_aside_opt_in_hint(opt_in: &str) -> String {
    format!(
        "To replace it anyway, pass {opt_in}: the current marker is renamed aside, the new \
         one moves into its place, and the old one is then deleted. For that moment the \
         vault has no marker, and anyone opening it then is refused. If the operation is \
         cut off in between, the marker is left under a `.aeroftp-replaced-` name in the \
         same folder and has to be renamed back by hand. On storage that keeps a trash or \
         file versions, the previous marker stays recoverable there after it is deleted."
    )
}

/// The error for copies of the previous marker still on the server after a
/// revocation, shared by the app and the CLI.
pub fn revoked_marker_copies_left(left: &[(String, ProviderError)]) -> String {
    let listed = left
        .iter()
        .map(|(path, e)| format!("{path} ({e})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "The marker was updated and the slot is revoked in it, but a copy of the previous \
         marker could not be confirmed gone: {listed}. Such a copy still opens the vault with \
         the revoked factor. Delete every `.aeroftp-replaced-*..aerocrypt.tsv` in that folder \
         by hand."
    )
}

/// How a crypt marker that already exists can be replaced on this backend,
/// for a dialog that asks the user's consent before it offers the operation
/// ([`ensure_marker_replace`] decides the same at publish time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkerReplaceMode {
    /// One step: no consent needed.
    Atomic,
    /// Through a set-aside: possible with the user's consent.
    SetsAside,
    /// Not at all on this backend.
    Unsupported,
}

/// [`MarkerReplaceMode`] of `provider`.
pub async fn marker_replace_mode(
    provider: &mut dyn StorageProvider,
) -> Result<MarkerReplaceMode, ProviderError> {
    Ok(if provider.supports_atomic_replace().await? {
        MarkerReplaceMode::Atomic
    } else if provider.replace_sets_aside() {
        MarkerReplaceMode::SetsAside
    } else {
        MarkerReplaceMode::Unsupported
    })
}

/// The check behind [`ensure_atomic_replace`] and [`ensure_marker_replace`]:
/// how to publish, or `None` where this backend cannot do it.
async fn staged_publish(
    provider: &mut dyn StorageProvider,
    target: &str,
) -> Result<Option<StagedPublish>, ProviderError> {
    if provider.supports_atomic_replace().await? {
        return Ok(Some(StagedPublish::Replace));
    }
    if provider.replace_sets_aside() && !provider.exists(target).await? {
        return Ok(Some(StagedPublish::Create));
    }
    Ok(None)
}

/// The preflight of an edit that publishes a staged temporary with
/// [`StorageProvider::replace`] (CLI `edit`, AeroAgent `remote_edit`). Call it
/// BEFORE the upload, so a refusal can say that nothing was written (G119).
///
/// A backend that replaces atomically passes. Without the opt-in any other
/// one refuses as [`ensure_atomic_replace`] does, and the refusal names the
/// opt-in (`opt_in`, the caller's spelling of it) only where it can work: a
/// backend whose replace sets the previous file aside
/// ([`StorageProvider::replace_sets_aside`]). With the opt-in that backend
/// passes, and every other one is refused here: its replace would refuse
/// the taken name after the temporary had been uploaded.
///
/// The crypt and AeroCrypt marker paths call [`ensure_marker_replace`],
/// which has its own opt-in and its own refusal.
///
/// The answer is the publish, as for [`ensure_atomic_replace`]: on a
/// set-aside backend the opt-in is a replace of the file still at the
/// target, and onto a free name (the file went away after it was read) the
/// publish is a creation with or without it, so a file another client puts
/// there after the check is refused by the rename, not set aside and
/// deleted under an edit made from what was read before.
pub async fn ensure_edit_can_replace(
    provider: &mut dyn StorageProvider,
    target: &str,
    allow_non_atomic: bool,
    opt_in: &str,
) -> Result<StagedPublish, ProviderError> {
    if allow_non_atomic {
        // The opt-in covers the one case it was asked for, a set-aside
        // backend and a file at the target, as for a marker
        // ([`ensure_marker_replace`]).
        return match staged_publish(provider, target).await? {
            Some(publish) => Ok(publish),
            None if provider.replace_sets_aside() => Ok(StagedPublish::Replace),
            None => Err(ProviderError::NotSupported(opt_in_cannot_set_aside(
                target, opt_in,
            ))),
        };
    }
    match ensure_atomic_replace(provider, target).await {
        Err(ProviderError::NotSupported(refusal)) if provider.replace_sets_aside() => Err(
            ProviderError::NotSupported(format!("{refusal} {}", set_aside_opt_in_hint(opt_in))),
        ),
        other => other,
    }
}

/// How a temporary staged beside its target goes into the target's place,
/// as [`ensure_atomic_replace`], [`ensure_marker_replace`] or
/// [`ensure_edit_can_replace`] answered before it was staged. The answer
/// carries why the check passed, and the publish keeps to it: publish with
/// [`StagedPublish::publish`], never with a bare [`StorageProvider::replace`].
///
/// The check and the publish are separate requests (CodeRabbit on #1154).
/// On a backend that replaces atomically the check passes whatever is at
/// the target and the publish replaces it: whoever writes last wins, which
/// is what a replace is. A backend whose replace sets the old item aside
/// passes without an opt-in only because nothing was at the target, and
/// with one it is a replace only of an item the check found there; onto a
/// name found free a replace would set aside and delete an item another
/// client put there in between. Its publish is therefore the rename, which refuses
/// a taken name. On WebDAV (a MOVE with `Overwrite: F`, 412), Dropbox,
/// Koofr, kDrive (409) and Drime (a rename within one folder) the server
/// refuses it, which closes that window. MEGA, Filen and FileLu keep two
/// items under one name, so their rename looks for the name first: an item
/// written between that look and the move ends up beside ours under the
/// same name, and neither is deleted.
///
/// Bind the answer (`let publish = ensure_atomic_replace(..)?`): the
/// `must_use` below catches one dropped by `?;`, not one an `if let Err`
/// never bound.
#[must_use = "publish with `StagedPublish::publish`: a bare `replace` deletes what another \
              client wrote at a target the check found free"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedPublish {
    /// [`StorageProvider::replace`]: the backend replaces atomically, or an
    /// edit opted into the replace that sets the old file aside.
    Replace,
    /// [`StorageProvider::rename`]: the check passed only because nothing was
    /// at the target, so the target has to be free still.
    Create,
}

impl StagedPublish {
    /// Put the staged `from` at `to`. Under [`StagedPublish::Create`] an item
    /// found at `to` is left as it is, and the answer is AlreadyExists: nothing
    /// was published. On any error the caller deletes `from`, as after every
    /// other failed publish.
    pub async fn publish(
        self,
        provider: &mut dyn StorageProvider,
        from: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        match self {
            Self::Replace => provider.replace(from, to).await,
            Self::Create => match provider.rename(from, to).await {
                Err(ProviderError::AlreadyExists(_)) => Err(ProviderError::AlreadyExists(format!(
                    "{to} was free when checked and is taken now: another client wrote it in \
                     the meantime. It was left as it is and nothing was published"
                ))),
                outcome => outcome,
            },
        }
    }
}

/// The sentence an edit refusal adds on a backend whose replace sets the
/// previous file aside: the opt-in `opt_in` and what it does.
pub fn set_aside_opt_in_hint(opt_in: &str) -> String {
    format!(
        "To edit it anyway, pass {opt_in}: the previous file is renamed aside, the new one \
         moves into its place, and the old one is then deleted. There is a short moment \
         with no file, and the old one is not lost."
    )
}

/// The refusal of an edit's non-atomic opt-in on a backend whose replace
/// neither works in one step nor sets the previous file aside.
pub fn opt_in_cannot_set_aside(target: &str, opt_in: &str) -> String {
    format!(
        "cannot edit `{target}` with {opt_in}: this server can neither put one file over \
         another in a single step nor set the previous file aside first, so the new file \
         could not be put in its place. Nothing was written and `{target}` is unchanged."
    )
}

/// The Unix mode in a permission string as providers report it: nine
/// `rwx` letters (`rw-r--r--`), the same after a type letter as `ls` and
/// SFTP write it (`-rw-r--r--`), or octal (`644`, `0644`, the MLSD
/// `unix.mode` fact). `s`, `S`, `t` and `T` carry the setuid, setgid and
/// sticky bits. `None` for anything else, such as the MLSD `perm` fact
/// (`adfrw`), which lists the operations allowed and is not a mode.
pub fn permission_mode(permissions: &str) -> Option<u32> {
    let text = permissions.trim();
    if !text.is_ascii() || text.is_empty() {
        return None;
    }
    if text.len() <= 6 && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return u32::from_str_radix(text, 8).ok().map(|mode| mode & 0o7777);
    }
    // `ls -l` marks an ACL or extended attributes after the nine letters.
    let letters = text.trim_end_matches(['+', '@', '.']);
    let letters = match letters.len() {
        10 => &letters[1..],
        9 => letters,
        _ => return None,
    };
    let mut mode = 0;
    // Owner, group, others; the third letter of each also carries setuid,
    // setgid and sticky: lower case with `x`, upper case without.
    for (class, triplet) in letters.as_bytes().chunks(3).enumerate() {
        let shift = 6 - 3 * class as u32;
        let (special, with_x, without_x) = match class {
            0 => (0o4000, b's', b'S'),
            1 => (0o2000, b's', b'S'),
            _ => (0o1000, b't', b'T'),
        };
        match triplet[0] {
            b'r' => mode |= 4 << shift,
            b'-' => {}
            _ => return None,
        }
        match triplet[1] {
            b'w' => mode |= 2 << shift,
            b'-' => {}
            _ => return None,
        }
        match triplet[2] {
            b'x' => mode |= 1 << shift,
            b'-' => {}
            letter if letter == with_x => mode |= (1 << shift) | special,
            letter if letter == without_x => mode |= special,
            _ => return None,
        }
    }
    Some(mode)
}

/// What an edit that publishes a staged temporary with
/// [`StorageProvider::replace`] carries over from the file it replaces
/// (CLI `edit`, MCP and CLI-agent `aeroftp_edit`, AeroAgent `remote_edit`).
///
/// The replace puts a NEW file in the target's place, so nothing the server
/// kept on the old one survives unless the edit copies it: on SFTP the mode
/// came back as the server default (a 0600 `.env` became 0644, a 0755
/// script lost its `x`). An upload over the file in place, which the GUI
/// edit made until 4.2.0, truncated the same file and kept it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOriginal {
    /// Nothing to carry over: the provider reports no permissions, or has no
    /// `chmod` to set them with.
    Nothing,
    /// The Unix mode to set on the temporary before the replace.
    Mode(u32),
    /// Permissions the provider reports that are not a Unix mode (an MLSD
    /// `perm` fact), so the new file gets the server's default ones.
    Unreadable(String),
}

impl EditOriginal {
    /// What an edit of `entry` carries over; `can_chmod` is
    /// [`StorageProvider::supports_chmod`].
    pub fn of(entry: &RemoteEntry, can_chmod: bool) -> Self {
        match entry.permissions.as_deref() {
            Some(permissions) if can_chmod => match permission_mode(permissions) {
                Some(mode) => Self::Mode(mode),
                None => Self::Unreadable(permissions.to_string()),
            },
            _ => Self::Nothing,
        }
    }
}

/// Refuse an edit of a symbolic link, BEFORE anything is staged. The
/// replace would put a regular file in the link's place, and the file the
/// link points to, the one the caller meant, would stay as it was. The
/// refusal names that file, resolved against the link's folder when the
/// link is relative, so the caller can edit it instead.
pub fn refuse_edit_of_symlink(entry: &RemoteEntry, target: &str) -> Result<(), String> {
    if !entry.is_symlink {
        return Ok(());
    }
    let instead = match entry.link_target.as_deref() {
        Some(link) if !link.is_empty() => {
            let resolved = match (link.starts_with('/'), target.rsplit_once('/')) {
                (false, Some((parent, _))) => format!("{parent}/{link}"),
                _ => link.to_string(),
            };
            format!("edit the file it points to, `{resolved}`, instead")
        }
        _ => "edit the file it points to instead".to_string(),
    };
    Err(format!(
        "cannot edit `{target}`: it is a symbolic link, and publishing the edit would put a \
         regular file in the link's place while the file it points to stays unchanged; \
         {instead}. Nothing was written and `{target}` is unchanged."
    ))
}

/// The warning of an edit of `target` whose new file could not be given the
/// mode `mode` of the old one because `chmod` failed with `error`.
pub fn edit_mode_not_kept(target: &str, mode: u32, error: &str) -> String {
    format!(
        "edited `{target}`, but its permissions ({mode:04o}) could not be set on the new \
         file ({error}): it has the server's default permissions now; set them again with \
         chmod"
    )
}

/// The warning of an edit of `target` whose permissions, as the provider
/// reports them (`permissions`), are not a mode that can be set again.
pub fn edit_permissions_not_readable(target: &str, permissions: &str) -> String {
    format!(
        "edited `{target}`, but its permissions (`{permissions}`) are not a Unix mode that \
         can be set again: the new file has the server's default permissions"
    )
}

/// Look at the target of an edit BEFORE anything is staged: a symbolic
/// link is refused ([`refuse_edit_of_symlink`]), and the answer is what the
/// temporary must carry ([`EditOriginal`]). A `stat` that cannot describe
/// the path ([`stat_cannot_describe`]) leaves nothing to carry over, as
/// before this look existed; any other `stat` failure is returned, with
/// nothing written.
pub async fn inspect_edit_target(
    provider: &mut dyn StorageProvider,
    target: &str,
) -> Result<EditOriginal, ProviderError> {
    let entry = match provider.stat(target).await {
        Ok(entry) => entry,
        Err(e) if stat_cannot_describe(&e) => return Ok(EditOriginal::Nothing),
        Err(e) => return Err(e),
    };
    refuse_edit_of_symlink(&entry, target).map_err(ProviderError::InvalidPath)?;
    Ok(EditOriginal::of(&entry, provider.supports_chmod()))
}

/// Carry `original` over to the staged temporary `temp` of an edit of
/// `target`: after its upload, before the replace. Best effort: a `chmod`
/// the server refuses does not fail an edit that is otherwise done, and the
/// answer is the warning that says what was not kept, for the caller to
/// show once the replace has succeeded.
pub async fn keep_edit_original(
    provider: &mut dyn StorageProvider,
    temp: &str,
    target: &str,
    original: &EditOriginal,
) -> Option<String> {
    match original {
        EditOriginal::Nothing => None,
        EditOriginal::Unreadable(permissions) => {
            Some(edit_permissions_not_readable(target, permissions))
        }
        EditOriginal::Mode(mode) => match provider.chmod(temp, *mode).await {
            Ok(()) => None,
            Err(e) => Some(edit_mode_not_kept(target, *mode, &e.to_string())),
        },
    }
}

/// The name an item displaced by a replace takes until it is deleted, on a
/// backend that can neither overwrite on a move nor swap two items in one
/// call (MEGA, Filen, FileLu, Google Drive for folders, and through
/// [`replace_by_setting_aside`] Dropbox, Koofr, Drime and kDrive), or whose
/// overwriting move deletes the destination first (WebDAV, through the same
/// helper). Their `replace` renames the item at the destination to this,
/// moves the new one in, and only then deletes it: no step can lose either
/// item, and the name is hidden and unique so it never meets another. The
/// destination is empty between the first two steps, which is why those
/// backends answer `false` to [`StorageProvider::supports_atomic_replace`].
///
/// The marker comes before the name, so the name never begins with `.ht`,
/// which Apache's default configuration refuses (`<Files ".ht*">`): with
/// the name first, setting `html.txt` aside on such a WebDAV server was a
/// 403 before anything moved.
pub(crate) fn set_aside_name(name: &str) -> String {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    format!(".aeroftp-replaced-{}.{name}", &unique[..8])
}

/// Whether `entry` is a copy of `name` set aside by [`set_aside_name`]:
/// `.aeroftp-replaced-` and eight lowercase hex digits, a dot, then `name`.
pub(crate) fn is_set_aside_copy_of(entry: &str, name: &str) -> bool {
    entry
        .strip_prefix(".aeroftp-replaced-")
        .and_then(|rest| rest.split_at_checked(8))
        .is_some_and(|(hex, rest)| {
            hex.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && rest.strip_prefix('.') == Some(name)
        })
}

/// Delete every copy of `dir`/`name` that a set-aside replace left in `dir`
/// (its delete failed, or the run that made it was cut off), this run's and
/// any earlier one's. Answers the copies that are still there, with why:
/// the ones whose delete failed, or, when `dir` could not be listed, `dir`
/// itself with the listing error, since then nothing says there are none.
///
/// For a crypt marker after a revocation (`remove-slot`, `rotate-slot`) a
/// copy is not litter: it still opens with the factor just revoked, so the
/// caller reports what is left as an error (policy C, owner 2026-10-11).
pub async fn remove_set_aside_copies(
    provider: &mut dyn StorageProvider,
    dir: &str,
    name: &str,
) -> Vec<(String, ProviderError)> {
    let entries = match provider.list(dir).await {
        Ok(entries) => entries,
        Err(e) => return vec![(dir.to_string(), e)],
    };
    let mut left = Vec::new();
    for entry in entries {
        if entry.is_dir || !is_set_aside_copy_of(&entry.name, name) {
            continue;
        }
        let path = if dir.ends_with('/') {
            format!("{dir}{}", entry.name)
        } else {
            format!("{dir}/{}", entry.name)
        };
        if let Err(e) = provider.delete(&path).await {
            left.push((path, e));
        }
    }
    left
}

/// The error of a set-aside replace whose move of the new item into `to`
/// failed: `error` alone when the item set aside as `aside` got its name
/// back, and both failures with where that item is when it did not.
pub(crate) fn set_aside_move_failed(
    to: &str,
    aside: &str,
    error: ProviderError,
    restored: Result<(), ProviderError>,
) -> ProviderError {
    match restored {
        Ok(()) => error,
        Err(restore) => ProviderError::Other(format!(
            "replace could not move the new item to {to} ({error}), and giving the previous one \
             its name back failed too ({restore}): it is kept as {aside}"
        )),
    }
}

/// Report the leftover of a set-aside replace that put the new item in
/// place but could not delete the one set aside as `aside`. The replace is
/// done, so it is a success: an error made callers undo or retry a replace
/// that had happened (a WebDAV client retrying the MOVE, an edit deleting
/// its temporary). What is left over goes to the log, and to the pending
/// warnings a front end renders its own way ([`take_warnings`]): the log
/// reaches no one where no subscriber is installed (the CLI without `-v` or
/// `RUST_LOG`, `serve webdav`), and the leftover is a hidden name holding
/// the old content, which no one would otherwise find.
pub(crate) fn report_set_aside_leftover(to: &str, aside: &str, error: &ProviderError) {
    let message = format!(
        "replaced {to}, but deleting the previous version, set aside as {aside}, failed: \
         {error}; delete it by hand"
    );
    tracing::warn!("{message}");
    report_warning(message);
}

/// Keep `message` for the front end to show ([`take_warnings`]): a warning
/// the user should see that a successful call cannot return. Inside
/// [`CallWarnings::scope`] it is kept for that call; elsewhere it goes to
/// the process queue. When the front end never asks (the GUI, which has the
/// log), the oldest go first and are counted, so the queue stays bounded and
/// the newest survive.
pub fn report_warning(message: String) {
    let mut message = Some(message);
    let _ = CALL_WARNINGS.try_with(|call| {
        if let Some(message) = message.take() {
            call.lock().push(message);
        }
    });
    if let Some(message) = message {
        with_pending_warnings(|pending| pending.push(message));
    }
}

/// Take the warnings reported since the last call, oldest first, for the
/// front end to show in its own format (the CLI: a line on stderr, or a JSON
/// object there with `--json`; MCP: a text block of the tool result): inside
/// [`CallWarnings::scope`] the call's own, then the process queue's. When
/// some were dropped to keep a queue bounded, the first says how many.
pub fn take_warnings() -> Vec<String> {
    let mut taken = CALL_WARNINGS
        .try_with(CallWarnings::take)
        .unwrap_or_default();
    taken.extend(with_pending_warnings(PendingWarnings::take));
    taken
}

/// The warnings one call reports, kept apart from the process queue so they
/// reach that call's answer and no other: a server answering several calls
/// at once (MCP, `serve webdav`) gave one call's warning to whichever call
/// took the queue next. They stay readable after the call is dropped (a
/// timeout, a cancellation).
#[derive(Clone, Default)]
pub struct CallWarnings(std::sync::Arc<std::sync::Mutex<PendingWarnings>>);

tokio::task_local! {
    static CALL_WARNINGS: CallWarnings;
}

impl CallWarnings {
    /// Run `call`, keeping here what it reports through [`report_warning`].
    pub async fn scope<F: std::future::Future>(&self, call: F) -> F::Output {
        CALL_WARNINGS.scope(self.clone(), call).await
    }

    /// Take what the call reported so far, oldest first.
    pub fn take(&self) -> Vec<String> {
        self.lock().take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PendingWarnings> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Default)]
struct PendingWarnings {
    messages: std::collections::VecDeque<String>,
    dropped: usize,
}

impl PendingWarnings {
    fn push(&mut self, message: String) {
        if self.messages.len() == MAX_PENDING_WARNINGS {
            self.messages.pop_front();
            self.dropped += 1;
        }
        self.messages.push_back(message);
    }

    fn take(&mut self) -> Vec<String> {
        let mut taken = Vec::with_capacity(self.messages.len() + 1);
        if self.dropped > 0 {
            taken.push(format!(
                "{} earlier warnings were dropped before anyone read them",
                self.dropped
            ));
            self.dropped = 0;
        }
        taken.extend(self.messages.drain(..));
        taken
    }
}

const MAX_PENDING_WARNINGS: usize = 64;

/// The queue of [`report_warning`]: one for the process, and one per thread
/// in this crate's tests, so a test reads only the warnings it caused.
#[cfg(not(test))]
fn with_pending_warnings<R>(f: impl FnOnce(&mut PendingWarnings) -> R) -> R {
    static PENDING: std::sync::Mutex<PendingWarnings> = std::sync::Mutex::new(PendingWarnings {
        messages: std::collections::VecDeque::new(),
        dropped: 0,
    });
    let mut pending = PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut pending)
}

#[cfg(test)]
fn with_pending_warnings<R>(f: impl FnOnce(&mut PendingWarnings) -> R) -> R {
    thread_local! {
        static PENDING: std::cell::RefCell<PendingWarnings> =
            std::cell::RefCell::new(PendingWarnings::default());
    }
    PENDING.with(|pending| f(&mut pending.borrow_mut()))
}

/// Refuse `rename(from, to)` when `to` is taken, on a backend whose own move
/// would overwrite the item there, move the source inside it, or put a
/// second item beside it under the same name. The trait promises none of
/// that happens, and these backends have no call that refuses on their own.
///
/// `stat` of `to` decides: found is AlreadyExists, not found is free, and any
/// other answer is passed on (the rename does not go out on a guess). The one
/// exception is a rename that only changes the letter case: a
/// case-insensitive backend finds the source itself under the new spelling
/// and reports the name it has stored, so an entry named exactly like the
/// source is the source. A backend that echoes the spelling it was asked for
/// makes such a rename refused, which is the safe way to be wrong.
///
/// The look and the move are separate requests, so an item created at `to`
/// between them is still overwritten or doubled: the window is declared, not
/// closed, on every backend that uses this.
pub(crate) async fn refuse_occupied_destination(
    provider: &mut dyn StorageProvider,
    from: &str,
    to: &str,
) -> Result<(), ProviderError> {
    match provider.stat(to).await {
        Ok(found) if is_the_source_under_another_case(from, to, &found.name) => Ok(()),
        Ok(_) => Err(ProviderError::AlreadyExists(to.to_string())),
        Err(ProviderError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Put `from` in place of `to` on a backend whose rename refuses a taken
/// name and which has no call that overwrites: the item at `to` is renamed
/// aside under [`set_aside_name`], `from` is renamed in, and only then is
/// the one set aside deleted. If the rename in fails, the item set aside
/// gets its name back (see [`set_aside_move_failed`]); if the final delete
/// fails, the replace is done and the leftover is reported (see
/// [`report_set_aside_leftover`]). Onto a free name, or onto the source
/// itself under another letter case, it is the rename; across file and
/// folder it is refused before anything changes. `to` is empty between the
/// first two steps, so a backend that uses this answers `false` to
/// [`StorageProvider::supports_atomic_replace`].
///
/// `from` and `to` are the backend's resolved absolute paths.
pub(crate) async fn replace_by_setting_aside(
    provider: &mut dyn StorageProvider,
    from: &str,
    to: &str,
) -> Result<(), ProviderError> {
    let (from, to) = (from.trim_end_matches('/'), to.trim_end_matches('/'));
    if from == to {
        return Ok(());
    }
    let occupant = match provider.stat(to).await {
        Ok(found) if !is_the_source_under_another_case(from, to, &found.name) => found,
        Ok(_) | Err(ProviderError::NotFound(_)) => return provider.rename(from, to).await,
        Err(e) => return Err(e),
    };
    let source = provider.stat(from).await?;
    refuse_replace_across_types(to, source.is_dir, occupant.is_dir)?;
    let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
    let aside = format!("{parent}/{}", set_aside_name(name));

    provider.rename(to, &aside).await?;
    if let Err(e) = provider.rename(from, to).await {
        let restored = provider.rename(&aside, to).await;
        return Err(set_aside_move_failed(to, &aside, e, restored));
    }
    let removed = if occupant.is_dir {
        provider.rmdir_recursive(&aside).await
    } else {
        provider.delete(&aside).await
    };
    if let Err(e) = removed {
        report_set_aside_leftover(to, &aside, &e);
    }
    Ok(())
}

/// The error of a rename done in two steps (a move that keeps the name and
/// a rename in place, in either order) whose second step failed with
/// `error` after the first had succeeded. When the first step was undone
/// nothing changed, and `error` is the answer as it came. When the undo
/// failed too, the item is at `now_at`: the error names both failures and
/// that path, and is never AlreadyExists, which would say nothing changed.
///
/// Declared, not closed: a second step whose answer was lost after the
/// server applied it (a timeout) reads as failed, so the undo moves back an
/// item that had arrived, or the error names a place it has left. No
/// backend that renames in two steps offers a way to ask which it was.
pub(crate) fn second_step_failed(
    from: &str,
    to: &str,
    now_at: &str,
    error: ProviderError,
    undone: Result<(), ProviderError>,
) -> ProviderError {
    match undone {
        Ok(()) => error,
        Err(undo) => ProviderError::Other(format!(
            "renaming {from} to {to} stopped halfway: the second step failed ({error}) and \
             undoing the first failed too ({undo}): the item is now at {now_at}"
        )),
    }
}

/// Drop from a path-keyed id cache the entry for `path` and every entry
/// under it. After a rename or a replace the ids cached for the old path,
/// the new one and everything below them point at items that moved or went
/// to the trash: a later lookup would act on the wrong item.
pub(crate) fn forget_cached_subtree<V>(cache: &mut HashMap<String, V>, path: &str) {
    let path = path.trim_end_matches('/');
    let below = format!("{path}/");
    cache.retain(|cached, _| cached != path && !cached.starts_with(&below));
}

/// `forget_cached_subtree` under every capitalization of `path`: a key cached
/// under another capitalization of `path`, or of a folder above it, is
/// forgotten too. On a backend whose paths ignore letter case (OneDrive, Box)
/// such a key names the same item. On one that resolves a name ignoring case
/// only as a fallback (kDrive, Drime, Internxt, Google Drive's name search)
/// it may name the same item, and forgetting a sibling of another case costs
/// one lookup, while keeping a stale id sends a later write to a moved or
/// trashed item. Keys are never lowercased: where `A` and `a` are two items
/// they stay two keys.
pub(crate) fn forget_cached_subtree_ignoring_case<V>(cache: &mut HashMap<String, V>, path: &str) {
    let path = path.trim_end_matches('/').to_lowercase();
    let below = format!("{path}/");
    cache.retain(|cached, _| {
        let cached = cached.to_lowercase();
        cached != path && !cached.starts_with(&below)
    });
}

/// Whether the item `stat(to)` found, named `found_name`, is the source of a
/// rename that only changes the letter case, found again by a
/// case-insensitive backend under the name it has stored.
pub(crate) fn is_the_source_under_another_case(from: &str, to: &str, found_name: &str) -> bool {
    let (from, to) = (from.trim_end_matches('/'), to.trim_end_matches('/'));
    from != to
        && from.to_lowercase() == to.to_lowercase()
        && found_name == from.rsplit('/').next().unwrap_or(from)
}

/// Refuse a replace that would put a file in place of a folder or a folder
/// in place of a file. On a backend that sets the old item aside and then
/// deletes it, a file replacing a folder deleted the whole folder, contents
/// and all (for good on FileLu, which has no trash), to leave a file under
/// its name. Nothing a caller means by "replace" asks for that, so it is
/// refused before anything changes. AlreadyExists, because the destination
/// is taken by an item this call will not displace.
pub(crate) fn refuse_replace_across_types(
    to: &str,
    source_is_dir: bool,
    occupant_is_dir: bool,
) -> Result<(), ProviderError> {
    if source_is_dir == occupant_is_dir {
        return Ok(());
    }
    let (occupant, source) = if occupant_is_dir {
        ("folder", "file")
    } else {
        ("file", "folder")
    };
    Err(ProviderError::AlreadyExists(format!(
        "{to} is a {occupant}, and a replace puts a {source} only in place of a {source}: \
         nothing was changed"
    )))
}

/// The native Proton Drive client for a profile with `proton_mode=native`.
#[cfg(feature = "proton-native")]
fn native_proton_provider(config: ProtonConfig) -> Result<Box<dyn StorageProvider>, ProviderError> {
    Ok(Box::new(
        proton_native::provider::ProtonNativeProvider::new(config),
    ))
}

/// Without the `proton-native` feature a native profile is refused, not
/// connected through the CLI.
#[cfg(not(feature = "proton-native"))]
fn native_proton_provider(
    _config: ProtonConfig,
) -> Result<Box<dyn StorageProvider>, ProviderError> {
    Err(ProviderError::InvalidConfig(
        NATIVE_PROTON_UNAVAILABLE.to_string(),
    ))
}

/// What a build without the native Proton Drive client answers a profile
/// that asks for it.
pub const NATIVE_PROTON_UNAVAILABLE: &str =
    "This build does not include the native Proton Drive client. Switch the profile to the Proton Drive CLI, or use a build with the native client.";

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
                    "OAuth2 providers must be connected using oauth2_full_auth and oauth2_connect commands".to_string()
                ))
            }
            ProviderType::FourShared => {
                // OAuth1 provider: use fourshared_connect command
                Err(ProviderError::NotSupported(
                    "4shared must be connected using fourshared_full_auth and fourshared_connect commands".to_string()
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
            ProviderType::Proton => {
                let proton_config = ProtonConfig::from_provider_config(config)?;
                match proton_config.connection_mode {
                    ProtonConnectionMode::Cli => Ok(Box::new(ProtonCliProvider::new(proton_config))),
                    // Never a silent fall back to the CLI: it may be signed
                    // in to another account than the one this profile chose.
                    ProtonConnectionMode::Native => native_proton_provider(proton_config),
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
            ProviderType::Twake => {
                // Built from the sign-in blob stored as the profile password, so
                // the MCP pool, the CLI profile path and AeroCloud reach it too.
                let twake_config = twake::TwakeConfig::from_provider_config(config)?;
                Ok(Box::new(TwakeProvider::new(twake_config)))
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
            ProviderType::Local => Ok(Box::new(directory::DirectoryProvider::local(
                directory::local_root_from_config(config)?,
            ))),
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
            ProviderType::Proton,
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
            ProviderType::Twake,
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

/// The progress callback of a download. Lent as `&mut`, never `&`: the
/// callback is `Send` but not `Sync`, and a shared borrow held across an await
/// would make the download's future require `Sync` of it.
type ProgressCallback = Option<Box<dyn Fn(u64, u64) + Send>>;

/// Stream an HTTP answer into a part, tracking progress.
///
/// With `overlap`, the answer starts one byte before the part's end, and that
/// byte must be the part's last one: it is compared, never written. `Ok(false)`
/// when it differs (or the answer is empty), before anything is written.
///
/// Progress reports `(transferred_total, total_size)` where `transferred_total`
/// includes the bytes the part already held.
async fn stream_answer_to_part(
    response: reqwest::Response,
    part: &mut atomic_write::ResumableFile,
    total_size: Option<u64>,
    mut overlap: Option<u8>,
    on_progress: &mut ProgressCallback,
) -> Result<bool, ProviderError> {
    use futures_util::StreamExt;

    let mut stream = response.bytes_stream();
    let mut transferred = part.offset();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        crate::transfer_dag::throttle::charge(
            crate::transfer_dag::governor::TransferDirection::Download,
            chunk.len() as u64,
        )
        .await;
        let mut data = &chunk[..];
        if let Some(expected) = overlap {
            let Some((&first, rest)) = data.split_first() else {
                continue;
            };
            if first != expected {
                return Ok(false);
            }
            overlap = None;
            data = rest;
        }
        let next = transferred
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::TransferFailed("download size overflow".to_string()))?;
        if total_size.is_some_and(|total| next > total) {
            return Err(ProviderError::TransferFailed(
                "HTTP download exceeds the declared object size".to_string(),
            ));
        }
        part.write_all(data)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        transferred = next;
        if let Some(progress) = on_progress {
            progress(transferred, total_size.unwrap_or(0));
        }
    }

    if overlap.is_none() && total_size.is_some_and(|total| transferred != total) {
        return Err(ProviderError::TransferFailed(format!(
            "HTTP download ended at {transferred} of {} bytes",
            total_size.unwrap()
        )));
    }
    Ok(overlap.is_none())
}

/// A resumable HTTP download, driven by the provider that sends its requests
/// (each authenticates or signs its own): [`Self::range`] says what to ask
/// for, [`Self::accept`] judges the answer against the part on disk and
/// writes it.
///
/// A part is gone on from only when the answer shows the file is still the
/// version the part's record names (see
/// [`atomic_write::ResumableFile`]); otherwise it is started over with a
/// request for the whole file. A resume asks from the part's last byte, not
/// after it: the answer is then a `206` that carries the file's length and
/// validators even when the part already holds the whole file, where asking
/// from its end is answered `416`, which names no version. The byte asked for
/// twice must match the part's, a second check that costs one byte.
struct HttpResume {
    part: atomic_write::ResumableFile,
}

/// What [`HttpResume::accept`] made of an answer.
enum HttpResumeStep {
    /// The file is published.
    Done,
    /// The part could not be gone on from and was started over: send the
    /// request again, for the whole file this time.
    StartOver(Box<HttpResume>),
}

impl HttpResume {
    /// Open the part a download of `local_path` goes on from, if any. Out of
    /// place a part whose record names no version is started over here,
    /// before a request is spent on it.
    async fn open(
        local_path: &str,
        source: &atomic_write::ResumeSource,
    ) -> Result<Self, ProviderError> {
        let mut part = atomic_write::ResumableFile::open_resume(local_path)
            .await
            .map_err(ProviderError::IoError)?
            .bind_source(source)
            .await
            .map_err(ProviderError::IoError)?;
        let unrecorded = !part
            .recorded_source()
            .is_some_and(|recorded| recorded.pins_a_version());
        if part.offset() > 0 && !part.is_inplace() && unrecorded {
            tracing::info!(
                "partial download of {} started over: it has no record of the version it was started from",
                local_path
            );
            part = part.start_over().await.map_err(ProviderError::IoError)?;
        }
        Ok(Self { part })
    }

    /// The `Range` header to send, `None` for the whole file.
    fn range(&self) -> Option<String> {
        let offset = self.part.offset();
        (offset > 0).then(|| format!("bytes={}-", offset - 1))
    }

    /// Judge `response`, the answer to a request with [`Self::range`], and
    /// write it. `what` names the file in a not-found error.
    async fn accept(
        self,
        response: reqwest::Response,
        what: &str,
        on_progress: &mut ProgressCallback,
    ) -> Result<HttpResumeStep, ProviderError> {
        use reqwest::StatusCode;

        let mut part = self.part;
        let offset = part.offset();
        let answer = multi_thread::RangeSourceFingerprint::of_answer(&response);
        match response.status() {
            StatusCode::PARTIAL_CONTENT if offset > 0 => {
                let refusal = Self::refusal(&part, &response, answer.as_ref(), offset);
                let overlap = match refusal {
                    Some(why) => Err(why),
                    None => match part.last_byte().await {
                        Ok(byte) => Ok(byte),
                        Err(e) => Err(format!("its last byte could not be read ({e})")),
                    },
                };
                let started_over = match overlap {
                    Ok(byte) => {
                        let total = answer.as_ref().map_or(0, |a| a.size());
                        if stream_answer_to_part(
                            response,
                            &mut part,
                            Some(total),
                            Some(byte),
                            on_progress,
                        )
                        .await?
                        {
                            Self::publish(part).await?;
                            return Ok(HttpResumeStep::Done);
                        }
                        "the byte asked for twice is not the one the part holds".to_string()
                    }
                    Err(why) => why,
                };
                Self::start_over(part, &started_over).await
            }
            StatusCode::RANGE_NOT_SATISFIABLE if offset > 0 => {
                Self::start_over(part, "the file is shorter than the part").await
            }
            StatusCode::OK => {
                // The whole file: asked for, or a server that ignores ranges.
                if offset > 0 {
                    part = part.start_over().await.map_err(ProviderError::IoError)?;
                }
                if let Some(answer) = answer.as_ref() {
                    part.record_source(answer).await;
                }
                let total = response.content_length();
                stream_answer_to_part(response, &mut part, total, None, on_progress).await?;
                Self::publish(part).await?;
                Ok(HttpResumeStep::Done)
            }
            StatusCode::NOT_FOUND => {
                let _ = part.discard().await;
                Err(ProviderError::NotFound(what.to_string()))
            }
            // Kept: the part is still good for a later attempt.
            status => Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            ))),
        }
    }

    /// Why the `206` cannot extend `part`, or `None` when it can.
    fn refusal(
        part: &atomic_write::ResumableFile,
        response: &reqwest::Response,
        answer: Option<&multi_thread::RangeSourceFingerprint>,
        offset: u64,
    ) -> Option<String> {
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(multi_thread::parse_content_range);
        let Some((start, end, Some(total))) = range else {
            return Some("the answer does not give a complete Content-Range".to_string());
        };
        if start != offset - 1 || total == 0 || end != total - 1 || start > end {
            return Some(
                "the server answered another range than the complete tail asked for".to_string(),
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length != end - start + 1)
        {
            return Some("the response length disagrees with its Content-Range".to_string());
        }
        let Some(answer) = answer else {
            return Some("the answer does not give the file's length".to_string());
        };
        if part.is_inplace() {
            // In place the destination is the part, as the caller measured
            // it, and keeps no record: only its length is checked.
            return (offset > answer.size())
                .then(|| "the file is shorter than the part".to_string());
        }
        answer.refuses_part(part.recorded_source(), offset)
    }

    async fn start_over(
        part: atomic_write::ResumableFile,
        why: &str,
    ) -> Result<HttpResumeStep, ProviderError> {
        tracing::info!("partial download started over: {}", why);
        let part = part.start_over().await.map_err(ProviderError::IoError)?;
        Ok(HttpResumeStep::StartOver(Box::new(Self { part })))
    }

    async fn publish(part: atomic_write::ResumableFile) -> Result<(), ProviderError> {
        part.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })
    }
}

/// Run an `HttpResume` with `send`, which sends the download request with
/// the given `Range` header (none for the whole file). At most two requests:
/// a resume, and the whole file when the part had to start over.
pub async fn resume_http_download<S, Fut>(
    local_path: &str,
    what: &str,
    source: &atomic_write::ResumeSource,
    send: S,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError>
where
    S: Fn(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, ProviderError>>,
{
    let mut on_progress = on_progress;
    let mut download = HttpResume::open(local_path, source).await?;
    loop {
        let resuming = download.range().is_some();
        let response = send(download.range()).await?;
        match download.accept(response, what, &mut on_progress).await? {
            HttpResumeStep::Done => return Ok(()),
            HttpResumeStep::StartOver(next) if resuming => download = *next,
            HttpResumeStep::StartOver(_) => {
                return Err(ProviderError::TransferFailed(
                    "a download of the whole file was answered with a part of it".to_string(),
                ))
            }
        }
    }
}

/// Perform a resumable HTTP GET download for any provider that uses reqwest
/// (see `HttpResume`).
///
/// `build_request` takes an optional `Range` header value (e.g.
/// `"bytes=12345-"`) and returns a configured `reqwest::RequestBuilder`, so
/// each provider adds its own auth headers. It may be called twice: a resume,
/// then the whole file when the part had to start over.
pub async fn http_resumable_download<F>(
    local_path: &str,
    source: &atomic_write::ResumeSource,
    build_request: F,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError>
where
    F: Fn(Option<&str>) -> reqwest::RequestBuilder,
{
    resume_http_download(
        local_path,
        local_path,
        source,
        |range| {
            let request = build_request(range.as_deref());
            async move {
                request
                    .send()
                    .await
                    .map_http_err(|e| ProviderError::NetworkError(e.to_string()))
            }
        },
        on_progress,
    )
    .await
}

/// Requested 16 of #1202, through the shared HTTP resume: a part goes on
/// only against the version it was started from.
#[cfg(test)]
mod http_resume_tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The file a test server holds, and the `Range` of every request.
    struct Served {
        body: Vec<u8>,
        tag: &'static str,
        ignore_ranges: bool,
        asked: Vec<Option<String>>,
        bad_range: Option<&'static str>,
        bad_body: Vec<u8>,
        chunked: bool,
    }

    fn v1() -> Vec<u8> {
        (0..1000u32).map(|i| (i % 251) as u8).collect()
    }

    /// Another version of the same length, edited in the middle: the bytes a
    /// resume asks for twice are the same as `v1`'s, so only the record can
    /// tell the two apart.
    fn v2() -> Vec<u8> {
        let mut body = v1();
        body[100..200].iter_mut().for_each(|b| *b ^= 0x55);
        body
    }

    fn served(body: Vec<u8>, tag: &'static str) -> Arc<Mutex<Served>> {
        Arc::new(Mutex::new(Served {
            body,
            tag,
            ignore_ranges: false,
            asked: Vec::new(),
            bad_range: None,
            bad_body: Vec::new(),
            chunked: false,
        }))
    }

    /// Answer `bytes=<start>-` with a 206 (416 past the end), anything else
    /// with the whole file, always with the file's entity tag.
    async fn serve(state: Arc<Mutex<Served>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let range = text.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("range")
                            .then(|| value.trim().to_string())
                    });
                    let (head, body) = {
                        let mut state = state.lock().unwrap();
                        state.asked.push(range.clone());
                        let len = state.body.len();
                        let start =
                            range
                                .as_deref()
                                .filter(|_| !state.ignore_ranges)
                                .and_then(|r| {
                                    r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok()
                                });
                        let (status, body) = if let Some(range) = state.bad_range {
                            (
                                format!("206 Partial Content\r\nContent-Range: {range}"),
                                state.bad_body.clone(),
                            )
                        } else {
                            match start {
                                Some(start) if start >= len => (
                                    format!(
                                        "416 Range Not Satisfiable\r\nContent-Range: bytes */{len}"
                                    ),
                                    Vec::new(),
                                ),
                                Some(start) => (
                                    format!(
                                    "206 Partial Content\r\nContent-Range: bytes {start}-{}/{len}",
                                    len - 1
                                ),
                                    state.body[start..].to_vec(),
                                ),
                                None => ("200 OK".to_string(), state.body.clone()),
                            }
                        };
                        let (length, body) = if state.chunked {
                            let mut framed = format!("{:x}\r\n", body.len()).into_bytes();
                            framed.extend_from_slice(&body);
                            framed.extend_from_slice(b"\r\n0\r\n\r\n");
                            ("Transfer-Encoding: chunked".to_string(), framed)
                        } else {
                            (format!("Content-Length: {}", body.len()), body)
                        };
                        (
                            format!(
                                "HTTP/1.1 {status}\r\n{length}\r\nETag: {}\r\nLast-Modified: Wed, 15 Oct 2026 10:00:00 GMT\r\nConnection: close\r\n\r\n",
                                state.tag
                            ),
                            body,
                        )
                    };
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{addr}/f.bin")
    }

    async fn download(url: &str, local: &str) -> Result<(), ProviderError> {
        let client = reqwest::Client::new();
        http_resumable_download(
            local,
            &atomic_write::ResumeSource::new("test", url, "account", "/f.bin"),
            |range| {
                let request = client.get(url);
                match range {
                    Some(range) => request.header("Range", range),
                    None => request,
                }
            },
            None,
        )
        .await
    }

    /// Leave behind what an interrupted download of the file the server
    /// holds now leaves: `bytes`, recorded as that version.
    async fn leave_part(state: &Arc<Mutex<Served>>, url: &str, local: &str, bytes: &[u8]) {
        let answer = reqwest::get(url).await.unwrap();
        let version = multi_thread::RangeSourceFingerprint::of_answer(&answer).unwrap();
        drop(answer);
        state.lock().unwrap().asked.clear();
        let mut part = atomic_write::ResumableFile::open_fresh(local)
            .await
            .unwrap()
            .bind_source(&atomic_write::ResumeSource::new(
                "test", url, "account", "/f.bin",
            ))
            .await
            .unwrap();
        part.record_source(&version).await;
        part.write_all(bytes).await.unwrap();
        part.file_mut().flush().await.unwrap();
    }

    struct Case {
        dir: tempfile::TempDir,
        state: Arc<Mutex<Served>>,
        url: String,
    }

    impl Case {
        async fn new() -> Self {
            let state = served(v1(), "\"v1\"");
            let url = serve(Arc::clone(&state)).await;
            Self {
                dir: tempfile::tempdir().unwrap(),
                state,
                url,
            }
        }

        fn local(&self) -> String {
            self.dir.path().join("f.bin").to_str().unwrap().to_string()
        }

        async fn leave_part(&self, bytes: &[u8]) {
            leave_part(&self.state, &self.url, &self.local(), bytes).await;
        }

        fn replace(&self, body: Vec<u8>, tag: &'static str) {
            let mut state = self.state.lock().unwrap();
            state.body = body;
            state.tag = tag;
        }

        /// Download, and tell what was asked and what landed.
        async fn run(&self) -> (Vec<Option<String>>, Vec<u8>) {
            download(&self.url, &self.local()).await.unwrap();
            let local = self.local();
            assert!(
                !Path::new(&format!("{local}.aerotmp")).exists(),
                "the part outlived the download"
            );
            assert!(
                !Path::new(&format!("{local}.aerosrctmp")).exists(),
                "the record outlived the download"
            );
            let asked = std::mem::take(&mut self.state.lock().unwrap().asked);
            (asked, std::fs::read(local).unwrap())
        }
    }

    fn asked(ranges: &[Option<&str>]) -> Vec<Option<String>> {
        ranges.iter().map(|r| r.map(str::to_string)).collect()
    }

    #[tokio::test]
    async fn a_part_of_the_same_version_goes_on_from_its_last_byte() {
        let case = Case::new().await;
        case.leave_part(&v1()[..400]).await;
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=399-")]));
        assert!(landed == v1(), "a wrong file landed");
    }

    #[tokio::test]
    async fn a_partial_response_never_publishes_a_short_or_oversized_object() {
        let mut oversized = v1()[399..].to_vec();
        oversized.extend_from_slice(b"extra");
        for (range, body, chunked) in [
            ("bytes 399-499/1000", v1()[399..500].to_vec(), false),
            ("bytes 399-1001/1000", oversized.clone(), false),
            ("bytes 399-999/1000", v1()[399..500].to_vec(), false),
            ("bytes 399-999/1000", v1()[399..500].to_vec(), true),
            ("bytes 399-999/1000", oversized, true),
        ] {
            let case = Case::new().await;
            case.leave_part(&v1()[..400]).await;
            std::fs::write(case.local(), b"old destination").unwrap();
            {
                let mut state = case.state.lock().unwrap();
                state.bad_range = Some(range);
                state.bad_body = body;
                state.chunked = chunked;
            }
            assert!(
                download(&case.url, &case.local()).await.is_err(),
                "{range}, chunked={chunked}"
            );
            assert_eq!(std::fs::read(case.local()).unwrap(), b"old destination");
        }
    }

    #[tokio::test]
    async fn another_http_endpoint_cannot_resume_a_same_tagged_part() {
        let first = Case::new().await;
        let second = Case::new().await;
        first.leave_part(&v1()[..400]).await;
        second.replace(v2(), "\"v1\"");
        download(&second.url, &first.local()).await.unwrap();
        assert_eq!(second.state.lock().unwrap().asked, asked(&[None]));
        assert_eq!(std::fs::read(first.local()).unwrap(), v2());
    }

    #[tokio::test]
    async fn a_part_of_a_replaced_file_starts_over() {
        let case = Case::new().await;
        case.leave_part(&v1()[..400]).await;
        case.replace(v2(), "\"v2\"");
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=399-"), None]));
        assert!(landed == v2(), "two versions were joined");
    }

    /// The case found first: a part already as long as the file was
    /// published without a byte read, even when the file had been replaced.
    #[tokio::test]
    async fn a_whole_part_is_published_only_for_its_own_version() {
        let case = Case::new().await;
        case.leave_part(&v1()).await;
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=999-")]));
        assert!(landed == v1(), "a wrong file landed");

        case.leave_part(&v1()).await;
        case.replace(v2(), "\"v2\"");
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=999-"), None]));
        assert!(landed == v2(), "the old version was published");
    }

    #[tokio::test]
    async fn a_part_with_no_record_is_not_asked_about() {
        let case = Case::new().await;
        std::fs::write(format!("{}.aerotmp", case.local()), &v2()[..400]).unwrap();
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[None]));
        assert!(landed == v1(), "a wrong file landed");
    }

    /// The byte asked for twice is a second check, independent of the
    /// record: a part whose bytes are not the version its record names.
    #[tokio::test]
    async fn a_part_whose_last_byte_differs_starts_over() {
        let case = Case::new().await;
        let mut wrong = v1()[..400].to_vec();
        wrong[399] ^= 0xff;
        case.leave_part(&wrong).await;
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=399-"), None]));
        assert!(landed == v1(), "a wrong file landed");
    }

    #[tokio::test]
    async fn a_file_shorter_than_the_part_starts_it_over() {
        let case = Case::new().await;
        case.leave_part(&v1()[..400]).await;
        case.replace(v2()[..300].to_vec(), "\"v3\"");
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=399-"), None]));
        assert!(landed[..] == v2()[..300], "a wrong file landed");
    }

    #[tokio::test]
    async fn a_server_that_ignores_ranges_rewrites_the_part_whole() {
        let case = Case::new().await;
        case.leave_part(&v1()[..400]).await;
        case.state.lock().unwrap().ignore_ranges = true;
        let (requests, landed) = case.run().await;
        assert_eq!(requests, asked(&[Some("bytes=399-")]));
        assert!(landed == v1(), "a wrong file landed");
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

    /// A queue no front end reads keeps the newest warnings and counts the
    /// ones it dropped: it kept the oldest and dropped the newest silently.
    #[test]
    fn the_warning_queue_keeps_the_newest_and_counts_the_dropped() {
        for i in 0..MAX_PENDING_WARNINGS + 3 {
            report_warning(format!("w{i}"));
        }
        let taken = take_warnings();
        assert_eq!(taken.len(), MAX_PENDING_WARNINGS + 1, "{taken:?}");
        assert!(
            taken[0].starts_with("3 earlier warnings were dropped"),
            "{taken:?}"
        );
        assert_eq!(taken[1], "w3");
        assert_eq!(
            taken.last().unwrap(),
            &format!("w{}", MAX_PENDING_WARNINGS + 2)
        );
        assert!(take_warnings().is_empty());
    }

    /// The shared look before a rename, on a local folder: a file or a folder
    /// at the destination is AlreadyExists, a free name passes, and the
    /// source found under another letter case (as a case-insensitive backend
    /// reports it) is not another item.
    #[tokio::test]
    async fn the_shared_look_refuses_a_taken_destination_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"B").unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let mut provider = directory::DirectoryProvider::mtp(
            dir.path().to_path_buf(),
            "dev".to_string(),
            "Device".to_string(),
        );
        provider.connect().await.expect("connect");
        for to in ["/b.txt", "/d", "/d/"] {
            let outcome = refuse_occupied_destination(&mut provider, "/a.txt", to).await;
            assert!(
                matches!(outcome, Err(ProviderError::AlreadyExists(_))),
                "{to}: {outcome:?}"
            );
        }
        refuse_occupied_destination(&mut provider, "/a.txt", "/c.txt")
            .await
            .expect("a free name");
    }

    /// A case-insensitive backend answers `stat("/Readme.TXT")` with the
    /// source it stored as `readme.txt`: that is no other item. The other
    /// spelling stored as such, or a different name, is.
    #[test]
    fn only_the_source_found_under_another_case_is_not_another_item() {
        assert!(is_the_source_under_another_case(
            "/d/readme.txt",
            "/d/Readme.TXT",
            "readme.txt"
        ));
        assert!(!is_the_source_under_another_case(
            "/d/readme.txt",
            "/d/Readme.TXT",
            "Readme.TXT"
        ));
        assert!(!is_the_source_under_another_case(
            "/d/a.txt", "/d/b.txt", "a.txt"
        ));
        assert!(!is_the_source_under_another_case(
            "/d/a.txt",
            "/d/a.txt/",
            "a.txt"
        ));
    }

    #[test]
    fn a_replace_across_types_is_refused_as_already_exists() {
        assert!(refuse_replace_across_types("/x", false, false).is_ok());
        assert!(refuse_replace_across_types("/x", true, true).is_ok());
        for (source_is_dir, occupant_is_dir) in [(false, true), (true, false)] {
            let refused = refuse_replace_across_types("/x", source_is_dir, occupant_is_dir);
            assert!(
                matches!(refused, Err(ProviderError::AlreadyExists(_))),
                "{refused:?}"
            );
        }
    }

    /// Hidden, unique, naming what it stands in for, and never a name that a
    /// server refuses out of the box: Apache's default configuration denies
    /// every name that begins with `.ht` (measured on the lab WebDAV on
    /// 2026-10-10: PUT and MOVE to `.html.txt.aeroftp-replaced-1` answer
    /// 403), so setting `html.txt` aside failed before anything moved.
    #[test]
    fn a_set_aside_name_is_hidden_unique_readable_and_never_refused_by_default() {
        for name in ["report.pdf", "html.txt", "htdocs", "ht", ".htaccess", "a"] {
            let aside = set_aside_name(name);
            assert!(aside.starts_with('.'), "{aside}");
            assert!(aside.contains(name), "{aside}");
            assert!(aside.contains("aeroftp-replaced"), "{aside}");
            assert!(!aside.starts_with(".ht"), "{aside}");
            assert_ne!(aside, set_aside_name(name));
        }
    }

    /// Row 4: an empty body degrades to a stable placeholder, never panics.
    #[test]
    fn sanitize_api_error_empty_body_falls_back() {
        assert_eq!(sanitize_api_error(""), "unknown error");
    }
}

#[cfg(test)]
mod documented_file_limits_tests {
    use super::*;

    /// No official number, no value: these must never produce a warning.
    #[test]
    fn providers_without_a_documented_limit_have_none() {
        for provider in [
            ProviderType::Ftp,
            ProviderType::Sftp,
            ProviderType::WebDav,
            ProviderType::S3,
            ProviderType::Swift,
            ProviderType::Mega,
            ProviderType::Filen,
            ProviderType::Jottacloud,
            ProviderType::PCloud,
            ProviderType::OpenDrive,
            ProviderType::GitLab,
            ProviderType::Immich,
        ] {
            assert_eq!(
                documented_file_limits(provider),
                DocumentedFileLimits::default(),
                "{provider:?}"
            );
        }
    }

    #[test]
    fn a_documented_limit_is_the_highest_plan_and_the_larger_reading() {
        // Zoho WorkDrive: Starter 10 GB, Team 50 GB, Business 250 GB.
        assert_eq!(
            documented_file_limits(ProviderType::ZohoWorkdrive).max_file_size,
            Some(250 * GIB)
        );
        // Dropbox: 2 TB in the help center against 350 GB in the API spec.
        assert_eq!(
            documented_file_limits(ProviderType::Dropbox).max_file_size,
            Some(2_199_019_061_248)
        );
        assert_eq!(
            documented_file_limits(ProviderType::Box).max_name_chars,
            Some(255)
        );
    }

    /// What the provider set itself (S3 on AWS) is never replaced.
    #[test]
    fn provider_values_win_over_the_table() {
        let hints = TransferOptimizationHints {
            max_file_size: Some(7),
            ..Default::default()
        }
        .with_documented_limits(documented_file_limits(ProviderType::Box));
        assert_eq!(hints.max_file_size, Some(7));
        assert_eq!(hints.max_name_chars, Some(255));
    }
}

#[cfg(test)]
mod non_recursive_delete_tests {
    use super::*;

    type Answer<T> = fn() -> Result<T, ProviderError>;

    /// Scripted `stat`, `list`, `delete` and `rmdir`; every call is recorded.
    struct Scripted {
        stat: Answer<RemoteEntry>,
        list: Answer<Vec<RemoteEntry>>,
        delete: Answer<()>,
        rmdir: Answer<()>,
        calls: Vec<&'static str>,
    }

    impl Scripted {
        fn new(stat: Answer<RemoteEntry>, list: Answer<Vec<RemoteEntry>>) -> Self {
            Self {
                stat,
                list,
                delete: || Ok(()),
                rmdir: || Ok(()),
                calls: Vec::new(),
            }
        }
    }

    fn file() -> Result<RemoteEntry, ProviderError> {
        Ok(RemoteEntry::file("f".to_string(), "/f".to_string(), 1))
    }

    fn dir() -> Result<RemoteEntry, ProviderError> {
        Ok(RemoteEntry::directory("d".to_string(), "/d".to_string()))
    }

    fn one_child() -> Result<Vec<RemoteEntry>, ProviderError> {
        Ok(vec![RemoteEntry::file(
            ".keep".to_string(),
            "/d/.keep".to_string(),
            0,
        )])
    }

    fn no_child() -> Result<Vec<RemoteEntry>, ProviderError> {
        Ok(Vec::new())
    }

    fn not_found<T>() -> Result<T, ProviderError> {
        Err(ProviderError::NotFound("/d".to_string()))
    }

    #[async_trait]
    impl StorageProvider for Scripted {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::S3
        }
        fn display_name(&self) -> String {
            "scripted".to_string()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            self.calls.push("list");
            (self.list)()
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }
        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            _local_path: &str,
            _remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("upload".to_string()))
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("mkdir".to_string()))
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            self.calls.push("delete");
            (self.delete)()
        }
        async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
            self.calls.push("rmdir");
            // The contract every backend keeps now: a directory that lists
            // anything is refused, and only an empty one reaches the delete.
            let children = (self.list)()?;
            if !children.is_empty() {
                return Err(directory_not_empty(path, children.len()));
            }
            (self.rmdir)()
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            self.calls.push("rmdir_recursive");
            Ok(())
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(&mut self, _path: &str) -> Result<RemoteEntry, ProviderError> {
            self.calls.push("stat");
            (self.stat)()
        }
        async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("scripted".to_string())
        }
    }

    #[tokio::test]
    async fn recursive_remove_selects_the_file_verb_and_keeps_failures_closed() {
        let mut regular = Scripted::new(file, no_child);
        delete_recursive(&mut regular, "/f").await.unwrap();
        assert_eq!(regular.calls, ["stat", "delete"]);

        let mut directory = Scripted::new(dir, one_child);
        delete_recursive(&mut directory, "/d").await.unwrap();
        assert_eq!(directory.calls, ["stat", "rmdir_recursive"]);

        let mut link = Scripted::new(
            || {
                let mut entry = dir()?;
                entry.is_symlink = true;
                Ok(entry)
            },
            one_child,
        );
        delete_recursive(&mut link, "/l").await.unwrap();
        assert_eq!(link.calls, ["stat", "delete"]);

        let mut denied = Scripted::new(
            || Err(ProviderError::PermissionDenied("denied".into())),
            no_child,
        );
        assert!(matches!(
            delete_recursive(&mut denied, "/d").await,
            Err(ProviderError::PermissionDenied(_))
        ));
        assert_eq!(denied.calls, ["stat"]);

        let mut prefix = Scripted::new(not_found, one_child);
        delete_recursive(&mut prefix, "/d").await.unwrap();
        assert_eq!(prefix.calls, ["stat", "list", "rmdir_recursive"]);

        let mut stray = Scripted::new(not_found, || {
            Ok(vec![RemoteEntry::file("other".into(), "/other".into(), 1)])
        });
        assert!(delete_recursive(&mut stray, "/d").await.is_err());
        assert_eq!(stray.calls, ["stat", "list"]);

        for root in ["/", "", ".", "/a/..", "/a/../"] {
            let mut provider = Scripted::new(dir, one_child);
            assert!(
                delete_recursive(&mut provider, root).await.is_err(),
                "{root:?}"
            );
            assert!(
                provider.calls.is_empty(),
                "no mutation or listing of {root:?}"
            );
        }
    }

    #[tokio::test]
    async fn cross_profile_skip_uses_the_one_way_window_and_size_only_fallback() {
        use crate::cross_profile_transfer::{should_skip_existing, CrossProfileTransferEntry};
        let mut dest = Scripted::new(
            || {
                let mut entry = file()?;
                entry.modified = Some("2026-10-19T10:00:10Z".into());
                Ok(entry)
            },
            no_child,
        );
        for (modified, size, precision, expected) in [
            (
                Some("2026-10-19T12:00:10+02:00"),
                1,
                Some(std::time::Duration::from_secs(1)),
                true,
            ),
            (
                Some("2026-10-19T10:00:12Z"),
                1,
                Some(std::time::Duration::from_secs(1)),
                true,
            ),
            (
                Some("2026-10-19T10:00:13Z"),
                1,
                Some(std::time::Duration::from_secs(1)),
                false,
            ),
            (
                Some("2020-01-01T00:00:00Z"),
                1,
                Some(std::time::Duration::from_secs(1)),
                true,
            ),
            (Some("2030-01-01T00:00:00Z"), 1, None, true),
            (None, 1, Some(std::time::Duration::from_secs(1)), true),
            (
                Some("not a date"),
                1,
                Some(std::time::Duration::from_secs(1)),
                true,
            ),
            (None, 2, None, false),
        ] {
            let source = CrossProfileTransferEntry {
                source_path: "/src".into(),
                dest_path: "/f".into(),
                display_name: "f".into(),
                size,
                modified: modified.map(str::to_owned),
                is_dir: false,
            };
            assert_eq!(
                should_skip_existing(precision, &mut dest, "/f", &source)
                    .await
                    .unwrap(),
                expected,
                "{source:?}"
            );
        }
        let source = CrossProfileTransferEntry {
            source_path: "/src".into(),
            dest_path: "/d".into(),
            display_name: "d".into(),
            size: 0,
            modified: None,
            is_dir: false,
        };
        assert!(
            !should_skip_existing(None, &mut Scripted::new(dir, no_child), "/d", &source)
                .await
                .unwrap()
        );
        assert!(!should_skip_existing(
            None,
            &mut Scripted::new(not_found, no_child),
            "/d",
            &source
        )
        .await
        .unwrap());
        let mut denied = Scripted::new(
            || Err(ProviderError::PermissionDenied("denied".into())),
            no_child,
        );
        assert!(matches!(
            should_skip_existing(None, &mut denied, "/d", &source).await,
            Err(ProviderError::PermissionDenied(_))
        ));
    }

    /// `rm` without `-r` of a folder that still held files deleted them on
    /// every backend whose delete or rmdir of a folder takes its content
    /// along (S3, Azure, Drive, OneDrive, Dropbox, pCloud, Box, MEGA, ...).
    #[tokio::test]
    async fn a_directory_with_content_is_refused() {
        let mut p = Scripted::new(dir, one_child);
        let result = delete_non_recursive(&mut p, "/d").await;
        assert!(
            matches!(result, Err(ProviderError::DirectoryNotEmpty(ref m)) if m.contains("1 entry")),
            "{result:?}"
        );
        assert_eq!(p.calls, ["stat", "rmdir"]);
    }

    #[tokio::test]
    async fn an_empty_directory_is_removed_with_rmdir() {
        let mut p = Scripted::new(dir, no_child);
        delete_non_recursive(&mut p, "/d").await.expect("rm");
        assert_eq!(p.calls, ["stat", "rmdir"]);
    }

    #[tokio::test]
    async fn a_file_and_a_link_to_a_directory_go_through_delete() {
        let mut f = Scripted::new(file, one_child);
        delete_non_recursive(&mut f, "/f").await.expect("rm");
        assert_eq!(f.calls, ["stat", "delete"]);

        let link = || {
            let mut entry = RemoteEntry::directory("l".to_string(), "/l".to_string());
            entry.is_symlink = true;
            Ok(entry)
        };
        let mut l = Scripted::new(link, one_child);
        delete_non_recursive(&mut l, "/l").await.expect("rm");
        assert_eq!(l.calls, ["stat", "delete"]);
    }

    /// An object store sees no directory behind `d` (NotFound), Box and
    /// GitHub fail to parse a folder (ParseError): the listing decides.
    #[tokio::test]
    async fn a_directory_stat_cannot_describe_is_judged_by_its_listing() {
        let mut full = Scripted::new(not_found, one_child);
        let result = delete_non_recursive(&mut full, "/d").await;
        assert!(
            matches!(result, Err(ProviderError::DirectoryNotEmpty(_))),
            "{result:?}"
        );
        assert_eq!(full.calls, ["stat", "list"]);

        let mut parse = Scripted::new(
            || Err(ProviderError::ParseError("an array".to_string())),
            one_child,
        );
        let result = delete_non_recursive(&mut parse, "/d").await;
        assert!(
            matches!(result, Err(ProviderError::DirectoryNotEmpty(_))),
            "{result:?}"
        );

        // The directory marker of an empty S3 folder goes with rmdir; a
        // delete of `d` would answer 204 and leave `d/` in place.
        let mut empty = Scripted::new(not_found, no_child);
        delete_non_recursive(&mut empty, "/d").await.expect("rm");
        assert_eq!(empty.calls, ["stat", "list", "rmdir"]);

        let mut unseen_file = Scripted::new(not_found, no_child);
        unseen_file.rmdir = || Err(ProviderError::ServerError("not a folder".to_string()));
        delete_non_recursive(&mut unseen_file, "/f")
            .await
            .expect("rm");
        assert_eq!(unseen_file.calls, ["stat", "list", "rmdir", "delete"]);

        let mut missing = Scripted::new(not_found, not_found);
        missing.delete = not_found;
        let result = delete_non_recursive(&mut missing, "/x").await;
        assert!(
            matches!(result, Err(ProviderError::NotFound(_))),
            "{result:?}"
        );
        assert_eq!(missing.calls, ["stat", "list", "delete"]);
    }

    /// A listing that failed says nothing about the path. A timeout, a 503
    /// or a lost connection sent a path nobody had looked at to `delete`,
    /// which takes a folder's content along on several backends: the very
    /// thing a non-recursive delete refuses. Only a listing that says there
    /// is no folder there (NotFound) lets the delete go.
    #[tokio::test]
    async fn a_listing_that_failed_removes_nothing() {
        for list in [
            (|| Err(ProviderError::Timeout)) as Answer<Vec<RemoteEntry>>,
            || Err(ProviderError::ServerError("503".to_string())),
            || Err(ProviderError::ConnectionLost("reset".to_string())),
            || Err(ProviderError::NetworkError("reset".to_string())),
            || Err(ProviderError::PermissionDenied("/d".to_string())),
            || Err(ProviderError::ParseError("an html page".to_string())),
        ] {
            let mut p = Scripted::new(not_found, list);
            let result = delete_non_recursive(&mut p, "/d").await;
            assert!(result.is_err(), "{result:?}");
            assert_eq!(p.calls, ["stat", "list"], "{result:?}");
        }
    }

    /// The served DELE and REMOVE delete files only: a directory, empty or
    /// not, is refused with nothing removed, and so is a path whose listing
    /// failed for a reason that says nothing about it.
    #[tokio::test]
    async fn a_file_only_delete_refuses_every_directory() {
        let mut empty = Scripted::new(dir, no_child);
        let result = delete_file_only(&mut empty, "/d").await;
        assert!(
            matches!(result, Err(ProviderError::InvalidPath(ref m)) if m.contains("is a directory")),
            "{result:?}"
        );
        assert_eq!(empty.calls, ["stat"]);

        let mut f = Scripted::new(file, one_child);
        delete_file_only(&mut f, "/f")
            .await
            .expect("DELE of a file");
        assert_eq!(f.calls, ["stat", "delete"]);

        let mut listed = Scripted::new(not_found, one_child);
        assert!(delete_file_only(&mut listed, "/d").await.is_err());
        assert_eq!(listed.calls, ["stat", "list"]);

        let mut unparsed_folder = Scripted::new(
            || Err(ProviderError::ParseError("an array".to_string())),
            no_child,
        );
        assert!(delete_file_only(&mut unparsed_folder, "/d").await.is_err());
        assert_eq!(unparsed_folder.calls, ["stat", "list"]);

        let mut unlisted = Scripted::new(not_found, || Err(ProviderError::Timeout));
        assert!(delete_file_only(&mut unlisted, "/d").await.is_err());
        assert_eq!(unlisted.calls, ["stat", "list"]);

        let mut object_name = Scripted::new(not_found, no_child);
        delete_file_only(&mut object_name, "/f")
            .await
            .expect("DELE");
        assert_eq!(object_name.calls, ["stat", "list", "delete"]);
    }

    /// An ambiguous path (Cloudinary) and a failed `stat` remove nothing.
    #[tokio::test]
    async fn a_stat_that_failed_removes_nothing() {
        for stat in [
            (|| Err(ProviderError::InvalidPath("two items".to_string()))) as Answer<RemoteEntry>,
            || Err(ProviderError::ServerError("503".to_string())),
            || Err(ProviderError::NetworkError("reset".to_string())),
            || Err(ProviderError::Timeout),
            || Err(ProviderError::Cancelled),
        ] {
            let mut p = Scripted::new(stat, no_child);
            assert!(delete_non_recursive(&mut p, "/d").await.is_err());
            assert_eq!(p.calls, ["stat"]);
        }
    }

    /// RMD, SFTP RMDIR, the mount's rmdir and the GUI's non-recursive
    /// folder delete: one call, the backend's own `rmdir`, which refuses a
    /// directory that holds anything (round 2 of the 4.2.1 review).
    #[tokio::test]
    async fn remove_empty_directory_is_the_backends_rmdir() {
        let mut full = Scripted::new(dir, one_child);
        let result = remove_empty_directory(&mut full, "/d").await;
        assert!(
            matches!(result, Err(ProviderError::DirectoryNotEmpty(_))),
            "{result:?}"
        );
        assert_eq!(full.calls, ["rmdir"]);

        let mut empty = Scripted::new(dir, no_child);
        remove_empty_directory(&mut empty, "/d")
            .await
            .expect("rmdir");
        assert_eq!(empty.calls, ["rmdir"]);

        let mut unreadable = Scripted::new(dir, || Err(ProviderError::Timeout));
        assert!(remove_empty_directory(&mut unreadable, "/d").await.is_err());
        assert_eq!(unreadable.calls, ["rmdir"]);
    }

    /// The shared check a backend whose delete takes a directory's content
    /// along runs inside its `rmdir`: dotfiles count, a failed listing
    /// removes nothing.
    #[tokio::test]
    async fn refuse_non_empty_dir_counts_every_entry() {
        let mut full = Scripted::new(dir, || {
            Ok(vec![RemoteEntry::file(
                ".env".to_string(),
                "/d/.env".to_string(),
                1,
            )])
        });
        let result = full.refuse_non_empty_dir("/d").await;
        assert!(
            matches!(result, Err(ProviderError::DirectoryNotEmpty(ref m)) if m.contains("1 entry")),
            "{result:?}"
        );
        let mut empty = Scripted::new(dir, no_child);
        empty.refuse_non_empty_dir("/d").await.expect("empty");
        let mut unreadable = Scripted::new(dir, || Err(ProviderError::Timeout));
        assert!(matches!(
            unreadable.refuse_non_empty_dir("/d").await,
            Err(ProviderError::Timeout)
        ));
    }
}

/// The edit preflight and its refusal texts, and the fake the GUI edit's
/// tests share (`ai_core::gui_tools`).
#[cfg(test)]
pub(crate) mod edit_replace_tests {
    use super::*;
    use std::collections::HashMap;

    /// A backend that answers the two replace questions as told and whose
    /// `replace` behaves accordingly: atomic or set-aside, it puts the file
    /// in place; otherwise it is the rename, which refuses a taken name.
    /// Every upload, replace and delete is recorded.
    ///
    /// `stat` finds nothing unless `modes` or `links` name the path, so the
    /// tests that do not stand on either see the answer of a backend that
    /// cannot describe a file.
    pub(crate) struct EditFake {
        pub(crate) files: HashMap<String, Vec<u8>>,
        pub(crate) uploads: Vec<String>,
        pub(crate) replaces: Vec<(String, String)>,
        pub(crate) deleted: Vec<String>,
        pub(crate) atomic: bool,
        pub(crate) sets_aside: bool,
        /// Unix mode per path, reported by `stat` as `-rw-r--r--`, the way
        /// SFTP reports it. A new path gets 0644, the server default, and a
        /// replace moves the mode of the file it moves, as posix-rename does.
        pub(crate) modes: HashMap<String, u32>,
        /// Symbolic links: path to the target `stat` reports.
        pub(crate) links: HashMap<String, String>,
        /// What `supports_chmod` answers.
        pub(crate) chmod: bool,
        /// When set, `chmod` fails with this.
        pub(crate) chmod_fails_with: Option<String>,
        /// Another client's write, taken by the next upload: the path and
        /// its bytes land on the server right after the upload, between a
        /// caller's preflight and its publish.
        pub(crate) appears_on_upload: Option<(String, Vec<u8>)>,
        /// When set, `list` answers the files directly under the folder
        /// (otherwise an empty folder, which the edit tests rely on).
        pub(crate) lists_files: bool,
        /// Paths whose `delete` fails with PermissionDenied.
        pub(crate) undeletable: Vec<String>,
    }

    impl EditFake {
        /// `/t.txt` holds `old`.
        pub(crate) fn new(atomic: bool, sets_aside: bool) -> Self {
            Self {
                files: HashMap::from([("/t.txt".to_string(), b"old".to_vec())]),
                uploads: Vec::new(),
                replaces: Vec::new(),
                deleted: Vec::new(),
                atomic,
                sets_aside,
                modes: HashMap::new(),
                links: HashMap::new(),
                chmod: false,
                chmod_fails_with: None,
                appears_on_upload: None,
                lists_files: false,
                undeletable: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl StorageProvider for EditFake {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "edit-fake".to_string()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            if !self.lists_files {
                return Ok(Vec::new());
            }
            let dir = path.trim_end_matches('/');
            Ok(self
                .files
                .iter()
                .filter_map(|(p, data)| {
                    let (parent, name) = p.rsplit_once('/')?;
                    (parent == dir)
                        .then(|| RemoteEntry::file(name.to_string(), p.clone(), data.len() as u64))
                })
                .collect())
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }
        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(&mut self, path: &str) -> Result<Vec<u8>, ProviderError> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
        }
        async fn upload(
            &mut self,
            local_path: &str,
            remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let data = std::fs::read(local_path).map_err(ProviderError::IoError)?;
            self.uploads.push(remote_path.to_string());
            self.modes.entry(remote_path.to_string()).or_insert(0o644);
            self.files.insert(remote_path.to_string(), data);
            if let Some((path, theirs)) = self.appears_on_upload.take() {
                self.modes.insert(path.clone(), 0o644);
                self.files.insert(path, theirs);
            }
            Ok(())
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
            if self.undeletable.iter().any(|p| p == path) {
                return Err(ProviderError::PermissionDenied(format!("delete {path}")));
            }
            self.deleted.push(path.to_string());
            self.files.remove(path);
            Ok(())
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            if self.files.contains_key(to) {
                return Err(ProviderError::AlreadyExists(to.to_string()));
            }
            let data = self
                .files
                .remove(from)
                .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;
            self.files.insert(to.to_string(), data);
            Ok(())
        }
        async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            if !self.atomic && !self.sets_aside {
                return self.rename(from, to).await;
            }
            let data = self
                .files
                .remove(from)
                .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;
            self.files.insert(to.to_string(), data);
            if let Some(mode) = self.modes.remove(from) {
                self.modes.insert(to.to_string(), mode);
            }
            self.links.remove(to);
            self.replaces.push((from.to_string(), to.to_string()));
            Ok(())
        }
        async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
            Ok(self.atomic)
        }
        fn replace_sets_aside(&self) -> bool {
            self.sets_aside
        }
        fn supports_chmod(&self) -> bool {
            self.chmod
        }
        async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
            if let Some(message) = &self.chmod_fails_with {
                return Err(ProviderError::ServerError(message.clone()));
            }
            self.modes.insert(path.to_string(), mode);
            Ok(())
        }
        async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
            let link = self.links.get(path).cloned();
            let mode = self.modes.get(path).copied();
            if link.is_none() && mode.is_none() {
                return Err(ProviderError::NotFound(path.to_string()));
            }
            let size = self.files.get(path).map_or(0, |data| data.len() as u64);
            let mut entry = RemoteEntry::file(path.to_string(), path.to_string(), size);
            entry.is_symlink = link.is_some();
            entry.link_target = link;
            entry.permissions = mode.map(|mode| {
                let bit = |mask: u32, letter: char| if mode & mask != 0 { letter } else { '-' };
                [
                    '-',
                    bit(0o400, 'r'),
                    bit(0o200, 'w'),
                    bit(0o100, 'x'),
                    bit(0o040, 'r'),
                    bit(0o020, 'w'),
                    bit(0o010, 'x'),
                    bit(0o004, 'r'),
                    bit(0o002, 'w'),
                    bit(0o001, 'x'),
                ]
                .iter()
                .collect()
            });
            Ok(entry)
        }
        async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.files.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("edit-fake".to_string())
        }
    }

    /// The four answers of the edit preflight: atomic passes either way; a
    /// set-aside backend passes only with the opt-in and is offered it
    /// without; a backend with neither refuses both, and never offers it.
    #[tokio::test]
    async fn the_edit_preflight_offers_the_opt_in_only_where_it_works() {
        for (atomic, sets_aside, allow, passes, names_opt_in) in [
            (true, false, false, true, false),
            (true, false, true, true, false),
            (false, true, false, false, true),
            (false, true, true, true, false),
            (false, false, false, false, false),
            (false, false, true, false, true),
        ] {
            let mut p = EditFake::new(atomic, sets_aside);
            let outcome = ensure_edit_can_replace(&mut p, "/t.txt", allow, "`--opt`").await;
            let case = format!("atomic {atomic}, sets aside {sets_aside}, opt-in {allow}");
            assert_eq!(outcome.is_ok(), passes, "{case}: {outcome:?}");
            if let Ok(publish) = outcome {
                assert_eq!(publish, StagedPublish::Replace, "{case}: the file is there");
                continue;
            }
            if let Err(e) = outcome {
                let text = e.to_string();
                assert!(text.contains("Nothing was written"), "{case}: {text}");
                assert_eq!(text.contains("`--opt`"), names_opt_in, "{case}: {text}");
            }
            assert!(p.uploads.is_empty() && p.replaces.is_empty(), "{case}");
        }
    }

    /// CodeRabbit on #1180: the opt-in replaces through a set-aside only a
    /// file the check found at the target. Onto a free name the publish is
    /// the creation, which refuses what another client writes there after
    /// the check instead of setting it aside and deleting it.
    #[tokio::test]
    async fn an_opt_in_onto_a_free_name_stays_a_creation() {
        let mut p = EditFake::new(false, true);
        p.files.clear();
        let edit = ensure_edit_can_replace(&mut p, "/t.txt", true, "`--opt`").await;
        assert_eq!(edit.ok(), Some(StagedPublish::Create));
        let marker = ensure_marker_replace(&mut p, "/.aerocrypt.tsv", true, None).await;
        assert_eq!(marker.ok(), Some(StagedPublish::Create));
        p.files.insert("/t.txt".to_string(), b"theirs".to_vec());
        p.files
            .insert("/.aerocrypt.tsv".to_string(), b"theirs".to_vec());
        let edit = ensure_edit_can_replace(&mut p, "/t.txt", true, "`--opt`").await;
        assert_eq!(edit.ok(), Some(StagedPublish::Replace));
        let marker = ensure_marker_replace(&mut p, "/.aerocrypt.tsv", true, None).await;
        assert_eq!(marker.ok(), Some(StagedPublish::Replace));
        assert!(p.uploads.is_empty() && p.replaces.is_empty());
    }

    #[test]
    fn permission_mode_reads_the_forms_providers_report() {
        for (text, mode) in [
            ("rw-------", Some(0o600)),
            ("-rw-------", Some(0o600)),
            ("-rwxr-xr-x", Some(0o755)),
            ("-rw-r--r--+", Some(0o644)),
            ("-rwsr-xr-x", Some(0o4755)),
            ("-rwxr-sr-T", Some(0o3754)),
            ("drwxrwxrwt", Some(0o1777)),
            ("0644", Some(0o644)),
            ("644", Some(0o644)),
            ("100600", Some(0o600)),
            ("adfrw", None),
            ("public", None),
            ("-rw-r--r", None),
            ("-rw-r--r-q", None),
            ("", None),
        ] {
            assert_eq!(permission_mode(text), mode, "{text:?}");
        }
    }

    /// A link's target is named so the caller can edit it instead: as it
    /// is when absolute, resolved against the link's folder when relative.
    #[test]
    fn an_edit_of_a_link_names_the_file_it_points_to() {
        let mut entry = RemoteEntry::file(".env".into(), "/srv/app/.env".into(), 0);
        assert!(refuse_edit_of_symlink(&entry, "/srv/app/.env").is_ok());
        entry.is_symlink = true;
        entry.link_target = Some("../shared/.env".into());
        let text = refuse_edit_of_symlink(&entry, "/srv/app/.env").unwrap_err();
        assert!(text.contains("`/srv/app/../shared/.env`"), "{text}");
        assert!(text.contains("Nothing was written"), "{text}");
        entry.link_target = Some("/etc/app.env".into());
        let text = refuse_edit_of_symlink(&entry, "/srv/app/.env").unwrap_err();
        assert!(text.contains("`/etc/app.env`"), "{text}");
        entry.link_target = None;
        let text = refuse_edit_of_symlink(&entry, "/srv/app/.env").unwrap_err();
        assert!(text.contains("symbolic link"), "{text}");
    }

    /// Only a provider with `chmod` has a mode to carry over, and a
    /// permission string that is not a mode is reported, not guessed.
    #[test]
    fn an_edit_carries_the_mode_only_where_chmod_can_set_it() {
        let mut entry = RemoteEntry::file("f".into(), "/f".into(), 0);
        assert_eq!(EditOriginal::of(&entry, true), EditOriginal::Nothing);
        entry.permissions = Some("-rw-------".into());
        assert_eq!(EditOriginal::of(&entry, false), EditOriginal::Nothing);
        assert_eq!(EditOriginal::of(&entry, true), EditOriginal::Mode(0o600));
        entry.permissions = Some("adfrw".into());
        assert_eq!(
            EditOriginal::of(&entry, true),
            EditOriginal::Unreadable("adfrw".into())
        );
    }

    /// A refused marker publish names no way around it. The generic refusal
    /// offers `put` over the file, and before that the edit flags: a marker
    /// has neither, since the new one exists only inside the operation that
    /// built it and a marker cut short by a rewrite in place cannot open the
    /// vault. It says that nothing was written, and which marker.
    #[tokio::test]
    async fn a_marker_refusal_offers_no_put_and_no_edit_opt_in() {
        for sets_aside in [false, true] {
            let mut p = EditFake::new(false, sets_aside);
            p.files
                .insert("/.aerocrypt.tsv".to_string(), b"marker".to_vec());
            let text = ensure_marker_replace(&mut p, "/.aerocrypt.tsv", false, None)
                .await
                .unwrap_err()
                .to_string();
            assert!(text.contains("Nothing was written"), "{text}");
            assert!(text.contains("`/.aerocrypt.tsv`"), "{text}");
            assert!(
                !text.contains("`put`"),
                "a marker refusal must not send the user to upload over it: {text}"
            );
            assert!(
                !text.contains("allow-non-atomic") && !text.contains("allow_non_atomic"),
                "a marker refusal must not suggest an edit flag: {text}"
            );
            assert!(p.uploads.is_empty() && p.replaces.is_empty());
            assert_eq!(
                p.files.get("/.aerocrypt.tsv").map(Vec::as_slice),
                Some(&b"marker"[..])
            );
        }
    }

    /// Policy C (owner, 2026-10-11): a marker that already exists is
    /// replaced through a set-aside only with the user's consent. Every
    /// combination of backend, target and consent, before anything is
    /// staged: with consent a set-aside backend replaces the marker that is
    /// there and still only creates onto a free name, and a backend with
    /// neither is refused; without it the check is the file one, and a refusal
    /// on a set-aside backend names the caller's opt-in, nowhere else.
    #[tokio::test]
    async fn a_marker_is_replaced_through_a_set_aside_only_with_consent() {
        const OPT_IN: &str = "`--allow-non-atomic`";
        for atomic in [false, true] {
            for sets_aside in [false, true] {
                for occupied in [false, true] {
                    for consent in [false, true] {
                        let mut p = EditFake::new(atomic, sets_aside);
                        if occupied {
                            p.files
                                .insert("/.aerocrypt.tsv".to_string(), b"marker".to_vec());
                        }
                        let outcome =
                            ensure_marker_replace(&mut p, "/.aerocrypt.tsv", consent, Some(OPT_IN))
                                .await;
                        let case = format!(
                            "atomic {atomic}, sets aside {sets_aside}, occupied {occupied}, \
                             consent {consent}: {outcome:?}"
                        );
                        // Consent widens a set-aside backend to the marker
                        // that is there, never to a name found free.
                        let expected = if atomic {
                            Some(StagedPublish::Replace)
                        } else if sets_aside && !occupied {
                            Some(StagedPublish::Create)
                        } else if sets_aside && consent {
                            Some(StagedPublish::Replace)
                        } else {
                            None
                        };
                        match (&outcome, expected) {
                            (Ok(publish), Some(want)) => assert_eq!(*publish, want, "{case}"),
                            (Err(ProviderError::NotSupported(text)), None) => {
                                assert!(text.contains("Nothing was written"), "{case}");
                                assert_eq!(
                                    text.contains(OPT_IN),
                                    sets_aside && !consent,
                                    "the opt-in is named only where it would work: {case}"
                                );
                            }
                            _ => panic!("{case}"),
                        }
                        assert!(p.uploads.is_empty() && p.replaces.is_empty(), "{case}");
                    }
                }
            }
        }
        let mut no_opt_in = EditFake::new(false, true);
        no_opt_in
            .files
            .insert("/.aerocrypt.tsv".to_string(), b"marker".to_vec());
        let text = ensure_marker_replace(&mut no_opt_in, "/.aerocrypt.tsv", false, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(!text.contains("allow-non-atomic"), "{text}");
    }

    #[test]
    fn a_set_aside_copy_is_recognised_by_its_exact_shape() {
        let name = ".aerocrypt.tsv";
        let made = set_aside_name(name);
        assert!(is_set_aside_copy_of(&made, name), "{made}");
        assert!(is_set_aside_copy_of(
            ".aeroftp-replaced-0a1b2c3d..aerocrypt.tsv",
            name
        ));
        for other in [
            ".aerocrypt.tsv",
            ".aeroftp-replaced-0a1b2c3d.aerocrypt.tsv",
            ".aeroftp-replaced-0A1B2C3D..aerocrypt.tsv",
            ".aeroftp-replaced-0a1b2c3..aerocrypt.tsv",
            ".aeroftp-replaced-0a1b2c3d..aerocrypt.tsv.bak",
            ".aeroftp-replaced-0a1b2c3d..aeroftp-crypt.json",
        ] {
            assert!(!is_set_aside_copy_of(other, name), "{other}");
        }
    }

    /// After a revocation a set-aside copy of the marker still opens with
    /// the revoked factor: every copy in the folder goes, this run's and
    /// older ones, and one that cannot be deleted is answered by its path.
    #[tokio::test]
    async fn set_aside_copies_are_deleted_and_the_undeletable_named() {
        let mut p = EditFake::new(false, true);
        p.lists_files = true;
        for path in [
            "/v/.aerocrypt.tsv",
            "/v/.aeroftp-replaced-00000001..aerocrypt.tsv",
            "/v/.aeroftp-replaced-00000002..aerocrypt.tsv",
            "/v/.aeroftp-replaced-00000003.other.txt",
            "/w/.aeroftp-replaced-00000004..aerocrypt.tsv",
        ] {
            p.files.insert(path.to_string(), b"x".to_vec());
        }
        p.undeletable
            .push("/v/.aeroftp-replaced-00000002..aerocrypt.tsv".to_string());
        let left = remove_set_aside_copies(&mut p, "/v", ".aerocrypt.tsv").await;
        assert_eq!(p.deleted, ["/v/.aeroftp-replaced-00000001..aerocrypt.tsv"]);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, "/v/.aeroftp-replaced-00000002..aerocrypt.tsv");
        assert!(matches!(left[0].1, ProviderError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn the_marker_replace_mode_matches_what_the_publish_check_allows() {
        for (atomic, sets_aside, mode) in [
            (true, false, MarkerReplaceMode::Atomic),
            (true, true, MarkerReplaceMode::Atomic),
            (false, true, MarkerReplaceMode::SetsAside),
            (false, false, MarkerReplaceMode::Unsupported),
        ] {
            let mut p = EditFake::new(atomic, sets_aside);
            assert_eq!(marker_replace_mode(&mut p).await.unwrap(), mode);
        }
    }

    /// A marker written where none is (the heal of a missing one, a legacy
    /// migration) replaces nothing. On a backend whose replace sets the old
    /// item aside, the publish onto a free name is its rename, one step with
    /// no moment in which a file is missing, so the preflight passes and
    /// answers a creation; a backend whose replace is only its rename may
    /// have no rename at all, and is still refused.
    #[tokio::test]
    async fn a_publish_onto_a_free_name_passes_where_the_replace_sets_aside() {
        for (sets_aside, passes) in [(true, true), (false, false)] {
            let mut p = EditFake::new(false, sets_aside);
            let marker = ensure_marker_replace(&mut p, "/.aerocrypt.tsv", false, None).await;
            let file = ensure_atomic_replace(&mut p, "/.aerocrypt.tsv").await;
            for outcome in [&marker, &file] {
                assert_eq!(
                    outcome.as_ref().ok(),
                    passes.then_some(&StagedPublish::Create),
                    "sets aside {sets_aside}: {outcome:?}"
                );
            }
            assert!(p.uploads.is_empty() && p.replaces.is_empty());
        }
        let mut atomic = EditFake::new(true, false);
        let publish = ensure_marker_replace(&mut atomic, "/.aerocrypt.tsv", false, None).await;
        assert_eq!(publish.ok(), Some(StagedPublish::Replace));
        let publish = ensure_atomic_replace(&mut atomic, "/.aerocrypt.tsv").await;
        assert_eq!(publish.ok(), Some(StagedPublish::Replace));
    }

    /// CodeRabbit on #1154: a creation leaves alone an item another client
    /// put at the target after the check, answers AlreadyExists, and never
    /// replaces; onto a name still free it moves the staged file in.
    #[tokio::test]
    async fn a_creation_never_replaces_what_another_client_wrote() {
        let mut p = EditFake::new(false, true);
        p.files = HashMap::from([
            ("/m.tmp".to_string(), b"ours".to_vec()),
            ("/m.tsv".to_string(), b"theirs".to_vec()),
        ]);
        let text = StagedPublish::Create
            .publish(&mut p, "/m.tmp", "/m.tsv")
            .await
            .unwrap_err();
        assert!(matches!(text, ProviderError::AlreadyExists(_)), "{text}");
        assert!(text.to_string().contains("nothing was published"), "{text}");
        assert!(p.replaces.is_empty(), "{:?}", p.replaces);
        assert_eq!(
            p.files.get("/m.tsv").map(Vec::as_slice),
            Some(&b"theirs"[..])
        );
        assert_eq!(p.files.get("/m.tmp").map(Vec::as_slice), Some(&b"ours"[..]));

        p.files.remove("/m.tsv");
        StagedPublish::Create
            .publish(&mut p, "/m.tmp", "/m.tsv")
            .await
            .expect("a free name");
        assert_eq!(
            p.files,
            HashMap::from([("/m.tsv".to_string(), b"ours".to_vec())])
        );
        assert!(p.replaces.is_empty(), "{:?}", p.replaces);
    }
}

/// A local HTTP endpoint that accepts every connection and never answers,
/// the shape of a server that hangs after `accept()`. The accepted sockets
/// stay open for the life of the test process, so a client sees no reset.
#[cfg(test)]
pub(crate) fn mute_http_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a mute listener");
    let url = format!(
        "http://{}/",
        listener.local_addr().expect("listener address")
    );
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    url
}

/// A local HTTP endpoint that answers `200` with a `Content-Length` of 1000,
/// sends the first 7 bytes of the body and then goes quiet, the shape of a
/// download whose server stalls halfway. The sockets are held like the mute
/// endpoint's, so a client sees no reset.
#[cfg(test)]
pub(crate) fn stalled_body_endpoint() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a stalling listener");
    let url = format!(
        "http://{}/",
        listener.local_addr().expect("listener address")
    );
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial");
            let _ = stream.flush();
            held.push(stream);
        }
    });
    url
}

#[cfg(test)]
mod http_send_error_tests {
    use super::*;
    use std::time::Duration;

    async fn failed_send(url: &str, read_timeout: Duration) -> reqwest::Error {
        // The providers' own limit: a read timeout, not a total one.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(read_timeout)
            .build()
            .expect("client");
        client
            .get(url)
            .send()
            .await
            .expect_err("the request must fail")
    }

    #[tokio::test]
    async fn a_request_that_gets_no_reply_is_a_timeout_whatever_the_site_calls_it() {
        let e = failed_send(&mute_http_endpoint(), Duration::from_millis(300)).await;
        let mapped = http_send_error(e, |e| ProviderError::ConnectionFailed(e.to_string()));
        assert!(matches!(mapped, ProviderError::Timeout), "{mapped:?}");
    }

    #[tokio::test]
    async fn a_refused_connection_keeps_the_sites_own_error() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/", closed.local_addr().expect("address"));
        drop(closed);
        // reqwest starts its read timeout when the request is sent, so it
        // covers the connect too, and Windows answers a connect to a closed
        // localhost port only after about 2 s of SYN retries: a read timeout
        // shorter than that turns the refusal into a timeout there. The
        // providers' own read timeout is far longer than that wait.
        let e = failed_send(&url, Duration::from_secs(30)).await;
        let mapped = http_send_error(e, |e| ProviderError::ConnectionFailed(e.to_string()));
        assert!(
            matches!(mapped, ProviderError::ConnectionFailed(_)),
            "{mapped:?}"
        );
    }

    /// A body that stops coming is the same timeout as an answer that never
    /// came: the limited reader used to file it as `TransferFailed`.
    #[tokio::test]
    async fn a_body_that_stops_coming_is_a_timeout() {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_millis(300))
            .build()
            .expect("client");
        let response = client
            .get(stalled_body_endpoint())
            .send()
            .await
            .expect("the head arrives");
        let read = response_bytes_with_limit(response, 1 << 20).await;
        assert!(matches!(read, Err(ProviderError::Timeout)), "{read:?}");
    }

    #[test]
    fn a_wrapper_passes_a_timeout_through_and_wraps_the_rest() {
        let wrap = |e: ProviderError| ProviderError::AuthenticationFailed(format!("step: {e}"));
        assert!(matches!(
            keep_timeout(ProviderError::Timeout, wrap),
            ProviderError::Timeout
        ));
        let wrapped = keep_timeout(ProviderError::ConnectionFailed("refused".into()), wrap);
        assert!(
            matches!(&wrapped, ProviderError::AuthenticationFailed(m) if m.contains("refused")),
            "{wrapped:?}"
        );
    }
}

/// The class behind `http_send_error`: no provider maps the result of a
/// request or of a body read with a plain `map_err`, which files a timeout
/// under the call site's own variant. Read from the sources, so a new
/// provider written the old way fails here instead of in a script that
/// waited 30 minutes for an exit code that says "connection refused".
#[cfg(test)]
mod http_timeout_class_tests {
    use std::path::Path;

    /// Mappers that already classify a timeout, by name.
    const CLASSIFYING: &[&str] = &[
        "http_send_error",
        "timed_out_or",
        "send_error",
        "connect_send_error",
        "map_reqwest_error",
        // A range window's request and body: built on `http_send_error`, plus
        // a body cut by a dropped connection, so the window is resumed from
        // its mark.
        "range_read_error",
    ];

    /// Files outside the class, and why. Their errors never become a
    /// `ProviderError`: the GitHub device flow and OAuth 1 sign-in return a
    /// `String` to the interactive sign-in dialog, and OAuth 2 hands its
    /// error to the `oauth2` crate's own type.
    const OUTSIDE: &[&str] = &["github/auth.rs", "oauth1.rs", "oauth2.rs"];

    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read providers dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The index of the quote that closes a character literal opening at
    /// `quote` (`'"'`, `'('`, `'\''`, `'\u{2014}'`), or `None` for a lifetime
    /// (`'a`, `'static`), which has no closing quote.
    fn char_literal_end(s: &[u8], quote: usize) -> Option<usize> {
        let first = *s.get(quote + 1)?;
        let close = if first == b'\\' {
            (quote + 3..s.len().min(quote + 12)).find(|&i| s[i] == b'\'')?
        } else {
            // One character, which in UTF-8 takes up to four bytes.
            quote
                + 1
                + std::str::from_utf8(&s[quote + 1..s.len().min(quote + 5)])
                    .or_else(|e| std::str::from_utf8(&s[quote + 1..quote + 1 + e.valid_up_to()]))
                    .ok()?
                    .chars()
                    .next()?
                    .len_utf8()
        };
        (s.get(close) == Some(&b'\'')).then_some(close)
    }

    /// Index just past the parenthesis that closes the one at `open`. String
    /// and character literals are skipped, so a `"("` or a `'"'` inside the
    /// call does not move the count. Raw strings are read as plain ones, which
    /// is wrong only for a raw string that ends in a backslash.
    fn past_close(s: &[u8], open: usize) -> usize {
        past_close_pair(s, open, b'(', b')')
    }

    /// [`past_close`] for any pair of delimiters, braces included.
    fn past_close_pair(s: &[u8], open: usize, opening: u8, closing: u8) -> usize {
        let mut depth = 0usize;
        let mut i = open;
        while i < s.len() {
            match s[i] {
                b'"' => {
                    i += 1;
                    while i < s.len() && s[i] != b'"' {
                        if s[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'\'' => {
                    if let Some(end) = char_literal_end(s, i) {
                        i = end;
                    }
                }
                b if b == opening => depth += 1,
                b if b == closing => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        s.len()
    }

    #[test]
    fn the_scan_skips_literals_that_hold_a_parenthesis_or_a_quote() {
        let call = br#"f(a, '(', b'"', "x)", 'static_lifetime, '\u{29}', ')') tail"#;
        let end = past_close(call, 1);
        assert_eq!(&call[..end], &call[..call.len() - " tail".len()]);
    }

    #[test]
    fn no_provider_maps_a_request_failure_without_classifying_a_timeout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let request = regex::Regex::new(
            r"\.(?:send|text|bytes|chunk|json(?:::<[^()]*?>)?)\(\)|\bsend_with_retry(?:_replayable)?\(|\.execute\(",
        )
        .expect("regex");
        // The mapper is read after the `.await` and after any calls that keep
        // the error as it is (`.map(|b| b.to_vec())`, `.and_then(..)`, a log
        // in `.inspect_err(..)`): `get_raw_bytes` of the GitHub client hid its
        // `map_err` behind a `.map` and filed a body timeout as a network
        // error. A call that changes the error ends the chain.
        let awaited = regex::Regex::new(r"^\s*\.await").expect("regex");
        let keeps_error =
            regex::Regex::new(r"^\s*\.(?:map|and_then|inspect|inspect_err)\(").expect("regex");
        let then_map_err = regex::Regex::new(r"^\s*\.map_err\(").expect("regex");
        let mut plain = Vec::new();
        for file in &files {
            let relative = file
                .strip_prefix(&root)
                .expect("under providers")
                .to_string_lossy()
                .replace('\\', "/");
            if OUTSIDE.contains(&relative.as_str()) {
                continue;
            }
            let source = std::fs::read_to_string(file).expect("read source");
            let bytes = source.as_bytes();
            for found in request.find_iter(&source) {
                let end = if found.as_str().ends_with("()") {
                    found.end()
                } else {
                    past_close(bytes, found.end() - 1)
                };
                let Some(wait) = awaited.find(&source[end..]) else {
                    continue;
                };
                let mut end = end + wait.end();
                while let Some(call) = keeps_error.find(&source[end..]) {
                    end = past_close(bytes, end + call.end() - 1);
                }
                let Some(map) = then_map_err.find(&source[end..]) else {
                    continue;
                };
                let open = end + map.end() - 1;
                let mapper = &source[open..past_close(bytes, open)];
                if !CLASSIFYING.iter().any(|name| mapper.contains(name)) {
                    let line = source[..found.start()].matches('\n').count() + 1;
                    plain.push(format!("{relative}:{line}"));
                }
            }
        }
        assert!(
            plain.is_empty(),
            "map_err on a request or body read that files a timeout under another variant \
             (use `map_http_err`, see `http_send_error`): {plain:?}"
        );
    }

    /// Requests whose failure is handled by a `match` arm and deliberately
    /// never becomes a `ProviderError`, by file and enclosing function, and
    /// why. A best-effort cleanup or discovery logs and gives up; a probe
    /// falls back to the path that reports its own failure.
    const ARM_OUTSIDE: &[(&str, &str)] = &[
        // Logs and moves on; the upload it follows has already succeeded.
        ("onedrive.rs", "purge_old_versions"),
        // Optional discovery: the listing goes on without team folders.
        ("zoho_workdrive.rs", "discover_team_folders"),
        // A probe: on any failure the v2 sign-in runs and reports its own.
        ("swift.rs", "detect_auth_version"),
        // An opportunistic probe: the provider's own download reports.
        ("multi_thread.rs", "try_http_concurrent_range_download"),
        // A reason string for "did the object change", not an error.
        ("multi_thread.rs", "http_range_source_moved"),
    ];

    /// The body of the arm that starts at `start` (just past its `=>`): a
    /// block, or an expression up to the comma or brace that ends it.
    fn arm_body(s: &[u8], start: usize) -> &[u8] {
        let mut i = start;
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        if s.get(i) == Some(&b'{') {
            return &s[i..past_close_brace(s, i)];
        }
        let begin = i;
        let mut depth = 0usize;
        while i < s.len() {
            match s[i] {
                b'"' => {
                    i += 1;
                    while i < s.len() && s[i] != b'"' {
                        if s[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        &s[begin..i]
    }

    fn past_close_brace(s: &[u8], open: usize) -> usize {
        past_close_pair(s, open, b'{', b'}')
    }

    #[test]
    fn an_arm_body_ends_at_its_comma_or_block() {
        let arms = br#"{ Err(e) => f(e, "a,b"), Ok(r) => r }"#;
        let start = arms.windows(2).position(|w| w == b"=>").unwrap() + 2;
        assert_eq!(arm_body(arms, start), br#"f(e, "a,b")"#);
        let block = b"{ Err(e) => { g(e); continue; } Ok(r) => r }";
        let start = block.windows(2).position(|w| w == b"=>").unwrap() + 2;
        assert_eq!(arm_body(block, start), b"{ g(e); continue; }");
    }

    /// The reads of one `read_range` body, each judged by itself (offsets
    /// into the body).
    #[derive(Debug, Default, PartialEq)]
    struct RangedReads {
        /// Requests whose error goes through `range_read_error` and then to
        /// the caller as it is (`?`).
        requests: Vec<usize>,
        /// Body reads whose error does the same, and capped body reads made
        /// with `range_window_with_limit`, whose error goes to the caller as
        /// it is.
        bodies: Vec<usize>,
        /// Every other read: a mapper that files a dropped connection as a
        /// hard failure (before `range_read_error` or after it), the
        /// whole-object reader, or a shape the scan cannot follow (a `match`
        /// or an `if let` on the read, a request not awaited where it is
        /// made). The scan cannot tell what such a shape does with the
        /// error, so it does not pass it.
        plain: Vec<usize>,
    }

    fn ranged_reads(body: &str) -> RangedReads {
        let read = regex::Regex::new(
            r"\.(?:send|text|bytes|chunk|json(?:::<[^()]*?>)?)\(\)|\bsend_with_retry(?:_replayable)?\(|\.execute\(",
        )
        .expect("regex");
        let awaited = regex::Regex::new(r"^\s*\.await").expect("regex");
        let keeps_error =
            regex::Regex::new(r"^\s*\.(?:map|and_then|inspect|inspect_err)\(").expect("regex");
        let mapped = regex::Regex::new(r"^\s*\.(?:map_err|map_http_err)\(").expect("regex");
        let propagated = regex::Regex::new(r"^\s*\?").expect("regex");
        // The text of an error status, read for its message: its error
        // cannot leave the call.
        let dropped = regex::Regex::new(
            r"^\s*\.(?:unwrap_or_default\(\)|unwrap_or\(|unwrap_or_else\(|ok\(\))",
        )
        .expect("regex");
        // Spelled in two pieces so this test is not one of its own matches.
        let whole = concat!("response_bytes", "_with_limit(");
        let window = concat!("range_window", "_with_limit(");
        let bytes = body.as_bytes();
        // Past the calls that keep the error as it is, the error has to go
        // to the caller: a classified error converted again is lost.
        let reaches_caller = |mut end: usize| {
            while let Some(call) = keeps_error.find(&body[end..]) {
                end = past_close(bytes, end + call.end() - 1);
            }
            propagated.is_match(&body[end..])
        };
        let mut reads = RangedReads::default();
        reads
            .plain
            .extend(body.match_indices(whole).map(|(at, _)| at));
        for (at, _) in body.match_indices(window) {
            let end = past_close(bytes, at + window.len() - 1);
            let kept = awaited
                .find(&body[end..])
                .is_some_and(|wait| reaches_caller(end + wait.end()));
            if kept {
                reads.bodies.push(at);
            } else {
                reads.plain.push(at);
            }
        }
        for found in read.find_iter(body) {
            let text = found.as_str();
            let request =
                text.starts_with(".send") || text.starts_with("send_with") || text == ".execute(";
            let end = if text.ends_with("()") {
                found.end()
            } else {
                past_close(bytes, found.end() - 1)
            };
            let Some(wait) = awaited.find(&body[end..]) else {
                // A body helper of another type (`str::bytes`) is no read; a
                // request whose future is awaited elsewhere is one the scan
                // cannot follow.
                if request {
                    reads.plain.push(found.start());
                }
                continue;
            };
            let mut end = end + wait.end();
            while let Some(call) = keeps_error.find(&body[end..]) {
                end = past_close(bytes, end + call.end() - 1);
            }
            if dropped.is_match(&body[end..]) {
                continue;
            }
            let kept = mapped.find(&body[end..]).is_some_and(|map| {
                let open = end + map.end() - 1;
                let close = past_close(bytes, open);
                body[open..close].contains("range_read_error") && reaches_caller(close)
            });
            match (kept, request) {
                (true, true) => reads.requests.push(found.start()),
                (true, false) => reads.bodies.push(found.start()),
                (false, _) => reads.plain.push(found.start()),
            }
        }
        reads
    }

    /// The scan judges each read by itself, up to where its error leaves. A
    /// request handled by a `match` passed it while the body read of the same
    /// provider was classified, with an arm that kept the timeout and filed
    /// the dropped connection as a hard failure; then a capped body read
    /// passed it on its name alone, with its error converted again to
    /// `TransferFailed`. The reads are spelled through constants so that
    /// these lines are not reads of this file for the other scans.
    #[test]
    fn the_ranged_read_scan_judges_every_read_by_itself() {
        const SEND: &str = concat!(".se", "nd()");
        const BYTES: &str = concat!(".by", "tes()");
        const TEXT: &str = concat!(".te", "xt()");
        let counts =
            |reads: &RangedReads| (reads.requests.len(), reads.bodies.len(), reads.plain.len());
        let classified = format!(
            "let resp = client.get(url){SEND}.await.map_err(super::multi_thread::range_read_error)?;
             let body = resp{BYTES}.await.map_err(super::multi_thread::range_read_error)?;"
        );
        assert_eq!(counts(&ranged_reads(&classified)), (1, 1, 0));
        let by_hand = format!(
            "let resp = match client.get(url){SEND}.await {{
                 Ok(resp) => resp,
                 Err(e) => return Err(super::http_send_error(e, |e| {{
                     ProviderError::TransferFailed(e.to_string())
                 }})),
             }};
             let body = super::{}resp, MAX).await?;",
            concat!("range_window", "_with_limit(")
        );
        assert_eq!(counts(&ranged_reads(&by_hand)), (0, 1, 1));
        let hard = format!(
            "resp{BYTES}.await.map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?"
        );
        assert_eq!(counts(&ranged_reads(&hard)), (0, 0, 1));
        let whole = concat!("super::response_bytes", "_with_limit(resp, MAX).await?");
        assert_eq!(counts(&ranged_reads(whole)), (0, 0, 1));
        let deferred = format!("let pending = client.get(url){SEND}; timeout(wait, pending).await");
        assert_eq!(counts(&ranged_reads(&deferred)), (0, 0, 1));
        let status = format!("let text = resp{TEXT}.await.unwrap_or_default();");
        assert_eq!(ranged_reads(&status), RangedReads::default());
        let capped_again = format!(
            "let bytes = super::{}resp, MAX)
                 .await
                 .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;",
            concat!("range_window", "_with_limit(")
        );
        assert_eq!(counts(&ranged_reads(&capped_again)), (0, 0, 1));
        let capped_kept = format!(
            "let bytes = super::{}resp, MAX).await.map(|b| b.len())?;",
            concat!("range_window", "_with_limit(")
        );
        assert_eq!(counts(&ranged_reads(&capped_kept)), (0, 1, 0));
        let mapped_again = format!(
            "resp{BYTES}.await.map_err(super::multi_thread::range_read_error)
                 .map_err(|e| ProviderError::TransferFailed(e.to_string()))?"
        );
        assert_eq!(counts(&ranged_reads(&mapped_again)), (0, 0, 1));
    }

    /// The class behind `range_read_error`: inside the `read_range` of every
    /// provider, a request or a body read is mapped with `range_read_error`,
    /// and a capped body is read with `range_window_with_limit`, so a window
    /// the connection drops is resumed from its mark instead of failing the
    /// download. B2 and Koofr filed a request with no answer as
    /// `ConnectionFailed` / `TransferFailed`, and every HTTP provider filed a
    /// body cut halfway as a hard failure. A request sent through a
    /// provider's own sender (WebDAV, S3, Azure) is not seen here: that
    /// sender files it as a `NetworkError`, and the provider's
    /// `a_window_the_connection_drops_is_recoverable` reads through it.
    #[test]
    fn every_ranged_read_keeps_a_dropped_connection_resumable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
        let mut files = Vec::new();
        sources(&root, &mut files);
        // Spelled in two pieces so this test is not one of its own matches.
        let signature = concat!("async fn ", "read_range(");
        let mut plain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for file in &files {
            let relative = file
                .strip_prefix(&root)
                .expect("under providers")
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(file).expect("read source");
            for (at, _) in source.match_indices(signature) {
                let open = at + source[at..].find('{').expect("a body");
                let body = &source[open..past_close_brace(source.as_bytes(), open)];
                let line_of = |offset: usize| source[..open + offset].matches('\n').count() + 1;
                let reads = ranged_reads(body);
                plain.extend(
                    reads
                        .plain
                        .iter()
                        .map(|&offset| format!("{relative}:{}", line_of(offset))),
                );
                if !reads.requests.is_empty() {
                    seen.insert((relative.clone(), "request"));
                }
                if !reads.bodies.is_empty() {
                    seen.insert((relative.clone(), "body"));
                }
            }
        }
        assert!(
            plain.is_empty(),
            "a ranged read that files a dropped connection as a hard failure, or that the scan \
             cannot follow (map it with `multi_thread::range_read_error`, read a capped body \
             with `range_window_with_limit`): {plain:?}"
        );
        // The scan has to see each read it is about: a request moved into a
        // helper it does not read would pass on nothing. WebDAV, S3 and Azure
        // send through their own sender.
        for (file, kind) in [
            ("azure.rs", "body"),
            ("b2.rs", "request"),
            ("b2.rs", "body"),
            ("cloudinary.rs", "request"),
            ("cloudinary.rs", "body"),
            ("imagekit.rs", "request"),
            ("imagekit.rs", "body"),
            ("koofr.rs", "request"),
            ("koofr.rs", "body"),
            ("s3.rs", "body"),
            ("uploadcare.rs", "request"),
            ("uploadcare.rs", "body"),
            ("webdav.rs", "body"),
        ] {
            assert!(
                seen.contains(&(file.to_string(), kind)),
                "no classified {kind} seen in the read_range of {file}: {seen:?}"
            );
        }
    }

    /// The same class in a second shape: the request is the scrutinee of a
    /// `match` (or an `if let`) and its `Err(e)` arm handles the error by
    /// hand. Filen's chunk download kept only the error's text, Yandex's
    /// chunked PUT and Internxt's start-upload filed a timeout as
    /// `TransferFailed` / `ServerError`; the `map_err` scan above never saw
    /// them. An arm passes when it classifies (a name from `CLASSIFYING`, or
    /// `is_timeout`) or hands the error on unchanged.
    #[test]
    fn no_provider_handles_a_failed_request_without_classifying_a_timeout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let request = regex::Regex::new(
            r"\.(?:send|text|bytes|chunk|json(?:::<[^()]*?>)?)\(\)|\bsend_with_retry(?:_replayable)?\(|\.execute\(",
        )
        .expect("regex");
        let awaited = regex::Regex::new(r"^\s*\.await").expect("regex");
        let keeps_error =
            regex::Regex::new(r"^\s*\.(?:map|and_then|inspect|inspect_err)\(").expect("regex");
        let opens_block = regex::Regex::new(r"^\s*\{").expect("regex");
        let err_arm = regex::Regex::new(r"Err\((\w+)\)\s*=>").expect("regex");
        let enclosing = regex::Regex::new(r"\bfn\s+(\w+)").expect("regex");
        let mut plain = Vec::new();
        for file in &files {
            let relative = file
                .strip_prefix(&root)
                .expect("under providers")
                .to_string_lossy()
                .replace('\\', "/");
            if OUTSIDE.contains(&relative.as_str()) {
                continue;
            }
            let source = std::fs::read_to_string(file).expect("read source");
            let bytes = source.as_bytes();
            for found in request.find_iter(&source) {
                let end = if found.as_str().ends_with("()") {
                    found.end()
                } else {
                    past_close(bytes, found.end() - 1)
                };
                let Some(wait) = awaited.find(&source[end..]) else {
                    continue;
                };
                let mut end = end + wait.end();
                while let Some(call) = keeps_error.find(&source[end..]) {
                    end = past_close(bytes, end + call.end() - 1);
                }
                let Some(open) = opens_block.find(&source[end..]) else {
                    continue;
                };
                let open = end + open.end() - 1;
                let block = &source[open..past_close_brace(bytes, open)];
                let function = enclosing
                    .captures_iter(&source[..found.start()])
                    .last()
                    .map(|c| c[1].to_string())
                    .unwrap_or_default();
                if ARM_OUTSIDE
                    .iter()
                    .any(|(f, name)| *f == relative && *name == function)
                {
                    continue;
                }
                for arm in err_arm.captures_iter(block) {
                    let name = &arm[1];
                    let body = String::from_utf8_lossy(arm_body(
                        block.as_bytes(),
                        arm.get(0).expect("whole match").end(),
                    ))
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                    let classifies =
                        CLASSIFYING.iter().any(|c| body.contains(c)) || body.contains("is_timeout");
                    let handed_on = body == format!("Err({name})")
                        || body == format!("return Err({name})")
                        || body == format!("return Err({name});");
                    if !classifies && !handed_on {
                        let line = source[..found.start()].matches('\n').count() + 1;
                        plain.push(format!("{relative}:{line} ({function})"));
                    }
                }
            }
        }
        assert!(
            plain.is_empty(),
            "a request failure handled in a match arm that files a timeout under another \
             variant (classify it with `http_send_error`, or list the site in ARM_OUTSIDE \
             with the reason): {plain:?}"
        );
    }

    /// The other side of the class: `map_http_err` only where a timeout can
    /// happen. Building a request or a client, and naming a multipart part's
    /// type, fail before anything is sent, so the timeout branch there would
    /// only tell the reader a story that cannot happen.
    #[test]
    fn no_provider_classifies_a_timeout_where_nothing_was_sent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let offline = regex::Regex::new(
            r"(?:\.build\(\)|\.mime_str\((?:[^()]|\([^()]*\))*\))\s*\.map_http_err\(",
        )
        .expect("regex");
        let mut misplaced = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file).expect("read source");
            for found in offline.find_iter(&source) {
                let relative = file.strip_prefix(&root).expect("under providers");
                let line = source[..found.start()].matches('\n').count() + 1;
                misplaced.push(format!("{}:{line}", relative.display()));
            }
        }
        assert!(
            misplaced.is_empty(),
            "map_http_err where nothing was sent yet (use `map_err`): {misplaced:?}"
        );
    }
}

#[cfg(test)]
mod folder_chain_tests {
    use super::probe_fake::ProbeFake;
    use super::*;

    /// A folder put into one that exists costs one request; the levels
    /// above are walked only when the parent is missing.
    #[tokio::test]
    async fn a_folder_chain_starts_from_the_deepest_folder() {
        let mut fake = ProbeFake::new().with_folders(&["/a", "/a/b"]);
        create_folder_chain(&mut fake, "/a/b/c", true)
            .await
            .unwrap();
        assert_eq!(fake.writes, vec!["mkdir /a/b/c"]);

        let mut fake = ProbeFake::new().with_folders(&["/a", "/a/b", "/a/b/c"]);
        create_folder_chain(&mut fake, "/a/b/c", true)
            .await
            .unwrap();
        assert_eq!(fake.writes, vec!["mkdir /a/b/c"]);

        let mut fake = ProbeFake::new().with_folders(&["/a"]);
        create_folder_chain(&mut fake, "/a/b/c", true)
            .await
            .unwrap();
        assert_eq!(
            fake.writes,
            vec!["mkdir /a/b/c", "mkdir /a", "mkdir /a/b", "mkdir /a/b/c"]
        );
        assert!(fake.has_folder("/a/b/c"));
    }

    /// A refusal that is not a missing parent stops a strict chain at the
    /// folder it was for; a lenient one goes on to the next level.
    #[tokio::test]
    async fn a_strict_folder_chain_reports_the_folder_that_failed() {
        let mut fake = ProbeFake::new().with_folders(&[]);
        fake.refuse_mkdir = Some("/a".into());
        let (folder, error) = create_folder_chain(&mut fake, "/a/b", true)
            .await
            .unwrap_err();
        assert_eq!(folder, "/a");
        assert!(
            matches!(error, ProviderError::PermissionDenied(_)),
            "{error:?}"
        );

        let mut fake = ProbeFake::new().with_folders(&[]);
        fake.refuse_mkdir = Some("/a".into());
        create_folder_chain(&mut fake, "/a/b", false).await.unwrap();
    }
}
