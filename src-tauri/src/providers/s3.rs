//! S3 Storage Provider
//!
//! Implementation of the StorageProvider trait for Amazon S3 and S3-compatible storage.
//! Supports AWS S3, MinIO, Backblaze B2, DigitalOcean Spaces, Cloudflare R2, Wasabi, etc.
//!
//! This implementation uses reqwest with AWS Signature Version 4 for authentication,
//! avoiding the heavyweight aws-sdk-s3 dependency for better compile times and smaller binaries.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::{Client, Method, StatusCode};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::sts;
use super::{
    sanitize_api_error, FileVersion, MultipartHandle, ProviderError, ProviderTransferExecutorKind,
    ProviderType, RemoteEntry, S3Config, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult,
    StorageProvider, TrashEntry, UploadedPart,
};

/// Returns true when the S3 endpoint targets a loopback address or a known
/// local-bridge hostname (Filen Desktop S3 at local.s3.filen.io, MEGAcmd, ...).
/// Used to auto-trust self-signed TLS certificates in S3Provider::new without
/// requiring the user to flip verify_cert manually for every loopback profile.
/// URL-encode an S3 key path segment-by-segment, preserving `/` as the
/// path separator. AWS SigV4 and any compliant S3 server expect the wire
/// URL to contain percent-encoded keys (spaces -> `%20`, emojis -> UTF-8
/// percent triplets). `urlencoding::encode` alone would also escape `/`,
/// so we split on `/` first.
///
/// Spaces in mkdir/put paths previously hit Filen's local S3 bridge with
/// a 401 Unauthorized because `url::Url::parse` lenient-encoded the URL
/// while the signature canonical path was computed differently. Canonicalise
/// here. Issue #128.
fn encode_s3_key_path(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    key.split('/')
        .map(|segment| {
            if segment.is_empty() {
                String::new()
            } else {
                urlencoding::encode(segment).into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Decode a listed S3 `<Key>` for endpoints that return keys already percent-
/// encoded. Filen's S3 bridge does this (issue #196); AWS/MinIO/Wasabi return
/// `<Key>` verbatim, so `filen_decode` is false there and the key is untouched.
/// Factored out of `list_keys_with_prefix` so the double-encode regression
/// (#368: an emoji child re-encoded to `%25F0...` broke SigV4 on copy/delete)
/// has a unit test without HTTP mocking.
fn filen_decode_listed_key(key: String, filen_decode: bool) -> String {
    if filen_decode {
        urlencoding::decode(&key)
            .map(|c| c.into_owned())
            .unwrap_or(key)
    } else {
        key
    }
}

/// Convert a raw S3 `ETag` value to a usable MD5 hex digest, or `None`.
///
/// An S3 ETag equals the object MD5 ONLY for single-part uploads without
/// SSE-KMS/SSE-C: a quoted 32-hex string. Multipart uploads use
/// `"<hash>-<partcount>"` (the `-N` suffix is not the object MD5) and SSE
/// objects use an opaque value. We accept exactly 32 lowercase-hex chars
/// (which also rejects any `-N` suffix), mirroring rclone's S3 hash
/// behaviour: omit rather than report a wrong digest.
fn etag_to_md5(raw: &str) -> Option<String> {
    let v = raw.trim().trim_matches('"').to_ascii_lowercase();
    if v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(v)
    } else {
        None
    }
}

/// Plan the `UploadPartCopy` parts for a server-side multipart copy of
/// `source_size` bytes at the given `part_size`. Returns a vector of
/// `(part_number, range_start, range_end_inclusive)` triples ready to be
/// dispatched as `x-amz-copy-source-range: bytes=A-B` headers.
///
/// Pure function so the part-planning logic can be unit-tested without
/// any HTTP mocking. The S3 cap of 10000 parts per upload is not
/// enforced here: the caller decides how to surface it.
fn plan_copy_parts(source_size: u64, part_size: u64) -> Vec<(u32, u64, u64)> {
    if source_size == 0 || part_size == 0 {
        return Vec::new();
    }
    let total = source_size.div_ceil(part_size);
    let mut out = Vec::with_capacity(total as usize);
    let mut offset = 0u64;
    let mut part_number = 1u32;
    while offset < source_size {
        let end_inclusive = (offset + part_size - 1).min(source_size - 1);
        out.push((part_number, offset, end_inclusive));
        offset = end_inclusive + 1;
        part_number = part_number.saturating_add(1);
    }
    out
}

fn is_local_s3_endpoint(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    let stripped = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .unwrap_or(&lower);
    let host_only = stripped
        .split('/')
        .next()
        .unwrap_or(stripped)
        .split('@')
        .next_back()
        .unwrap_or(stripped);
    let host = host_only
        .rsplit_once(':')
        .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
        .map(|(h, _)| h)
        .unwrap_or(host_only);
    matches!(host, "127.0.0.1" | "::1" | "[::1]" | "localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "local.s3.filen.io"
}

/// KE-E1: S3 rate-limit detection.
///
/// AWS S3 and every S3-compatible backend (MinIO, Wasabi, Backblaze S3,
/// Cloudflare R2, Storj, DO Spaces, IDrive e2, ...) signal throttling
/// through one of two codes:
/// - **429 Too Many Requests** (Wasabi, Backblaze S3, R2, generic compat),
/// - **503 Slow Down** with `<Code>SlowDown</Code>` in the XML body
///   (AWS S3 canonical throttle signal).
///
/// Other 503 reasons (`<Code>ServiceUnavailable</Code>`, `InternalError`)
/// are surfaced as retryable transients by `send_with_retry`, but they are
/// NOT rate-limit signals: feeding them to AIMD as a Retry-After hint
/// would conflate genuine backend faults with quota pressure. Only the
/// explicit `SlowDown` body counts.
fn s3_is_rate_limited(status: u16, body: &str) -> bool {
    if status == 429 {
        return true;
    }
    if status == 503 {
        // SlowDown is the canonical AWS throttle code. Match case-sensitively
        // on the XML tag form because S3 always upper-cases the first letter.
        return body.contains("<Code>SlowDown</Code>");
    }
    false
}

/// KE-E1: Compute the marker tail to append to an S3 `ProviderError`
/// message when the response was rate-limited and a usable `Retry-After`
/// header was present. Returns `None` when the response is not a throttle
/// signal or the hint is missing/unparseable. Pure-fn for test coverage;
/// the call site does the I/O.
fn s3_retry_marker_tail(status: u16, body: &str, retry_header: Option<&str>) -> Option<String> {
    if !s3_is_rate_limited(status, body) {
        return None;
    }
    let hint = super::retry_after::parse_retry_after_seconds(retry_header.unwrap_or(""))?;
    Some(crate::transfer_dag::adaptive::embed_retry_after_marker(
        hint.as_secs(),
    ))
}

/// KE-E1: Build the error message tail for an S3 HTTP failure, appending
/// the Retry-After marker if the response is a throttle signal. Use this
/// at every error site that returns `ProviderError::TransferFailed(...)`
/// or `ProviderError::Other(...)` from an S3 HTTP response.
///
/// The `prefix` is prepended verbatim; the standard tail
/// `<status>: <sanitised body>` is appended; the optional marker
/// ` [retry-after-secs=NN]` is appended last.
fn format_s3_error(
    prefix: &str,
    status: reqwest::StatusCode,
    body: &str,
    retry_header: Option<&str>,
) -> String {
    let mut msg = format!("{} ({}): {}", prefix, status, sanitize_api_error(body));
    if let Some(tail) = s3_retry_marker_tail(status.as_u16(), body, retry_header) {
        msg.push_str(&tail);
    }
    msg
}

/// S3 Storage Provider
#[derive(Clone)]
pub struct S3Provider {
    config: S3Config,
    client: Client,
    current_prefix: String,
    connected: bool,
    /// Clock offset in seconds to compensate for local system clock skew.
    /// Auto-detected from the server's Date header on time-related auth errors.
    clock_offset_secs: i64,
    /// Override for multipart upload part size (default: 5 MB)
    upload_chunk_override: Option<usize>,
    /// Number of concurrent Range streams for multi-thread download (1 = disabled).
    /// Used by `download_multi_thread`. Set via `set_multi_thread_download`.
    multi_thread_streams: usize,
    /// Minimum file size (bytes) above which multi-thread download is engaged.
    /// Below this threshold, the standard single-stream path is always used.
    multi_thread_cutoff: u64,
    /// KE-B1.1: Override for multipart upload parallelism. `None` keeps the
    /// historical 4-part-in-flight ceiling used by both `upload_multipart_streaming`
    /// and the server-side multipart copy planner. Set via
    /// `set_upload_concurrency`.
    upload_concurrency_override: Option<usize>,
    /// KE-B1.2: When `true`, `connect()` skips the GET-prefix probe against
    /// the bucket root and assumes the credentials are valid. Used when the
    /// account is allowed to write to a bucket it cannot list (typical S3
    /// IAM policy that grants `PutObject` but denies `ListBucket`). Set via
    /// `set_no_check_bucket`.
    no_check_bucket: bool,
    /// KE-B1.3: When `true`, signed requests use `UNSIGNED-PAYLOAD` instead of
    /// the SHA-256 of the body in the `x-amz-content-sha256` header. Skips
    /// the per-part hashing cost on large multipart uploads. Trades off
    /// SigV4 integrity verification for throughput on trusted networks. Set
    /// via `set_disable_checksum`. The CompleteMultipartUpload request body
    /// is excluded from this optimisation because it must remain SIGNED for
    /// AWS to validate the etag list (Filen, MinIO, B2 follow suit).
    disable_checksum: bool,
    /// KE-B1.4: Canned ACL override for upload operations
    /// (`private` / `public-read` / `public-read-write` /
    /// `authenticated-read` / `aws-exec-read` / `bucket-owner-read` /
    /// `bucket-owner-full-control`). Emitted as `x-amz-acl` on
    /// CreateMultipartUpload and single-PUT. `None` = backend default
    /// (typically `private`). Set via `set_acl`.
    acl_override: Option<String>,
    /// KE-B1.5: Storage class override for upload operations. Takes
    /// precedence over the saved-profile `storage_class` setting. Emitted
    /// as `x-amz-storage-class` on CreateMultipartUpload and single-PUT.
    /// `None` = fall back to `config.storage_class`, else backend default.
    /// Set via `set_storage_class_override`.
    storage_class_override: Option<String>,
    /// Temporary credentials acquired through STS `AssumeRole` (issue #301).
    /// Populated by `connect()` when `config.role_arn` is set and refreshed
    /// proactively before expiry (Fase 3); the data-plane signers use these
    /// instead of the long-term base credentials in `config`, which are left
    /// untouched so a reconnect or refresh can re-assume the role. `None` means
    /// the base credentials are used directly. Behind `Arc<RwLock>` so the
    /// clones spawned for parallel multipart parts share one refreshing cell.
    temp_credentials: Arc<std::sync::RwLock<Option<sts::TempCredentials>>>,
    /// Serializes STS refreshes so concurrent multipart parts re-assume the
    /// role once instead of stampeding the STS endpoint near expiry.
    sts_refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Owned snapshot of the credentials a single signing pass uses, taken under
/// one read lock so the access key, secret and session token always come from
/// the same credential set (issue #301, Fase 3).
struct EffectiveCredentials {
    access_key_id: String,
    secret_access_key: secrecy::SecretString,
    session_token: Option<secrecy::SecretString>,
}

impl S3Provider {
    /// Create a new S3 provider with the given configuration
    pub fn new(config: S3Config) -> Result<Self, ProviderError> {
        debug!(
            "[S3] new(): endpoint={:?} bucket={} region={} path_style={} verify_cert={}",
            config.endpoint, config.bucket, config.region, config.path_style, config.verify_cert,
        );
        // Auto-trust self-signed certs for loopback / local-bridge endpoints
        // (Filen Desktop S3, MEGAcmd S3, MinIO localhost, ...). Reqwest 0.13 with
        // rustls-platform-verifier rejects CA-as-end-entity certs even with
        // danger_accept_invalid_certs in some paths, so we force the unsafe
        // verifier when the host is loopback (127.0.0.1, ::1, localhost) or a
        // known local-bridge hostname.
        let endpoint_is_local = config
            .endpoint
            .as_deref()
            .map(is_local_s3_endpoint)
            .unwrap_or(false);
        let accept_invalid_certs = !config.verify_cert || endpoint_is_local;
        debug!(
            "[S3] new(): endpoint_is_local={} accept_invalid_certs={}",
            endpoint_is_local, accept_invalid_certs,
        );
        let mut client_builder = Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .http1_only();
        if accept_invalid_certs {
            debug!("[S3] accepting invalid TLS certificates (self-signed / loopback)");
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }
        let client = client_builder.build().map_err(|e| {
            ProviderError::ConnectionFailed(format!("HTTP client init failed: {e}"))
        })?;

        Ok(Self {
            config,
            client,
            current_prefix: String::new(),
            connected: false,
            clock_offset_secs: 0,
            upload_chunk_override: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: Self::MULTI_THREAD_CUTOFF_DEFAULT,
            upload_concurrency_override: None,
            no_check_bucket: false,
            disable_checksum: false,
            acl_override: None,
            storage_class_override: None,
            temp_credentials: Arc::new(std::sync::RwLock::new(None)),
            sts_refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Default parallelism for multipart upload parts.
    ///
    /// 4 keeps `tokio::JoinSet` lightweight on consumer connections (cap at
    /// the historical legacy value) while still saturating typical 1 Gbit
    /// links to AWS S3 / MinIO. Overridable through
    /// [`set_upload_concurrency`].
    pub const UPLOAD_CONCURRENCY_DEFAULT: usize = 4;
    /// Hard ceiling for `set_upload_concurrency` — beyond this, additional
    /// parallelism mostly burns sockets and adds 429 risk.
    pub const UPLOAD_CONCURRENCY_MAX: usize = 64;

    /// KE-B1.1: Override the number of parts that
    /// `upload_multipart_streaming` and the server-side multipart copy
    /// planner keep in flight. Clamped to `[1, UPLOAD_CONCURRENCY_MAX]`.
    /// Passing `0` resets to default.
    pub fn set_upload_concurrency(&mut self, parts_in_flight: usize) {
        if parts_in_flight == 0 {
            self.upload_concurrency_override = None;
            return;
        }
        self.upload_concurrency_override =
            Some(parts_in_flight.clamp(1, Self::UPLOAD_CONCURRENCY_MAX));
    }

    /// Number of multipart upload parts to keep in flight, honoring any
    /// `set_upload_concurrency` override. Pure read-only inspector used by
    /// the multipart upload + server-side-copy paths.
    pub fn effective_upload_concurrency(&self) -> usize {
        self.upload_concurrency_override
            .unwrap_or(Self::UPLOAD_CONCURRENCY_DEFAULT)
    }

    /// KE-B1.2: Skip the bucket-existence probe inside `connect()`. Use when
    /// the credentials are scoped to write-only access on a known bucket
    /// (`ListBucket` denied, `PutObject` allowed). Matches rclone's
    /// `--s3-no-check-bucket`.
    pub fn set_no_check_bucket(&mut self, enabled: bool) {
        self.no_check_bucket = enabled;
    }

    /// KE-B1.3: Suppress payload SHA-256 hashing in signed requests by
    /// using the SigV4 `UNSIGNED-PAYLOAD` placeholder. Big win on CPU when
    /// uploading multipart parts (~500 MiB+ each). Matches rclone's
    /// `--s3-disable-checksum`.
    pub fn set_disable_checksum(&mut self, enabled: bool) {
        self.disable_checksum = enabled;
    }

    /// KE-B1.4: Set the canned ACL applied to subsequent uploads.
    /// Validation is permissive (we accept any non-empty string) so users
    /// of S3-compatible backends with vendor-specific ACL extensions are
    /// not blocked; AWS rejects unknown values at the API level.
    /// Passing `None` clears the override and lets the backend apply its
    /// default ACL (usually `private`).
    pub fn set_acl(&mut self, acl: Option<String>) {
        self.acl_override = acl.filter(|s| !s.trim().is_empty());
    }

    /// KE-B1.5: Override the storage class for upload operations.
    /// Takes precedence over `S3Config::storage_class`. Validation is
    /// permissive (vendor-specific tiers accepted). Passing `None` clears
    /// the override and falls back to the profile-level storage class, or
    /// the backend default if both are unset.
    pub fn set_storage_class_override(&mut self, sc: Option<String>) {
        self.storage_class_override = sc.filter(|s| !s.trim().is_empty());
    }

    /// Resolved storage class: the runtime override wins, else the
    /// profile-level setting, else `None` (backend default).
    fn effective_storage_class(&self) -> Option<&str> {
        self.storage_class_override
            .as_deref()
            .or(self.config.storage_class.as_deref())
    }

    /// Resolved canned ACL for upload operations, or `None` if untouched.
    fn effective_acl(&self) -> Option<&str> {
        self.acl_override.as_deref()
    }

    /// Maximum number of concurrent download streams accepted by `set_multi_thread_download`.
    pub const MULTI_THREAD_MAX_STREAMS: usize = 16;
    /// Default cutoff above which multi-thread download engages (250 MiB).
    /// Mirrors rclone's `--multi-thread-cutoff` default.
    pub const MULTI_THREAD_CUTOFF_DEFAULT: u64 = 250 * 1024 * 1024;

    /// Returns the current UTC time adjusted for any detected clock skew.
    fn now_adjusted(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(self.clock_offset_secs)
    }

    /// Re-assume the role this many seconds before the temporary credentials
    /// expire, so long operations (and slow multipart uploads) never sign with
    /// a token that lapses mid-flight. 5 minutes mirrors the AWS SDK default.
    const STS_REFRESH_THRESHOLD_SECS: i64 = 300;

    /// A consistent snapshot of the credentials the data-plane signers must
    /// use: the STS-issued temporary credentials when a role has been assumed
    /// (issue #301), else the long-term base credentials (or a manually-supplied
    /// session token) from `config`. Snapshotting under a single read lock keeps
    /// the access key, secret and session token from a single credential set
    /// even if another task refreshes concurrently.
    fn effective_credentials(&self) -> EffectiveCredentials {
        let guard = self
            .temp_credentials
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_ref() {
            Some(tc) => EffectiveCredentials {
                access_key_id: tc.access_key_id.clone(),
                secret_access_key: tc.secret_access_key.clone(),
                session_token: Some(tc.session_token.clone()),
            },
            None => EffectiveCredentials {
                access_key_id: self.config.access_key_id.clone(),
                secret_access_key: self.config.secret_access_key.clone(),
                session_token: self.config.session_token.clone(),
            },
        }
    }

    /// True when the currently-held temporary credentials are missing or within
    /// [`Self::STS_REFRESH_THRESHOLD_SECS`] of expiry. Credentials with no
    /// expiry (STS always returns one, so this is theoretical) are treated as
    /// fresh: a hard `ExpiredToken` from the server forces a reconnect instead.
    fn temp_credentials_need_refresh(&self) -> bool {
        let guard = self
            .temp_credentials
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_ref() {
            None => true,
            Some(tc) => match tc.expiration {
                Some(exp) => {
                    exp - self.now_adjusted()
                        <= chrono::Duration::seconds(Self::STS_REFRESH_THRESHOLD_SECS)
                }
                None => false,
            },
        }
    }

    /// Ensure valid temporary credentials are available when `config.role_arn`
    /// is set: acquire them on the first call and re-assume the role before they
    /// expire. No-op when no role is configured (long-term keys or a manual
    /// session token are used as-is). Always signs the STS request with the base
    /// credentials from `config`. Refreshes are serialized through
    /// `sts_refresh_lock` so concurrent multipart parts re-assume once, and the
    /// staleness check is repeated after acquiring the lock to avoid a redundant
    /// `AssumeRole` when another task just refreshed.
    async fn ensure_fresh_credentials(&self) -> Result<(), ProviderError> {
        let Some(role_arn) = self.config.role_arn.as_deref() else {
            return Ok(());
        };
        if !self.temp_credentials_need_refresh() {
            return Ok(());
        }

        let _guard = self.sts_refresh_lock.lock().await;
        // Another task may have refreshed while we waited for the lock.
        if !self.temp_credentials_need_refresh() {
            return Ok(());
        }

        let session_name = self
            .config
            .role_session_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("aeroftp-session");

        // MFA (issue #301). An MFA-protected AssumeRole consumes a one-time
        // TOTP code that cannot be replayed, so it works only for the FIRST
        // acquisition. A later refresh (temp credentials already present and
        // now near expiry) has no fresh code: surface an explicit reconnect
        // error instead of replaying a stale code into a guaranteed rejection.
        let mfa_serial = self
            .config
            .role_mfa_serial
            .as_deref()
            .filter(|s| !s.is_empty());
        if mfa_serial.is_some() {
            let is_initial_acquire = self
                .temp_credentials
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none();
            if !is_initial_acquire {
                return Err(ProviderError::AuthenticationFailed(
                    "MFA-protected AssumeRole session expired. \
                     Reconnect and enter a new MFA token code."
                        .to_string(),
                ));
            }
        }
        let mfa_token_code = self
            .config
            .role_mfa_token_code
            .as_ref()
            .map(|c| c.expose_secret().to_string());
        if mfa_serial.is_some()
            && mfa_token_code
                .as_deref()
                .filter(|c| !c.is_empty())
                .is_none()
        {
            return Err(ProviderError::AuthenticationFailed(
                "This role requires MFA but no MFA token code was provided. \
                 Reconnect and enter your MFA code."
                    .to_string(),
            ));
        }

        let req = sts::AssumeRoleRequest {
            region: &self.config.region,
            access_key_id: &self.config.access_key_id,
            secret_access_key: &self.config.secret_access_key,
            role_arn,
            role_session_name: session_name,
            duration_seconds: self.config.role_duration_seconds,
            external_id: self.config.role_external_id.as_deref(),
            mfa_serial,
            mfa_token_code: mfa_token_code.as_deref(),
            base_session_token: self.config.session_token.as_ref(),
        };
        // Sign the STS request with the same skew-adjusted clock as the data
        // plane (issue #301, M1), not the raw system clock.
        let creds = sts::assume_role(&self.client, &req, self.now_adjusted()).await?;
        info!(
            "[S3] STS AssumeRole succeeded for {} (expires {:?})",
            role_arn, creds.expiration
        );
        *self
            .temp_credentials
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(creds);
        Ok(())
    }

    /// Get the S3 endpoint URL
    fn endpoint(&self) -> String {
        if let Some(ref endpoint) = self.config.endpoint {
            endpoint.trim_end_matches('/').to_string()
        } else {
            format!("https://s3.{}.amazonaws.com", self.config.region)
        }
    }

    /// Build URL for S3 operations
    fn build_url(&self, key: &str) -> String {
        let endpoint = self.endpoint();
        let key = key.trim_start_matches('/');
        let encoded_key = encode_s3_key_path(key);

        if self.config.path_style {
            // Path-style: https://endpoint/bucket/key
            // Filen Desktop S3 (strict) returns "BadRequest: Invalid prefix specified"
            // when the bucket-only URL is sent without a trailing slash. AWS, MinIO,
            // Wasabi all accept both forms, so adding the trailing slash for
            // bucket-only requests is universally safe.
            if encoded_key.is_empty() {
                format!("{}/{}/", endpoint, self.config.bucket)
            } else {
                format!("{}/{}/{}", endpoint, self.config.bucket, encoded_key)
            }
        } else {
            // Virtual-hosted style: https://bucket.endpoint/key
            let endpoint_without_scheme = endpoint.replace("https://", "").replace("http://", "");
            let scheme = if endpoint.starts_with("http://") {
                "http"
            } else {
                "https"
            };

            if encoded_key.is_empty() {
                format!(
                    "{}://{}.{}",
                    scheme, self.config.bucket, endpoint_without_scheme
                )
            } else {
                format!(
                    "{}://{}.{}/{}",
                    scheme, self.config.bucket, endpoint_without_scheme, encoded_key
                )
            }
        }
    }

    fn is_filelu_s3_endpoint(&self) -> bool {
        self.config
            .endpoint
            .as_deref()
            .map(|ep| {
                let lower = ep.to_ascii_lowercase();
                lower.contains("s5lu.com") || lower.contains("filelu")
            })
            .unwrap_or(false)
    }

    /// Detect Filen Desktop S3 endpoints. Per filen-s3 source, the server
    /// implements a strict subset of the S3 API: ListObjects(V2) accepts
    /// only `Prefix` and `Delimiter`, refusing `list-type`, `max-keys`,
    /// and `continuation-token` with "BadRequest: Invalid prefix specified".
    /// HeadBucket returns 404 in some paths, multipart uploads aren't
    /// supported, ETags are UUIDs, and there are no presigned URLs.
    fn is_filen_s3_endpoint(&self) -> bool {
        self.config
            .endpoint
            .as_deref()
            .map(|ep| {
                let lower = ep.to_ascii_lowercase();
                lower.contains("local.s3.filen.io")
                    || (self.config.region == "filen" && is_local_s3_endpoint(ep))
            })
            .unwrap_or(false)
    }

    /// Detect MEGA S4 Object Storage endpoints.
    /// S4 deviates from standard S3 in several ways: no versioning, no tagging,
    /// no SSE headers, no storage classes, presigned URL max 7 days.
    fn is_mega_s4_endpoint(&self) -> bool {
        self.config
            .endpoint
            .as_deref()
            .map(|ep| ep.to_ascii_lowercase().contains("s4.mega.io"))
            .unwrap_or(false)
    }

    fn bucket_addressing_error(xml: &str) -> Option<ProviderError> {
        if xml.contains("<ListAllMyBucketsResult") {
            Some(ProviderError::InvalidConfig(
                "S3 request returned the account bucket list instead of the configured bucket. Check the endpoint and Path-style setting.".to_string(),
            ))
        } else {
            None
        }
    }

    async fn verify_copy_target_exists(&self, to: &str) -> Result<(), ProviderError> {
        let to_key = to.trim_start_matches('/');
        let mut last_status: Option<StatusCode> = None;

        for attempt in 0..5 {
            let response = self.s3_request(Method::HEAD, to_key, None, None).await?;
            let status = response.status();
            debug!(
                "S3 rename verify attempt {}: HEAD {} -> {}",
                attempt + 1,
                to_key,
                status
            );

            if status == StatusCode::OK {
                return Ok(());
            }

            // Some S3-compatible providers may return temporary/inconsistent HEAD results
            // immediately after CopyObject. Fall back to prefix listing and exact-key match.
            if matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
            ) {
                let listed_keys = self.list_keys_with_prefix(to_key).await?;
                debug!(
                    "S3 rename verify attempt {}: list prefix '{}' returned {} keys",
                    attempt + 1,
                    to_key,
                    listed_keys.len()
                );
                if listed_keys.iter().any(|k| k == to_key) {
                    debug!(
                        "S3 rename verify attempt {}: destination '{}' found via list",
                        attempt + 1,
                        to_key
                    );
                    return Ok(());
                }
            }

            last_status = Some(status);

            if !matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
            ) || attempt == 4
            {
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(75 * (1 << attempt))).await;
        }

        Err(ProviderError::ServerError(format!(
            "Copy verification failed: destination {} not readable after copy (status: {})",
            to,
            last_status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )))
    }

    async fn rename_filelu_safe(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_key = from.trim_start_matches('/');
        let to_key = to.trim_start_matches('/');

        debug!("FileLu S3 safe rename start: {} -> {}", from_key, to_key);

        let source_response = self.s3_request(Method::GET, from_key, None, None).await?;
        let source_status = source_response.status();
        if source_status != StatusCode::OK {
            return Err(ProviderError::ServerError(format!(
                "FileLu safe rename read failed ({}): {}",
                source_status, from
            )));
        }

        let source_bytes = source_response
            .bytes()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let put_response = self
            .s3_request(Method::PUT, to_key, None, Some(source_bytes.to_vec()))
            .await?;
        let put_status = put_response.status();
        let put_body = put_response.text().await.unwrap_or_default();

        match put_status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if put_body.to_ascii_lowercase().contains("<error>") {
                    let err_code = put_body
                        .split("<Code>")
                        .nth(1)
                        .and_then(|s| s.split("</Code>").next())
                        .unwrap_or("PutError");
                    let err_msg = put_body
                        .split("<Message>")
                        .nth(1)
                        .and_then(|s| s.split("</Message>").next())
                        .unwrap_or("S3 provider returned an error during put");
                    return Err(ProviderError::ServerError(format!(
                        "FileLu safe rename write failed ({}): {} - {}",
                        put_status,
                        sanitize_api_error(err_code),
                        sanitize_api_error(err_msg)
                    )));
                }
            }
            _ => {
                return Err(ProviderError::ServerError(format!(
                    "FileLu safe rename write failed ({}): {}",
                    put_status,
                    sanitize_api_error(&put_body)
                )));
            }
        }

        self.delete(from).await?;
        info!("Renamed file (FileLu safe path) {} to {}", from, to);
        Ok(())
    }

    /// Sign a request using AWS Signature Version 4
    /// This is a simplified implementation - for production, consider using aws-sigv4
    fn sign_request(
        &self,
        method: &str,
        url: &str,
        headers: &mut HashMap<String, String>,
        payload_hash: &str,
    ) -> Result<String, ProviderError> {
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256};

        type HmacSha256 = Hmac<Sha256>;

        // Single consistent snapshot of the effective credentials for this
        // signing pass (temporary STS creds when a role is assumed, else base).
        let creds = self.effective_credentials();

        let now: DateTime<Utc> = self.now_adjusted();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        headers.insert("x-amz-date".to_string(), amz_date.clone());
        headers.insert("x-amz-content-sha256".to_string(), payload_hash.to_string());

        // Temporary credentials (STS AssumeRole / SSO): the session token must
        // be carried in `x-amz-security-token` AND included in the canonical
        // request so it is covered by the SigV4 signature. Inserting it into
        // `headers` before the canonical headers are built achieves both, since
        // `s3_request_ext` replays every entry of this map as a real header.
        if let Some(token) = &creds.session_token {
            headers.insert(
                "x-amz-security-token".to_string(),
                token.expose_secret().to_string(),
            );
        }

        // Parse URL to get host and path
        let parsed =
            url::Url::parse(url).map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        let host = parsed.host_str().unwrap_or("");
        let path = parsed.path();

        // Query parameters must be sorted alphabetically for canonical request
        let canonical_query = {
            let mut params: Vec<(String, String)> = parsed
                .query_pairs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            params.sort_by(|a, b| {
                // Sort by key first, then by value
                match a.0.cmp(&b.0) {
                    std::cmp::Ordering::Equal => a.1.cmp(&b.1),
                    other => other,
                }
            });
            params
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&")
        };

        headers.insert("host".to_string(), host.to_string());

        // Create canonical request
        let mut signed_headers: Vec<&str> = headers.keys().map(|s| s.as_str()).collect();
        signed_headers.sort();
        let signed_headers_str = signed_headers.join(";");

        let mut canonical_headers = String::new();
        for header in &signed_headers {
            if let Some(value) = headers.get(*header) {
                canonical_headers.push_str(&format!(
                    "{}:{}\n",
                    header.to_lowercase(),
                    value.trim()
                ));
            }
        }

        // URI-encode each path segment individually (H-10: SigV4 requires encoded segments)
        // parsed.path() returns already-percent-encoded path, so decode first to avoid double-encoding
        // (e.g. "File%20Name.pdf" → decode → "File Name.pdf" → encode → "File%20Name.pdf")
        let canonical_path = if path.is_empty() || path == "/" {
            "/".to_string()
        } else {
            let encoded_segments: Vec<String> = path
                .split('/')
                .map(|segment| {
                    if segment.is_empty() {
                        String::new()
                    } else {
                        let decoded = urlencoding::decode(segment)
                            .unwrap_or(std::borrow::Cow::Borrowed(segment));
                        urlencoding::encode(&decoded).into_owned()
                    }
                })
                .collect();
            encoded_segments.join("/")
        };

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            canonical_path,
            canonical_query,
            canonical_headers,
            signed_headers_str,
            payload_hash
        );

        let canonical_request_hash = {
            let mut hasher = Sha256::new();
            hasher.update(canonical_request.as_bytes());
            hex::encode(hasher.finalize())
        };

        // Create string to sign
        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.config.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_request_hash
        );

        // Calculate signature
        fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }

        let k_date = hmac_sha256(
            format!("AWS4{}", creds.secret_access_key.expose_secret()).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.config.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        // Create authorization header
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            creds.access_key_id, credential_scope, signed_headers_str, signature
        );

        Ok(authorization)
    }

    /// Make a signed request to S3
    async fn s3_request(
        &self,
        method: Method,
        key: &str,
        query_params: Option<&[(&str, &str)]>,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, ProviderError> {
        self.s3_request_ext(method, key, query_params, body, &[])
            .await
    }

    /// Make a signed request to S3 with extra headers included in the signature
    async fn s3_request_ext(
        &self,
        method: Method,
        key: &str,
        query_params: Option<&[(&str, &str)]>,
        body: Option<Vec<u8>>,
        extra_headers: &[(&str, &str)],
    ) -> Result<reqwest::Response, ProviderError> {
        use sha2::{Digest, Sha256};

        // Refresh STS credentials before they expire (issue #301, Fase 3).
        // Covers list / delete / stat / mkdir and every multipart part upload,
        // which funnel through here; no-op when no role is configured.
        self.ensure_fresh_credentials().await?;

        let mut url = self.build_url(key);
        if let Some(params) = query_params {
            let query: String = params
                .iter()
                .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            if !query.is_empty() {
                url = format!("{}?{}", url, query);
            }
        }

        // Per-request diagnostics stay on the gated `debug!` target so a plain
        // `ls` / `tree` on an S3 profile is not cluttered with GET lines (#196).
        debug!(
            "[S3] {} {} (bucket={} region={} path_style={})",
            method, url, self.config.bucket, self.config.region, self.config.path_style
        );

        let payload = body.as_deref().unwrap_or(&[]);
        // KE-B1.3: skip the per-payload SHA-256 when the user opted into
        // `--s3-disable-checksum`. SigV4 accepts the literal
        // `UNSIGNED-PAYLOAD` in `x-amz-content-sha256`, which trades client
        // CPU for a small reduction in tamper protection. Always hash when
        // the payload is empty: it's a 32-byte constant, no CPU savings,
        // and some S3 gateways still validate the empty-body hash.
        let payload_hash = if self.disable_checksum && !payload.is_empty() {
            "UNSIGNED-PAYLOAD".to_string()
        } else {
            let mut hasher = Sha256::new();
            hasher.update(payload);
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        // Insert extra headers before signing so they become part of the canonical request
        for (k, v) in extra_headers {
            headers.insert(k.to_string(), v.to_string());
        }
        let authorization =
            self.sign_request(method.as_str(), &url, &mut headers, &payload_hash)?;

        let mut request = self.client.request(method.clone(), &url);

        for (key, value) in headers.iter() {
            request = request.header(key, value);
        }
        request = request.header("Authorization", &authorization);

        // SEC-06: Redact sensitive headers before logging
        {
            let redacted: HashMap<&String, String> = headers
                .iter()
                .map(|(k, v)| {
                    let lower = k.to_lowercase();
                    if lower == "authorization" || lower == "x-amz-security-token" {
                        (k, "[REDACTED]".to_string())
                    } else {
                        (k, v.clone())
                    }
                })
                .collect();
            debug!("S3 Headers: {:?}", redacted);
        }

        if let Some(body_data) = body {
            // Explicitly set Content-Length for empty bodies (required by some S3-compatible services like Backblaze B2)
            request = request.header("Content-Length", body_data.len().to_string());
            request = request.body(body_data);
        }

        // ERR-03: Use retry wrapper for transient errors (429, 500, 502, 503, 504)
        let built_request = request
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {e}")))?;
        let response = super::send_with_retry(
            &self.client,
            built_request,
            &super::HttpRetryConfig::default(),
        )
        .await
        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            warn!("S3 Response Status: {} for {} {}", status, method, url);
        }

        Ok(response)
    }

    /// Parse S3 ListObjectsV2 XML response using quick-xml (M-11/M-12)
    fn parse_list_response(
        &self,
        xml_str: &str,
    ) -> Result<(Vec<RemoteEntry>, Option<String>), ProviderError> {
        let mut entries = Vec::new();

        debug!(
            "Parsing S3 ListObjectsV2 XML response, {} bytes",
            xml_str.len()
        );

        // Filen Desktop S3 returns <Key>/<Prefix> percent-encoded (e.g. `my%20folder`
        // for "my folder"), unlike AWS-standard which returns them verbatim.
        // Decode here so RemoteEntry holds the logical name and downstream `build_url`
        // re-encodes consistently. Reported in #196 (Filen S3 tree shows `%20`).
        let filen_decode = self.is_filen_s3_endpoint();

        let mut reader = Reader::from_str(xml_str);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        // State machine for tracking current element context
        enum Context {
            None,
            CommonPrefixes,
            Contents,
        }
        let mut context = Context::None;
        let mut current_tag = String::new();

        // Fields for CommonPrefixes
        let mut cp_prefix: Option<String> = None;

        // Fields for Contents
        let mut c_key: Option<String> = None;
        let mut c_size: Option<String> = None;
        let mut c_modified: Option<String> = None;
        let mut c_etag: Option<String> = None;
        let mut c_storage_class: Option<String> = None;

        // Top-level field
        let mut top_next_token: Option<String> = None;
        let mut in_next_token = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag_name.as_str() {
                        "CommonPrefixes" => {
                            context = Context::CommonPrefixes;
                            cp_prefix = None;
                        }
                        "Contents" => {
                            context = Context::Contents;
                            c_key = None;
                            c_size = None;
                            c_modified = None;
                            c_etag = None;
                            c_storage_class = None;
                        }
                        "NextContinuationToken" => {
                            in_next_token = true;
                        }
                        _ => {
                            current_tag = tag_name;
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    // Do NOT trim: leading/trailing whitespace inside an
                    // S3 Key is significant. Trimming the whole-element text
                    // is also wrong for entity-split fragments (see below):
                    // for a key like "a&b.txt" quick-xml emits
                    //   Text("a") + GeneralRef("amp") + Text("b.txt")
                    // and trimming each piece is fine, but blindly assigning
                    // the last fragment overwrites the preceding "a": which
                    // is exactly how `a&b.txt` was being shown as `b.txt`.
                    let raw = String::from_utf8_lossy(e.as_ref()).to_string();
                    if raw.is_empty() {
                        buf.clear();
                        continue;
                    }

                    if in_next_token {
                        top_next_token
                            .get_or_insert_with(String::new)
                            .push_str(&raw);
                    }

                    match context {
                        Context::CommonPrefixes => {
                            if current_tag == "Prefix" {
                                cp_prefix.get_or_insert_with(String::new).push_str(&raw);
                            }
                        }
                        Context::Contents => match current_tag.as_str() {
                            "Key" => c_key.get_or_insert_with(String::new).push_str(&raw),
                            "Size" => c_size.get_or_insert_with(String::new).push_str(&raw),
                            "LastModified" => {
                                c_modified.get_or_insert_with(String::new).push_str(&raw)
                            }
                            "ETag" => c_etag.get_or_insert_with(String::new).push_str(&raw),
                            "StorageClass" => c_storage_class
                                .get_or_insert_with(String::new)
                                .push_str(&raw),
                            _ => {}
                        },
                        Context::None => {}
                    }
                }
                // S3 keys with `&`, `'`, `<`, `>`, `"` arrive XML-escaped as
                // `&amp;`, `&apos;`, etc. quick-xml surfaces these as a
                // separate `GeneralRef` event between the surrounding text
                // fragments. Without this branch the entity is dropped and
                // the key is rebuilt with a hole: which is the actual root
                // cause of "a&b.txt" being listed as "b.txt".
                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if in_next_token {
                            top_next_token.get_or_insert_with(String::new).push_str(&ch);
                        }
                        match context {
                            Context::CommonPrefixes => {
                                if current_tag == "Prefix" {
                                    cp_prefix.get_or_insert_with(String::new).push_str(&ch);
                                }
                            }
                            Context::Contents => match current_tag.as_str() {
                                "Key" => c_key.get_or_insert_with(String::new).push_str(&ch),
                                "Size" => c_size.get_or_insert_with(String::new).push_str(&ch),
                                "LastModified" => {
                                    c_modified.get_or_insert_with(String::new).push_str(&ch)
                                }
                                "ETag" => c_etag.get_or_insert_with(String::new).push_str(&ch),
                                "StorageClass" => c_storage_class
                                    .get_or_insert_with(String::new)
                                    .push_str(&ch),
                                _ => {}
                            },
                            Context::None => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag_name.as_str() {
                        "CommonPrefixes" => {
                            if let Some(ref raw_prefix) = cp_prefix {
                                let full_prefix: String = if filen_decode {
                                    urlencoding::decode(raw_prefix)
                                        .map(|c| c.into_owned())
                                        .unwrap_or_else(|_| raw_prefix.clone())
                                } else {
                                    raw_prefix.clone()
                                };
                                let name = full_prefix
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&full_prefix)
                                    .to_string();

                                if !name.is_empty() {
                                    entries.push(RemoteEntry::directory(
                                        name,
                                        format!("/{}", full_prefix.trim_end_matches('/')),
                                    ));
                                }
                            }
                            context = Context::None;
                        }
                        "Contents" => {
                            if let Some(ref raw_key) = c_key {
                                let key: String = if filen_decode {
                                    urlencoding::decode(raw_key)
                                        .map(|c| c.into_owned())
                                        .unwrap_or_else(|_| raw_key.clone())
                                } else {
                                    raw_key.clone()
                                };
                                let key = key.as_str();
                                // Skip directory markers
                                if !key.ends_with('/') {
                                    // Skip if key equals current prefix
                                    let dominated = key == self.current_prefix
                                        || key.trim_start_matches('/')
                                            == self.current_prefix.trim_start_matches('/');
                                    if !dominated {
                                        let name =
                                            key.rsplit('/').next().unwrap_or(key).to_string();
                                        if !name.is_empty() {
                                            let size: u64 = c_size
                                                .as_ref()
                                                .and_then(|s| s.parse().ok())
                                                .unwrap_or(0);

                                            let mut metadata = HashMap::new();
                                            if let Some(raw_etag) = c_etag.as_ref() {
                                                let etag = raw_etag.trim_matches('"').to_string();
                                                if let Some(md5) = etag_to_md5(&etag) {
                                                    metadata.insert("md5".to_string(), md5);
                                                }
                                                metadata.insert("etag".to_string(), etag);
                                            }
                                            if let Some(ref sc) = c_storage_class {
                                                metadata.insert(
                                                    "storage_class".to_string(),
                                                    sc.clone(),
                                                );
                                            }

                                            entries.push(RemoteEntry {
                                                name,
                                                path: format!("/{}", key),
                                                is_dir: false,
                                                size,
                                                modified: c_modified.clone(),
                                                permissions: None,
                                                owner: None,
                                                group: None,
                                                is_symlink: false,
                                                link_target: None,
                                                mime_type: None,
                                                metadata,
                                            });
                                        }
                                    }
                                }
                            }
                            context = Context::None;
                        }
                        "NextContinuationToken" => {
                            in_next_token = false;
                        }
                        _ => {}
                    }
                    current_tag.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                }
                _ => {}
            }
            buf.clear();
        }

        Ok((entries, top_next_token))
    }

    /// Extract content from an XML tag using quick-xml (M-11/M-12)
    fn extract_xml_tag(&self, xml_str: &str, tag: &str) -> Option<String> {
        let mut reader = Reader::from_str(xml_str);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut inside_target = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    let tag_name = String::from_utf8_lossy(name.as_ref());
                    if tag_name == tag {
                        inside_target = true;
                    }
                }
                Ok(Event::Text(ref e)) if inside_target => {
                    let trimmed = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let tag_name = String::from_utf8_lossy(name.as_ref());
                    if tag_name == tag {
                        inside_target = false;
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    /// Append S3 enterprise headers (ACL, storage class, SSE) to a headers
    /// map. Skipped entirely for MEGA S4 which does not support storage
    /// classes, ACLs or SSE.
    fn append_upload_headers(&self, headers: &mut HashMap<String, String>) {
        if self.is_mega_s4_endpoint() {
            return;
        }
        // KE-B1.5: runtime override > profile setting > backend default.
        if let Some(sc) = self.effective_storage_class() {
            headers.insert("x-amz-storage-class".to_string(), sc.to_string());
        }
        // KE-B1.4: canned ACL header. Skipped when no override is active so
        // bucket-policy-managed buckets (the AWS default since 2023) are
        // not surprised by an explicit `x-amz-acl: private` that contradicts
        // their bucket-policy setup.
        if let Some(acl) = self.effective_acl() {
            headers.insert("x-amz-acl".to_string(), acl.to_string());
        }
        match self.config.sse_mode.as_deref() {
            Some("AES256") => {
                headers.insert(
                    "x-amz-server-side-encryption".to_string(),
                    "AES256".to_string(),
                );
            }
            Some("aws:kms") => {
                headers.insert(
                    "x-amz-server-side-encryption".to_string(),
                    "aws:kms".to_string(),
                );
                if let Some(ref key_id) = self.config.sse_kms_key_id {
                    headers.insert(
                        "x-amz-server-side-encryption-aws-kms-key-id".to_string(),
                        key_id.clone(),
                    );
                }
            }
            _ => {}
        }
    }

    /// Cutoff above which we switch to multipart upload. Bumped from the
    /// historical 5 MiB to 200 MiB to match rclone's `--s3-upload-cutoff`
    /// default. The previous aggressive cutoff produced 20 small parts for
    /// a 100 MiB payload, which on the Storj S3 gateway results in segment
    /// distributions that are measurably slower to read back later (~2x
    /// download regression observed in the 2026-05-08 cross-tool bench).
    /// Single-PUT is preferred when the file size fits, because every
    /// supported S3 backend ships single-PUT semantics that map cleanly
    /// onto their underlying storage layout.
    const MULTIPART_THRESHOLD: usize = 200 * 1024 * 1024;
    /// Minimum part size required by S3 spec for any multipart upload (5 MiB).
    /// Used as a floor for `effective_part_size`.
    const MULTIPART_PART_MIN: usize = 5 * 1024 * 1024;
    /// Default part size for multipart upload chunks. Kept at 16 MiB so that
    /// payloads above the new 200 MiB cutoff still split into a manageable
    /// number of parts (e.g. 1 GiB → 64 parts) without flooding the part
    /// list with thousands of tiny segments.
    const MULTIPART_PART_SIZE: usize = 16 * 1024 * 1024;

    /// S3 spec: single-PUT CopyObject (`x-amz-copy-source`) is hard-capped
    /// at 5 GiB. Above this size the copy must be expressed as a multipart
    /// upload whose parts are filled by UploadPartCopy. Below or equal,
    /// the single-PUT path is preferred (one round trip, atomic).
    const COPY_OBJECT_MAX: u64 = 5 * 1024 * 1024 * 1024;
    /// Part size for server-side multipart copy. 100 MiB keeps the part
    /// count bounded (5 GiB → 50 parts, 100 GiB → 1000 parts; S3 max is
    /// 10000 parts per upload). Each UploadPartCopy is server-to-server,
    /// so no client disk read happens and part-size tuning is purely an
    /// API round-trip vs. parallelism tradeoff.
    const COPY_MULTIPART_PART_SIZE: u64 = 100 * 1024 * 1024;

    /// Effective part size, using override if set. Capped on the low end
    /// at the S3 spec minimum (5 MiB), not at the multipart cutoff (which
    /// is now 200 MiB and would otherwise reject any sane part size).
    fn effective_part_size(&self) -> usize {
        self.upload_chunk_override
            .unwrap_or(Self::MULTIPART_PART_SIZE)
            .max(Self::MULTIPART_PART_MIN)
    }

    /// Initiate a multipart upload, returns the UploadId.
    /// Optionally sets Content-Type for the resulting object (UPLOAD-01).
    async fn create_multipart_upload(
        &self,
        key: &str,
        content_type: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.ensure_fresh_credentials().await?;
        // For multipart, Content-Type must be set on initiation, not on individual parts.
        // We build a custom request to include the header.
        let url = {
            let base = self.build_url(key);
            format!("{}?uploads=", base)
        };

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        if let Some(ct) = content_type {
            headers.insert("content-type".to_string(), ct.to_string());
        }
        // B2: Add storage class + SSE headers on multipart initiation
        self.append_upload_headers(&mut headers);
        let authorization = self.sign_request("POST", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.post(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format_s3_error(
                "CreateMultipartUpload failed",
                status,
                &body,
                retry_header.as_deref(),
            )));
        }

        self.extract_xml_tag(&body, "UploadId")
            .ok_or_else(|| ProviderError::ParseError("Missing UploadId in response".to_string()))
    }

    /// Upload a single part, returns the ETag.
    ///
    /// Internal inherent method; the public, trait-level entry point is
    /// `<S3Provider as StorageProvider>::upload_part`. The `_internal`
    /// suffix avoids shadowing the trait method when callers use
    /// dot-notation on a concrete `S3Provider`.
    async fn upload_part_internal(
        &self,
        key: &str,
        upload_id: &str,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<String, ProviderError> {
        let part_num_str = part_number.to_string();
        let params: &[(&str, &str)] = &[("partNumber", &part_num_str), ("uploadId", upload_id)];

        let response = self
            .s3_request(Method::PUT, key, Some(params), Some(data))
            .await?;

        let status = response.status();
        if !status.is_success() {
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format_s3_error(
                &format!("UploadPart {} failed", part_number),
                status,
                &body,
                retry_header.as_deref(),
            )));
        }

        // ETag is in the response headers
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ProviderError::ParseError("Missing ETag in UploadPart response".to_string())
            })?;

        Ok(etag)
    }

    /// UploadPartCopy: server-side copy of a byte range from `copy_source`
    /// into the destination multipart part. Used by `server_side_copy`
    /// when the source object exceeds the single-PUT CopyObject limit of
    /// 5 GiB.
    ///
    /// `copy_source` is the SigV4-canonical form `/<bucket>/<encoded-key>`
    /// (same encoding rule as `server_side_copy`'s `x-amz-copy-source`).
    /// `range_start..=range_end_inclusive` is sent verbatim in the
    /// `x-amz-copy-source-range` header (S3 spec: inclusive on both ends).
    ///
    /// Unlike UploadPart, the response ETag arrives inside the XML body
    /// (`<CopyPartResult><ETag>...</ETag></CopyPartResult>`), not in the
    /// `ETag` response header, which is why we parse the body explicitly.
    async fn upload_part_copy_internal(
        &self,
        dest_key: &str,
        upload_id: &str,
        part_number: u32,
        copy_source: &str,
        range_start: u64,
        range_end_inclusive: u64,
    ) -> Result<String, ProviderError> {
        let part_num_str = part_number.to_string();
        let range_value = format!("bytes={}-{}", range_start, range_end_inclusive);
        let params: &[(&str, &str)] = &[("partNumber", &part_num_str), ("uploadId", upload_id)];
        let extra: &[(&str, &str)] = &[
            ("x-amz-copy-source", copy_source),
            ("x-amz-copy-source-range", &range_value),
        ];
        let response = self
            .s3_request_ext(Method::PUT, dest_key, Some(params), None, extra)
            .await?;
        let status = response.status();
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format_s3_error(
                &format!("UploadPartCopy {} failed", part_number),
                status,
                &body,
                retry_header.as_deref(),
            )));
        }
        // S3-compatible servers (AWS + MinIO + Filen bridge) can return HTTP
        // 200 with an `<Error>` XML body when validation fails late on the
        // server side. Mirror the single-PUT copy path's 200-with-error
        // handling so the caller doesn't silently complete a broken upload.
        if body.to_ascii_lowercase().contains("<error>") {
            let err_code = self
                .extract_xml_tag(&body, "Code")
                .unwrap_or_else(|| "CopyPartError".to_string());
            let err_msg = self
                .extract_xml_tag(&body, "Message")
                .unwrap_or_else(|| "Server reported error during UploadPartCopy".to_string());
            return Err(ProviderError::TransferFailed(format!(
                "UploadPartCopy {} 200-with-error ({}): {}",
                part_number,
                sanitize_api_error(&err_code),
                sanitize_api_error(&err_msg)
            )));
        }
        self.extract_xml_tag(&body, "ETag").ok_or_else(|| {
            ProviderError::ParseError(format!(
                "Missing ETag in UploadPartCopy {} response",
                part_number
            ))
        })
    }

    /// Complete a multipart upload (internal inherent path).
    ///
    /// Public trait-level entry point:
    /// `<S3Provider as StorageProvider>::complete_multipart_upload`. The
    /// `_internal` suffix avoids shadowing the trait method.
    async fn complete_multipart_upload_internal(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[(u32, String)],
    ) -> Result<(), ProviderError> {
        // Build XML body
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (part_number, etag) in parts {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part_number, etag,
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");

        let response = self
            .s3_request(
                Method::POST,
                key,
                Some(&[("uploadId", upload_id)]),
                Some(xml.into_bytes()),
            )
            .await?;

        let status = response.status();
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format_s3_error(
                "CompleteMultipartUpload failed",
                status,
                &body,
                retry_header.as_deref(),
            )));
        }

        // UPLOAD-07: AWS S3 can return HTTP 200 but include an <Error> in the XML body
        if body.contains("<Error>") {
            let error_msg = self
                .extract_xml_tag(&body, "Message")
                .or_else(|| self.extract_xml_tag(&body, "Code"))
                .unwrap_or_else(|| "Unknown error in CompleteMultipartUpload response".to_string());
            return Err(ProviderError::TransferFailed(format!(
                "CompleteMultipartUpload 200-with-error: {}",
                sanitize_api_error(&error_msg)
            )));
        }

        Ok(())
    }

    /// Upload a file using S3 multipart upload with streaming (no full-file buffering).
    /// UPLOAD-02: Reads chunks from disk instead of loading entire file into RAM.
    async fn upload_multipart_streaming(
        &self,
        key: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        // UPLOAD-01: Detect MIME type from filename for multipart uploads
        let content_type = mime_guess::from_path(local_path)
            .first_or_octet_stream()
            .to_string();
        let upload_id = self
            .create_multipart_upload(key, Some(&content_type))
            .await?;
        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut part_number = 1u32;
        let mut uploaded: u64 = 0;

        let part_size = self.effective_part_size();
        // KE-B1.1: parallelism override (default 4). Each part is held in
        // RAM during upload, so the actual memory footprint is
        // `max_parallel * part_size`; document this for users who push the
        // knob aggressively on tiny VMs.
        let max_parallel = self.effective_upload_concurrency();

        loop {
            // Pre-read up to max_parallel parts from disk
            let mut batch: Vec<(u32, Vec<u8>)> = Vec::with_capacity(max_parallel);
            for _ in 0..max_parallel {
                let mut buf = vec![0u8; part_size];
                let mut filled = 0;
                while filled < part_size {
                    let n = file
                        .read(&mut buf[filled..])
                        .await
                        .map_err(|e| ProviderError::TransferFailed(format!("Read error: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                if filled == 0 {
                    break;
                }
                buf.truncate(filled);
                batch.push((part_number, buf));
                part_number += 1;
            }

            if batch.is_empty() {
                break;
            }

            // Upload batch in parallel via JoinSet so the first failure aborts
            // every sibling instead of letting them continue burning bandwidth
            // (and S3 request billing) against an upload we've already decided
            // to abort.
            let mut joinset = tokio::task::JoinSet::new();
            for (pn, data) in batch {
                let provider = self.clone();
                let key_owned = key.to_string();
                let uid = upload_id.clone();
                let data_len = data.len() as u64;
                joinset.spawn(async move {
                    let etag = provider
                        .upload_part_internal(&key_owned, &uid, pn, data)
                        .await?;
                    Ok::<(u32, String, u64), ProviderError>((pn, etag, data_len))
                });
            }

            while let Some(joined) = joinset.join_next().await {
                match joined {
                    Ok(Ok((pn, etag, data_len))) => {
                        parts.push((pn, etag));
                        uploaded += data_len;
                        if let Some(ref progress) = on_progress {
                            progress(uploaded, total_size);
                        }
                    }
                    Ok(Err(e)) => {
                        joinset.abort_all();
                        // Drain aborted futures so JoinSet drops cleanly before
                        // we fire the S3 AbortMultipartUpload.
                        while joinset.join_next().await.is_some() {}
                        let _ = self.abort_multipart_upload_internal(key, &upload_id).await;
                        return Err(e);
                    }
                    Err(e) => {
                        joinset.abort_all();
                        while joinset.join_next().await.is_some() {}
                        let _ = self.abort_multipart_upload_internal(key, &upload_id).await;
                        return Err(ProviderError::TransferFailed(format!(
                            "Upload task panicked: {e}"
                        )));
                    }
                }
            }

            // Sort parts by number (parallel completion may be out of order)
            parts.sort_by_key(|(pn, _)| *pn);
        }

        self.complete_multipart_upload_internal(key, &upload_id, &parts)
            .await
    }

    /// Abort a multipart upload (internal inherent path).
    ///
    /// Public trait-level entry point:
    /// `<S3Provider as StorageProvider>::abort_multipart_upload`. The
    /// `_internal` suffix avoids shadowing the trait method.
    async fn abort_multipart_upload_internal(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<(), ProviderError> {
        let _ = self
            .s3_request(Method::DELETE, key, Some(&[("uploadId", upload_id)]), None)
            .await;
        Ok(())
    }

    /// Single-PUT server-side copy (`x-amz-copy-source` header). Used by
    /// `server_side_copy` for sources ≤ 5 GiB. Caller must have already
    /// validated `self.connected`.
    ///
    /// Issue #128 follow-up: `x-amz-copy-source` must be percent-encoded
    /// the same way the destination URL is encoded under `build_url`,
    /// otherwise SigV4 canonicalisation reconstructs a different wire
    /// path from the (lenient) request and the bridge returns
    /// `401 SignatureDoesNotMatch`. Filen Desktop's local S3 bridge is
    /// strict about this: any space, emoji or RFC-3986 reserved char in
    /// the source key triggered the mismatch on rename / move. AWS and
    /// MinIO tolerated the unencoded form, which is why this went
    /// unnoticed until the Filen reproduction.
    async fn server_side_copy_single(
        &self,
        from_key: &str,
        to_key: &str,
    ) -> Result<(), ProviderError> {
        self.ensure_fresh_credentials().await?;
        let copy_source = format!("/{}/{}", self.config.bucket, encode_s3_key_path(from_key));

        let url = self.build_url(to_key);

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("x-amz-copy-source".to_string(), copy_source);
        // COPY-01: Preserve original object metadata during copy
        headers.insert("x-amz-metadata-directive".to_string(), "COPY".to_string());
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (key, value) in headers.iter() {
            request = request.header(key, value);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        let retry_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.text().await.unwrap_or_default();

        match status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                // S3-compatible providers may return HTTP 200 with an XML <Error> payload.
                // Treat this as a failed copy to avoid deleting the source during rename.
                if body.to_ascii_lowercase().contains("<error>") {
                    let err_code = body
                        .split("<Code>")
                        .nth(1)
                        .and_then(|s| s.split("</Code>").next())
                        .unwrap_or("CopyError");
                    let err_msg = body
                        .split("<Message>")
                        .nth(1)
                        .and_then(|s| s.split("</Message>").next())
                        .unwrap_or("S3 provider returned an error during copy");
                    return Err(ProviderError::ServerError(format!(
                        "Copy failed ({}): {} - {}",
                        status,
                        sanitize_api_error(err_code),
                        sanitize_api_error(err_msg)
                    )));
                }

                info!("Copied {} to {}", from_key, to_key);
                Ok(())
            }
            _ => Err(ProviderError::ServerError(format_s3_error(
                "Copy failed",
                status,
                &body,
                retry_header.as_deref(),
            ))),
        }
    }

    /// Server-side multipart copy for sources > 5 GiB (T-DEBT-08).
    ///
    /// Implementation:
    /// 1. `CreateMultipartUpload(to_key)` → `upload_id`, preserving the
    ///    source `content_type` best-effort.
    /// 2. Plan the parts via `plan_copy_parts(source_size, part_size)`.
    /// 3. Fan out `UploadPartCopy` up to 4 concurrent server-to-server
    ///    requests (no client disk read, no client egress beyond signed
    ///    headers): `x-amz-copy-source` + `x-amz-copy-source-range`.
    /// 4. Collect ETags, sort by part number, `CompleteMultipartUpload`.
    /// 5. On any error along the way, `AbortMultipartUpload` best-effort
    ///    so we don't leak server-side storage cost.
    ///
    /// Caller must have validated `self.connected`. The destination
    /// `to_key` should be the trimmed key (no leading `/`).
    async fn server_side_copy_multipart(
        &self,
        from_key: &str,
        to_key: &str,
        source_size: u64,
        content_type: Option<&str>,
    ) -> Result<(), ProviderError> {
        let copy_source = format!("/{}/{}", self.config.bucket, encode_s3_key_path(from_key));

        let part_size = Self::COPY_MULTIPART_PART_SIZE;
        let planned = plan_copy_parts(source_size, part_size);
        // S3 caps multipart at 10000 parts per upload. With a 100 MiB
        // part size that's 1 TiB; refuse louder than failing mid-stream
        // so the caller can re-tune part size in a follow-up if they
        // really need to copy a >1 TiB object.
        if planned.len() > 10_000 {
            return Err(ProviderError::ServerError(format!(
                "Server-side copy of {} ({} bytes) would need {} parts at {} MiB each; \
                 exceeds S3 cap of 10000 parts per multipart upload",
                from_key,
                source_size,
                planned.len(),
                part_size / (1024 * 1024)
            )));
        }

        let upload_id = self.create_multipart_upload(to_key, content_type).await?;

        // KE-B1.1: same parallelism cap as multipart upload. UploadPartCopy
        // is server-to-server so the parallelism bottleneck is the source
        // bucket's read fan-out, not local CPU.
        let max_parallel = self.effective_upload_concurrency();
        let mut parts: Vec<(u32, String)> = Vec::with_capacity(planned.len());
        let mut cursor = planned.into_iter();

        loop {
            let mut joinset = tokio::task::JoinSet::new();
            for _ in 0..max_parallel {
                let Some((part_number, range_start, range_end_inclusive)) = cursor.next() else {
                    break;
                };
                let provider = self.clone();
                let dest_key = to_key.to_string();
                let upload_id_owned = upload_id.clone();
                let copy_source_owned = copy_source.clone();
                joinset.spawn(async move {
                    let etag = provider
                        .upload_part_copy_internal(
                            &dest_key,
                            &upload_id_owned,
                            part_number,
                            &copy_source_owned,
                            range_start,
                            range_end_inclusive,
                        )
                        .await?;
                    Ok::<(u32, String), ProviderError>((part_number, etag))
                });
            }

            if joinset.is_empty() {
                break;
            }

            while let Some(joined) = joinset.join_next().await {
                match joined {
                    Ok(Ok((pn, etag))) => parts.push((pn, etag)),
                    Ok(Err(e)) => {
                        joinset.abort_all();
                        while joinset.join_next().await.is_some() {}
                        let _ = self
                            .abort_multipart_upload_internal(to_key, &upload_id)
                            .await;
                        return Err(e);
                    }
                    Err(e) => {
                        joinset.abort_all();
                        while joinset.join_next().await.is_some() {}
                        let _ = self
                            .abort_multipart_upload_internal(to_key, &upload_id)
                            .await;
                        return Err(ProviderError::TransferFailed(format!(
                            "UploadPartCopy task panicked: {e}"
                        )));
                    }
                }
            }
        }

        parts.sort_by_key(|(pn, _)| *pn);

        if let Err(e) = self
            .complete_multipart_upload_internal(to_key, &upload_id, &parts)
            .await
        {
            let _ = self
                .abort_multipart_upload_internal(to_key, &upload_id)
                .await;
            return Err(e);
        }

        info!(
            "Server-side multipart copied {} -> {} ({} bytes in {} parts)",
            from_key,
            to_key,
            source_size,
            parts.len()
        );
        Ok(())
    }

    /// Multi-thread chunk-parallel download for a single S3 object.
    ///
    /// Splits the object into N contiguous byte ranges and downloads them
    /// concurrently via independent `GET` requests with `Range: bytes=start-end`.
    /// Each task seeks to its offset on a pre-allocated `.aerotmp` file, so the
    /// final file is assembled in place: no concatenation step.
    ///
    /// Equivalent to rclone `--multi-thread-streams N`.
    /// Caller must ensure `total_size > 0` and the server advertises range support.
    async fn download_multi_thread(
        &self,
        key: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let streams = self
            .multi_thread_streams
            .clamp(2, Self::MULTI_THREAD_MAX_STREAMS);
        let ranges = crate::providers::multi_thread::plan_multi_thread_ranges(
            total_size,
            streams,
            Self::MULTI_THREAD_MAX_STREAMS,
        );
        if ranges.is_empty() {
            return Err(ProviderError::TransferFailed(
                "Multi-thread download: empty range plan".to_string(),
            ));
        }

        // Compute temp path matching `AtomicFile::temp_path_for` so existing
        // cleanup tooling and the resume path stay consistent.
        let final_pathbuf = PathBuf::from(local_path);
        let temp_path: PathBuf = {
            let mut p = final_pathbuf.as_os_str().to_owned();
            p.push(".aerotmp");
            PathBuf::from(p)
        };

        if let Some(parent) = final_pathbuf.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(ProviderError::IoError)?;
            }
        }

        // Pre-allocate the temp file. `set_len` reserves the full size up front so
        // that concurrent seek+writes don't race on file extension.
        {
            let f = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            f.set_len(total_size)
                .await
                .map_err(ProviderError::IoError)?;
            f.sync_all().await.map_err(ProviderError::IoError)?;
        }

        // RAII guard: remove the .aerotmp on early return unless we mark it committed.
        struct TempGuard {
            path: PathBuf,
            committed: bool,
        }
        impl Drop for TempGuard {
            fn drop(&mut self) {
                if !self.committed {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
        let mut guard = TempGuard {
            path: temp_path.clone(),
            committed: false,
        };

        // Aggregate counter of bytes written across all streams (lock-free).
        let aggregate = Arc::new(AtomicU64::new(0));

        // Background progress emitter: ticks every 100 ms, reads the aggregate
        // and forwards it to the user-supplied callback. Decouples the workers
        // from the (Send-only, !Sync) `on_progress` closure.
        let progress_stop = Arc::new(AtomicBool::new(false));
        let progress_handle = if let Some(cb) = on_progress {
            let agg = aggregate.clone();
            let stop = progress_stop.clone();
            Some(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut last_emitted: u64 = u64::MAX;
                loop {
                    ticker.tick().await;
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let cur = agg.load(Ordering::Relaxed);
                    if cur != last_emitted {
                        cb(cur, total_size);
                        last_emitted = cur;
                    }
                    if cur >= total_size {
                        break;
                    }
                }
                // Final flush: ensures the user sees the last byte counts even if
                // the download finished between two ticks.
                let cur = agg.load(Ordering::Relaxed);
                if cur != last_emitted {
                    cb(cur, total_size);
                }
            }))
        } else {
            None
        };

        // Spawn one task per range. JoinSet so the first failure aborts siblings,
        // mirroring the multipart upload pattern (`upload_multipart`).
        let mut joinset = tokio::task::JoinSet::new();
        for (start, end) in ranges {
            let provider = self.clone();
            let key_owned = key.to_string();
            let temp = temp_path.clone();
            let agg = aggregate.clone();
            joinset.spawn(async move {
                download_range_to_offset(provider, key_owned, temp, start, end, agg).await
            });
        }

        let mut first_error: Option<ProviderError> = None;
        while let Some(joined) = joinset.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    joinset.abort_all();
                    while joinset.join_next().await.is_some() {}
                    break;
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(ProviderError::TransferFailed(format!(
                            "Multi-thread download task panicked: {e}"
                        )));
                    }
                    joinset.abort_all();
                    while joinset.join_next().await.is_some() {}
                    break;
                }
            }
        }

        // Stop the progress emitter and wait for it to drain before returning,
        // otherwise the user-supplied callback could be invoked after we've
        // declared the download finished.
        progress_stop.store(true, Ordering::Relaxed);
        if let Some(h) = progress_handle {
            let _ = h.await;
        }

        if let Some(err) = first_error {
            return Err(err);
        }

        // All ranges committed: atomic rename .aerotmp → final path.
        tokio::fs::rename(&temp_path, &final_pathbuf)
            .await
            .map_err(ProviderError::IoError)?;
        guard.committed = true;
        Ok(())
    }

    /// List all object keys under a given prefix (non-recursive, no delimiter).
    /// Used by rename (folder) and rmdir_recursive.
    /// Includes pagination via continuation-token (H-05).
    async fn list_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>, ProviderError> {
        let mut all_keys = Vec::new();
        // Filen's S3 bridge returns <Key> percent-encoded (issue #196). Decode to
        // the logical key here so the single downstream encode_s3_key_path() call
        // (server_side_copy_single / build_url) encodes exactly once. Without this,
        // a listed emoji key like "folder/%F0%9F%9A%80file.txt" gets re-encoded to
        // "%25F0...", producing a copy-source / delete key that fails SigV4 with
        // "401 The signature does not match" (issue #368). Mirrors parse_list_response.
        let filen_decode = self.is_filen_s3_endpoint();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> =
                vec![("list-type", "2"), ("prefix", prefix), ("max-keys", "1000")];

            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            if response.status() != StatusCode::OK {
                return Err(ProviderError::ServerError(
                    "Failed to list objects by prefix".to_string(),
                ));
            }

            let xml_str = response
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Parse keys and next token using quick-xml
            let mut reader = Reader::from_str(&xml_str);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();
            let mut inside_key = false;
            let mut inside_next_token = false;
            let mut next_token: Option<String> = None;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(ref e)) => {
                        let name = e.name();
                        let tag = String::from_utf8_lossy(name.as_ref());
                        match tag.as_ref() {
                            "Key" => inside_key = true,
                            "NextContinuationToken" => inside_next_token = true,
                            _ => {}
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            if inside_key {
                                // #368: decode Filen-encoded keys so the single
                                // downstream encode_s3_key_path() encodes once.
                                all_keys.push(filen_decode_listed_key(text, filen_decode));
                            } else if inside_next_token {
                                next_token = Some(text);
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let name = e.name();
                        let tag = String::from_utf8_lossy(name.as_ref());
                        match tag.as_ref() {
                            "Key" => inside_key = false,
                            "NextContinuationToken" => inside_next_token = false,
                            _ => {}
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                buf.clear();
            }

            if let Some(token) = next_token {
                continuation_token = Some(token);
            } else {
                break;
            }
        }

        Ok(all_keys)
    }
}

