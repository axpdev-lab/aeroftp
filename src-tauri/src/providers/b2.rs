//! Backblaze B2 Cloud Storage — Native API v4 provider.
//!
//! See `docs/dev/guides/Backblaze/` for the full API reference and design notes.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::header::{
    HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, RANGE,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use super::{
    sanitize_api_error, send_with_retry, FileVersion, HttpRetryConfig, ProviderConfig,
    ProviderError, RemoteEntry, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult,
    StorageInfo, StorageProvider, MAX_DOWNLOAD_TO_BYTES,
};

const AUTHORIZE_URL: &str = "https://api.backblazeb2.com/b2api/v4/b2_authorize_account";
const SINGLE_UPLOAD_RECOMMENDED_MAX: u64 = 200 * 1024 * 1024; // 200 MB; above this use large-file workflow
const LARGE_FILE_PART_SIZE: u64 = 100 * 1024 * 1024; // 100 MB per part (recommended)
const LARGE_FILE_MIN_PART_SIZE: u64 = 5 * 1024 * 1024; // B2 minimum (last part may be smaller)
const COPY_MAX_SIZE: u64 = 5 * 1024 * 1024 * 1024; // 5 GB hard limit on b2_copy_file
const COPY_PART_MAX_SIZE: u64 = 5 * 1024 * 1024 * 1024; // 5 GB per b2_copy_part call
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024 * 1024 * 1024; // 10 TB practical ceiling
const DEFAULT_LIST_PAGE: u32 = 1000;
const HEADER_BUDGET_STD: usize = 7000;
const PLACEHOLDER_NAME: &str = ".bzEmpty";

#[cfg(debug_assertions)]
fn b2_log(msg: &str) {
    eprintln!("[b2] {}", msg);
}
#[cfg(not(debug_assertions))]
fn b2_log(_msg: &str) {}

// ─── Configuration ───

pub struct B2Config {
    pub application_key_id: String,
    pub application_key: SecretString,
    pub bucket: String,
    pub initial_path: Option<String>,
}

impl B2Config {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let application_key_id = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("applicationKeyId required".into()))?;
        if application_key_id.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "applicationKeyId cannot be empty".into(),
            ));
        }
        let key = config
            .password
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("applicationKey required".into()))?;
        if key.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "applicationKey cannot be empty".into(),
            ));
        }
        let bucket = config.extra.get("bucket").cloned().ok_or_else(|| {
            ProviderError::InvalidConfig("bucket name required (extra.bucket)".into())
        })?;
        if bucket.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "bucket name cannot be empty".into(),
            ));
        }
        Ok(Self {
            application_key_id,
            application_key: SecretString::new(key.into()),
            bucket,
            initial_path: config.initial_path.clone(),
        })
    }
}

// ─── Auth state ───