/// Extract error message from S3 XML error response
fn extract_s3_error(body: &str) -> String {
    if body.contains("<Message>") {
        body.split("<Message>")
            .nth(1)
            .and_then(|s| s.split("</Message>").next())
            .unwrap_or("Access denied")
            .to_string()
    } else {
        body.to_string()
    }
}

#[async_trait]
impl StorageProvider for S3Provider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::S3
    }

    fn display_name(&self) -> String {
        if self.config.endpoint.is_some() {
            format!("s3://{} (custom)", self.config.bucket)
        } else {
            format!("s3://{} ({})", self.config.bucket, self.config.region)
        }
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // #389: the Filen Desktop S3 bridge protocol (HTTP vs HTTPS) is a user
        // setting in the Filen app, independent of the scheme saved in this
        // profile. Reconcile the endpoint against the live bridge (scheme +
        // loopback IP) so the connect survives either choice. The client already
        // trusts loopback / local.s3.filen.io self-signed certs (is_local_s3_endpoint).
        if let Some(ep) = self.config.endpoint.clone() {
            let fixed = crate::local_bridge::reconcile_local_bridge_url(&ep).await;
            if fixed != ep {
                tracing::info!("[S3] Filen bridge endpoint reconciled {} -> {}", ep, fixed);
                self.config.endpoint = Some(fixed);
            }
        }
        // STS AssumeRole (issue #301): when a role ARN is configured, exchange
        // the long-term base credentials for temporary ones before any signed S3
        // request is made. Runs ahead of the bucket probe (and the
        // no_check_bucket early return) because the assumed role, not the base
        // key, is what is authorized for the data-plane workload. Subsequent
        // requests re-check freshness and re-assume before expiry (Fase 3).
        self.ensure_fresh_credentials().await?;

        // Guard against the silent AWS auto-region fallback. With no endpoint
        // configured, `endpoint()` builds `https://s3.{region}.amazonaws.com`,
        // which is only correct for real AWS regions. Every other S3-compatible
        // provider (Backblaze B2, R2, MinIO, Storj, ...) needs an explicit
        // endpoint. A profile saved without the endpoint field and with a
        // non-AWS placeholder region (`auto` or empty) would otherwise dial
        // `s3.auto.amazonaws.com` and surface an opaque DNS/network error
        // (observed on a Backblaze profile saved without the endpoint). Fail
        // fast with an actionable message instead.
        let endpoint_missing = self
            .config
            .endpoint
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty();
        if endpoint_missing {
            let region = self.config.region.trim();
            if region.is_empty() || region.eq_ignore_ascii_case("auto") {
                return Err(ProviderError::InvalidConfig(
                    "S3 endpoint required: this profile has no endpoint and the \
                     region is 'auto'/empty (which has no AWS endpoint). Set the \
                     Endpoint field, e.g. s3.<region>.backblazeb2.com for \
                     Backblaze B2, or use the native provider preset."
                        .to_string(),
                ));
            }
        }

        // Reset clock offset for fresh connection
        self.clock_offset_secs = 0;

        // KE-B1.2: --s3-no-check-bucket skips the bucket-existence probe
        // entirely. Use when the IAM policy grants PutObject but denies
        // ListBucket: the probe would 403 even though the credentials are
        // valid for the actual upload workload.
        if self.no_check_bucket {
            self.connected = true;
            if let Some(ref prefix) = self.config.prefix {
                self.current_prefix = prefix.trim_matches('/').to_string();
            }
            return Ok(());
        }

        // Connection probe: GET on the bucket root with an explicit empty
        // `prefix=` query parameter (legacy ListObjects v1).
        // Per filen-s3 source (FilenCloudDienste/filen-s3 README), the Filen
        // Desktop S3 server "only supports Prefix parameter" on ListObjects/V2
        // and rejects list-type=2, max-keys, continuation tokens, and bare
        // bucket-only requests with "BadRequest: Invalid prefix specified".
        // Sending `?prefix=` explicitly is universally accepted by AWS, MinIO,
        // Wasabi, B2, R2, and Filen, and is the most compatible probe.
        let response = self
            .s3_request(Method::GET, "", Some(&[("prefix", "")]), None)
            .await?;

        match response.status() {
            StatusCode::OK => {
                self.connected = true;
                if let Some(ref prefix) = self.config.prefix {
                    self.current_prefix = prefix.trim_matches('/').to_string();
                }
                Ok(())
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                // Grab server Date header before consuming response body
                let server_date = response
                    .headers()
                    .get("date")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                let body = response.text().await.unwrap_or_default();
                let error_msg = extract_s3_error(&body);

                // Detect clock skew: error mentions "time" or "expired" and we haven't retried yet
                let is_time_error = {
                    let lower = error_msg.to_lowercase();
                    lower.contains("time")
                        || lower.contains("expired")
                        || body.contains("RequestTimeTooSkewed")
                };

                if is_time_error {
                    // Try server Date header first, then <ServerTime> from XML body
                    let server_time = server_date.or_else(|| {
                        body.split("<ServerTime>")
                            .nth(1)
                            .and_then(|s| s.split("</ServerTime>").next())
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&Utc))
                    });

                    if let Some(st) = server_time {
                        let offset = (st - Utc::now()).num_seconds();
                        info!(
                            "S3 clock skew detected ({offset}s), retrying with corrected timestamp"
                        );
                        self.clock_offset_secs = offset;

                        // Retry with corrected clock
                        let retry = self
                            .s3_request(
                                Method::GET,
                                "",
                                Some(&[("list-type", "2"), ("max-keys", "1")]),
                                None,
                            )
                            .await?;

                        return match retry.status() {
                            StatusCode::OK => {
                                self.connected = true;
                                if let Some(ref prefix) = self.config.prefix {
                                    self.current_prefix = prefix.trim_matches('/').to_string();
                                }
                                Ok(())
                            }
                            _ => {
                                let retry_body = retry.text().await.unwrap_or_default();
                                Err(ProviderError::AuthenticationFailed(format!(
                                    "S3 auth error: {}",
                                    sanitize_api_error(&extract_s3_error(&retry_body))
                                )))
                            }
                        };
                    }
                }

                Err(ProviderError::AuthenticationFailed(format!(
                    "S3 auth error: {}",
                    sanitize_api_error(&error_msg)
                )))
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(format!(
                "Bucket '{}' not found",
                self.config.bucket
            ))),
            status => {
                let body = response.text().await.unwrap_or_default();
                debug!("[S3] connect() failed with status={} body={}", status, body);
                Err(ProviderError::ConnectionFailed(format!(
                    "S3 error ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = if path.is_empty() || path == "/" || path == "." {
            self.current_prefix.clone()
        } else {
            path.trim_matches('/').to_string()
        };

        let prefix_with_slash = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };

        let mut all_entries = Vec::new();
        let mut continuation_token: Option<String> = None;
        let filen_dialect = self.is_filen_s3_endpoint();

        // LIST-01: Pagination loop handles >1000 items via NextContinuationToken.
        // Filen Desktop S3 dialect (filen-s3): ListObjects supports only `Prefix`
        // (and implicit Delimiter); list-type, max-keys, continuation-token are
        // rejected with "BadRequest: Invalid prefix specified". Filen always
        // returns the full result set (no pagination), so the loop runs once.
        loop {
            let mut params: Vec<(&str, &str)> = if filen_dialect {
                // Filen always returns all results in one shot, no pagination.
                vec![("delimiter", "/"), ("prefix", &prefix_with_slash)]
            } else {
                vec![("list-type", "2"), ("delimiter", "/"), ("max-keys", "1000")]
            };

            if !filen_dialect && !prefix_with_slash.is_empty() {
                params.push(("prefix", &prefix_with_slash));
            }

            let token_str: String;
            if !filen_dialect {
                if let Some(ref token) = continuation_token {
                    token_str = token.clone();
                    params.push(("continuation-token", &token_str));
                }
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            match response.status() {
                StatusCode::OK => {
                    let xml = response
                        .text()
                        .await
                        .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                    // Debug: Log raw XML response (truncated for readability).
                    // Must NOT use `&xml[..2000]`: a byte slice that lands
                    // inside a multi-byte UTF-8 codepoint (emoji in an
                    // object key, non-ASCII bucket/prefix) panics with
                    // "end byte index is not a char boundary". Iterate on
                    // chars + head cap instead.
                    let xml_preview = if xml.len() > 2000 {
                        let head: String = xml.chars().take(2000).collect();
                        format!("{head}... [truncated, total {} bytes]", xml.len())
                    } else {
                        xml.clone()
                    };
                    debug!("S3 LIST response XML:\n{}", xml_preview);

                    if let Some(error) = Self::bucket_addressing_error(&xml) {
                        return Err(error);
                    }

                    let (entries, next_token) = self.parse_list_response(&xml)?;
                    info!("S3 LIST parsed {} entries from response", entries.len());
                    all_entries.extend(entries);

                    // Filen returns the full result set in one shot (no pagination).
                    if filen_dialect {
                        break;
                    }
                    if let Some(token) = next_token {
                        continuation_token = Some(token);
                    } else {
                        break;
                    }
                }
                status => {
                    let body = response.text().await.unwrap_or_default();
                    // Extract error message from XML if present
                    let error_msg = if body.contains("<Message>") {
                        body.split("<Message>")
                            .nth(1)
                            .and_then(|s| s.split("</Message>").next())
                            .unwrap_or(&body)
                            .to_string()
                    } else if body.contains("<Code>") {
                        // Try to get the error code
                        body.split("<Code>")
                            .nth(1)
                            .and_then(|s| s.split("</Code>").next())
                            .unwrap_or(&body)
                            .to_string()
                    } else {
                        body
                    };
                    return Err(ProviderError::ServerError(format!(
                        "List failed ({}): {}",
                        status,
                        sanitize_api_error(&error_msg)
                    )));
                }
            }
        }

        Ok(all_entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_prefix.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_prefix))
        }
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let new_prefix = if path == "/" || path.is_empty() {
            String::new()
        } else if path == ".." {
            // Go up one level
            let parts: Vec<&str> = self.current_prefix.split('/').collect();
            if parts.len() > 1 {
                parts[..parts.len() - 1].join("/")
            } else {
                String::new()
            }
        } else if path.starts_with('/') || self.current_prefix.is_empty() {
            path.trim_matches('/').to_string()
        } else {
            format!("{}/{}", self.current_prefix, path.trim_matches('/'))
        };

        // Verify the prefix exists by listing it
        let prefix_check = if new_prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", new_prefix)
        };

        let response = self
            .s3_request(
                Method::GET,
                "",
                Some(&[
                    ("list-type", "2"),
                    ("prefix", &prefix_check),
                    ("max-keys", "1"),
                ]),
                None,
            )
            .await?;

        if response.status() == StatusCode::OK {
            self.current_prefix = new_prefix;
            Ok(())
        } else {
            Err(ProviderError::NotFound(path.to_string()))
        }
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = remote_path.trim_start_matches('/');

        // U-13 Phase 1: multi-thread chunk-parallel download.
        // Engaged only when:
        //   1. user opted in (`set_multi_thread_download(streams >= 2, ...)`),
        //   2. HEAD succeeds and reports a known content length,
        //   3. file size meets the configured cutoff,
        //   4. server advertises Accept-Ranges (or omits it, since S3 supports
        //      ranges by default: only an explicit "none" disables it).
        // On any HEAD-side problem we fall through to the single-stream path so
        // a one-off mismatch never fails an otherwise downloadable transfer.
        let on_progress = if self.multi_thread_streams >= 2 {
            match self.s3_request(Method::HEAD, key, None, None).await {
                Ok(head) if head.status() == StatusCode::OK => {
                    let size = head.content_length().unwrap_or(0);
                    let accepts_ranges = head
                        .headers()
                        .get("accept-ranges")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| !s.eq_ignore_ascii_case("none"))
                        .unwrap_or(true);
                    if size >= self.multi_thread_cutoff && accepts_ranges {
                        return self
                            .download_multi_thread(key, local_path, size, on_progress)
                            .await;
                    }
                    if !accepts_ranges {
                        warn!(
                            "S3 multi-thread download disabled: server advertised Accept-Ranges: none for {}",
                            key
                        );
                    }
                    on_progress
                }
                Ok(other) => {
                    debug!(
                        "S3 multi-thread HEAD probe returned {} for {}, falling back to single-stream",
                        other.status(),
                        key
                    );
                    on_progress
                }
                Err(e) => {
                    debug!(
                        "S3 multi-thread HEAD probe failed for {}: {}, falling back to single-stream",
                        key, e
                    );
                    on_progress
                }
            }
        } else {
            on_progress
        };

        // DL-01: Retry handled by s3_request → send_with_retry (429, 5xx)
        let response = self.s3_request(Method::GET, key, None, None).await?;

        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);

                // H-01: Streaming download: write chunks as they arrive (atomic)
                let mut stream = response.bytes_stream();
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let mut downloaded: u64 = 0;

                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    atomic
                        .write_all(&chunk)
                        .await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    downloaded += chunk.len() as u64;
                    if let Some(ref progress) = on_progress {
                        progress(downloaded, total_size);
                    }
                }
                atomic.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;

                Ok(())
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            ))),
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = remote_path.trim_start_matches('/');
        let range_value = format!("bytes={}-", offset);
        let response = self
            .s3_request_ext(Method::GET, key, None, None, &[("range", &range_value)])
            .await?;

        match response.status() {
            StatusCode::PARTIAL_CONTENT => {
                let content_len = response.content_length().unwrap_or(0);
                let total_size = offset + content_len;
                let mut resumable = super::atomic_write::ResumableFile::open(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                super::stream_response_to_resumable(
                    response,
                    &mut resumable,
                    total_size,
                    on_progress,
                )
                .await?;
                resumable.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
                Ok(())
            }
            StatusCode::OK => {
                // Server ignored Range: full content returned, restart from scratch
                let total_size = response.content_length().unwrap_or(0);
                let mut fresh = super::atomic_write::ResumableFile::open_fresh(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                super::stream_response_to_resumable(response, &mut fresh, total_size, on_progress)
                    .await?;
                fresh.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
                Ok(())
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                // Discard stale .aerotmp to prevent infinite 416 loop on next attempt
                let tmp = format!("{}.aerotmp", local_path);
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(ProviderError::TransferFailed(
                    "Range not satisfiable: file may have changed on server".to_string(),
                ))
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Resume download failed with status: {}",
                status
            ))),
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = remote_path.trim_start_matches('/');
        let response = self.s3_request(Method::GET, key, None, None).await?;

        match response.status() {
            StatusCode::OK => {
                // H2: Size-limited download to prevent OOM on large files
                super::response_bytes_with_limit(response, super::MAX_DOWNLOAD_TO_BYTES).await
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            ))),
        }
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;

        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total_size = file_meta.len();
        let key = remote_path.trim_start_matches('/');

        // UPLOAD-02: Use streaming multipart upload for files larger than 5MB.
        // Reads chunks from disk instead of buffering entire file in RAM.
        // Filen Desktop S3 (filen-s3) returns 501 Not Implemented for
        // CreateMultipartUpload, so we route every upload through the
        // single-PUT path on that dialect (the server buffers the whole
        // request body in memory by design, per filen-s3 README).
        let force_single_put = self.is_filen_s3_endpoint();
        if total_size > Self::MULTIPART_THRESHOLD as u64 && !force_single_put {
            return self
                .upload_multipart_streaming(key, local_path, total_size, on_progress)
                .await;
        }

        // Streaming upload for small files (< 5MB)
        use tokio_util::io::ReaderStream;
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let stream = ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        // Build the request manually with streaming body (cannot use s3_request helper for streaming)
        let url = self.build_url(key);
        // For streaming, we use UNSIGNED-PAYLOAD since we cannot hash the stream upfront
        let payload_hash = "UNSIGNED-PAYLOAD";
        let mut headers = HashMap::new();
        // B2: Add storage class + SSE headers before signing
        self.append_upload_headers(&mut headers);
        let authorization = self.sign_request("PUT", &url, &mut headers, payload_hash)?;

        // UPLOAD-01: Detect MIME type from filename extension
        let content_type = mime_guess::from_path(local_path)
            .first_or_octet_stream()
            .to_string();

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", total_size.to_string());
        request = request.header("Content-Type", &content_type);
        request = request.body(body);

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if let Some(progress) = on_progress {
                    progress(total_size, total_size);
                }
                Ok(())
            }
            status => {
                let retry_header = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(String::from);
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format_s3_error(
                    "Upload failed",
                    status,
                    &body,
                    retry_header.as_deref(),
                )))
            }
        }
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // S3 has no real directories; rclone-style, we do not persist an
        // empty-folder marker object (owner decision #266, see
        // docs/dev/DECISION-s3-marker-266.md). A prefix comes into existence
        // once it holds an object, so mkdir is a no-op.
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        let response = self.s3_request(Method::DELETE, key, None, None).await?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Delete failed with status: {}",
                status
            ))),
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // In S3, directories are virtual (just key prefixes). MinIO and some
        // S3-compatible providers may not create/delete marker objects reliably.
        // Use rmdir_recursive to clean up the marker AND any lingering objects.
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;

        // Guard: refuse to wipe the entire bucket
        if path.trim_matches('/').is_empty() {
            return Err(ProviderError::InvalidPath(
                "Refusing to recursively delete root '/'. This would erase the entire bucket."
                    .into(),
            ));
        }

        let prefix = format!("{}/", path.trim_matches('/'));
        let mut keys = self.list_keys_with_prefix(&prefix).await?;

        // Always include the directory marker itself (key with trailing slash).
        // MinIO and some S3-compatible providers create this marker on mkdir
        // but list_keys_with_prefix may not return it as a regular key.
        if !keys.contains(&prefix) {
            keys.push(prefix.clone());
        }
        // Also try without trailing slash (some providers use both)
        let no_slash = path.trim_matches('/').to_string();
        if !keys.contains(&no_slash) {
            keys.push(no_slash);
        }

        tracing::info!(
            "rmdir_recursive: deleting {} keys under prefix '{}'",
            keys.len(),
            prefix
        );

        // DELETE-01: Use S3 batch delete (POST /?delete) for up to 1000 keys per
        // request. Delete the current version of each key (no version id).
        let objects: Vec<(String, Option<String>)> = keys.into_iter().map(|k| (k, None)).collect();
        self.batch_delete_objects(&objects).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_trimmed = from.trim_matches('/');
        let to_trimmed = to.trim_matches('/');
        let prefix = format!("{}/", from_trimmed);

        // Check if this is a directory by listing objects under the prefix
        let keys = self.list_keys_with_prefix(&prefix).await?;

        if keys.is_empty() {
            if self.is_filelu_s3_endpoint() {
                return self.rename_filelu_safe(from, to).await;
            }

            // ListObjectsV2 with the source as prefix returned nothing. Two
            // cases collapse here: (a) `from` is a real file (no children),
            // (b) `from` is a virtual folder with no marker key. Some
            // S3-compatible bridges (Filen's local S3 in particular)
            // represent empty folders as CommonPrefixes generated from
            // internal metadata, with no actual key. Attempting Copy on
            // such a phantom returns 412 Precondition Failed, surfacing as
            // a confusing error to the user. Probe with HEAD: if the
            // source has no underlying object, fail with a clear message
            // rather than letting the wrapper return 412. Issue #128.
            let from_key = from.trim_start_matches('/');
            // Shared message for the phantom-folder case: an empty folder is a
            // virtual prefix with no marker key, so it cannot be renamed
            // server-side (issue #128).
            let virtual_folder_err = || {
                ProviderError::NotSupported(format!(
                    "Cannot rename '{}': the path does not exist as an \
                     S3 object. Some S3-compatible backends (e.g. Filen's \
                     local S3 bridge) represent empty folders as virtual \
                     prefixes without a marker key, which precludes \
                     server-side rename. Add a file inside the folder \
                     first, or use the native API / WebDAV bridge.",
                    from
                ))
            };
            match self.s3_request(Method::HEAD, from_key, None, None).await {
                Ok(resp) if resp.status() == StatusCode::OK => {
                    // Real file: proceed with single-file rename.
                    self.server_copy(from, to).await?;
                    self.verify_copy_target_exists(to).await?;
                    self.delete(from).await?;
                    info!("Renamed file (copy+delete) {} to {}", from, to);
                }
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                    return Err(virtual_folder_err());
                }
                // Filen's local S3 bridge (the Windows build in particular,
                // issue #368) answers HEAD on a virtual-folder key with 401/403
                // instead of 404. Reaching this arm means the prefix listing
                // already succeeded, so credentials are valid and this is the
                // same phantom-folder case, not a real auth failure. Map it to
                // the actionable message instead of leaking the raw status.
                Ok(resp)
                    if self.is_filen_s3_endpoint()
                        && (resp.status() == StatusCode::UNAUTHORIZED
                            || resp.status() == StatusCode::FORBIDDEN) =>
                {
                    return Err(virtual_folder_err());
                }
                Ok(resp) => {
                    return Err(ProviderError::ServerError(format!(
                        "HEAD on rename source returned status {}",
                        resp.status()
                    )));
                }
                Err(e) => return Err(e),
            }
        } else {
            // Directory rename: copy all objects to new prefix, then delete originals
            let to_prefix = format!("{}/", to_trimmed);

            for old_key in &keys {
                let new_key = old_key.replacen(&prefix, &to_prefix, 1);
                self.server_copy(&format!("/{}", old_key), &format!("/{}", new_key))
                    .await?;
            }

            // Delete all original objects
            for old_key in &keys {
                let _ = self.s3_request(Method::DELETE, old_key, None, None).await;
            }

            // Also try to delete the old directory marker (if exists)
            let _ = self.s3_request(Method::DELETE, &prefix, None, None).await;

            info!(
                "Renamed directory (copy+delete {} objects) {} to {}",
                keys.len(),
                from,
                to
            );
        }

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');

        // Use HEAD request to get object metadata
        let response = self.s3_request(Method::HEAD, key, None, None).await?;

        match response.status() {
            StatusCode::OK => {
                let size = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);

                let modified = response
                    .headers()
                    .get("last-modified")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let etag = response
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.trim_matches('"').to_string());

                let name = key.rsplit('/').next().unwrap_or(key).to_string();
                let is_dir = key.ends_with('/') && size == 0;

                let mut metadata = HashMap::new();
                if let Some(etag) = etag {
                    if let Some(md5) = etag_to_md5(&etag) {
                        metadata.insert("md5".to_string(), md5);
                    }
                    metadata.insert("etag".to_string(), etag);
                }

                Ok(RemoteEntry {
                    name,
                    path: format!("/{}", key),
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: content_type,
                    metadata,
                })
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "HEAD failed with status: {}",
                status
            ))),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    /// Server-side MD5 derived from the object ETag (HEAD only, no download).
    ///
    /// Returns an empty map for multipart or SSE-encrypted objects whose
    /// ETag is not the object MD5: honest, matching rclone (omit over
    /// guess). `stat()` already normalises the ETag into `metadata["md5"]`.
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut out = HashMap::new();
        if let Some(md5) = entry.metadata.get("md5") {
            out.insert("md5".to_string(), md5.clone());
        }
        Ok(out)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // S3 is stateless, just verify credentials still work
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .s3_request(
                Method::GET,
                "",
                Some(&[("list-type", "2"), ("max-keys", "0")]),
                None,
            )
            .await?;

        if response.status() == StatusCode::FORBIDDEN {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed(
                "Credentials expired".to_string(),
            ));
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let endpoint = if let Some(ref ep) = self.config.endpoint {
            ep.clone()
        } else {
            format!("AWS S3 ({})", self.config.region)
        };

        Ok(format!(
            "S3 Storage: {} - Bucket: {}",
            endpoint, self.config.bucket
        ))
    }

    // QUOTA-01: S3 buckets have no inherent storage quota. AWS S3 provides unlimited storage
    // with pay-per-use pricing. There is no API to query "used/total" space for a bucket.
    // CloudWatch metrics (BucketSizeBytes) are delayed by ~24h and require separate permissions.
    // Returning NotSupported is the correct behavior for S3.
    //
    // Filen Desktop's local S3 bridge is an exception worth surfacing explicitly: users
    // who attached AeroFTP through the local S3 endpoint expect to see the same quota
    // bar they get on the native Filen API and WebDAV bridges, but the local S3 bridge
    // simply does not expose any "used / available" pair through S3 verbs (HeadBucket
    // returns no quota headers, no proprietary GET endpoint is published, and querying
    // the cloud Filen API would require the account password / API key which the S3
    // attachment never collects). The override below catches the Filen S3 case and
    // returns a precise, user-facing message pointing at the native API / WebDAV
    // attachments rather than the generic `NotSupported("storage_info")` placeholder.
    // Issue #128 follow-up.
    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        if self.is_filen_s3_endpoint() {
            return Err(ProviderError::NotSupported(
                "Filen Desktop's local S3 bridge does not expose storage quota over S3. \
                 Connect to the same Filen account through the native Filen API or the \
                 WebDAV bridge to see the used/total bar; the S3 attachment is intended \
                 for raw object access without quota reporting."
                    .to_string(),
            ));
        }
        Err(ProviderError::NotSupported("storage_info".to_string()))
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: false,
            supports_permissions: false,
            available_permissions: vec![],
            ..Default::default()
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        // Generate a presigned URL
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256};

        type HmacSha256 = Hmac<Sha256>;

        // A presigned URL cannot outlive the session token, so refresh first
        // (issue #301, Fase 3).
        self.ensure_fresh_credentials().await?;

        let key = path.trim_start_matches('/');
        // MEGA S4 presigned URLs have a maximum expiration of 7 days (604800 seconds)
        let max_expires = if self.is_mega_s4_endpoint() {
            604800_u64
        } else {
            u64::MAX
        };
        let expires = options.expires_in_secs.unwrap_or(3600).min(max_expires);

        let now: DateTime<Utc> = self.now_adjusted();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Single consistent snapshot of the effective credentials (issue #301).
        let creds = self.effective_credentials();

        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.config.region);
        let credential = format!("{}/{}", creds.access_key_id, credential_scope);

        let url = self.build_url(key);
        let parsed =
            url::Url::parse(&url).map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        let host = parsed.host_str().unwrap_or("");
        let raw_path = parsed.path();

        // M-13: URI-encode each path segment for the canonical URI
        let canonical_path = if raw_path.is_empty() || raw_path == "/" {
            "/".to_string()
        } else {
            let encoded_segments: Vec<String> = raw_path
                .split('/')
                .map(|segment| {
                    if segment.is_empty() {
                        String::new()
                    } else {
                        urlencoding::encode(segment).into_owned()
                    }
                })
                .collect();
            encoded_segments.join("/")
        };

        // Build canonical query string. SigV4 requires the query parameters to
        // appear in alphabetical order in the canonical request; the same string
        // is reused verbatim for the final presigned URL. With temporary
        // credentials the session token is carried as `X-Amz-Security-Token`,
        // which sorts between `X-Amz-Expires` and `X-Amz-SignedHeaders`.
        let signed_headers = "host";
        let security_token_param = creds
            .session_token
            .as_ref()
            .map(|t| {
                format!(
                    "X-Amz-Security-Token={}&",
                    urlencoding::encode(t.expose_secret())
                )
            })
            .unwrap_or_default();
        let query_params = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&{}X-Amz-SignedHeaders={}",
            urlencoding::encode(&credential),
            amz_date,
            expires,
            security_token_param,
            signed_headers
        );

        // Canonical request
        let canonical_request = format!(
            "GET\n{}\n{}\nhost:{}\n\n{}\nUNSIGNED-PAYLOAD",
            canonical_path, query_params, host, signed_headers
        );

        let canonical_hash = {
            let mut hasher = Sha256::new();
            hasher.update(canonical_request.as_bytes());
            hex::encode(hasher.finalize())
        };

        // String to sign
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_hash
        );

        // Calculate signature
        fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }

        let k_date = hmac_sha256(
            format!("AWS4{}", creds.secret_access_key.expose_secret()).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.config.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        Ok(ShareLinkResult {
            url: format!("{}?{}&X-Amz-Signature={}", url, query_params, signature),
            password: None,
            expires_at: None,
        })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = path.trim_matches('/');
        let prefix_with_slash = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };

        // M1: Cap search results to prevent unbounded memory growth on large buckets.
        // S3 buckets can contain millions of objects; without a cap, a broad pattern
        // could return all of them, causing OOM.
        const MAX_SEARCH_RESULTS: usize = 10_000;

        // Use ListObjectsV2 with prefix (no delimiter to get all recursive objects)
        let mut all_entries = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> = vec![("list-type", "2"), ("max-keys", "1000")];

            if !prefix_with_slash.is_empty() {
                params.push(("prefix", &prefix_with_slash));
            }

            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            if response.status() != StatusCode::OK {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Search failed: {}",
                    sanitize_api_error(&body)
                )));
            }

            let xml_str = response
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Parse keys, sizes, and filter by pattern using quick-xml
            let mut find_reader = Reader::from_str(&xml_str);
            find_reader.config_mut().trim_text(true);
            let mut find_buf = Vec::new();
            let mut in_contents = false;
            let mut in_next_tok = false;
            let mut find_tag = String::new();
            let mut find_key: Option<String> = None;
            let mut find_size: Option<String> = None;
            let mut find_modified: Option<String> = None;
            let mut next_tok_val: Option<String> = None;

            loop {
                match find_reader.read_event_into(&mut find_buf) {
                    Ok(Event::Start(ref e)) => {
                        let tn = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tn.as_str() {
                            "Contents" => {
                                in_contents = true;
                                find_key = None;
                                find_size = None;
                                find_modified = None;
                            }
                            "NextContinuationToken" => in_next_tok = true,
                            _ => find_tag = tn,
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let t = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !t.is_empty() {
                            if in_next_tok {
                                next_tok_val = Some(t.clone());
                            }
                            if in_contents {
                                match find_tag.as_str() {
                                    "Key" => find_key = Some(t),
                                    "Size" => find_size = Some(t),
                                    "LastModified" => find_modified = Some(t),
                                    _ => {}
                                }
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let tn = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tn.as_str() {
                            "Contents" => {
                                if let Some(ref key) = find_key {
                                    if !key.ends_with('/') {
                                        let name = key.rsplit('/').next().unwrap_or(key);
                                        if super::matches_find_pattern(name, pattern) {
                                            let size: u64 = find_size
                                                .as_ref()
                                                .and_then(|s| s.parse().ok())
                                                .unwrap_or(0);
                                            all_entries.push(RemoteEntry {
                                                name: name.to_string(),
                                                path: format!("/{}", key),
                                                is_dir: false,
                                                size,
                                                modified: find_modified.clone(),
                                                permissions: None,
                                                owner: None,
                                                group: None,
                                                is_symlink: false,
                                                link_target: None,
                                                mime_type: None,
                                                metadata: HashMap::new(),
                                            });
                                        }
                                    }
                                }
                                in_contents = false;
                            }
                            "NextContinuationToken" => in_next_tok = false,
                            _ => {}
                        }
                        find_tag.clear();
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                find_buf.clear();
            }

            // M1: Stop paginating once we've collected enough results
            if all_entries.len() >= MAX_SEARCH_RESULTS {
                info!(
                    "S3 find: reached {} result cap, stopping pagination",
                    MAX_SEARCH_RESULTS
                );
                break;
            }

            match next_tok_val {
                Some(token) => continuation_token = Some(token),
                None => break,
            }
        }

        Ok(all_entries)
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so the rest of the codebase
        // (provider_commands::provider_supports_server_copy, CLI helpers,
        // MCP tools) keeps working unchanged. New DAG runner code reaches
        // for `server_side_copy` directly, which now owns the real
        // x-amz-copy-source implementation.
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_key = from.trim_start_matches('/');
        let to_key = to.trim_start_matches('/');

        // HEAD source up front so we know whether to take the single-PUT
        // CopyObject path or compose the copy as a multipart sequence of
        // UploadPartCopy operations. The HEAD is one cheap round trip per
        // copy and surfaces a `NotFound` immediately instead of waiting
        // for the PUT to fail downstream. Network egress is unchanged:
        // headers only.
        let source_meta = self.stat(from).await?;
        let source_size = source_meta.size;

        if source_size <= Self::COPY_OBJECT_MAX {
            return self.server_side_copy_single(from_key, to_key).await;
        }

        // T-DEBT-08: source > 5 GiB, single CopyObject would fail with
        // `EntityTooLarge`. Fall through to multipart copy. Content-Type
        // is preserved best-effort from the HEAD response; the rest of
        // the source metadata is not (S3 multipart copy lacks the
        // single-PUT `metadata-directive: COPY` slot).
        let content_type = source_meta.mime_type;
        self.server_side_copy_multipart(from_key, to_key, source_size, content_type.as_deref())
            .await
    }

    // Shaped-graph multipart trait wiring.
    //
    // The S3 path already had a private multipart pipeline used by
    // `upload()` when total_size exceeds MULTIPART_THRESHOLD. Exposing it
    // through the trait lets the DAG runner dispatch UploadPart nodes
    // directly (chunk-parallel, AIMD-controlled) without going through
    // the monolithic `upload_multipart_streaming` orchestrator.

    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        _total_size: u64,
        content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // Filen Desktop's local S3 bridge returns 501 on CreateMultipartUpload
        // by design (it buffers the whole request body), so fail fast so the
        // runner falls back to a single-PUT shape instead of burning a
        // round trip.
        if self.is_filen_s3_endpoint() {
            return Err(ProviderError::NotSupported(
                "S3 multipart upload disabled on filen-s3 endpoint".to_string(),
            ));
        }
        let key = remote_path.trim_start_matches('/');
        let upload_id = self.create_multipart_upload(key, content_type).await?;
        Ok(MultipartHandle {
            upload_id,
            remote_path: key.to_string(),
        })
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let etag = self
            .upload_part_internal(&handle.remote_path, &handle.upload_id, part_number, data)
            .await?;
        Ok(UploadedPart { part_number, etag })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let mut numbered: Vec<(u32, String)> =
            parts.into_iter().map(|p| (p.part_number, p.etag)).collect();
        numbered.sort_by_key(|(pn, _)| *pn);
        self.complete_multipart_upload_internal(&handle.remote_path, &handle.upload_id, &numbered)
            .await
    }

    async fn abort_multipart_upload(
        &mut self,
        handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.abort_multipart_upload_internal(&handle.remote_path, &handle.upload_id)
            .await
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Filen Desktop's local S3 bridge returns 501 on CreateMultipartUpload
        // by design. The DAG shaped-file path fans any file larger than the
        // chunk size into UploadPart nodes whenever `supports_multipart` is
        // true (it does NOT consult MULTIPART_THRESHOLD: that gate lives only
        // in the legacy `upload()` path). Advertising multipart for filen-s3
        // therefore made the lazy `begin_multipart_upload` fail the whole
        // transfer for anything above the chunk size (observed: 100M/1G
        // uploads returned code 7). Report single-shot only so the runner
        // builds a whole-file UploadFile node instead.
        let supports_multipart = !self.is_filen_s3_endpoint();
        super::TransferOptimizationHints {
            supports_multipart,
            multipart_threshold: if supports_multipart {
                Self::MULTIPART_THRESHOLD as u64
            } else {
                u64::MAX
            },
            multipart_part_size: self.effective_part_size() as u64,
            multipart_max_parallel: 4,
            supports_range_download: true,
            supports_resume_download: true,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("ETag".to_string()),
            ..Default::default()
        }
    }

    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        8
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Ok(Box::new(self.clone()))
    }

    fn list_executor_kind(&self) -> super::ProviderListExecutorKind {
        super::ProviderListExecutorKind::HttpClonePool
    }

    fn list_executor_max_sessions(&self) -> u16 {
        8
    }

    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Ok(Box::new(self.clone()))
    }

    fn set_chunk_sizes(&mut self, upload: Option<u64>, _download: Option<u64>) {
        if let Some(size) = upload {
            // Cap at 512 MB per part (S3 max is 5 GB, but 512 MB is practical)
            let capped = (size as usize).min(512 * 1024 * 1024);
            self.upload_chunk_override = Some(capped);
        }
    }

    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        // Clamp streams to [1, MAX]: 1 is the disabled state; values above the
        // cap rarely improve throughput and waste sockets. A cutoff of 0 would
        // engage multi-thread on every file regardless of size, which the
        // handoff explicitly warns against (overhead on small files), so we
        // floor the cutoff at 1 MiB.
        self.multi_thread_streams = streams.clamp(1, Self::MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes.max(1024 * 1024);
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(format!(
                "Read range size {} exceeds maximum {} bytes",
                len, MAX_READ_RANGE
            )));
        }

        let key = path.trim_start_matches('/');
        let end = offset + len - 1; // HTTP Range is inclusive
        let range_value = format!("bytes={}-{}", offset, end);

        let response = self
            .s3_request_ext(Method::GET, key, None, None, &[("range", &range_value)])
            .await?;

        match response.status() {
            StatusCode::PARTIAL_CONTENT | StatusCode::OK => {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                Ok(bytes.to_vec())
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            StatusCode::RANGE_NOT_SATISFIABLE => Err(ProviderError::NotSupported(
                "Server does not support range requests".to_string(),
            )),
            status => Err(ProviderError::TransferFailed(format!(
                "Range download failed with status: {}",
                status
            ))),
        }
    }

    fn supports_versions(&self) -> bool {
        // MEGA S4 does not support object versioning
        !self.is_mega_s4_endpoint()
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        let mut all_versions = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_id_marker: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> = vec![("versions", ""), ("prefix", key)];

            let km_str: String;
            let vm_str: String;
            if let Some(ref km) = key_marker {
                km_str = km.clone();
                params.push(("key-marker", &km_str));
            }
            if let Some(ref vm) = version_id_marker {
                vm_str = vm.clone();
                params.push(("version-id-marker", &vm_str));
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "ListObjectVersions failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )));
            }

            let xml_str = response
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            debug!("S3 ListObjectVersions response, {} bytes", xml_str.len());

            // Parse ListVersionsResult XML using quick-xml
            let mut reader = Reader::from_str(&xml_str);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();

            let mut in_version = false;
            let mut _in_delete_marker = false;
            let mut current_tag = String::new();

            // Fields for <Version> elements
            let mut v_key: Option<String> = None;
            let mut v_version_id: Option<String> = None;
            let mut v_is_latest: Option<String> = None;
            let mut v_last_modified: Option<String> = None;
            let mut v_size: Option<String> = None;

            // Pagination fields
            let mut is_truncated = false;
            let mut next_key_marker: Option<String> = None;
            let mut next_version_id_marker: Option<String> = None;
            let mut in_is_truncated = false;
            let mut in_next_key_marker = false;
            let mut in_next_version_id_marker = false;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(ref e)) => {
                        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tag_name.as_str() {
                            "Version" => {
                                in_version = true;
                                v_key = None;
                                v_version_id = None;
                                v_is_latest = None;
                                v_last_modified = None;
                                v_size = None;
                            }
                            "DeleteMarker" => {
                                _in_delete_marker = true;
                            }
                            "IsTruncated" => in_is_truncated = true,
                            "NextKeyMarker" => in_next_key_marker = true,
                            "NextVersionIdMarker" => in_next_version_id_marker = true,
                            _ => {
                                current_tag = tag_name;
                            }
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if text.is_empty() {
                            buf.clear();
                            continue;
                        }

                        if in_is_truncated {
                            is_truncated = text == "true";
                        }
                        if in_next_key_marker {
                            next_key_marker = Some(text.clone());
                        }
                        if in_next_version_id_marker {
                            next_version_id_marker = Some(text.clone());
                        }

                        if in_version {
                            match current_tag.as_str() {
                                "Key" => v_key = Some(text),
                                "VersionId" => v_version_id = Some(text),
                                "IsLatest" => v_is_latest = Some(text),
                                "LastModified" => v_last_modified = Some(text),
                                "Size" => v_size = Some(text),
                                _ => {}
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tag_name.as_str() {
                            "Version" => {
                                // Only include versions whose key exactly matches
                                if let Some(ref vk) = v_key {
                                    if vk == key {
                                        let version_id = v_version_id.clone().unwrap_or_default();
                                        let is_latest = v_is_latest.as_deref() == Some("true");
                                        let size: u64 = v_size
                                            .as_ref()
                                            .and_then(|s| s.parse().ok())
                                            .unwrap_or(0);

                                        let mut modified_by_str = None;
                                        if is_latest {
                                            modified_by_str = Some("(latest)".to_string());
                                        }

                                        all_versions.push(FileVersion {
                                            id: version_id,
                                            modified: v_last_modified.clone(),
                                            size,
                                            modified_by: modified_by_str,
                                        });
                                    }
                                }
                                in_version = false;
                            }
                            "DeleteMarker" => {
                                _in_delete_marker = false;
                            }
                            "IsTruncated" => in_is_truncated = false,
                            "NextKeyMarker" => in_next_key_marker = false,
                            "NextVersionIdMarker" => in_next_version_id_marker = false,
                            _ => {}
                        }
                        current_tag.clear();
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                buf.clear();
            }

            if is_truncated {
                key_marker = next_key_marker;
                version_id_marker = next_version_id_marker;
            } else {
                break;
            }
        }

        info!(
            "S3 ListObjectVersions: found {} versions for '{}'",
            all_versions.len(),
            key
        );
        Ok(all_versions)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        let response = self
            .s3_request(Method::GET, key, Some(&[("versionId", version_id)]), None)
            .await?;

        match response.status() {
            StatusCode::OK => {
                let mut stream = response.bytes_stream();
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;

                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    atomic
                        .write_all(&chunk)
                        .await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                }
                atomic.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;

                info!(
                    "Downloaded version '{}' of '{}' to '{}'",
                    version_id, key, local_path
                );
                Ok(())
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(format!(
                "Version '{}' of '{}' not found",
                version_id, path
            ))),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format!(
                    "Download version failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;

        let key = path.trim_start_matches('/');
        // Restore by copying the old version to itself. `encode_s3_key_path`
        // preserves slashes (unlike `urlencoding::encode` which encodes
        // them as `%2F`), so a nested key survives the copy-source
        // canonicalisation that strict S3 bridges (Filen Desktop local
        // bridge, in particular) perform during SigV4 verification.
        let copy_source = format!(
            "/{}/{}?versionId={}",
            self.config.bucket,
            encode_s3_key_path(key),
            urlencoding::encode(version_id)
        );

        let url = self.build_url(key);

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("x-amz-copy-source".to_string(), copy_source);
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                info!("Restored '{}' to version '{}'", key, version_id);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Restore version failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    /// Hard-delete a single version or delete marker: `DELETE /{key}?versionId={id}`.
    ///
    /// With a delete marker's own version id this "undeletes" a soft-deleted
    /// object (makes the prior version current again with no data copy); with a
    /// content version's id it permanently purges that version.
    async fn delete_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;

        let key = path.trim_start_matches('/');
        let response = self
            .s3_request(
                Method::DELETE,
                key,
                Some(&[("versionId", version_id)]),
                None,
            )
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                info!("Purged version '{}' of '{}'", version_id, key);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Delete version failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    /// List every version AND delete marker under `prefix`, grouped by key
    /// (powers the trash browse). Unlike `list_versions` (per-key, drops
    /// markers), this keeps `<DeleteMarker>` elements and does not filter to an
    /// exact key. When `include_noncurrent` is false, only delete markers and
    /// the latest version of each key are returned.
    async fn list_object_versions(
        &mut self,
        prefix: &str,
        include_noncurrent: bool,
    ) -> Result<Vec<TrashEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = prefix.trim_start_matches('/');
        let mut all_entries: Vec<TrashEntry> = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_id_marker: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> = vec![("versions", ""), ("prefix", prefix)];

            let km_str: String;
            let vm_str: String;
            if let Some(ref km) = key_marker {
                km_str = km.clone();
                params.push(("key-marker", &km_str));
            }
            if let Some(ref vm) = version_id_marker {
                vm_str = vm.clone();
                params.push(("version-id-marker", &vm_str));
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "ListObjectVersions failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )));
            }

            let xml_str = response
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let (entries, is_truncated, next_key_marker, next_version_id_marker) =
                parse_object_versions_page(&xml_str)?;
            all_entries.extend(entries);

            if is_truncated {
                key_marker = next_key_marker;
                version_id_marker = next_version_id_marker;
            } else {
                break;
            }
        }

        if !include_noncurrent {
            // Trash view: only delete markers and the current version of each key.
            all_entries.retain(|e| e.is_delete_marker || e.is_latest);
        }

        info!(
            "S3 ListObjectVersions: {} entries under prefix '{}' (include_noncurrent={})",
            all_entries.len(),
            prefix,
            include_noncurrent
        );
        Ok(all_entries)
    }
}

// =============================================================================
// S3 Enterprise Features (Storage Class, Tagging, SSE, Glacier, Checksum)
// =============================================================================

impl S3Provider {
    /// Batch-delete objects via `POST /?delete`, in chunks of 1000.
    ///
    /// Each entry is `(key, Option<version_id>)`: `None` deletes the current
    /// version (recursive delete), `Some(id)` deletes that specific version or
    /// delete marker (version-aware purge / empty-trash sweep). Reuses the
    /// Content-MD5 + SigV4 signed path; a failed chunk falls back to sequential
    /// deletes so a single bad key does not abort the whole sweep.
    pub async fn batch_delete_objects(
        &self,
        objects: &[(String, Option<String>)],
    ) -> Result<(), ProviderError> {
        for chunk in objects.chunks(1000) {
            // A large multi-chunk delete can outrun the STS credential lifetime;
            // re-check freshness per chunk (no-op when not near expiry / no role).
            self.ensure_fresh_credentials().await?;

            let xml_bytes = build_batch_delete_xml(chunk);

            // S3 batch delete requires Content-MD5
            let md5_digest = {
                use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
                use md5::{Digest, Md5};
                let mut hasher = Md5::new();
                hasher.update(&xml_bytes);
                BASE64.encode(hasher.finalize())
            };

            // Build signed request manually (need custom Content-MD5 header)
            let url = format!("{}?delete", self.build_url(""));
            let payload_hash = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&xml_bytes);
                hex::encode(hasher.finalize())
            };