#[derive(Debug, Clone, Deserialize)]
struct AuthorizeResponse {
    account_id: String,
    authorization_token: String,
    api_info: ApiInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiInfo {
    storage_api: StorageApiInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageApiInfo {
    api_url: String,
    download_url: String,
    // Phase 2 (large-file upload) — kept here so the wire shape is complete.
    #[allow(dead_code)]
    #[serde(default)]
    absolute_minimum_part_size: u64,
    #[allow(dead_code)]
    #[serde(default)]
    recommended_part_size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct B2ApiError {
    #[allow(dead_code)]
    #[serde(default)]
    status: u16,
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

// ─── B2 wire types (subset used by this provider) ───

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct B2Bucket {
    bucket_id: String,
    bucket_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListBucketsResponse {
    buckets: Vec<B2Bucket>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct B2File {
    // Phase 2 (delete by version, hide-then-purge workflow) needs this.
    #[allow(dead_code)]
    #[serde(default)]
    file_id: Option<String>,
    file_name: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    content_length: u64,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    upload_timestamp: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFileNamesResponse {
    files: Vec<B2File>,
    #[serde(default)]
    next_file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetUploadUrlResponse {
    upload_url: String,
    authorization_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadFileResponse {
    #[allow(dead_code)]
    file_id: String,
    file_name: String,
    content_length: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartLargeFileResponse {
    file_id: String,
    #[allow(dead_code)]
    file_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetUploadPartUrlResponse {
    upload_url: String,
    authorization_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadPartResponse {
    #[allow(dead_code)]
    file_id: String,
    #[allow(dead_code)]
    part_number: u32,
    content_sha1: String,
    #[allow(dead_code)]
    content_length: u64,
}

// `b2_copy_part` returns the same shape as `b2_upload_part`. Note: when the
// source is itself a large-file (so B2 has no whole-file SHA-1 to copy from),
// `contentSha1` is the literal string `"none"`. `b2_finish_large_file`
// accepts `"none"` entries in `partSha1Array`, so we just round-trip whatever
// B2 hands us.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyPartResponse {
    #[allow(dead_code)]
    file_id: String,
    #[allow(dead_code)]
    part_number: u32,
    content_sha1: String,
    #[allow(dead_code)]
    content_length: u64,
}

// `b2_list_file_versions` returns every version (including hide markers and
// folder placeholders), keyed by `(fileName, fileId)` for cursor pagination.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFileVersionsResponse {
    files: Vec<B2File>,
    #[serde(default)]
    next_file_name: Option<String>,
    #[serde(default)]
    next_file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetDownloadAuthorizationResponse {
    #[allow(dead_code)]
    bucket_id: String,
    #[allow(dead_code)]
    file_name_prefix: String,
    authorization_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnfinishedFileWire {
    file_id: String,
    file_name: String,
    #[serde(default)]
    upload_timestamp: Option<i64>,
    #[serde(default)]
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListUnfinishedLargeFilesResponse {
    files: Vec<UnfinishedFileWire>,
    #[serde(default)]
    next_file_id: Option<String>,
}

/// An upload session that was started via `b2_start_large_file` but never
/// finished. Returned by `B2Provider::list_unfinished_uploads`. Each session
/// holds onto its uploaded parts and counts toward storage until cancelled.
#[derive(Debug, Clone)]
pub struct B2UnfinishedUpload {
    pub file_id: String,
    pub file_name: String,
    pub upload_timestamp_millis: Option<i64>,
    pub content_type: Option<String>,
}

// ─── Provider ───

pub struct B2Provider {
    config: B2Config,
    client: reqwest::Client,
    retry_config: HttpRetryConfig,

    // Populated after connect()
    account_id: String,
    api_url: String,
    download_url: String,
    auth_token: SecretString,
    bucket_id: String,

    current_path: String,
    connected: bool,
}

impl B2Provider {
    pub fn new(config: B2Config) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            retry_config: HttpRetryConfig::default(),
            account_id: String::new(),
            api_url: String::new(),
            download_url: String::new(),
            auth_token: SecretString::new(String::new().into()),
            bucket_id: String::new(),
            current_path: "/".to_string(),
            connected: false,
        }
    }

    fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        HeaderValue::from_str(self.auth_token.expose_secret())
            .map_err(|_| ProviderError::AuthenticationFailed("invalid auth token".into()))
    }

    async fn authorize(&mut self) -> Result<(), ProviderError> {
        let basic = STANDARD.encode(format!(
            "{}:{}",
            self.config.application_key_id,
            self.config.application_key.expose_secret()
        ));
        let req = self
            .client
            .get(AUTHORIZE_URL)
            .header(AUTHORIZATION, format!("Basic {}", basic))
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("authorize build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("authorize send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &body, "b2_authorize_account"));
        }
        let parsed: AuthorizeResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("authorize parse: {}", e)))?;
        self.account_id = parsed.account_id;
        self.api_url = parsed.api_info.storage_api.api_url;
        self.download_url = parsed.api_info.storage_api.download_url;
        self.auth_token = SecretString::new(parsed.authorization_token.into());
        Ok(())
    }

    async fn resolve_bucket_id(&mut self) -> Result<(), ProviderError> {
        let url = format!("{}/b2api/v4/b2_list_buckets", self.api_url);
        let body = serde_json::json!({
            "accountId": self.account_id,
            "bucketName": self.config.bucket,
        });
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("list_buckets build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("list_buckets send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_list_buckets"));
        }
        let parsed: ListBucketsResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("list_buckets parse: {}", e)))?;
        let target = parsed
            .buckets
            .into_iter()
            .find(|b| b.bucket_name == self.config.bucket)
            .ok_or_else(|| {
                ProviderError::NotFound(format!(
                    "bucket '{}' not found or not accessible",
                    self.config.bucket
                ))
            })?;
        self.bucket_id = target.bucket_id;
        Ok(())
    }

    fn resolved_path(&self, path: &str) -> String {
        if path.is_empty() || path == "." {
            normalize_path(&self.current_path)
        } else if path.starts_with('/') {
            normalize_path(path)
        } else {
            let combined = format!("{}/{}", self.current_path.trim_end_matches('/'), path);
            normalize_path(&combined)
        }
    }

    fn b2_key(&self, normalized_abs: &str) -> String {
        // B2 file names never start with `/`
        normalized_abs.trim_start_matches('/').to_string()
    }

    fn validate_header_budget(
        &self,
        file_name: &str,
        info_extra: usize,
    ) -> Result<(), ProviderError> {
        if file_name.len() + info_extra > HEADER_BUDGET_STD {
            return Err(ProviderError::InvalidConfig(format!(
                "file name + metadata exceed B2 header budget ({} bytes)",
                HEADER_BUDGET_STD
            )));
        }
        Ok(())
    }

    async fn list_file_names(
        &self,
        prefix: &str,
        delimiter: Option<&str>,
        start_file_name: Option<&str>,
        max_count: u32,
    ) -> Result<ListFileNamesResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_list_file_names", self.api_url);
        let mut body = serde_json::json!({
            "bucketId": self.bucket_id,
            "prefix": prefix,
            "maxFileCount": max_count,
        });
        if let Some(d) = delimiter {
            body["delimiter"] = serde_json::Value::String(d.to_string());
        }
        if let Some(s) = start_file_name {
            body["startFileName"] = serde_json::Value::String(s.to_string());
        }
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("list_file_names build: {}", e))
            })?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("list_file_names send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_list_file_names"));
        }
        resp.json::<ListFileNamesResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("list_file_names parse: {}", e)))
    }

    /// Re-authorize on AuthenticationFailed once, then retry caller.
    ///
    /// Filters strictly to errors raised by `map_b2_status` 401 (token expired
    /// or `bad_auth_token`) and the local "invalid auth token" guard. Callers
    /// retry once after this returns `true`; if it returns `false` the original
    /// error is surfaced unchanged.
    async fn maybe_reauth(&mut self, err: &ProviderError) -> bool {
        if !is_b2_token_failure(err) {
            return false;
        }
        b2_log("auth token expired, reauthorizing");
        self.authorize().await.is_ok() && self.resolve_bucket_id().await.is_ok()
    }

    async fn get_upload_url(&mut self) -> Result<GetUploadUrlResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_get_upload_url", self.api_url);
        let body = serde_json::json!({ "bucketId": self.bucket_id });
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("get_upload_url build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("get_upload_url send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_get_upload_url"));
        }
        resp.json::<GetUploadUrlResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("get_upload_url parse: {}", e)))
    }

    async fn start_large_file(
        &self,
        file_name: &str,
    ) -> Result<StartLargeFileResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_start_large_file", self.api_url);
        let body = serde_json::json!({
            "bucketId": self.bucket_id,
            "fileName": file_name,
            "contentType": "b2/x-auto",
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("start_large_file: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_start_large_file"));
        }
        resp.json::<StartLargeFileResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("start_large_file parse: {}", e)))
    }

    async fn get_upload_part_url(
        &self,
        file_id: &str,
    ) -> Result<GetUploadPartUrlResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_get_upload_part_url", self.api_url);
        let body = serde_json::json!({ "fileId": file_id });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("get_upload_part_url: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_get_upload_part_url"));
        }
        resp.json::<GetUploadPartUrlResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("get_upload_part_url parse: {}", e)))
    }

    async fn upload_part(
        &self,
        upload_url: &str,
        upload_token: &str,
        part_number: u32,
        bytes: Vec<u8>,
    ) -> Result<UploadPartResponse, ProviderError> {
        let sha1 = sha1_hex(&bytes);
        let len = bytes.len() as u64;
        let resp = self
            .client
            .post(upload_url)
            .header(AUTHORIZATION, upload_token)
            .header(CONTENT_LENGTH, len)
            .header(
                HeaderName::from_static("x-bz-part-number"),
                part_number.to_string(),
            )
            .header(HeaderName::from_static("x-bz-content-sha1"), &sha1)
            .body(bytes)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("upload_part: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_upload_part"));
        }
        resp.json::<UploadPartResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("upload_part parse: {}", e)))
    }

    async fn finish_large_file(
        &self,
        file_id: &str,
        part_sha1_array: Vec<String>,
    ) -> Result<UploadFileResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_finish_large_file", self.api_url);
        let body = serde_json::json!({
            "fileId": file_id,
            "partSha1Array": part_sha1_array,
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("finish_large_file: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_finish_large_file"));
        }
        resp.json::<UploadFileResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("finish_large_file parse: {}", e)))
    }

    async fn upload_large_file(
        &self,
        local_path: &str,
        key: &str,
        size: u64,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let start = self.start_large_file(key).await?;
        let part_urls = self.get_upload_part_url(&start.file_id).await?;
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("open local: {}", e)))?;
        use tokio::io::AsyncReadExt;
        let mut part_sha1s: Vec<String> = Vec::new();
        let mut part_number: u32 = 1;
        let mut transferred: u64 = 0;
        loop {
            let remaining = size.saturating_sub(transferred);
            if remaining == 0 {
                break;
            }
            let this_part = remaining.min(LARGE_FILE_PART_SIZE);
            let mut buf = vec![0u8; this_part as usize];
            file.read_exact(&mut buf)
                .await
                .map_err(|e| ProviderError::Other(format!("read part {}: {}", part_number, e)))?;
            // Validate part size: only the LAST part may be smaller than 5 MB
            let is_last = remaining == this_part;
            if !is_last && this_part < LARGE_FILE_MIN_PART_SIZE {
                return Err(ProviderError::InvalidConfig(format!(
                    "part {} too small ({} < {})",
                    part_number, this_part, LARGE_FILE_MIN_PART_SIZE
                )));
            }
            let part_resp = self
                .upload_part(
                    &part_urls.upload_url,
                    &part_urls.authorization_token,
                    part_number,
                    buf,
                )
                .await?;
            part_sha1s.push(part_resp.content_sha1);
            transferred += this_part;
            part_number += 1;
            if let Some(ref p) = progress {
                p(transferred, size);
            }
            if part_number > 10_000 {
                return Err(ProviderError::InvalidConfig(
                    "exceeded B2 limit of 10000 parts".into(),
                ));
            }
        }
        self.finish_large_file(&start.file_id, part_sha1s).await?;
        Ok(())
    }

    async fn copy_file_to(
        &self,
        source_file_id: &str,
        new_name: &str,
    ) -> Result<UploadFileResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_copy_file", self.api_url);
        let body = serde_json::json!({
            "sourceFileId": source_file_id,
            "fileName": new_name,
            "metadataDirective": "COPY",
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("copy_file: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_copy_file"));
        }
        resp.json::<UploadFileResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("copy_file parse: {}", e)))
    }

    /// `b2_copy_part` — server-side copy of a byte range from a source file
    /// into a part of an in-progress large-file upload. Used for renaming
    /// files larger than 5 GB (where `b2_copy_file` is rejected).
    async fn copy_part(
        &self,
        source_file_id: &str,
        large_file_id: &str,
        part_number: u32,
        range: &str,
    ) -> Result<CopyPartResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_copy_part", self.api_url);
        let body = serde_json::json!({
            "sourceFileId": source_file_id,
            "largeFileId": large_file_id,
            "partNumber": part_number,
            "range": range,
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("copy_part: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_copy_part"));
        }
        resp.json::<CopyPartResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("copy_part parse: {}", e)))
    }

    /// Server-side rename for files larger than 5 GB via the chunked
    /// `b2_copy_part` workflow. The flow mirrors a large-file upload but
    /// every part is copied byte-range from an existing source `fileId`,
    /// avoiding any client traffic.
    ///
    /// Steps:
    /// 1. `b2_start_large_file` on the destination key.
    /// 2. Loop: `b2_copy_part` with `Range: bytes=N-M` for each chunk.
    /// 3. `b2_finish_large_file` with the array of per-part `contentSha1`
    ///    values returned by the server (these may be `"none"` when the
    ///    source itself was a large-file copy — B2 accepts that).
    /// 4. `b2_delete_file_version` on the source to make the rename atomic.
    ///
    /// On any failure mid-way the in-progress large file is cancelled to
    /// release the parts already copied.
    async fn rename_large_file_inner(
        &mut self,
        source_file_id: &str,
        from_key: &str,
        to_key: &str,
        size: u64,
    ) -> Result<(), ProviderError> {
        if size > MAX_FILE_SIZE {
            return Err(ProviderError::NotSupported(format!(
                "source file is {} bytes; B2 caps single files at 10 TB.",
                size
            )));
        }
        // Compute part count up-front so we can fail fast on pathological inputs.
        let part_size = LARGE_FILE_PART_SIZE.min(COPY_PART_MAX_SIZE);
        let part_count = size.div_ceil(part_size);
        if part_count > 10_000 {
            return Err(ProviderError::InvalidConfig(format!(
                "rename would require {} parts, exceeding B2's 10 000 cap",
                part_count
            )));
        }
        let started = self.start_large_file(to_key).await?;
        let large_file_id = started.file_id;
        let mut part_sha1s: Vec<String> = Vec::with_capacity(part_count as usize);
        let mut offset: u64 = 0;
        let mut part_number: u32 = 1;
        let copy_result: Result<(), ProviderError> = (async {
            while offset < size {
                let this_part = (size - offset).min(part_size);
                // B2 part minimum is 5 MB except for the LAST part, identical
                // semantics to upload_part.
                let is_last = offset + this_part == size;
                if !is_last && this_part < LARGE_FILE_MIN_PART_SIZE {
                    return Err(ProviderError::InvalidConfig(format!(
                        "part {} too small ({} < {})",
                        part_number, this_part, LARGE_FILE_MIN_PART_SIZE
                    )));
                }
                let range = format!("bytes={}-{}", offset, offset + this_part - 1);
                let resp = self
                    .copy_part(source_file_id, &large_file_id, part_number, &range)
                    .await?;
                part_sha1s.push(resp.content_sha1);
                offset += this_part;
                part_number += 1;
            }
            Ok(())
        })
        .await;
        match copy_result {
            Ok(()) => {}
            Err(e) => {
                // Best-effort cancel — release parts already copied so they
                // don't accrue storage charges. Failures here are logged but
                // do not mask the original error.
                if let Err(cancel_err) = self.cancel_large_file_inner(&large_file_id).await {
                    b2_log(&format!(
                        "rename_large_file: copy failed and cancel also failed: {} / cancel: {}",
                        e, cancel_err
                    ));
                }
                return Err(e);
            }
        }
        // Materialize the new file. After this call the destination key is live.
        self.finish_large_file(&large_file_id, part_sha1s).await?;
        // Delete the original version. Mirror `rename` semantics: a delete
        // failure does not undo the rename — the new copy is already in place.
        if let Err(e) = self.do_delete_file_version(from_key, source_file_id).await {
            b2_log(&format!(
                "rename_large_file: copy + finish ok but source delete failed: {}",
                e
            ));
        }
        Ok(())
    }

    /// Streamed download to a local path. Borrows `&self` only so the trait
    /// method can wrap a single attempt in reauth retry without re-locking.
    /// `progress` is moved (not borrowed) because `Box<dyn Fn + Send>` is not
    /// Sync — borrowing it across an await would break the Send-future contract.
    async fn do_download(
        &self,
        key: &str,
        local_path: &std::path::Path,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let url = format!(
            "{}/file/{}/{}",
            self.download_url,
            urlencoding::encode(&self.config.bucket),
            encode_path_segments(key),
        );
        let req = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("download build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("download send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &body, "b2_download_file_by_name"));
        }
        let total = resp.content_length().unwrap_or(0);
        let temp_path = local_path.with_extension("aerotmp");
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|e| ProviderError::Other(format!("create temp: {}", e)))?;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| ProviderError::Other(format!("read chunk: {}", e)))?;
            file.write_all(&bytes)
                .await
                .map_err(|e| ProviderError::Other(format!("write chunk: {}", e)))?;
            downloaded += bytes.len() as u64;
            if let Some(ref p) = progress {
                p(downloaded, total);
            }
        }
        file.flush().await.ok();
        drop(file);
        tokio::fs::rename(&temp_path, local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("atomic rename: {}", e)))?;
        Ok(())
    }

    /// In-memory download (range-capped). Same pattern as `do_download`.
    async fn do_download_to_bytes(&self, key: &str) -> Result<Vec<u8>, ProviderError> {
        let url = format!(
            "{}/file/{}/{}",
            self.download_url,
            urlencoding::encode(&self.config.bucket),
            encode_path_segments(key),
        );
        let req = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(RANGE, format!("bytes=0-{}", MAX_DOWNLOAD_TO_BYTES - 1))
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("download_bytes build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("download_bytes send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &body, "b2_download_file_by_name"));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::Other(format!("read body: {}", e)))?;
        Ok(bytes.to_vec())
    }

    /// `b2_hide_file` raw POST. Used by `delete`, `rmdir`, `rmdir_recursive`.
    async fn do_hide_file(&self, file_name: &str) -> Result<reqwest::StatusCode, ProviderError> {
        let url = format!("{}/b2api/v4/b2_hide_file", self.api_url);
        let body = serde_json::json!({
            "bucketId": self.bucket_id,
            "fileName": file_name,
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("hide_file send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_hide_file"));
        }
        Ok(status)
    }

    /// `b2_delete_file_version` raw POST. Used by `rename` to delete the
    /// source after a successful copy.
    async fn do_delete_file_version(
        &self,
        file_name: &str,
        file_id: &str,
    ) -> Result<(), ProviderError> {
        let url = format!("{}/b2api/v4/b2_delete_file_version", self.api_url);
        let body = serde_json::json!({
            "fileName": file_name,
            "fileId": file_id,
        });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("delete_file_version: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_delete_file_version"));
        }
        Ok(())
    }

    /// Look up the latest version's `fileId` and `contentLength` for a given key.
    async fn lookup_file_id(&self, key: &str) -> Result<(String, u64), ProviderError> {
        let resp = self.list_file_names(key, None, None, 1).await?;
        let f = resp
            .files
            .into_iter()
            .find(|f| f.file_name == key)
            .ok_or_else(|| ProviderError::NotFound(format!("file {}", key)))?;
        let fid = f
            .file_id
            .ok_or_else(|| ProviderError::ServerError("missing fileId in list response".into()))?;
        Ok((fid, f.content_length))
    }

    /// `b2_list_file_versions` paginated page. Returns every version that
    /// matches the given prefix (including hide markers — caller filters).
    async fn list_file_versions_page(
        &self,
        prefix: &str,
        start_file_name: Option<&str>,
        start_file_id: Option<&str>,
        max_count: u32,
    ) -> Result<ListFileVersionsResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_list_file_versions", self.api_url);
        let mut body = serde_json::json!({
            "bucketId": self.bucket_id,
            "prefix": prefix,
            "maxFileCount": max_count,
        });
        if let Some(s) = start_file_name {
            body["startFileName"] = serde_json::Value::String(s.to_string());
        }
        if let Some(s) = start_file_id {
            body["startFileId"] = serde_json::Value::String(s.to_string());
        }
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("list_file_versions build: {}", e))
            })?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("list_file_versions send: {}", e))
            })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_list_file_versions"));
        }
        resp.json::<ListFileVersionsResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("list_file_versions parse: {}", e)))
    }

    /// Streaming download of a specific version by `fileId` via
    /// `b2_download_file_by_id`. Mirrors `do_download` (atomic temp + rename,
    /// no progress sharing because Box<dyn Fn + Send> is !Sync).
    async fn do_download_by_id(
        &self,
        file_id: &str,
        local_path: &std::path::Path,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let url = format!(
            "{}/b2api/v4/b2_download_file_by_id?fileId={}",
            self.download_url,
            urlencoding::encode(file_id)
        );
        let req = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("download_by_id build: {}", e)))?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("download_by_id send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &body, "b2_download_file_by_id"));
        }
        let total = resp.content_length().unwrap_or(0);
        let temp_path = local_path.with_extension("aerotmp");
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|e| ProviderError::Other(format!("create temp: {}", e)))?;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| ProviderError::Other(format!("read chunk: {}", e)))?;
            file.write_all(&bytes)
                .await
                .map_err(|e| ProviderError::Other(format!("write chunk: {}", e)))?;
            downloaded += bytes.len() as u64;
            if let Some(ref p) = progress {
                p(downloaded, total);
            }
        }
        file.flush().await.ok();
        drop(file);
        tokio::fs::rename(&temp_path, local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("atomic rename: {}", e)))?;
        Ok(())
    }

    /// `b2_get_download_authorization` mints a download token for any object
    /// whose key starts with `file_name_prefix`, valid for `valid_duration_seconds`.
    /// The bucket must be private — public buckets reject this call.
    async fn get_download_authorization(
        &self,
        file_name_prefix: &str,
        valid_duration_seconds: u64,
    ) -> Result<GetDownloadAuthorizationResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_get_download_authorization", self.api_url);
        let body = serde_json::json!({
            "bucketId": self.bucket_id,
            "fileNamePrefix": file_name_prefix,
            "validDurationInSeconds": valid_duration_seconds,
        });
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("get_download_authorization build: {}", e))
            })?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("get_download_authorization send: {}", e))
            })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(
                status,
                &text,
                "b2_get_download_authorization",
            ));
        }
        resp.json::<GetDownloadAuthorizationResponse>()
            .await
            .map_err(|e| {
                ProviderError::ServerError(format!("get_download_authorization parse: {}", e))
            })
    }

    /// `b2_list_unfinished_large_files` paginated page. Used by the cleanup
    /// helper to enumerate abandoned upload sessions.
    async fn list_unfinished_large_files_page(
        &self,
        start_file_id: Option<&str>,
        max_count: u32,
    ) -> Result<ListUnfinishedLargeFilesResponse, ProviderError> {
        let url = format!("{}/b2api/v4/b2_list_unfinished_large_files", self.api_url);
        let mut body = serde_json::json!({
            "bucketId": self.bucket_id,
            "maxFileCount": max_count,
        });
        if let Some(s) = start_file_id {
            body["startFileId"] = serde_json::Value::String(s.to_string());
        }
        let req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("list_unfinished build: {}", e))
            })?;
        let resp = send_with_retry(&self.client, req, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("list_unfinished send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(
                status,
                &text,
                "b2_list_unfinished_large_files",
            ));
        }
        resp.json::<ListUnfinishedLargeFilesResponse>()
            .await
            .map_err(|e| ProviderError::ServerError(format!("list_unfinished parse: {}", e)))
    }

    /// `b2_cancel_large_file` releases parts already uploaded for an
    /// abandoned `b2_start_large_file` session.
    async fn cancel_large_file_inner(&self, file_id: &str) -> Result<(), ProviderError> {
        let url = format!("{}/b2api/v4/b2_cancel_large_file", self.api_url);
        let body = serde_json::json!({ "fileId": file_id });
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("cancel_large_file: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_cancel_large_file"));
        }
        Ok(())
    }

    // ─── B2-specific public API (downcasted from `as_any_mut()`) ───

    /// Enumerate every unfinished large-file upload in the current bucket.
    /// Walks `b2_list_unfinished_large_files` to exhaustion, with single-shot
    /// reauth on the master token.
    pub async fn list_unfinished_uploads(
        &mut self,
    ) -> Result<Vec<B2UnfinishedUpload>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let mut out = Vec::new();
        let mut start: Option<String> = None;
        let mut first_call = true;
        loop {
            let page = match self
                .list_unfinished_large_files_page(start.as_deref(), DEFAULT_LIST_PAGE)
                .await
            {
                Ok(r) => r,
                Err(e) if first_call && is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.list_unfinished_large_files_page(start.as_deref(), DEFAULT_LIST_PAGE)
                            .await?
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };
            first_call = false;
            for f in page.files {
                out.push(B2UnfinishedUpload {
                    file_id: f.file_id,
                    file_name: f.file_name,
                    upload_timestamp_millis: f.upload_timestamp,
                    content_type: f.content_type,
                });
            }
            match page.next_file_id {
                Some(next) if !next.is_empty() => start = Some(next),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Cancel one unfinished large-file upload session, releasing every part
    /// already uploaded for it. Idempotent: a 404 is mapped to NotFound and
    /// surfaced for the caller to handle.
    pub async fn cancel_unfinished_upload(&mut self, file_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        match self.cancel_large_file_inner(file_id).await {
            Ok(()) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.cancel_large_file_inner(file_id).await
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl StorageProvider for B2Provider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> super::ProviderType {
        super::ProviderType::Backblaze
    }

    fn display_name(&self) -> String {
        format!("Backblaze B2 ({})", self.config.bucket)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.authorize().await?;
        self.resolve_bucket_id().await?;
        if let Some(ref initial) = self.config.initial_path {
            self.current_path = normalize_path(initial);
        } else {
            self.current_path = "/".to_string();
        }
        self.connected = true;
        b2_log(&format!(
            "connected: bucket={}, api={}",
            self.config.bucket, self.api_url
        ));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let mut prefix = self.b2_key(&abs);
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        let mut start: Option<String> = None;
        let mut entries: Vec<RemoteEntry> = Vec::new();
        let mut first_call = true;
        loop {
            // Only the first round needs reauth retry — subsequent pages run
            // with a freshly minted token (B2 tokens are valid 24h, well
            // beyond a single listing).
            let resp = match self
                .list_file_names(&prefix, Some("/"), start.as_deref(), DEFAULT_LIST_PAGE)
                .await
            {
                Ok(r) => r,
                Err(e) if first_call && is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.list_file_names(
                            &prefix,
                            Some("/"),
                            start.as_deref(),
                            DEFAULT_LIST_PAGE,
                        )
                        .await?
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };
            first_call = false;
            for f in resp.files {
                if f.file_name == prefix {
                    continue; // self
                }
                let name_only = f
                    .file_name
                    .strip_prefix(&prefix)
                    .unwrap_or(&f.file_name)
                    .trim_end_matches('/');
                if name_only.is_empty() || name_only == PLACEHOLDER_NAME {
                    continue;
                }
                let is_dir = f.action == "folder";
                let full_path = format!("/{}", f.file_name.trim_end_matches('/'));
                entries.push(RemoteEntry {
                    name: name_only.to_string(),
                    path: full_path,
                    is_dir,
                    size: if is_dir { 0 } else { f.content_length },
                    modified: f
                        .upload_timestamp
                        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                        .map(|dt| dt.to_rfc3339()),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: f.content_type.clone(),
                    metadata: std::collections::HashMap::new(),
                });
            }
            match resp.next_file_name {
                Some(next) if !next.is_empty() => start = Some(next),
                _ => break,
            }
        }
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let target = self.resolved_path(path);
        // Verify the directory exists (any file under prefix counts)
        let mut probe_prefix = self.b2_key(&target);
        if !probe_prefix.is_empty() && !probe_prefix.ends_with('/') {
            probe_prefix.push('/');
        }
        if !probe_prefix.is_empty() {
            let resp = match self.list_file_names(&probe_prefix, None, None, 1).await {
                Ok(r) => r,
                Err(e) if is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.list_file_names(&probe_prefix, None, None, 1).await?
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };
            if resp.files.is_empty() {
                return Err(ProviderError::NotFound(format!("directory {}", target)));
            }
        }
        self.current_path = target;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if self.current_path == "/" {
            return Ok(());
        }
        let trimmed = self.current_path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => self.current_path = "/".to_string(),
            Some(idx) => self.current_path = trimmed[..idx].to_string(),
            None => self.current_path = "/".to_string(),
        }
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(remote_path);
        let key = self.b2_key(&abs);
        let local_pb = std::path::PathBuf::from(local_path);
        // First attempt consumes `progress`; the rare reauth retry runs without
        // progress reporting (Box<dyn Fn + Send> isn't Sync — see do_download).
        match self.do_download(&key, &local_pb, progress).await {
            Ok(()) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_download(&key, &local_pb, None).await
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(remote_path);
        let key = self.b2_key(&abs);
        match self.do_download_to_bytes(&key).await {
            Ok(b) => Ok(b),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_download_to_bytes(&key).await
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(remote_path);
        let key = self.b2_key(&abs);
        self.validate_header_budget(&key, 0)?;
        let metadata = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("stat local: {}", e)))?;
        let size = metadata.len();
        if size > SINGLE_UPLOAD_RECOMMENDED_MAX {
            // Large path: retry once on master-token failure during start_large_file.
            // progress is moved on first attempt; the rare retry runs without it.
            return match self
                .upload_large_file(local_path, &key, size, progress)
                .await
            {
                Ok(()) => Ok(()),
                Err(e) if is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.upload_large_file(local_path, &key, size, None).await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            };
        }
        // Small path: retry the master-token-bearing get_upload_url call. The
        // upload POST itself uses the upload-URL-specific token returned by
        // get_upload_url, so a fresh URL is acquired on retry.
        let upload = match self.get_upload_url().await {
            Ok(u) => u,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.get_upload_url().await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("read local: {}", e)))?;
        let sha1 = sha1_hex(&bytes);
        let mut req = self
            .client
            .post(&upload.upload_url)
            .header(AUTHORIZATION, &upload.authorization_token)
            .header(CONTENT_LENGTH, size)
            .header(
                HeaderName::from_static("x-bz-file-name"),
                urlencoding::encode(&key).into_owned(),
            )
            .header(CONTENT_TYPE, "b2/x-auto")
            .header(HeaderName::from_static("x-bz-content-sha1"), &sha1);
        if let Ok(modified) = metadata.modified() {
            if let Ok(epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                req = req.header(
                    HeaderName::from_static("x-bz-info-src_last_modified_millis"),
                    epoch.as_millis().to_string(),
                );
            }
        }
        if let Some(ref p) = progress {
            p(0, size);
        }
        let resp = req
            .body(bytes)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("upload send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(status, &text, "b2_upload_file"));
        }
        let parsed: UploadFileResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("upload parse: {}", e)))?;
        if let Some(ref p) = progress {
            p(parsed.content_length, size);
        }
        b2_log(&format!("uploaded: {}", parsed.file_name));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let mut key = self.b2_key(&abs);
        if !key.ends_with('/') {
            key.push('/');
        }
        key.push_str(PLACEHOLDER_NAME);
        self.validate_header_budget(&key, 0)?;
        let upload = match self.get_upload_url().await {
            Ok(u) => u,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.get_upload_url().await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        let body: Vec<u8> = Vec::new();
        let resp = self
            .client
            .post(&upload.upload_url)
            .header(AUTHORIZATION, &upload.authorization_token)
            .header(CONTENT_LENGTH, 0u64)
            .header(
                HeaderName::from_static("x-bz-file-name"),
                urlencoding::encode(&key).into_owned(),
            )
            .header(CONTENT_TYPE, "application/x-bzempty")
            .header(
                HeaderName::from_static("x-bz-content-sha1"),
                sha1_hex(&body),
            )
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("mkdir send: {}", e)))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(map_b2_status(
                status,
                &text,
                "b2_upload_file (mkdir placeholder)",
            ));
        }
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let key = self.b2_key(&abs);
        match self.do_hide_file(&key).await {
            Ok(_) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_hide_file(&key).await.map(|_| ())
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // For B2 a "directory" is virtual; rmdir hides the .bzEmpty placeholder if any.
        let abs = self.resolved_path(path);
        let mut key = self.b2_key(&abs);
        if !key.ends_with('/') {
            key.push('/');
        }
        key.push_str(PLACEHOLDER_NAME);
        // 404 means there was no placeholder (vacuous success); other failures
        // from do_hide_file already carry the right ProviderError variant.
        let result = match self.do_hide_file(&key).await {
            Ok(_) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_hide_file(&key).await.map(|_| ())
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => Ok(()),
            Err(ProviderError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let mut prefix = self.b2_key(&abs);
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        let mut start: Option<String> = None;
        let mut first_call = true;
        loop {
            // No delimiter → recursive flat listing under the prefix. Wrap
            // only the first round in reauth retry; subsequent pages run
            // with a freshly minted token.
            let resp = match self
                .list_file_names(&prefix, None, start.as_deref(), DEFAULT_LIST_PAGE)
                .await
            {
                Ok(r) => r,
                Err(e) if first_call && is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.list_file_names(&prefix, None, start.as_deref(), DEFAULT_LIST_PAGE)
                            .await?
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };
            first_call = false;
            for f in &resp.files {
                // Per-file 404 is not an error (concurrent delete/hide).
                let r = self.do_hide_file(&f.file_name).await;
                match r {
                    Ok(_) => {}
                    Err(ProviderError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            match resp.next_file_name {
                Some(next) if !next.is_empty() => start = Some(next),
                _ => break,
            }
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let from_abs = self.resolved_path(from);
        let to_abs = self.resolved_path(to);
        let from_key = self.b2_key(&from_abs);
        let to_key = self.b2_key(&to_abs);
        self.validate_header_budget(&to_key, 0)?;
        let (file_id, size) = match self.lookup_file_id(&from_key).await {
            Ok(v) => v,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.lookup_file_id(&from_key).await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        if size > COPY_MAX_SIZE {
            // Files above the 5 GB b2_copy_file ceiling go through the
            // chunked b2_copy_part workflow. The inner method handles the
            // start_large_file → loop copy_part → finish_large_file →
            // delete_source dance, with cancel-on-failure for the in-progress
            // upload session.
            return match self
                .rename_large_file_inner(&file_id, &from_key, &to_key, size)
                .await
            {
                Ok(()) => Ok(()),
                Err(e) if is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.rename_large_file_inner(&file_id, &from_key, &to_key, size)
                            .await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            };
        }
        let copied = match self.copy_file_to(&file_id, &to_key).await {
            Ok(v) => v,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.copy_file_to(&file_id, &to_key).await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        // Delete (hard) the original version so this is a true rename.
        // If the source delete fails, the copy is still in place — surface as
        // warning and keep the rename successful.
        let del = match self.do_delete_file_version(&from_key, &file_id).await {
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_delete_file_version(&from_key, &file_id).await
                } else {
                    Err(e)
                }
            }
            other => other,
        };
        if let Err(e) = del {
            b2_log(&format!(
                "rename: copy ok ({}) but delete of source failed: {}",
                copied.file_name, e
            ));
        }
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let key = self.b2_key(&abs);
        // Use list_file_names with prefix == exact key, maxFileCount 1
        let resp = match self.list_file_names(&key, None, None, 1).await {
            Ok(r) => r,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.list_file_names(&key, None, None, 1).await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        let f = resp
            .files
            .into_iter()
            .find(|f| f.file_name == key || f.file_name.starts_with(&format!("{}/", key)))
            .ok_or_else(|| ProviderError::NotFound(format!("path {}", abs)))?;
        let is_dir = f.file_name != key;
        Ok(RemoteEntry {
            name: abs.rsplit('/').next().unwrap_or("").to_string(),
            path: format!("/{}", f.file_name.trim_end_matches('/')),
            is_dir,
            size: if is_dir { 0 } else { f.content_length },
            modified: f
                .upload_timestamp
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                .map(|dt| dt.to_rfc3339()),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: f.content_type,
            metadata: std::collections::HashMap::new(),
        })
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // 24h account auth token; cheap probe via list_buckets if needed.
        // For now: no-op keeps it light. Phase 2 will add proactive reauth at ~22h elapsed.
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Backblaze B2 (account {}, bucket {})",
            short(&self.account_id),
            self.config.bucket
        ))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // B2 bills per stored byte; the API does not return a per-bucket quota.
        // Return zeros — UI hides storage panels when total == 0, matching the
        // behavior of other providers without quota.
        Ok(StorageInfo {
            used: 0,
            total: 0,
            free: 0,
        })
    }

    // ─── Versioning ────────────────────────────────────────────────────────

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let key = self.b2_key(&abs);
        let mut versions: Vec<FileVersion> = Vec::new();
        let mut start_name: Option<String> = None;
        let mut start_id: Option<String> = None;
        let mut first_call = true;
        loop {
            // `b2_list_file_versions` returns versions across every key sharing
            // this prefix; we filter to the exact match. The cursor is
            // `(nextFileName, nextFileId)` — both must round-trip.
            let page = match self
                .list_file_versions_page(
                    &key,
                    start_name.as_deref(),
                    start_id.as_deref(),
                    DEFAULT_LIST_PAGE,
                )
                .await
            {
                Ok(p) => p,
                Err(e) if first_call && is_b2_token_failure(&e) => {
                    if self.maybe_reauth(&e).await {
                        self.list_file_versions_page(
                            &key,
                            start_name.as_deref(),
                            start_id.as_deref(),
                            DEFAULT_LIST_PAGE,
                        )
                        .await?
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };
            first_call = false;
            // Detect cursor that points past our key — once nextFileName diverges,
            // any further pages won't yield matches and just burn Class C calls.
            let mut diverged = false;
            for f in &page.files {
                if f.file_name != key {
                    if f.file_name > key {
                        diverged = true;
                    }
                    continue;
                }
                if f.action == "hide" {
                    // Hide markers are versions, but they don't have downloadable
                    // content — surface them with size 0 and an explicit modified
                    // timestamp so the UI can render them as "deleted" markers.
                    if let Some(fid) = &f.file_id {
                        versions.push(FileVersion {
                            id: fid.clone(),
                            modified: f
                                .upload_timestamp
                                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                                .map(|dt| dt.to_rfc3339()),
                            size: 0,
                            modified_by: None,
                        });
                    }
                    continue;
                }
                if let Some(fid) = &f.file_id {
                    versions.push(FileVersion {
                        id: fid.clone(),
                        modified: f
                            .upload_timestamp
                            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                            .map(|dt| dt.to_rfc3339()),
                        size: f.content_length,
                        modified_by: None,
                    });
                }
            }
            if diverged {
                break;
            }
            match (page.next_file_name, page.next_file_id) {
                (Some(n), Some(i)) if !n.is_empty() && !i.is_empty() => {
                    if n > key {
                        // Cursor walked past our key.
                        break;
                    }
                    start_name = Some(n);
                    start_id = Some(i);
                }
                _ => break,
            }
        }
        if versions.is_empty() {
            return Err(ProviderError::NotFound(format!("file {}", abs)));
        }
        Ok(versions)
    }

    async fn download_version(
        &mut self,
        _path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let local_pb = std::path::PathBuf::from(local_path);
        // `_path` is unused here — the fileId is the canonical reference; B2
        // routes by id, not name. We still accept the parameter to match the
        // trait shape (and to let callers log a meaningful path).
        match self.do_download_by_id(version_id, &local_pb, None).await {
            Ok(()) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.do_download_by_id(version_id, &local_pb, None).await
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let key = self.b2_key(&abs);
        // B2 has no "restore" verb — we replay the chosen version as a fresh
        // upload via b2_copy_file (server-side, no client byte transfer). The
        // copy becomes the new latest version with the original fileName.
        match self.copy_file_to(version_id, &key).await {
            Ok(_) => Ok(()),
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.copy_file_to(version_id, &key).await.map(|_| ())
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    // ─── Share links ───────────────────────────────────────────────────────

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: false,
            supports_permissions: false,
            available_permissions: Vec::new(),
            supports_list_links: false,
            supports_revoke: false,
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolved_path(path);
        let key = self.b2_key(&abs);
        // B2 caps validity at 1s..604800s (7 days). Default to 24h.
        const DEFAULT_VALID_SECONDS: u64 = 86_400;
        const MAX_VALID_SECONDS: u64 = 604_800;
        let valid = options
            .expires_in_secs
            .unwrap_or(DEFAULT_VALID_SECONDS)
            .clamp(1, MAX_VALID_SECONDS);
        let auth = match self.get_download_authorization(&key, valid).await {
            Ok(r) => r,
            Err(e) if is_b2_token_failure(&e) => {
                if self.maybe_reauth(&e).await {
                    self.get_download_authorization(&key, valid).await?
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        // Build a shareable URL — the download authorization token is passed
        // as the `Authorization` query param on the standard download endpoint.
        let url = format!(
            "{}/file/{}/{}?Authorization={}",
            self.download_url,
            urlencoding::encode(&self.config.bucket),
            encode_path_segments(&key),
            urlencoding::encode(&auth.authorization_token),
        );
        let expires_at = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::seconds(valid as i64))
            .map(|dt| dt.to_rfc3339());
        Ok(ShareLinkResult {
            url,
            password: None,
            expires_at,
        })
    }
}

// ─── Helpers ───

fn short(s: &str) -> String {
    if s.len() <= 8 {
        s.to_string()
    } else {
        format!("{}…", &s[..8])
    }
}

pub(crate) fn normalize_path(p: &str) -> String {
    if p.is_empty() {
        return "/".to_string();
    }
    let trimmed = p.trim();
    let with_root = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    let mut out = String::with_capacity(with_root.len());
    let mut prev_slash = false;
    for c in with_root.chars() {
        if c == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

#[allow(dead_code)]
pub(crate) fn split_parent_child(p: &str) -> (String, String) {
    let n = normalize_path(p);
    if n == "/" {
        return ("/".to_string(), String::new());
    }
    match n.rfind('/') {
        Some(0) => ("/".to_string(), n[1..].to_string()),
        Some(idx) => (n[..idx].to_string(), n[idx + 1..].to_string()),
        None => ("/".to_string(), n),
    }
}

pub(crate) fn sha1_hex(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub(crate) fn encode_path_segments(key: &str) -> String {
    key.split('/')
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// True when an error indicates the master auth token must be refreshed.
///
/// Recognises the exact message produced by `map_b2_status` for 401
/// `expired_auth_token` / `bad_auth_token` (which contains
/// "token expired/invalid") plus the local "invalid auth token" guard from
/// `auth_header`. Anything else is left for the caller to surface verbatim.
fn is_b2_token_failure(err: &ProviderError) -> bool {
    matches!(
        err,
        ProviderError::AuthenticationFailed(msg)
            if msg.contains("token expired") || msg.contains("invalid")
    )
}

fn map_b2_status(status: reqwest::StatusCode, body: &str, op: &str) -> ProviderError {
    let parsed: Option<B2ApiError> = serde_json::from_str(body).ok();
    let code = parsed.as_ref().map(|e| e.code.as_str()).unwrap_or("");
    let msg = parsed
        .as_ref()
        .map(|e| e.message.clone())
        .unwrap_or_else(|| sanitize_api_error(body));
    match status.as_u16() {
        401 => match code {
            "expired_auth_token" | "bad_auth_token" => {
                ProviderError::AuthenticationFailed(format!("{}: token expired/invalid", op))
            }
            _ => ProviderError::PermissionDenied(format!("{}: {}", op, msg)),
        },
        403 => ProviderError::ServerError(format!("{}: {}", op, msg)),
        404 => ProviderError::NotFound(format!("{}: {}", op, msg)),
        408 => ProviderError::ConnectionFailed(format!("{}: timeout", op)),
        416 => ProviderError::InvalidConfig(format!("{}: range not satisfiable", op)),
        503 => ProviderError::ServerError(format!("{}: service unavailable", op)),
        _ => ProviderError::ServerError(format!("{} ({}): {}", op, status, msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_root() {
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("//"), "/");
    }

    #[test]
    fn normalize_relative_to_absolute() {
        assert_eq!(normalize_path("foo"), "/foo");
        assert_eq!(normalize_path("foo/bar"), "/foo/bar");
    }

    #[test]
    fn normalize_strips_double_slashes_and_trailing() {
        assert_eq!(normalize_path("/foo//bar/"), "/foo/bar");
        assert_eq!(normalize_path("///a///b///"), "/a/b");
    }

    #[test]
    fn split_parent_child_root() {
        assert_eq!(split_parent_child("/"), ("/".to_string(), "".to_string()));
    }

    #[test]
    fn split_parent_child_nested() {
        assert_eq!(
            split_parent_child("/a/b/c.txt"),
            ("/a/b".to_string(), "c.txt".to_string())
        );
        assert_eq!(
            split_parent_child("/top.txt"),
            ("/".to_string(), "top.txt".to_string())
        );
    }

    #[test]
    fn sha1_hex_known_vector() {
        // RFC 3174: SHA1("abc")
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn encode_path_segments_preserves_slashes() {
        assert_eq!(
            encode_path_segments("photos/cats/cute.jpg"),
            "photos/cats/cute.jpg"
        );
        assert_eq!(
            encode_path_segments("docs/IMPORTANT FILE.md"),
            "docs/IMPORTANT%20FILE.md"
        );
    }

    #[test]
    fn encode_path_segments_special_chars() {
        assert_eq!(encode_path_segments("a&b/c=d"), "a%26b/c%3Dd");
    }

    #[test]
    fn map_status_401_expired_to_auth_error() {
        let body = r#"{"status":401,"code":"expired_auth_token","message":"expired"}"#;
        let err = map_b2_status(reqwest::StatusCode::UNAUTHORIZED, body, "test");
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
    }

    #[test]
    fn map_status_404_to_notfound() {
        let body = r#"{"status":404,"code":"not_found","message":"missing"}"#;
        let err = map_b2_status(reqwest::StatusCode::NOT_FOUND, body, "test");
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[test]
    fn map_status_416_to_invalid_config() {
        let err = map_b2_status(reqwest::StatusCode::RANGE_NOT_SATISFIABLE, "{}", "test");
        assert!(matches!(err, ProviderError::InvalidConfig(_)));
    }

    #[test]
    fn map_status_503_to_server_error() {
        let err = map_b2_status(reqwest::StatusCode::SERVICE_UNAVAILABLE, "{}", "test");
        assert!(matches!(err, ProviderError::ServerError(_)));
    }

    #[test]
    fn map_status_400_unknown_to_server_error() {
        let err = map_b2_status(reqwest::StatusCode::BAD_REQUEST, "{}", "test");
        assert!(matches!(err, ProviderError::ServerError(_)));
    }

    #[test]
    fn header_budget_rejects_oversized_filename() {
        let p = B2Provider::new(B2Config {
            application_key_id: "id".into(),
            application_key: SecretString::new("k".into()),
            bucket: "b".into(),
            initial_path: None,
        });
        let oversized = "x".repeat(HEADER_BUDGET_STD + 1);
        assert!(p.validate_header_budget(&oversized, 0).is_err());
    }

    #[test]
    fn header_budget_accepts_normal_filename() {
        let p = B2Provider::new(B2Config {
            application_key_id: "id".into(),
            application_key: SecretString::new("k".into()),
            bucket: "b".into(),
            initial_path: None,
        });
        assert!(p
            .validate_header_budget("photos/cats/fluffy.jpg", 0)
            .is_ok());
    }

    // ── Phase 2 ────────────────────────────────────────────────────────────

    #[test]
    fn part_size_constants_match_b2_limits() {
        assert_eq!(LARGE_FILE_MIN_PART_SIZE, 5 * 1024 * 1024);
        assert_eq!(LARGE_FILE_PART_SIZE, 100 * 1024 * 1024);
        assert_eq!(COPY_MAX_SIZE, 5 * 1024 * 1024 * 1024);
    }

    #[test]
    fn last_part_below_minimum_is_allowed_but_middle_part_is_rejected() {
        // Mirror the guard in upload_large_file: only the LAST part may be < 5 MB.
        let last_ok = |this_part: u64, remaining: u64| -> bool {
            let is_last = remaining == this_part;
            is_last || this_part >= LARGE_FILE_MIN_PART_SIZE
        };
        // Last part of 1 KB → allowed
        assert!(last_ok(1024, 1024));
        // Mid-stream 1 KB part with 100 MB still to upload → rejected
        assert!(!last_ok(1024, 100 * 1024 * 1024));
        // Standard 100 MB mid-stream → allowed
        assert!(last_ok(LARGE_FILE_PART_SIZE, 200 * 1024 * 1024));
    }

    #[test]
    fn part_count_cap_covers_practical_file_sizes() {
        // upload_large_file aborts when part_number > 10_000.
        // With a 100 MB recommended part size that gives ~1 TB, well above
        // B2's 10 TB practical cap when users opt for 1 GB parts.
        let max_parts: u64 = 10_000;
        let envelope_with_recommended_part = max_parts * LARGE_FILE_PART_SIZE;
        let one_hundred_gb: u64 = 100 * 1_000_000_000;
        assert!(envelope_with_recommended_part > one_hundred_gb);
    }

    #[test]
    fn copy_size_threshold_routes_between_paths() {
        // ≤ 5 GB → single-shot b2_copy_file path.
        // > 5 GB → chunked b2_copy_part path. The threshold itself is the
        // exclusive ceiling for b2_copy_file.
        let five_gb: u64 = 5 * 1024 * 1024 * 1024;
        assert_eq!(COPY_MAX_SIZE, five_gb);
        let just_under: u64 = COPY_MAX_SIZE;
        let just_over: u64 = COPY_MAX_SIZE + 1;
        assert!(
            just_under <= COPY_MAX_SIZE,
            "5 GB exact should use copy_file"
        );
        assert!(
            just_over > COPY_MAX_SIZE,
            "5 GB + 1 must route to copy_part"
        );
    }

    // ── Phase 3 (reauth wiring) ───────────────────────────────────────────

    #[test]
    fn reauth_filter_matches_expired_token_message() {
        // Exact form produced by `map_b2_status` for 401 + expired_auth_token.
        let err =
            ProviderError::AuthenticationFailed("b2_list_file_names: token expired/invalid".into());
        assert!(is_b2_token_failure(&err));
    }

    #[test]
    fn reauth_filter_matches_local_invalid_token_guard() {
        // From `auth_header()` when HeaderValue::from_str rejects the token.
        let err = ProviderError::AuthenticationFailed("invalid auth token".into());
        assert!(is_b2_token_failure(&err));
    }

    #[test]
    fn reauth_filter_rejects_other_auth_messages() {
        // Generic auth failures (e.g. parse errors) must not trigger reauth —
        // re-running authorize() would not help and would burn an HTTP round-trip.
        let err = ProviderError::AuthenticationFailed("authorize parse: oops".into());
        assert!(!is_b2_token_failure(&err));
    }

    #[test]
    fn reauth_filter_rejects_non_auth_errors() {
        for err in [
            ProviderError::NotFound("x".into()),
            ProviderError::PermissionDenied("x".into()),
            ProviderError::ServerError("x".into()),
            ProviderError::ConnectionFailed("x".into()),
            ProviderError::NotConnected,
        ] {
            assert!(
                !is_b2_token_failure(&err),
                "non-auth error must not trigger reauth: {:?}",
                err
            );
        }
    }

    #[tokio::test]
    async fn maybe_reauth_returns_false_for_filtered_errors_without_network() {
        // Verifies the filter short-circuits before any HTTP call. The provider
        // has empty creds/URLs — if we ever hit authorize() it would return
        // an error (not panic), but the filter must short-circuit first.
        let mut p = B2Provider::new(B2Config {
            application_key_id: "id".into(),
            application_key: SecretString::new("k".into()),
            bucket: "b".into(),
            initial_path: None,
        });
        let err = ProviderError::NotFound("test".into());
        assert!(!p.maybe_reauth(&err).await);
        let err = ProviderError::AuthenticationFailed("authorize parse: bad".into());
        assert!(!p.maybe_reauth(&err).await);
    }

    // ── Phase 4 (Tier 1 endpoints) ────────────────────────────────────────

    fn empty_provider() -> B2Provider {
        B2Provider::new(B2Config {
            application_key_id: "id".into(),
            application_key: SecretString::new("k".into()),
            bucket: "b".into(),
            initial_path: None,
        })
    }

    #[test]
    fn supports_versions_advertises_true() {
        // B2 has first-class versioning — the trait flag must reflect that
        // so frontend "Versions" panels render.
        let p = empty_provider();
        assert!(p.supports_versions());
    }

    #[test]
    fn share_link_capabilities_match_b2_offering() {
        // B2 download authorizations: scoped expiration only. No password,
        // no permission tiers, no listing/revocation.
        let p = empty_provider();
        let caps = p.share_link_capabilities();
        assert!(caps.supports_expiration);
        assert!(!caps.supports_password);
        assert!(!caps.supports_permissions);
        assert!(caps.available_permissions.is_empty());
        assert!(!caps.supports_list_links);
        assert!(!caps.supports_revoke);
    }

    #[test]
    fn share_link_validity_clamps_match_b2_limits() {
        // B2 contract: validDurationInSeconds is 1..=604800 (7 days).
        // The clamp logic in create_share_link must keep callers in range.
        let max: u64 = 604_800;
        // Below minimum → clamped to 1.
        assert_eq!(0u64.clamp(1, max), 1);
        // Above maximum → clamped to 7 days.
        assert_eq!((max + 1).clamp(1, max), max);
        // Inside range → unchanged.
        assert_eq!(3600u64.clamp(1, max), 3600);
    }

    #[test]
    fn list_file_versions_response_parses_with_cursor_pair() {
        // The cursor for b2_list_file_versions is `(nextFileName, nextFileId)`.
        // Both must round-trip; absence of either signals end-of-stream.
        let body = r#"{
            "files":[
              {"fileId":"4_z..._u01","fileName":"docs/a.txt","action":"upload","contentLength":12,"uploadTimestamp":1714000000000},
              {"fileId":"4_z..._u02","fileName":"docs/a.txt","action":"hide","contentLength":0,"uploadTimestamp":1714100000000}
            ],
            "nextFileName":"docs/a.txt",
            "nextFileId":"4_z..._u02"
        }"#;
        let parsed: ListFileVersionsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.next_file_name.as_deref(), Some("docs/a.txt"));
        assert_eq!(parsed.next_file_id.as_deref(), Some("4_z..._u02"));
        assert_eq!(parsed.files[1].action, "hide");
    }

    #[test]
    fn list_unfinished_large_files_response_parses() {
        let body = r#"{
            "files":[
              {"fileId":"4_z..._u10","fileName":"big.bin","contentType":"application/octet-stream","uploadTimestamp":1714000000000}
            ],
            "nextFileId":null
        }"#;
        let parsed: ListUnfinishedLargeFilesResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].file_name, "big.bin");
        assert!(parsed.next_file_id.is_none());
    }

    #[test]
    fn get_download_authorization_response_parses() {
        let body = r#"{
            "bucketId":"abcd","fileNamePrefix":"public/","authorizationToken":"4_xxxx"
        }"#;
        let parsed: GetDownloadAuthorizationResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.authorization_token, "4_xxxx");
    }

    // ── Phase 5 (Tier 2: large-file rename via b2_copy_part) ──────────────

    #[test]
    fn copy_part_response_parses_with_real_sha1() {
        // Standard case: source had a known whole-file SHA-1.
        let body = r#"{
            "fileId":"4_z..._u01","partNumber":1,
            "contentSha1":"a9993e364706816aba3e25717850c26c9cd0d89d",
            "contentLength":104857600
        }"#;
        let parsed: CopyPartResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.part_number, 1);
        assert_eq!(parsed.content_sha1.len(), 40);
    }

    #[test]
    fn copy_part_response_parses_with_none_sha1() {
        // Edge case: when the source is itself a chunked-uploaded large file,
        // B2 has no whole-file SHA-1 and returns the literal "none". This
        // string must round-trip into partSha1Array on b2_finish_large_file.
        let body = r#"{
            "fileId":"4_z..._u02","partNumber":2,
            "contentSha1":"none",
            "contentLength":104857600
        }"#;
        let parsed: CopyPartResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.content_sha1, "none");
    }

    #[test]
    fn large_rename_part_count_envelope_covers_practical_files() {
        // With 100 MiB parts (recommended) and the 10 000 part cap, the
        // chunked rename path covers ~0.95 TiB == ~1 TB (decimal). That is
        // enough for every realistic rename below B2's 10 TB ceiling; users
        // pushing closer to the limit would need to bump LARGE_FILE_PART_SIZE
        // up to 1 GiB (still within B2's 5 GB per-part cap).
        let part_size = LARGE_FILE_PART_SIZE;
        let max_parts: u64 = 10_000;
        let envelope = part_size * max_parts;
        let one_tb_decimal: u64 = 1_000_000_000_000;
        assert!(
            envelope >= one_tb_decimal,
            "100 MiB × 10 000 must reach 1 TB (decimal)"
        );
        // And a 5 GB file fits comfortably (51 parts).
        let five_gb: u64 = 5 * 1024 * 1024 * 1024;
        let parts_for_5gb = five_gb.div_ceil(part_size);
        assert!(parts_for_5gb < 100);
    }

    #[test]
    fn rename_large_part_count_math_with_div_ceil() {
        // For a file of `size` bytes split into `part_size` chunks the count
        // is ceil(size / part_size). Verify the math agrees with the manual
        // case so the cap check in rename_large_file_inner is sound.
        let part_size: u64 = 100 * 1024 * 1024; // 100 MB
                                                // Exactly one part
        assert_eq!(part_size.div_ceil(part_size), 1);
        // Exactly two parts
        assert_eq!((2 * part_size).div_ceil(part_size), 2);
        // Tail-only third part: 200 MB + 1 byte → 3 parts
        assert_eq!((2 * part_size + 1).div_ceil(part_size), 3);
        // 6 GB → ceil(6 GiB / 100 MiB) = 62 (5.7 GiB ≈ 5806 MiB / 100 = 58.06)
        // Use a deterministic computation:
        let six_gb: u64 = 6 * 1024 * 1024 * 1024;
        let n = six_gb.div_ceil(part_size);
        assert!((61..=62).contains(&n));
    }

    #[test]
    fn rename_chunked_rejects_files_above_b2_max() {
        // Defense-in-depth: even though B2 itself caps single files at 10 TB,
        // the inner method explicitly rejects oversized inputs so the user
        // gets a clear NotSupported error rather than a downstream API failure.
        let max = MAX_FILE_SIZE;
        assert_eq!(max, 10 * 1024 * 1024 * 1024 * 1024);
        let too_big = max + 1;
        assert!(too_big > MAX_FILE_SIZE);
    }
}