            let mut headers = HashMap::new();
            headers.insert("content-md5".to_string(), md5_digest);
            let authorization = self.sign_request("POST", &url, &mut headers, &payload_hash)?;

            let mut request = self.client.post(&url);
            for (k, v) in headers.iter() {
                request = request.header(k, v);
            }
            request = request.header("Authorization", &authorization);
            request = request.header("Content-Length", xml_bytes.len().to_string());
            request = request.body(xml_bytes);

            let response = request
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !response.status().is_success() {
                // Fall back to sequential delete if batch fails
                tracing::warn!(
                    "S3 batch delete failed ({}), falling back to sequential",
                    response.status()
                );
                for (key, version_id) in chunk {
                    let params = version_id.as_ref().map(|v| vec![("versionId", v.as_str())]);
                    let _ = self
                        .s3_request(Method::DELETE, key, params.as_deref(), None)
                        .await;
                }
            }
        }

        Ok(())
    }

    /// Change the storage class of an existing object via server-side copy.
    /// Uses CopyObject with x-amz-storage-class to change class in-place.
    pub async fn change_storage_class(
        &self,
        path: &str,
        storage_class: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;
        let key = path.trim_start_matches('/');
        // Slashes preserved via encode_s3_key_path; see server_copy /
        // restore_version for the same rationale.
        let copy_source = format!("/{}/{}", self.config.bucket, encode_s3_key_path(key));
        let url = self.build_url(key);

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("x-amz-copy-source".to_string(), copy_source);
        headers.insert("x-amz-metadata-directive".to_string(), "COPY".to_string());
        headers.insert("x-amz-storage-class".to_string(), storage_class.to_string());
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                info!("Changed storage class of '{}' to '{}'", key, storage_class);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Change storage class failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    /// Initiate a Glacier or Deep Archive restore.
    /// `days` = number of days the restored copy remains accessible.
    /// `tier` = "Expedited" | "Standard" | "Bulk"
    pub async fn glacier_restore(
        &self,
        path: &str,
        days: u32,
        tier: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;
        let key = path.trim_start_matches('/');
        let body = format!(
            "<RestoreRequest><Days>{}</Days><GlacierJobParameters><Tier>{}</Tier></GlacierJobParameters></RestoreRequest>",
            days, tier
        );

        let url = {
            let base = self.build_url(key);
            format!("{}?restore=", base)
        };

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(body.as_bytes());
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/xml".to_string());
        let authorization = self.sign_request("POST", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.post(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", body.len().to_string());
        request = request.body(body);

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::ACCEPTED => {
                info!(
                    "Glacier restore initiated for '{}' ({} days, tier={})",
                    key, days, tier
                );
                Ok(())
            }
            StatusCode::CONFLICT => {
                // 409 = restore already in progress
                Err(ProviderError::Other(
                    "Restore already in progress for this object".to_string(),
                ))
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Glacier restore failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    /// Get all tags for an S3 object. Returns key-value pairs (max 10 per AWS).
    pub async fn get_object_tags(
        &self,
        path: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if self.is_mega_s4_endpoint() {
            return Err(ProviderError::NotSupported(
                "MEGA S4 does not support object tagging".to_string(),
            ));
        }
        let key = path.trim_start_matches('/');
        let response = self
            .s3_request(Method::GET, key, Some(&[("tagging", "")]), None)
            .await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "GetObjectTagging failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Parse <Tagging><TagSet><Tag><Key>k</Key><Value>v</Value></Tag>...</TagSet></Tagging>
        let mut tags = HashMap::new();
        let mut reader = Reader::from_str(&body);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut current_key: Option<String> = None;
        let mut current_value: Option<String> = None;
        let mut in_key = false;
        let mut in_value = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => match e.name().as_ref() {
                    b"Key" => in_key = true,
                    b"Value" => in_value = true,
                    _ => {}
                },
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                    if in_key {
                        current_key = Some(text.clone());
                    }
                    if in_value {
                        current_value = Some(text);
                    }
                }
                Ok(Event::End(ref e)) => match e.name().as_ref() {
                    b"Key" => in_key = false,
                    b"Value" => in_value = false,
                    b"Tag" => {
                        if let (Some(k), Some(v)) = (current_key.take(), current_value.take()) {
                            tags.insert(k, v);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(ProviderError::ParseError(format!("XML parse error: {}", e))),
                _ => {}
            }
            buf.clear();
        }

        Ok(tags)
    }

    /// Set tags on an S3 object. Max 10 tags per AWS limits.
    pub async fn set_object_tags(
        &self,
        path: &str,
        tags: &HashMap<String, String>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.ensure_fresh_credentials().await?;
        if self.is_mega_s4_endpoint() {
            return Err(ProviderError::NotSupported(
                "MEGA S4 does not support object tagging".to_string(),
            ));
        }
        let key = path.trim_start_matches('/');

        let tag_elements: String = tags
            .iter()
            .map(|(k, v)| {
                format!(
                    "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                    quick_xml::escape::escape(k),
                    quick_xml::escape::escape(v)
                )
            })
            .collect();
        let body = format!("<Tagging><TagSet>{}</TagSet></Tagging>", tag_elements);

        let url = {
            let base = self.build_url(key);
            format!("{}?tagging=", base)
        };

        use sha2::{Digest, Sha256};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(body.as_bytes());
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/xml".to_string());
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", body.len().to_string());
        request = request.body(body);

        let response = request
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                info!("Set {} tags on '{}'", tags.len(), key);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "PutObjectTagging failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }

    /// Delete all tags from an S3 object.
    pub async fn delete_object_tags(&self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if self.is_mega_s4_endpoint() {
            return Err(ProviderError::NotSupported(
                "MEGA S4 does not support object tagging".to_string(),
            ));
        }
        let key = path.trim_start_matches('/');
        let response = self
            .s3_request(Method::DELETE, key, Some(&[("tagging", "")]), None)
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "DeleteObjectTagging failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )))
            }
        }
    }
}

// ── S3 fast-list (recursive listing without delimiter) ────────────────────

/// Hard cap on entries materialized by `list_recursive` so a bucket with
/// millions of objects (or a hostile endpoint) cannot grow `all_entries`
/// without bound. Matches the project-wide scan entry cap; consumers treat
/// a capped result as a lower bound (truncated).
const S3_LIST_RECURSIVE_MAX_ENTRIES: usize = 500_000;

impl S3Provider {
    /// List all objects recursively under a prefix in a single API call sequence.
    /// Uses ListObjectsV2 WITHOUT Delimiter, returning a flat list of all files.
    /// Much faster than BFS directory-by-directory listing for large datasets
    /// (reduces API calls from O(dirs) to O(files/1000)).
    pub async fn list_recursive(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = if path.is_empty() || path == "/" || path == "." {
            self.current_prefix.clone()
        } else {
            path.trim_matches('/').to_string()
        };

        let prefix_with_slash = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };

        let mut all_entries = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            // NO delimiter → recursive flat listing
            let mut params: Vec<(&str, &str)> = vec![("list-type", "2"), ("max-keys", "1000")];

            if !prefix_with_slash.is_empty() {
                params.push(("prefix", &prefix_with_slash));
            }

            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }

            let response = self
                .s3_request(Method::GET, "", Some(&params), None)
                .await?;

            match response.status() {
                StatusCode::OK => {
                    let xml = response
                        .text()
                        .await
                        .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                    if let Some(error) = Self::bucket_addressing_error(&xml) {
                        return Err(error);
                    }

                    let (entries, next_token) = self.parse_list_response(&xml)?;
                    all_entries.extend(entries);

                    // Bound memory on a huge or hostile bucket: stop
                    // paginating once the project-wide scan cap is reached.
                    // Consumers (used_scan / provider_scan_used) treat a
                    // capped result as a lower bound (truncated), so this
                    // never produces a silently-wrong larger figure.
                    if all_entries.len() >= S3_LIST_RECURSIVE_MAX_ENTRIES {
                        break;
                    }
                    if let Some(token) = next_token {
                        continuation_token = Some(token);
                    } else {
                        break;
                    }
                }
                status => {
                    let body = response.text().await.unwrap_or_default();
                    return Err(ProviderError::ServerError(format!(
                        "List recursive failed ({}): {}",
                        status,
                        sanitize_api_error(&body)
                    )));
                }
            }
        }

        Ok(all_entries)
    }
}

/// Download a single byte range and write it at the matching offset of an
/// already-pre-allocated temp file. Used as the per-task body of
/// `S3Provider::download_multi_thread`.
async fn download_range_to_offset(
    provider: S3Provider,
    key: String,
    temp_path: PathBuf,
    start: u64,
    end: u64,
    aggregate: Arc<AtomicU64>,
) -> Result<(), ProviderError> {
    let range_value = format!("bytes={}-{}", start, end);
    let response = provider
        .s3_request_ext(Method::GET, &key, None, None, &[("range", &range_value)])
        .await?;

    let status = response.status();
    match status {
        StatusCode::PARTIAL_CONTENT | StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Err(ProviderError::NotFound(key)),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            return Err(ProviderError::NotSupported(
                "Server rejected Range request mid-flight (file may have changed)".to_string(),
            ));
        }
        other => {
            return Err(ProviderError::TransferFailed(format!(
                "Multi-thread range download failed with status: {}",
                other
            )));
        }
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temp_path)
        .await
        .map_err(ProviderError::IoError)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ProviderError::IoError)?;

    let expected = end - start + 1;
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        let chunk_len = chunk.len() as u64;
        if written + chunk_len > expected {
            // Server returned more than requested: truncate to the planned
            // window so we don't trample a neighboring range.
            let allowed = (expected - written) as usize;
            file.write_all(&chunk[..allowed])
                .await
                .map_err(ProviderError::IoError)?;
            aggregate.fetch_add(allowed as u64, Ordering::Relaxed);
            written = expected;
            break;
        }
        file.write_all(&chunk)
            .await
            .map_err(ProviderError::IoError)?;
        aggregate.fetch_add(chunk_len, Ordering::Relaxed);
        written += chunk_len;
    }

    if written != expected {
        return Err(ProviderError::TransferFailed(format!(
            "Multi-thread range download truncated: expected {} bytes, got {}",
            expected, written
        )));
    }

    file.flush().await.map_err(ProviderError::IoError)?;
    file.sync_all().await.map_err(ProviderError::IoError)?;
    Ok(())
}

/// Build the `<Delete>` request body for one chunk of a multi-object delete.
///
/// Each object may carry an optional version id: `Some(id)` targets that
/// specific version or delete marker (version-aware purge / empty-trash),
/// `None` targets the current version (plain recursive delete).
fn build_batch_delete_xml(chunk: &[(String, Option<String>)]) -> Vec<u8> {
    let mut xml = String::from("<Delete><Quiet>true</Quiet>");
    for (key, version_id) in chunk {
        xml.push_str("<Object><Key>");
        xml.push_str(&quick_xml::escape::escape(key));
        xml.push_str("</Key>");
        if let Some(vid) = version_id {
            xml.push_str("<VersionId>");
            xml.push_str(&quick_xml::escape::escape(vid));
            xml.push_str("</VersionId>");
        }
        xml.push_str("</Object>");
    }
    xml.push_str("</Delete>");
    xml.into_bytes()
}

/// One parsed page of a ListObjectVersions response: the entries plus the
/// pagination markers `(entries, is_truncated, next_key_marker, next_version_id_marker)`.
type VersionsPage = (Vec<TrashEntry>, bool, Option<String>, Option<String>);

/// Parse one page of a ListVersionsResult document into `TrashEntry` rows.
///
/// Captures both `<Version>` and `<DeleteMarker>` elements across every key (no
/// exact-key filter), so it powers the prefix-wide trash browse. Delete markers
/// report `size` 0. Returns the page entries plus the pagination markers.
fn parse_object_versions_page(xml_str: &str) -> Result<VersionsPage, ProviderError> {
    #[derive(PartialEq)]
    enum Elem {
        None,
        Version,
        DeleteMarker,
    }

    let mut reader = Reader::from_str(xml_str);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut entries: Vec<TrashEntry> = Vec::new();
    let mut elem = Elem::None;
    let mut current_tag = String::new();

    // Fields for the current <Version> / <DeleteMarker> element
    let mut e_key: Option<String> = None;
    let mut e_version_id: Option<String> = None;
    let mut e_is_latest: Option<String> = None;
    let mut e_last_modified: Option<String> = None;
    let mut e_size: Option<String> = None;

    // Pagination fields
    let mut is_truncated = false;
    let mut next_key_marker: Option<String> = None;
    let mut next_version_id_marker: Option<String> = None;
    let mut in_is_truncated = false;
    let mut in_next_key_marker = false;
    let mut in_next_version_id_marker = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match tag_name.as_str() {
                    "Version" | "DeleteMarker" => {
                        elem = if tag_name == "Version" {
                            Elem::Version
                        } else {
                            Elem::DeleteMarker
                        };
                        e_key = None;
                        e_version_id = None;
                        e_is_latest = None;
                        e_last_modified = None;
                        e_size = None;
                    }
                    "IsTruncated" => in_is_truncated = true,
                    "NextKeyMarker" => in_next_key_marker = true,
                    "NextVersionIdMarker" => in_next_version_id_marker = true,
                    _ => {
                        current_tag = tag_name;
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                if text.is_empty() {
                    buf.clear();
                    continue;
                }

                if in_is_truncated {
                    is_truncated = text == "true";
                }
                if in_next_key_marker {
                    next_key_marker = Some(text.clone());
                }
                if in_next_version_id_marker {
                    next_version_id_marker = Some(text.clone());
                }

                if elem != Elem::None {
                    match current_tag.as_str() {
                        "Key" => e_key = Some(text),
                        "VersionId" => e_version_id = Some(text),
                        "IsLatest" => e_is_latest = Some(text),
                        "LastModified" => e_last_modified = Some(text),
                        "Size" => e_size = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match tag_name.as_str() {
                    "Version" | "DeleteMarker" => {
                        let is_delete_marker = tag_name == "DeleteMarker";
                        if let Some(key) = e_key.take() {
                            let size = if is_delete_marker {
                                0
                            } else {
                                e_size.as_ref().and_then(|s| s.parse().ok()).unwrap_or(0)
                            };
                            entries.push(TrashEntry {
                                key,
                                version_id: e_version_id.clone().unwrap_or_default(),
                                is_delete_marker,
                                is_latest: e_is_latest.as_deref() == Some("true"),
                                size,
                                last_modified: e_last_modified.clone(),
                            });
                        }
                        elem = Elem::None;
                    }
                    "IsTruncated" => in_is_truncated = false,
                    "NextKeyMarker" => in_next_key_marker = false,
                    "NextVersionIdMarker" => in_next_version_id_marker = false,
                    _ => {}
                }
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok((
        entries,
        is_truncated,
        next_key_marker,
        next_version_id_marker,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url_path_style() {
        let provider = S3Provider::new(S3Config {
            endpoint: Some("http://localhost:9000".to_string()),
            region: "us-east-1".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_access_key: secrecy::SecretString::from("minioadmin".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("Failed to create S3Provider");

        assert_eq!(
            provider.build_url("path/to/file.txt"),
            "http://localhost:9000/test-bucket/path/to/file.txt"
        );
    }

    #[test]
    fn test_build_url_virtual_hosted() {
        let provider = S3Provider::new(S3Config {
            endpoint: None,
            region: "us-west-2".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "my-bucket".to_string(),
            prefix: None,
            path_style: false,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("Failed to create S3Provider");

        assert_eq!(
            provider.build_url("path/to/file.txt"),
            "https://my-bucket.s3.us-west-2.amazonaws.com/path/to/file.txt"
        );
    }

    /// Issue #128: spaces and emojis in S3 keys must be percent-encoded in
    /// the wire URL. Without this, Filen's local S3 bridge returned 401
    /// because the SigV4 signature didn't match the wire path that the
    /// server reconstructed from the (lenient) request line.
    #[test]
    fn encode_s3_key_path_handles_spaces_and_unicode() {
        // Plain key passes through.
        assert_eq!(encode_s3_key_path("hello.txt"), "hello.txt");
        // Path separators preserved, segments encoded individually.
        assert_eq!(encode_s3_key_path("a/b/c.txt"), "a/b/c.txt");
        // Spaces percent-encoded as %20 (NOT '+').
        assert_eq!(encode_s3_key_path("my folder"), "my%20folder");
        assert_eq!(
            encode_s3_key_path("my folder/file.txt"),
            "my%20folder/file.txt"
        );
        // Trailing slash preserved (folder-marker keys).
        assert_eq!(encode_s3_key_path("my folder/"), "my%20folder/");
        // Emoji encoded as UTF-8 percent triplets.
        assert_eq!(encode_s3_key_path("party"), "party");
        assert_eq!(encode_s3_key_path("🎉/notes.md"), "%F0%9F%8E%89/notes.md");
        // Special chars (RFC 3986 reserved) encoded.
        assert_eq!(encode_s3_key_path("a+b"), "a%2Bb");
        assert_eq!(encode_s3_key_path("a&b=c"), "a%26b%3Dc");
        // Empty stays empty.
        assert_eq!(encode_s3_key_path(""), "");
    }

    /// #368: on Filen's S3 bridge a listed <Key> comes back already percent-
    /// encoded, so it must be decoded once in list_keys_with_prefix. Otherwise
    /// the single downstream encode_s3_key_path() double-encodes an emoji child
    /// ("%F0..." -> "%25F0..."), and the x-amz-copy-source no longer matches the
    /// SigV4 signature ("401 The signature does not match").
    #[test]
    fn filen_decode_listed_key_prevents_double_encode() {
        // Filen returns the child key already encoded.
        let listed = "folder/%F0%9F%9A%80file.txt".to_string();
        // On a Filen endpoint we decode to the logical key...
        let logical = filen_decode_listed_key(listed.clone(), true);
        assert_eq!(logical, "folder/🚀file.txt");
        // ...so re-encoding for x-amz-copy-source is single-encoded (no %25).
        let reencoded = encode_s3_key_path(&logical);
        assert_eq!(reencoded, "folder/%F0%9F%9A%80file.txt");
        assert!(!reencoded.contains("%25"), "must not double-encode the key");

        // On AWS/MinIO/Wasabi keys come back verbatim: leave them untouched so a
        // real object whose name literally contains "%F0" is not corrupted.
        assert_eq!(
            filen_decode_listed_key(listed, false),
            "folder/%F0%9F%9A%80file.txt"
        );
        // A plain ASCII key is unaffected either way.
        assert_eq!(
            filen_decode_listed_key("a/b.txt".to_string(), true),
            "a/b.txt"
        );
    }

    #[test]
    fn test_build_url_custom_virtual_hosted_endpoint() {
        let provider = S3Provider::new(S3Config {
            endpoint: Some("http://s3.garage.localhost:3900".to_string()),
            region: "garage".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test".to_string(),
            prefix: None,
            path_style: false,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("Failed to create S3Provider");

        assert_eq!(
            provider.build_url("folder-blue.svg"),
            "http://test.s3.garage.localhost:3900/folder-blue.svg"
        );
    }

    #[test]
    fn test_bucket_listing_response_is_addressing_error() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><ListAllMyBucketsResult><Buckets><Bucket><Name>test</Name></Bucket></Buckets></ListAllMyBucketsResult>"#;

        assert!(matches!(
            S3Provider::bucket_addressing_error(xml),
            Some(ProviderError::InvalidConfig(_))
        ));
    }

    // U-13 multi-thread download: range planner tests moved to
    // `providers::multi_thread` (PD-HTTP-1). The S3-specific setter test
    // below stays here because it exercises `S3Provider` state.

    #[test]
    fn test_set_multi_thread_download_clamps_streams_and_floors_cutoff() {
        let mut provider = S3Provider::new(S3Config {
            endpoint: Some("http://localhost:9000".to_string()),
            region: "us-east-1".to_string(),
            access_key_id: "x".to_string(),
            secret_access_key: secrecy::SecretString::from("y".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "b".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("provider");

        // Above cap → clamped down
        provider.set_multi_thread_download(999, 0);
        assert_eq!(
            provider.multi_thread_streams,
            S3Provider::MULTI_THREAD_MAX_STREAMS
        );
        // Cutoff floored at 1 MiB
        assert_eq!(provider.multi_thread_cutoff, 1024 * 1024);

        // Below floor → clamped up to 1 (disabled)
        provider.set_multi_thread_download(0, 50 * 1024 * 1024);
        assert_eq!(provider.multi_thread_streams, 1);
        assert_eq!(provider.multi_thread_cutoff, 50 * 1024 * 1024);

        // Mid-range value passes through
        provider.set_multi_thread_download(4, 250 * 1024 * 1024);
        assert_eq!(provider.multi_thread_streams, 4);
        assert_eq!(provider.multi_thread_cutoff, 250 * 1024 * 1024);
    }

    fn make_provider(endpoint: Option<&str>) -> S3Provider {
        S3Provider::new(S3Config {
            endpoint: endpoint.map(String::from),
            region: "us-east-1".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("Failed to create S3Provider")
    }

    fn make_provider_with_token(session_token: Option<&str>) -> S3Provider {
        S3Provider::new(S3Config {
            endpoint: None,
            region: "us-east-1".to_string(),
            access_key_id: "AKIAEXAMPLE".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: session_token.map(|t| secrecy::SecretString::from(t.to_string())),
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("Failed to create S3Provider")
    }

    /// Issue #301 (Fase 1): temporary credentials carry the STS session token
    /// in `x-amz-security-token`, and it MUST be part of the SigV4 signed
    /// headers so the request is accepted by AWS. Verify both the emitted
    /// header and its presence in the `SignedHeaders` list of the
    /// Authorization value.
    #[test]
    fn sign_request_includes_session_token_when_present() {
        let provider = make_provider_with_token(Some("FwoGZXIvYXdzEXAMPLEtoken=="));
        let mut headers = std::collections::HashMap::new();
        let auth = provider
            .sign_request(
                "GET",
                "https://test-bucket.s3.amazonaws.com/object.txt",
                &mut headers,
                "UNSIGNED-PAYLOAD",
            )
            .expect("sign_request failed");

        assert_eq!(
            headers.get("x-amz-security-token").map(String::as_str),
            Some("FwoGZXIvYXdzEXAMPLEtoken=="),
            "session token must be emitted as x-amz-security-token"
        );
        assert!(
            auth.contains("x-amz-security-token"),
            "x-amz-security-token must appear in SignedHeaders; auth was: {auth}"
        );
    }

    /// Long-term IAM credentials (no session token) must NOT emit the header,
    /// keeping the canonical request and signature identical to legacy behavior.
    #[test]
    fn sign_request_omits_session_token_when_absent() {
        let provider = make_provider_with_token(None);
        let mut headers = std::collections::HashMap::new();
        let auth = provider
            .sign_request(
                "GET",
                "https://test-bucket.s3.amazonaws.com/object.txt",
                &mut headers,
                "UNSIGNED-PAYLOAD",
            )
            .expect("sign_request failed");

        assert!(!headers.contains_key("x-amz-security-token"));
        assert!(!auth.contains("x-amz-security-token"));
    }

    fn temp_creds(expiration: Option<DateTime<Utc>>) -> sts::TempCredentials {
        sts::TempCredentials {
            access_key_id: "ASIATEMP".to_string(),
            secret_access_key: secrecy::SecretString::from("tempsecret".to_string()),
            session_token: secrecy::SecretString::from("tok".to_string()),
            expiration,
        }
    }

    #[test]
    fn effective_credentials_prefers_temp_when_role_assumed() {
        let provider = make_provider(Some("http://localhost:9000"));
        // No role assumed yet: the signer uses the long-term base credentials.
        let base = provider.effective_credentials();
        assert_eq!(base.access_key_id, "key");
        assert_eq!(base.secret_access_key.expose_secret(), "secret");
        assert!(base.session_token.is_none());

        // After AssumeRole the temporary credentials take over.
        *provider.temp_credentials.write().unwrap() =
            Some(temp_creds(Some(Utc::now() + chrono::Duration::hours(1))));
        let temp = provider.effective_credentials();
        assert_eq!(temp.access_key_id, "ASIATEMP");
        assert_eq!(temp.secret_access_key.expose_secret(), "tempsecret");
        assert_eq!(
            temp.session_token.as_ref().map(|s| s.expose_secret()),
            Some("tok")
        );
    }

    #[test]
    fn temp_credentials_refresh_honors_threshold() {
        let provider = make_provider(Some("http://localhost:9000"));
        // First call: nothing assumed yet, so a refresh (the initial acquire)
        // is required.
        assert!(provider.temp_credentials_need_refresh());

        // Comfortably valid: no refresh.
        *provider.temp_credentials.write().unwrap() =
            Some(temp_creds(Some(Utc::now() + chrono::Duration::minutes(30))));
        assert!(!provider.temp_credentials_need_refresh());

        // Inside the 5-minute window: refresh.
        *provider.temp_credentials.write().unwrap() =
            Some(temp_creds(Some(Utc::now() + chrono::Duration::minutes(2))));
        assert!(provider.temp_credentials_need_refresh());

        // Already expired: refresh.
        *provider.temp_credentials.write().unwrap() =
            Some(temp_creds(Some(Utc::now() - chrono::Duration::minutes(1))));
        assert!(provider.temp_credentials_need_refresh());

        // No expiry returned (theoretical): treated as fresh, a hard 403 would
        // force a reconnect instead.
        *provider.temp_credentials.write().unwrap() = Some(temp_creds(None));
        assert!(!provider.temp_credentials_need_refresh());
    }

    #[test]
    fn ensure_fresh_credentials_is_noop_without_role() {
        // make_provider has no role_arn: ensure_fresh_credentials must not touch
        // the empty temp-credential cell (no network, no panic).
        let provider = make_provider(Some("http://localhost:9000"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(provider.ensure_fresh_credentials())
            .expect("no-op refresh without a role must succeed");
        assert!(provider.temp_credentials.read().unwrap().is_none());
    }

    #[test]
    fn mfa_refresh_after_initial_requires_reconnect() {
        // A role that requires MFA: the first AssumeRole consumed the one-time
        // code, so a later refresh cannot silently re-assume. It must fail with
        // an explicit reconnect error WITHOUT touching the network.
        let mut provider = make_provider(Some("https://s3.us-east-1.amazonaws.com"));
        provider.config.role_arn = Some("arn:aws:iam::123456789012:role/Demo".to_string());
        provider.config.role_mfa_serial = Some("arn:aws:iam::123456789012:mfa/user".to_string());
        // Simulate an already-acquired MFA session that has now lapsed.
        *provider.temp_credentials.write().unwrap() =
            Some(temp_creds(Some(Utc::now() - chrono::Duration::minutes(1))));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt
            .block_on(provider.ensure_fresh_credentials())
            .expect_err("expired MFA session must require reconnect, not re-assume");
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
    }

    #[test]
    fn mfa_initial_without_token_code_fails_fast() {
        // MFA serial configured but no one-time code provided on the initial
        // acquire: fail fast before any STS call.
        let mut provider = make_provider(Some("https://s3.us-east-1.amazonaws.com"));
        provider.config.role_arn = Some("arn:aws:iam::123456789012:role/Demo".to_string());
        provider.config.role_mfa_serial = Some("arn:aws:iam::123456789012:mfa/user".to_string());
        // No temp creds yet (initial acquire), no token code.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt
            .block_on(provider.ensure_fresh_credentials())
            .expect_err("missing MFA code must fail fast, before any STS call");
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
    }

    /// SG-T04 gate: the trait-level `begin_multipart_upload` is a different
    /// method from the inherent `*_internal` pipeline that `upload()` uses
    /// for multipart streaming. Both must coexist without shadowing.
    /// When the provider is disconnected, the trait method fails fast on
    /// the connection check before reaching the network, matching how
    /// `upload()` guards itself today.
    #[tokio::test]
    async fn s3_multipart_via_trait_matches_internal_upload() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        let result = StorageProvider::begin_multipart_upload(
            &mut provider,
            "/some/key.bin",
            10 * 1024 * 1024,
            Some("application/octet-stream"),
            None,
        )
        .await;
        assert!(matches!(result, Err(ProviderError::NotConnected)));
    }

    /// SG-T04 gate: the trait-level multipart entry point must short-circuit
    /// to NotSupported on Filen Desktop's local S3 bridge so the shaped-graph
    /// runner falls back to a single-PUT shape instead of burning a round
    /// trip on the 501 from CreateMultipartUpload.
    #[tokio::test]
    async fn s3_multipart_trait_rejects_filen_s3_bridge() {
        let mut provider = make_provider(Some("https://local.s3.filen.io"));
        provider.connected = true;
        let result = StorageProvider::begin_multipart_upload(
            &mut provider,
            "/some/key.bin",
            10 * 1024 * 1024,
            Some("application/octet-stream"),
            None,
        )
        .await;
        match result {
            Err(ProviderError::NotSupported(msg)) => {
                assert!(msg.contains("filen-s3"), "msg was {msg}");
            }
            other => panic!("expected NotSupported, got {other:?}"),
        }
    }

    /// The DAG shaped-file path fans uploads into UploadPart nodes whenever
    /// `supports_multipart` is advertised (it ignores MULTIPART_THRESHOLD).
    /// Filen Desktop's bridge rejects CreateMultipartUpload, so it must
    /// advertise single-shot only or every >chunk-size upload fails. A
    /// normal S3 endpoint keeps multipart on.
    #[test]
    fn s3_filen_bridge_disables_multipart_hint() {
        let filen = make_provider(Some("https://local.s3.filen.io"));
        let hints = filen.transfer_optimization_hints();
        assert!(
            !hints.supports_multipart,
            "filen-s3 must advertise single-shot only"
        );
        assert_eq!(hints.multipart_threshold, u64::MAX);

        let normal = make_provider(Some("http://localhost:9000"));
        let hints = normal.transfer_optimization_hints();
        assert!(
            hints.supports_multipart,
            "regular S3 must keep multipart enabled"
        );
    }

    /// Connect must fail fast with a clear error when the profile has no
    /// endpoint and a non-AWS placeholder region, instead of silently dialing
    /// `s3.auto.amazonaws.com` (the old behaviour produced an opaque network
    /// error on Backblaze profiles saved without the endpoint field).
    #[tokio::test]
    async fn s3_connect_rejects_missing_endpoint_with_auto_region() {
        let mut provider = S3Provider::new(S3Config {
            endpoint: None,
            region: "auto".to_string(),
            access_key_id: "k".to_string(),
            secret_access_key: secrecy::SecretString::from("s".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "b".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("create provider");
        let result = StorageProvider::connect(&mut provider).await;
        assert!(
            matches!(result, Err(ProviderError::InvalidConfig(_))),
            "expected InvalidConfig, got {result:?}"
        );
    }

    /// SG-T05 gate: S3 advertises the server_side_copy capability under
    /// both the legacy `supports_server_copy` and the new
    /// `supports_server_side_copy` slot so the DAG capability builder
    /// picks it up regardless of which name the wiring uses.
    #[test]
    fn s3_advertises_server_side_copy_capability() {
        let provider = make_provider(Some("http://localhost:9000"));
        assert!(provider.supports_server_copy());
        assert!(provider.supports_server_side_copy());
    }

    /// SG-T05 gate: the real x-amz-copy-source implementation lives on
    /// `server_side_copy`; `server_copy` is a thin delegate. Both paths
    /// must guard on the connection state before reaching the network.
    #[tokio::test]
    async fn s3_server_side_copy_within_bucket_requires_connection() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        let direct =
            StorageProvider::server_side_copy(&mut provider, "/src/key.bin", "/dst/key.bin").await;
        assert!(matches!(direct, Err(ProviderError::NotConnected)));

        let via_legacy = provider.server_copy("/src/key.bin", "/dst/key.bin").await;
        assert!(matches!(via_legacy, Err(ProviderError::NotConnected)));
    }

    /// Issue #196: Filen Desktop S3 returns `<Key>` / `<Prefix>` percent-encoded.
    /// Verify `parse_list_response` decodes them so names appear correctly in
    /// `ls`/`tree` and downstream `build_url` re-encodes once (no double encoding).
    #[test]
    fn parse_list_response_decodes_filen_keys_and_prefixes() {
        let provider = S3Provider::new(S3Config {
            endpoint: Some("https://local.s3.filen.io".to_string()),
            region: "filen".to_string(),
            access_key_id: "k".to_string(),
            secret_access_key: secrecy::SecretString::from("s".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "b".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: false,
        })
        .expect("provider");
        assert!(provider.is_filen_s3_endpoint());

        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>b</Name>
  <Prefix></Prefix>
  <Delimiter>/</Delimiter>
  <CommonPrefixes><Prefix>my%20folder/</Prefix></CommonPrefixes>
  <Contents>
    <Key>foto%20vacanze.jpg</Key>
    <Size>1024</Size>
    <LastModified>2026-05-24T10:00:00.000Z</LastModified>
    <ETag>"abc"</ETag>
  </Contents>
</ListBucketResult>"#;

        let (entries, _) = provider.parse_list_response(xml).expect("parse");
        let dir = entries.iter().find(|e| e.is_dir).expect("dir entry");
        assert_eq!(dir.name, "my folder", "Filen dir name must be decoded");
        assert_eq!(dir.path, "/my folder");

        let file = entries.iter().find(|e| !e.is_dir).expect("file entry");
        assert_eq!(file.name, "foto vacanze.jpg");
        assert_eq!(file.path, "/foto vacanze.jpg");
        assert_eq!(file.size, 1024);
    }

    /// T-DEBT-08: `plan_copy_parts` is the pure half of server-side
    /// multipart copy. Validates the part-planning math without touching
    /// any HTTP path: the actual UploadPartCopy sequencing is verified
    /// in the owner-side MinIO smoke documented in the task plan.
    #[test]
    fn plan_copy_parts_returns_empty_for_zero_size_or_part_size() {
        assert!(plan_copy_parts(0, 100).is_empty());
        assert!(plan_copy_parts(1024, 0).is_empty());
    }

    #[test]
    fn plan_copy_parts_single_aligned_full_part() {
        let parts = plan_copy_parts(100, 100);
        assert_eq!(parts, vec![(1, 0, 99)]);
    }

    #[test]
    fn plan_copy_parts_unaligned_tail_under_part_size() {
        // 250 bytes at part size 100 → parts [0-99], [100-199], [200-249]
        let parts = plan_copy_parts(250, 100);
        assert_eq!(parts, vec![(1, 0, 99), (2, 100, 199), (3, 200, 249)]);
    }

    #[test]
    fn plan_copy_parts_six_gib_at_hundred_mib_yields_sixty_one_parts() {
        // 6 GiB at 100 MiB parts: 6144 MiB / 100 MiB = 61.44 → ceil = 62.
        // Last part covers 44 MiB and 1-indexed numbering caps at 62.
        let six_gib: u64 = 6 * 1024 * 1024 * 1024;
        let hundred_mib: u64 = 100 * 1024 * 1024;
        let parts = plan_copy_parts(six_gib, hundred_mib);
        assert_eq!(parts.len(), 62);
        // Part numbers are contiguous starting at 1.
        for (idx, (pn, _, _)) in parts.iter().enumerate() {
            assert_eq!(*pn as usize, idx + 1);
        }
        // Ranges fully cover the source with no gap and no overlap.
        let mut expected_start = 0u64;
        for (_, start, end_inclusive) in &parts {
            assert_eq!(*start, expected_start);
            assert!(end_inclusive >= start);
            expected_start = end_inclusive + 1;
        }
        assert_eq!(expected_start, six_gib);
        // First 61 parts are full-size; the last carries the remainder.
        let last = parts.last().unwrap();
        let last_size = last.2 - last.1 + 1;
        let expected_last = six_gib - hundred_mib * 61;
        assert_eq!(last_size, expected_last);
    }

    #[test]
    fn plan_copy_parts_at_or_below_copy_object_max_still_plans() {
        // The pure planner doesn't know about the 5 GiB CopyObject cap;
        // the caller (`server_side_copy`) is responsible for routing
        // small sources to the single-PUT path. Verify the planner still
        // emits the obvious one-part plan when called below the cap.
        let four_gib: u64 = 4 * 1024 * 1024 * 1024;
        let huge_part: u64 = 8 * 1024 * 1024 * 1024;
        let parts = plan_copy_parts(four_gib, huge_part);
        assert_eq!(parts, vec![(1, 0, four_gib - 1)]);
    }

    /// AWS-standard S3 returns `<Key>` verbatim. A literal `%20` inside a key
    /// name must survive unchanged on non-Filen endpoints (no false decode).
    #[test]
    fn parse_list_response_keeps_literal_percent_on_aws() {
        let provider = S3Provider::new(S3Config {
            endpoint: None,
            region: "us-east-1".to_string(),
            access_key_id: "k".to_string(),
            secret_access_key: secrecy::SecretString::from("s".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "b".to_string(),
            prefix: None,
            path_style: false,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("provider");
        assert!(!provider.is_filen_s3_endpoint());

        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Contents>
    <Key>report%20final.pdf</Key>
    <Size>10</Size>
    <ETag>"x"</ETag>
  </Contents>
</ListBucketResult>"#;
        let (entries, _) = provider.parse_list_response(xml).expect("parse");
        let file = entries.iter().find(|e| !e.is_dir).expect("file entry");
        assert_eq!(file.name, "report%20final.pdf");
        assert_eq!(file.path, "/report%20final.pdf");
    }

    // ---------------------------------------------------------------------
    // KE-E1: S3 Retry-After detection (Sprint K1)
    // ---------------------------------------------------------------------

    #[test]
    fn s3_is_rate_limited_recognises_429() {
        assert!(s3_is_rate_limited(429, ""));
        assert!(s3_is_rate_limited(429, "anything"));
    }

    #[test]
    fn s3_is_rate_limited_recognises_503_slow_down() {
        let body = r#"<?xml version="1.0"?><Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>"#;
        assert!(s3_is_rate_limited(503, body));
    }

    #[test]
    fn s3_is_rate_limited_rejects_503_service_unavailable() {
        // Generic 503 is a transient retried by send_with_retry, but NOT a
        // throttle signal: AIMD should not get a Retry-After hint from it.
        let body =
            r#"<Error><Code>ServiceUnavailable</Code><Message>backend down</Message></Error>"#;
        assert!(!s3_is_rate_limited(503, body));
        assert!(!s3_is_rate_limited(503, ""));
    }

    #[test]
    fn s3_is_rate_limited_rejects_non_throttle_status() {
        assert!(!s3_is_rate_limited(500, "<Code>SlowDown</Code>"));
        assert!(!s3_is_rate_limited(404, ""));
        assert!(!s3_is_rate_limited(200, "<Code>SlowDown</Code>"));
        assert!(!s3_is_rate_limited(403, "<Code>SlowDown</Code>"));
    }

    #[test]
    fn s3_retry_marker_tail_emits_marker_on_429_with_header() {
        let tail = s3_retry_marker_tail(429, "", Some("12")).expect("rate-limited + hint");
        assert!(tail.contains("retry-after-secs=12"));
    }

    #[test]
    fn s3_retry_marker_tail_emits_marker_on_503_slow_down_with_header() {
        let body = r#"<Error><Code>SlowDown</Code></Error>"#;
        let tail = s3_retry_marker_tail(503, body, Some("30")).expect("SlowDown + hint");
        assert!(tail.contains("retry-after-secs=30"));
    }

    #[test]
    fn s3_retry_marker_tail_returns_none_without_header() {
        // 429 without Retry-After: marker absent → executor falls back to
        // default cooldown via parse_embedded_retry_after returning None.
        assert_eq!(s3_retry_marker_tail(429, "", None), None);
        assert_eq!(s3_retry_marker_tail(429, "", Some("")), None);
        assert_eq!(s3_retry_marker_tail(429, "", Some("not-a-number")), None);
    }

    #[test]
    fn s3_retry_marker_tail_returns_none_when_not_rate_limited() {
        // Generic 5xx / 4xx with Retry-After are NOT throttle signals: no marker.
        assert_eq!(s3_retry_marker_tail(500, "server error", Some("30")), None);
        assert_eq!(s3_retry_marker_tail(404, "missing", Some("30")), None);
        assert_eq!(
            s3_retry_marker_tail(503, r#"<Code>ServiceUnavailable</Code>"#, Some("30")),
            None
        );
    }

    #[test]
    fn format_s3_error_appends_marker_on_throttle() {
        let msg = format_s3_error(
            "UploadPart 3 failed",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "",
            Some("45"),
        );
        assert!(msg.contains("UploadPart 3 failed"));
        assert!(msg.contains("429"));
        assert!(msg.contains("retry-after-secs=45"));
    }

    #[test]
    fn format_s3_error_omits_marker_on_non_throttle() {
        let msg = format_s3_error(
            "UploadPart 3 failed",
            reqwest::StatusCode::NOT_FOUND,
            "key gone",
            Some("45"),
        );
        assert!(msg.contains("404"));
        assert!(!msg.contains("retry-after-secs"));
    }

    // ── KE-B1 per-backend S3 knob tests ────────────────────────────────

    /// KE-B1.1: default upload concurrency is the historical 4-in-flight ceiling
    /// and `set_upload_concurrency` overrides it, clamped to `[1, MAX]`.
    /// `0` is a sentinel that resets the override.
    #[test]
    fn set_upload_concurrency_clamps_and_resets() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        assert_eq!(
            provider.effective_upload_concurrency(),
            S3Provider::UPLOAD_CONCURRENCY_DEFAULT
        );

        provider.set_upload_concurrency(8);
        assert_eq!(provider.effective_upload_concurrency(), 8);

        // Clamp above ceiling
        provider.set_upload_concurrency(999);
        assert_eq!(
            provider.effective_upload_concurrency(),
            S3Provider::UPLOAD_CONCURRENCY_MAX
        );

        // `0` resets to default
        provider.set_upload_concurrency(0);
        assert_eq!(
            provider.effective_upload_concurrency(),
            S3Provider::UPLOAD_CONCURRENCY_DEFAULT
        );
    }

    /// KE-B1.2: setter toggles the `no_check_bucket` flag and `connect()`
    /// short-circuits when enabled. We can't exercise the full connect()
    /// path without a server, but the post-call state is observable.
    #[tokio::test]
    async fn no_check_bucket_short_circuits_connect_probe() {
        let mut provider = make_provider(Some("http://192.0.2.1:9000")); // RFC 5737 unreachable
        provider.set_no_check_bucket(true);
        assert!(provider.no_check_bucket);
        // With no_check_bucket=true, connect() must NOT make a network call
        // and must mark the provider as connected. The endpoint above is
        // guaranteed unreachable; if the probe still fired, this would
        // either hang or fail with ConnectionFailed.
        let res = provider.connect().await;
        assert!(res.is_ok(), "expected Ok(()), got {res:?}");
        assert!(provider.connected);
    }

    /// KE-B1.3: setter toggles the `disable_checksum` flag, and the
    /// payload-hash branch in `s3_request_ext` substitutes `UNSIGNED-PAYLOAD`
    /// for non-empty bodies. Empty bodies always use the hex SHA-256 of
    /// the empty string (constant, no CPU savings).
    #[test]
    fn set_disable_checksum_toggles_flag() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        assert!(!provider.disable_checksum);
        provider.set_disable_checksum(true);
        assert!(provider.disable_checksum);
        provider.set_disable_checksum(false);
        assert!(!provider.disable_checksum);
    }

    /// KE-B1.4: setter accepts arbitrary canned ACL strings (validation is
    /// permissive so vendor extensions pass through). Whitespace-only
    /// values are normalised to None.
    #[test]
    fn set_acl_stores_and_clears() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        assert!(provider.effective_acl().is_none());

        provider.set_acl(Some("public-read".to_string()));
        assert_eq!(provider.effective_acl(), Some("public-read"));

        // Vendor extension passes through
        provider.set_acl(Some("bucket-owner-full-control".to_string()));
        assert_eq!(provider.effective_acl(), Some("bucket-owner-full-control"));

        // Whitespace-only normalises to None
        provider.set_acl(Some("   ".to_string()));
        assert!(provider.effective_acl().is_none());

        // Explicit None clears the override
        provider.set_acl(Some("private".to_string()));
        provider.set_acl(None);
        assert!(provider.effective_acl().is_none());
    }

    /// KE-B1.4: ACL header lands in `append_upload_headers` only when the
    /// override is active and the endpoint is not MEGA S4 (which doesn't
    /// support ACLs).
    #[test]
    fn append_upload_headers_emits_acl_when_set() {
        let mut provider = make_provider(Some("http://localhost:9000"));
        let mut headers = HashMap::new();
        provider.append_upload_headers(&mut headers);
        assert!(!headers.contains_key("x-amz-acl"));

        provider.set_acl(Some("public-read".to_string()));
        let mut headers = HashMap::new();
        provider.append_upload_headers(&mut headers);
        assert_eq!(headers.get("x-amz-acl"), Some(&"public-read".to_string()));
    }

    /// KE-B1.4: MEGA S4 skips the entire enterprise-headers block, including
    /// the ACL override. Users wiring `--s3-acl` against MEGA must rely on
    /// MEGA's permission UI, not S3 ACLs.
    #[test]
    fn append_upload_headers_skips_acl_on_mega_s4() {
        let mut provider = make_provider(Some("https://eu-central-1.s4.mega.io"));
        // Need to actually set the bucket region to a valid MEGA S4 region;
        // the helper above uses us-east-1 which would fail S3Config::from_*
        // validation. We're constructing the provider directly so it
        // bypasses that path: the test still exercises the early-return.
        provider.set_acl(Some("public-read".to_string()));
        let mut headers = HashMap::new();
        provider.append_upload_headers(&mut headers);
        assert!(!headers.contains_key("x-amz-acl"));
    }

    /// KE-B1.5: setter accepts arbitrary storage class strings, and the
    /// override takes precedence over the profile-level setting. Clearing
    /// the override falls back to the profile-level value.
    #[test]
    fn set_storage_class_override_precedence_over_profile() {
        let mut provider = S3Provider::new(S3Config {
            endpoint: Some("http://localhost:9000".to_string()),
            region: "us-east-1".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
            storage_class: Some("STANDARD_IA".to_string()), // profile-level
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
        })
        .expect("provider");

        // Profile-level wins when no override
        assert_eq!(provider.effective_storage_class(), Some("STANDARD_IA"));

        // Override beats profile
        provider.set_storage_class_override(Some("GLACIER_IR".to_string()));
        assert_eq!(provider.effective_storage_class(), Some("GLACIER_IR"));

        // Clearing the override falls back to profile
        provider.set_storage_class_override(None);
        assert_eq!(provider.effective_storage_class(), Some("STANDARD_IA"));

        // Whitespace-only normalises to None
        provider.set_storage_class_override(Some("   ".to_string()));
        assert!(provider.storage_class_override.is_none());
        assert_eq!(provider.effective_storage_class(), Some("STANDARD_IA"));
    }

    /// KE-B1.5: `x-amz-storage-class` header lands in `append_upload_headers`
    /// with the effective (override > profile) value.
    #[test]
    fn append_upload_headers_emits_effective_storage_class() {
        let mut provider = make_provider(Some("http://localhost:9000"));

        // Nothing set → no header
        let mut headers = HashMap::new();
        provider.append_upload_headers(&mut headers);
        assert!(!headers.contains_key("x-amz-storage-class"));

        // Override wins
        provider.set_storage_class_override(Some("STANDARD_IA".to_string()));
        let mut headers = HashMap::new();
        provider.append_upload_headers(&mut headers);
        assert_eq!(
            headers.get("x-amz-storage-class"),
            Some(&"STANDARD_IA".to_string())
        );
    }

    /// Trash-01: `parse_object_versions_page` captures both `<Version>` and
    /// `<DeleteMarker>` elements across several keys, preserving grouping,
    /// `is_delete_marker`, `is_latest`, and sizes (0 for markers), plus the
    /// pagination markers.
    #[test]
    fn parse_object_versions_page_captures_versions_and_markers() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListVersionsResult>
    <IsTruncated>true</IsTruncated>
    <NextKeyMarker>docs/report.pdf</NextKeyMarker>
    <NextVersionIdMarker>v-next-999</NextVersionIdMarker>
    <DeleteMarker>
        <Key>docs/report.pdf</Key>
        <VersionId>del-marker-001</VersionId>
        <IsLatest>true</IsLatest>
        <LastModified>2026-07-09T10:00:00.000Z</LastModified>
    </DeleteMarker>
    <Version>
        <Key>docs/report.pdf</Key>
        <VersionId>v-002</VersionId>
        <IsLatest>false</IsLatest>
        <LastModified>2026-07-08T09:00:00.000Z</LastModified>
        <Size>2048</Size>
    </Version>
    <Version>
        <Key>photos/cat.jpg</Key>
        <VersionId>v-100</VersionId>
        <IsLatest>true</IsLatest>
        <LastModified>2026-07-07T08:00:00.000Z</LastModified>
        <Size>512000</Size>
    </Version>
</ListVersionsResult>"#;

        let (entries, is_truncated, next_key_marker, next_version_id_marker) =
            parse_object_versions_page(xml).expect("parse should succeed");

        assert_eq!(entries.len(), 3, "one marker + two versions");
        assert!(is_truncated);
        assert_eq!(next_key_marker.as_deref(), Some("docs/report.pdf"));
        assert_eq!(next_version_id_marker.as_deref(), Some("v-next-999"));

        // Row 0: the delete marker (latest state of docs/report.pdf), size 0.
        let marker = &entries[0];
        assert_eq!(marker.key, "docs/report.pdf");
        assert_eq!(marker.version_id, "del-marker-001");
        assert!(marker.is_delete_marker);
        assert!(marker.is_latest);
        assert_eq!(marker.size, 0);
        assert_eq!(
            marker.last_modified.as_deref(),
            Some("2026-07-09T10:00:00.000Z")
        );

        // Row 1: a recoverable non-current version of the same key.
        let old = &entries[1];
        assert_eq!(old.key, "docs/report.pdf");
        assert_eq!(old.version_id, "v-002");
        assert!(!old.is_delete_marker);
        assert!(!old.is_latest);
        assert_eq!(old.size, 2048);

        // Row 2: a different key's current version.
        let cat = &entries[2];
        assert_eq!(cat.key, "photos/cat.jpg");
        assert!(!cat.is_delete_marker);
        assert!(cat.is_latest);
        assert_eq!(cat.size, 512000);
    }

    /// Trash-02: `build_batch_delete_xml` emits `<VersionId>` for version-aware
    /// entries (and omits it for current-version entries), escapes special
    /// characters, and the batch helper chunks at 1000 objects per request.
    #[test]
    fn build_batch_delete_xml_carries_version_id_and_chunks() {
        // Mixed: one version-specific object, one current-version object with an
        // XML-sensitive key.
        let chunk = vec![
            (
                "docs/report.pdf".to_string(),
                Some("del-marker-001".to_string()),
            ),
            ("a&b/<c>.txt".to_string(), None),
        ];
        let xml = String::from_utf8(build_batch_delete_xml(&chunk)).unwrap();

        assert!(xml.starts_with("<Delete><Quiet>true</Quiet>"));
        assert!(xml.ends_with("</Delete>"));
        assert!(xml.contains(
            "<Object><Key>docs/report.pdf</Key><VersionId>del-marker-001</VersionId></Object>"
        ));
        // The current-version object carries no <VersionId>.
        assert!(xml.contains("<Object><Key>a&amp;b/&lt;c&gt;.txt</Key></Object>"));
        assert!(!xml.contains("<VersionId>del-marker-001</VersionId><VersionId>"));

        // Exactly one <VersionId> across the two objects.
        assert_eq!(xml.matches("<VersionId>").count(), 1);

        // Chunking: 2500 objects split into 1000 / 1000 / 500.
        let many: Vec<(String, Option<String>)> =
            (0..2500).map(|i| (format!("k{i}"), None)).collect();
        let chunk_sizes: Vec<usize> = many.chunks(1000).map(|c| c.len()).collect();
        assert_eq!(chunk_sizes, vec![1000, 1000, 500]);
    }
}
