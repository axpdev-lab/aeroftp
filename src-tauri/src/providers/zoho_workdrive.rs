//! Zoho WorkDrive Storage Provider
//!
//! Implements StorageProvider for Zoho WorkDrive using the WorkDrive API v1.
//! Uses OAuth2 for authentication. API follows JSON:API specification.
//!
//! Zoho WorkDrive hierarchy:
//!   Team → Workspaces → Team Folders → Files/Folders
//!
//! For personal use, we navigate the user's "privatespace" (My Folders).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::HttpResultExt;
use async_trait::async_trait;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{de, Deserialize, Deserializer};
use std::collections::HashMap;
use tracing::{debug, info};

use super::{
    http_retry::{send_with_retry_replayable, HttpRetryConfig},
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
    sanitize_api_error, FileVersion, ProviderError, ProviderType, RemoteEntry,
    ShareLinkCapabilities, ShareLinkInfo, ShareLinkOptions, ShareLinkResult, StorageInfo,
    StorageProvider,
};

/// Zoho WorkDrive provider configuration
#[derive(Debug, Clone)]
pub struct ZohoWorkdriveConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Zoho region: "us", "eu", "in", "au", "jp", "uk", "ca", "sa"
    pub region: String,
}

impl ZohoWorkdriveConfig {
    pub fn new(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            region: region.to_string(),
        }
    }

    /// Zoho API domain for this region (different from web domain!)
    /// Web: workdrive.zoho.{tld}: API: www.zohoapis.{ext}
    fn api_domain(&self) -> &str {
        match self.region.as_str() {
            "eu" => "www.zohoapis.eu",
            "in" => "www.zohoapis.in",
            "au" => "www.zohoapis.com.au",
            "jp" => "www.zohoapis.jp",
            "uk" => "www.zohoapis.uk",
            "ca" => "www.zohoapis.ca",
            "sa" => "www.zohoapis.sa",
            "cn" => "www.zohoapis.com.cn",
            "ae" => "www.zohoapis.ae",
            _ => "www.zohoapis.com", // US default
        }
    }

    /// Download domain for this region (dedicated download servers).
    /// Different from API domain! API: www.zohoapis.{ext}: Download: download.zoho.{tld}
    fn download_domain(&self) -> &str {
        match self.region.as_str() {
            "eu" => "download.zoho.eu",
            "in" => "download.zoho.in",
            "au" => "download.zoho.com.au",
            "jp" => "download.zoho.jp",
            "cn" => "download.zoho.com.cn",
            "ae" => "files.zoho.ae",
            "ca" => "download.zohocloud.ca",
            "sa" => "files.zoho.sa",
            "uk" => "download.zoho.uk",
            _ => "download.zoho.com", // US default
        }
    }

    /// Upload server of the stream upload API for this region, as the Upload
    /// Large File API page lists them:
    /// https://www.zoho.com/workdrive/developer/docs/api/v1/upload-large-file-250mb.html
    /// The page lists no server for the UK data centre, so that region keeps
    /// the single-request upload (and its 250 MB limit) rather than a guessed
    /// host.
    fn upload_domain(&self) -> Option<&'static str> {
        match self.region.as_str() {
            "eu" => Some("upload.zoho.eu"),
            "in" => Some("upload.zoho.in"),
            "au" => Some("upload.zoho.com.au"),
            "jp" => Some("upload.zoho.jp"),
            "cn" => Some("upload.zoho.com.cn"),
            "ae" => Some("files.zoho.ae"),
            "ca" => Some("upload.zohocloud.ca"),
            "sa" => Some("files.zoho.sa"),
            "uk" => None,
            _ => Some("upload.zoho.com"), // US default, as api_domain
        }
    }

    /// API base URL for this region: www.zohoapis.{ext}/workdrive/api/v1
    /// Used for metadata operations (list, mkdir, delete, rename).
    pub fn api_base(&self) -> String {
        format!("https://{}/workdrive/api/v1", self.api_domain())
    }
}

// ── JSON:API response structures ──────────────────────────────────────────

/// JSON:API wrapper for a single resource
#[derive(Debug, Deserialize)]
struct JsonApiResponse<T> {
    data: T,
}

/// JSON:API wrapper for a list of resources
#[derive(Debug, Deserialize)]
struct JsonApiListResponse<T> {
    data: Vec<T>,
}

/// One page of a folder listing, with the cursor to the next page.
#[derive(Debug, Deserialize)]
struct FolderPage {
    data: Vec<FileResource>,
    #[serde(default)]
    links: Option<PageLinks>,
}

#[derive(Debug, Deserialize)]
struct PageLinks {
    #[serde(default)]
    cursor: Option<PageCursor>,
}

/// `next` is the URL of the next page; its `page[next]` is the cursor.
#[derive(Debug, Deserialize)]
struct PageCursor {
    #[serde(default)]
    has_next: bool,
    #[serde(default)]
    next: Option<String>,
}

/// Team resource
#[derive(Debug, Deserialize)]
struct TeamResource {
    id: String,
    attributes: TeamAttributes,
}

#[derive(Debug, Deserialize)]
struct TeamAttributes {
    name: String,
    #[serde(
        default,
        alias = "used_storage",
        alias = "storage_used_in_bytes",
        alias = "used_storage_in_bytes",
        deserialize_with = "deserialize_optional_byte_amount"
    )]
    storage_used: Option<u64>,
    #[serde(
        default,
        alias = "storage_limit",
        alias = "storage_total",
        alias = "storage_quota_in_bytes",
        alias = "quota_in_bytes",
        deserialize_with = "deserialize_optional_byte_amount"
    )]
    storage_quota: Option<u64>,
}

fn deserialize_optional_byte_amount<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .map(|v| v as u64)
            })
            .ok_or_else(|| de::Error::custom("invalid byte amount number"))
            .map(Some),
        Some(serde_json::Value::String(s)) => {
            parse_byte_amount(&s).map(Some).map_err(de::Error::custom)
        }
        Some(other) => Err(de::Error::custom(format!("invalid byte amount: {}", other))),
    }
}

fn parse_byte_amount(raw: &str) -> Result<u64, String> {
    let cleaned = raw.trim().replace(',', "");
    if cleaned.is_empty() {
        return Ok(0);
    }
    let (number_part, unit_part) = if cleaned.contains(char::is_whitespace) {
        let mut parts = cleaned.split_whitespace();
        (
            parts
                .next()
                .ok_or_else(|| "empty byte amount".to_string())?,
            parts.next().unwrap_or("bytes"),
        )
    } else {
        let split_at = cleaned
            .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .unwrap_or(cleaned.len());
        let (number, unit) = cleaned.split_at(split_at);
        (number, if unit.is_empty() { "bytes" } else { unit })
    };
    let number = number_part
        .parse::<f64>()
        .map_err(|e| format!("invalid byte amount '{}': {}", raw, e))?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!("invalid byte amount '{}'", raw));
    }
    let unit = unit_part.to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "b" | "byte" | "bytes" => 1_f64,
        "kb" | "kib" => 1024_f64,
        "mb" | "mib" => 1024_f64.powi(2),
        "gb" | "gib" => 1024_f64.powi(3),
        "tb" | "tib" => 1024_f64.powi(4),
        "pb" | "pib" => 1024_f64.powi(5),
        _ => return Err(format!("unknown byte unit '{}'", unit)),
    };
    Ok((number * multiplier).round() as u64)
}

/// Largest file the single-request `POST /upload` accepts: "This API will
/// only upload files of size up to 250MB", and it answers 413 above that.
/// Read as decimal megabytes, the smaller reading, so a file either reading
/// would refuse takes the stream upload instead.
/// https://www.zoho.com/workdrive/developer/docs/api/v1/upload-file.html
const ZOHO_SINGLE_UPLOAD_MAX_BYTES: u64 = 250 * 1000 * 1000;

/// The upload server to use for a file of `size`, or `None` for the
/// single-request upload: at or below its documented limit, or in a region
/// whose upload server is not documented.
fn large_upload_host(config: &ZohoWorkdriveConfig, size: u64) -> Option<&'static str> {
    if size <= ZOHO_SINGLE_UPLOAD_MAX_BYTES {
        return None;
    }
    config.upload_domain()
}

/// A folder Zoho says is not there is that folder's failure. Anything else, a 403
/// included, may speak of the whole session, and stays a failure a search stops on.
fn list_folder_failure(status: reqwest::StatusCode, folder_id: &str, body: &str) -> ProviderError {
    let message = format!(
        "List files error {} (folder_id={}): {}",
        status,
        folder_id,
        sanitize_api_error(body)
    );
    if status == reqwest::StatusCode::NOT_FOUND {
        ProviderError::NotFound(message)
    } else {
        ProviderError::Other(message)
    }
}

/// A WorkDrive answer whose body broke off before it was read. Read as empty
/// text it failed later as `EOF while parsing a value at line 1 column 0`.
fn body_broke_off(what: &str, error: reqwest::Error) -> ProviderError {
    ProviderError::NetworkError(format!(
        "{what}: the answer broke off before its body was read: {error}"
    ))
}

/// The name an upload sends, in the single-request upload's `filename` field
/// and in the stream upload's `x-filename`: URL-encoded, since WorkDrive
/// decodes both.
fn upload_filename_field(file_name: &str) -> String {
    urlencoding::encode(file_name).into_owned()
}

/// Entries asked for per listing request, as rclone asks. At 50 a folder of
/// 1000 entries took 20 requests, and the 20th listing of one folder within
/// a minute is refused (see [`FolderListings`]).
const LIST_PAGE_LIMIT: usize = 1000;

/// The cursor of the page after one of `count` entries, or `None` after the
/// last. The answer's cursor decides, an empty page included. Without a
/// cursor in the answer a short page is the last, as it was with offset
/// paging; a full one is an error, since the entries after it could not be
/// asked for and a listing without them reads them as deleted.
fn next_page_cursor(links: Option<&PageLinks>, count: usize) -> Result<Option<String>, String> {
    match links.and_then(|links| links.cursor.as_ref()) {
        Some(cursor) if cursor.has_next => cursor
            .next
            .as_deref()
            .and_then(|next| reqwest::Url::parse(next).ok())
            .and_then(|next| {
                next.query_pairs()
                    .find(|(key, _)| key == "page[next]")
                    .map(|(_, value)| value.into_owned())
            })
            .filter(|cursor| !cursor.is_empty())
            .map(Some)
            .ok_or_else(|| {
                "more entries follow, but the answer names no cursor to them".to_string()
            }),
        Some(_) => Ok(None),
        None if count < LIST_PAGE_LIMIT => Ok(None),
        None => Err(format!(
            "a page of {count} entries came without a cursor to the rest"
        )),
    }
}

/// The steady rate of WorkDrive API calls, rclone's default for Zoho: its
/// comments say a faster pace drains the account's allowance and brings
/// 429 answers with a Retry-After near 119 s. Not measured here.
const ZOHO_CALLS_PER_SECOND: f64 = 6.0;

/// Listings of one folder WorkDrive accepts within [`FOLDER_LISTING_WINDOW`].
/// Measured 2026-11-01: a folder listed 20 times in 4.2 s answered the 20th
/// with `429 F7008`, and refused its listings for the next 600 to 615 s,
/// its Retry-After counting down to 0. rclone measured the same 19, in a
/// rolling window of about 60 s.
const FOLDER_LISTINGS_PER_WINDOW: usize = 19;

/// rclone's 60 s window, plus a second for the clocks of both ends.
const FOLDER_LISTING_WINDOW: std::time::Duration = std::time::Duration::from_secs(61);

/// What a Retry-After asks, plus a second (rclone's margin): a listing sent
/// when the Retry-After had reached 0 was refused again (measured 2026-11-01).
const RETRY_AFTER_MARGIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The wait after a 429 without Retry-After: rclone's, a minute and a second.
const RATE_LIMIT_FALLBACK: std::time::Duration = std::time::Duration::from_secs(61);

/// The longest Retry-After honoured. A refused listing measured waits about
/// 600 s; the shared parser stops at 300 s, and the retry it timed was
/// refused again with 299 s left.
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// How long WorkDrive asks a refused request to wait: whole seconds, as it
/// sends them, or an HTTP date read by the shared parser.
fn zoho_retry_after(response: &reqwest::Response) -> Option<std::time::Duration> {
    let value = response.headers().get(reqwest::header::RETRY_AFTER)?;
    match value.to_str().ok()?.trim().parse::<u64>() {
        Ok(secs) => Some(std::time::Duration::from_secs(secs).min(RETRY_AFTER_CAP)),
        Err(_) => super::http_retry::parse_retry_after(response),
    }
}

/// The wait before sending again what WorkDrive answered with `status`: a 429
/// waits what Retry-After asks plus a margin, or [`RATE_LIMIT_FALLBACK`]; a
/// 5xx waits the shared backoff, or Retry-After when there is one.
fn zoho_retry_wait(
    status: reqwest::StatusCode,
    retry_after: Option<std::time::Duration>,
    attempt: u32,
    config: &HttpRetryConfig,
) -> std::time::Duration {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        retry_after.map_or(RATE_LIMIT_FALLBACK, |asked| asked + RETRY_AFTER_MARGIN)
    } else {
        retry_after.unwrap_or_else(|| super::http_retry::calculate_delay(attempt, config))
    }
}

/// A 5xx, which WorkDrive can answer after it carried the request out. A
/// 429 comes before.
fn is_server_failure(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 500 | 502 | 503 | 504)
}

/// Which answers [`ZohoWorkdriveProvider::send_api`] sends a request again on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resend {
    /// A 429 or a 5xx: sent twice, the request leaves what it leaves once.
    RefusedOrFailed,
    /// A 429 only. A request that creates something is not sent again after
    /// a 5xx without looking first, or it can create it twice.
    RefusedOnly,
}

/// The recent listings of each folder, so that a folder is listed at most
/// `limit` times in any `window`: a listing past that waits until the
/// oldest of them leaves the window. Every page is one listing, since it is
/// one request to the folder.
struct FolderListings {
    limit: usize,
    window: std::time::Duration,
    recent: std::sync::Mutex<HashMap<String, std::collections::VecDeque<tokio::time::Instant>>>,
}

impl FolderListings {
    fn new(limit: usize, window: std::time::Duration) -> Self {
        Self {
            limit,
            window,
            recent: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Take a turn to list `folder` at `now`, or say how long to wait for one.
    fn reserve(&self, folder: &str, now: tokio::time::Instant) -> Option<std::time::Duration> {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.window;
        // Folders listed long ago are dropped once many are remembered.
        if recent.len() > 4096 {
            recent.retain(|_, stamps| stamps.back().is_some_and(|last| now < *last + window));
        }
        let stamps = recent.entry(folder.to_string()).or_default();
        while stamps.front().is_some_and(|first| *first + window <= now) {
            stamps.pop_front();
        }
        match stamps.front() {
            Some(first) if stamps.len() >= self.limit => Some(*first + window - now),
            _ => {
                stamps.push_back(now);
                None
            }
        }
    }
}

/// The pace of the WorkDrive requests of this process, one for all its
/// connections: WorkDrive counts the calls of an account and the listings
/// of a folder, not those of a connection. Two accounts used at once share
/// it, the cautious side.
struct ZohoPace {
    calls: super::tpslimit::TpsLimiter,
    listings: FolderListings,
}

impl ZohoPace {
    fn new() -> Self {
        Self {
            calls: super::tpslimit::TpsLimiter::new(ZOHO_CALLS_PER_SECOND, 1.0),
            listings: FolderListings::new(FOLDER_LISTINGS_PER_WINDOW, FOLDER_LISTING_WINDOW),
        }
    }

    fn shared() -> std::sync::Arc<Self> {
        static SHARED: std::sync::OnceLock<std::sync::Arc<ZohoPace>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| std::sync::Arc::new(ZohoPace::new()))
            .clone()
    }
}

/// Whether a stream upload response reports `file_name` as stored. The API
/// answers with the files it stored, `{"data":[{"attributes":{"file_name":
/// ...}}]}` (observed live on 2026-09-24), and leaves failed ones out, so an
/// absent name is a failure even under a success status.
fn stream_upload_stored(response: &str, file_name: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|v| v.get("data").and_then(|d| d.as_array()).cloned())
        .map(|entries| {
            entries.iter().any(|e| {
                e.pointer("/attributes/file_name").and_then(|n| n.as_str()) == Some(file_name)
            })
        })
        .unwrap_or(false)
}

/// The first `max` bytes of a response for a log line, cut on a character
/// boundary: slicing at a raw byte index panics when a multibyte character
/// (a file name in a Zoho response, for one) straddles it, and that panic
/// would fail an upload that had already been stored.
fn log_preview(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

const ZOHO_WORKDRIVE_FALLBACK_QUOTA_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Error reported when neither a team nor a personal privatespace can be found.
/// Personal accounts have no team, so "create a team" is never the only answer:
/// an empty result almost always means the wrong region or WorkDrive not enabled.
const NO_TEAM_NO_PRIVATESPACE_ERROR: &str = "No WorkDrive team and no personal My Folders (privatespace) found for this account. Check that the configured region matches your Zoho account (eu, com, in, com.au, jp, uk, ca, sa) and that WorkDrive is enabled for it. Personal accounts connect through the privatespace of their own user id, so an empty result is usually a region mismatch.";

const ZOHO_USED_QUOTA_KEYS: &[&str] = &[
    "storage_used",
    "used_storage",
    "storageUsed",
    "usedStorage",
    "storage_used_in_bytes",
    "used_storage_in_bytes",
    "storageUsedInBytes",
    "usedStorageInBytes",
    "used_space",
    "usedSpace",
    "space_used",
    "spaceUsed",
    "consumed_storage",
    "consumedStorage",
];

const ZOHO_TOTAL_QUOTA_KEYS: &[&str] = &[
    "storage_quota",
    "storage_limit",
    "storage_total",
    "storageQuota",
    "storageLimit",
    "storageTotal",
    "storage_quota_in_bytes",
    "quota_in_bytes",
    "storageQuotaInBytes",
    "quotaInBytes",
    "total_storage",
    "totalStorage",
    "total_space",
    "totalSpace",
    "space_total",
    "spaceTotal",
    "allocated_storage",
    "allocatedStorage",
    "quota",
];

fn parse_byte_amount_value(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| v as u64)
        }),
        serde_json::Value::String(s) => parse_byte_amount(s).ok(),
        serde_json::Value::Object(map) => map
            .get("bytes")
            .or_else(|| map.get("value"))
            .and_then(parse_byte_amount_value),
        _ => None,
    }
}

fn find_byte_amount_by_keys(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    match value {
        serde_json::Value::Object(map) => {
            for key in keys {
                if let Some(parsed) = map.get(*key).and_then(parse_byte_amount_value) {
                    return Some(parsed);
                }
            }
            map.values()
                .find_map(|nested| find_byte_amount_by_keys(nested, keys))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|nested| find_byte_amount_by_keys(nested, keys)),
        _ => None,
    }
}

/// Team current user info (for getting teamUserID)
#[derive(Debug, Deserialize)]
struct TeamCurrentUserResource {
    id: String,
}

/// Privatespace / workspace info
#[derive(Debug, Deserialize)]
struct WorkspaceResource {
    id: String,
}

/// Extract the privatespace (My Folders) root id from a
/// `GET /users/{id}/privatespace` body. Zoho answers either with a JSON:API
/// list (`{"data": [...]}`) or with a single object (`{"data": {...}}`),
/// depending on account type, so both shapes are accepted.
fn parse_privatespace_id(body: &str) -> Option<String> {
    if let Ok(list) = serde_json::from_str::<JsonApiListResponse<WorkspaceResource>>(body) {
        if let Some(first) = list.data.into_iter().next() {
            return Some(first.id);
        }
    }
    serde_json::from_str::<JsonApiResponse<WorkspaceResource>>(body)
        .ok()
        .map(|single| single.data.id)
}

/// Workspace list resource (for GET /teams/{id}/workspaces)
#[derive(Debug, Deserialize)]
struct WorkspaceListResource {
    id: String,
    #[allow(dead_code)]
    attributes: WorkspaceListAttributes,
}

#[derive(Debug, Deserialize)]
struct WorkspaceListAttributes {
    #[allow(dead_code)]
    name: String,
}

/// Team Folder resource (inside a workspace)
#[derive(Debug, Deserialize)]
struct TeamFolderResource {
    id: String,
    attributes: TeamFolderAttributes,
}

#[derive(Debug, Deserialize)]
struct TeamFolderAttributes {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    modified_time: Option<String>,
}

/// File/Folder resource from WorkDrive API
#[derive(Debug, Deserialize)]
struct FileResource {
    id: String,
    attributes: FileAttributes,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FileAttributes {
    #[serde(rename = "name")]
    name: String,
    /// "file" or "folder" (Zoho may use either "type" or "is_folder")
    #[serde(rename = "type", default)]
    file_type: String,
    /// Fallback: boolean is_folder field
    #[serde(default)]
    is_folder: Option<bool>,
    #[serde(rename = "storage_info", default)]
    storage_info: Option<StorageInfoAttr>,
    #[serde(rename = "modified_time", default)]
    modified_time: Option<String>,
    #[serde(rename = "modified_time_i18", default)]
    modified_time_i18: Option<String>,
    /// File extension (e.g. "pdf", "docx")
    #[serde(default)]
    extn: Option<String>,
    /// Direct download URL provided by Zoho (e.g. "https://download-accl.zoho.com/v1/workdrive/download/{id}")
    #[serde(default)]
    download_url: Option<String>,
    /// Labels applied to this file (returned by file list/detail APIs)
    #[serde(default)]
    labels: Option<Vec<serde_json::Value>>,
}

impl FileResource {
    fn is_folder(&self) -> bool {
        self.attributes.file_type == "folder" || self.attributes.is_folder.unwrap_or(false)
    }
}

#[derive(Debug, Deserialize)]
struct StorageInfoAttr {
    /// Human-readable size string (e.g. "6.35 KB"): not used for parsing
    #[serde(default)]
    #[allow(dead_code)]
    size: Option<String>,
    /// Numeric size in bytes
    #[serde(default)]
    size_in_bytes: Option<u64>,
}

/// User info from /users/me
#[derive(Debug, Deserialize)]
struct UserResource {
    #[allow(dead_code)]
    id: String,
    attributes: UserAttributes,
}

#[derive(Debug, Deserialize)]
struct UserAttributes {
    email_id: Option<String>,
    /// WorkDrive role (e.g. "admin", "organizer")
    #[serde(default)]
    #[allow(dead_code)]
    role: Option<String>,
    /// Account edition (e.g. "PERSONAL", "TEAM"): logged for diagnostics only,
    /// discovery never gates on it because some accounts omit the field.
    #[serde(default)]
    edition: Option<String>,
}

// ── Share link response structures ────────────────────────────────────────

/// Share link resource from WorkDrive Links API
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ZohoShareLink {
    pub id: String,
    pub attributes: ZohoShareLinkAttributes,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ZohoShareLinkAttributes {
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub link_name: Option<String>,
    #[serde(default)]
    pub link_type: Option<String>,
    #[serde(default)]
    pub allow_download: Option<bool>,
    #[serde(default)]
    pub request_user_data: Option<bool>,
    #[serde(default)]
    pub expiry_date: Option<String>,
    #[serde(default)]
    pub is_password_protected: Option<bool>,
    #[serde(default)]
    pub created_time: Option<String>,
    #[serde(default)]
    pub resource_id: Option<String>,
}

// ── Version response structures ───────────────────────────────────────────

/// Version resource from WorkDrive API
#[derive(Debug, Deserialize)]
struct VersionResource {
    id: String,
    attributes: VersionAttributes,
}

#[derive(Debug, Deserialize)]
struct VersionAttributes {
    #[serde(default)]
    modified_time: Option<String>,
    #[serde(default)]
    modified_time_in_millisecond: Option<i64>,
    #[serde(rename = "storage_info", default)]
    storage_info: Option<StorageInfoAttr>,
    #[serde(default)]
    modified_by: Option<String>,
    #[serde(default)]
    version_number: Option<String>,
    /// author display name (some API responses use "author_name")
    #[serde(default)]
    author_name: Option<String>,
}

// ── Zoho native document export map ───────────────────────────────────────

/// Zoho native doc type → (Writer/Sheet/Show API path, export format, Office extension)
/// When downloading native docs, we use the Zoho Writer/Sheet/Show API for conversion.
const ZOHO_NATIVE_EXPORT_MAP: &[(&str, &str, &str, &str)] = &[
    // (extension/file_type, API product path, export format, Office extension)
    ("zw", "writer", "docx", ".docx"),
    ("zohowriter", "writer", "docx", ".docx"),
    ("writer", "writer", "docx", ".docx"),
    ("zs", "sheet", "xlsx", ".xlsx"),
    ("zohosheet", "sheet", "xlsx", ".xlsx"),
    ("spreadsheet", "sheet", "xlsx", ".xlsx"),
    ("zp", "show", "pptx", ".pptx"),
    ("zohoshow", "show", "pptx", ".pptx"),
    ("presentation", "show", "pptx", ".pptx"),
];

/// Body of a relocation (`PATCH {api_base}/files`).
///
/// WorkDrive has no per-file `/files/{id}/move` route: that URL is rejected by
/// the API gateway itself with `F6016 — URL Rule is not configured` (#451).
/// Relocating is a bulk metadata write on the *collection* endpoint, so `data`
/// is an array and each element carries its own item id.
fn move_request_body(file_id: &str, parent_id: &str) -> serde_json::Value {
    serde_json::json!({
        "data": [{
            "attributes": { "parent_id": parent_id },
            "id": file_id,
            "type": "files"
        }]
    })
}

/// Check if file extension is a Zoho native document, return export info
fn zoho_native_export_info(extn: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let lower = extn.to_lowercase();
    ZOHO_NATIVE_EXPORT_MAP
        .iter()
        .find(|(ext, _, _, _)| *ext == lower)
        .map(|(_, product, format, office_ext)| (*product, *format, *office_ext))
}

// ── Provider implementation ───────────────────────────────────────────────

/// Zoho WorkDrive Storage Provider
pub struct ZohoWorkdriveProvider {
    config: ZohoWorkdriveConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    /// Current folder ID (starts with privatespace root)
    current_folder_id: String,
    /// Human-readable current path
    current_path: String,
    /// Cache: path → folder_id
    folder_cache: HashMap<String, String>,
    /// Team ID (discovered on connect)
    team_id: Option<String>,
    /// Privatespace (workspace) ID: needs different list endpoint than folder IDs
    privatespace_id: Option<String>,
    /// Authenticated user email
    account_email: Option<String>,
    /// Team-specific user ID (for labels API)
    team_user_id: Option<String>,
    /// Team folders discovered during connect: (id, name)
    team_folders: Vec<(String, String)>,
    /// Server profile identifier owning these OAuth tokens. Empty when the
    /// caller has not bound a profile (legacy singleton key path). Issue #214.
    profile_id: String,
    /// The pace WorkDrive accepts, shared by every connection of the process.
    /// A test gets one per connection, so tests do not wait on each other.
    pace: std::sync::Arc<ZohoPace>,
    /// How often, and after which backoff, a failed request is sent again.
    /// A test shortens the backoff.
    retry: HttpRetryConfig,
    /// Test-only base URL standing in for both the API domain and the upload
    /// server (`http://127.0.0.1:port`). Production paths never set it.
    #[cfg(test)]
    endpoint_override: Option<String>,
    /// Test-only access token for local HTTP fixtures (bypasses the vault).
    #[cfg(test)]
    test_access_token: Option<String>,
}

/// Pick rclone's `root_folder_id` from a completed `discover_team`.
///
/// rclone lists `GET /files/{root}/files`. The privatespace (My Folders)
/// id is that root. `discover_team`'s last fallback stores the *team* id
/// in `current_folder_id`; using it reproduces F6016, the failure this
/// helper exists to prevent. A `current_folder_id` that is not the team
/// id is a real folder and is kept.
fn rclone_root_folder_id_from_discovery(
    privatespace_id: Option<&str>,
    current_folder_id: &str,
    team_id: Option<&str>,
) -> Option<String> {
    if let Some(id) = privatespace_id.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(id.to_string());
    }
    let current = current_folder_id.trim();
    if current.is_empty() {
        return None;
    }
    if team_id.map(str::trim) == Some(current) {
        return None;
    }
    Some(current.to_string())
}

impl ZohoWorkdriveProvider {
    /// Build the provider and its HTTP client, with the limits of [`crate::providers::http_limits`].
    /// Nothing is sent until the first request.
    pub fn new(config: ZohoWorkdriveConfig) -> Self {
        // Build HTTP client with default Accept header required by Zoho JSON:API
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.api+json"));
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .default_headers(default_headers)
            .connect_timeout(crate::providers::http_limits::limit(
                std::time::Duration::from_secs(30),
            ))
            .read_timeout(crate::providers::http_limits::limit(
                crate::providers::http_limits::READ_TIMEOUT,
            ))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client,
            connected: false,
            current_folder_id: String::new(),
            current_path: "/".to_string(),
            folder_cache: HashMap::new(),
            team_id: None,
            privatespace_id: None,
            account_email: None,
            team_user_id: None,
            team_folders: Vec::new(),
            profile_id: String::new(),
            retry: HttpRetryConfig::default(),
            pace: if cfg!(test) {
                std::sync::Arc::new(ZohoPace::new())
            } else {
                ZohoPace::shared()
            },
            #[cfg(test)]
            endpoint_override: None,
            #[cfg(test)]
            test_access_token: None,
        }
    }

    /// Bind this provider to a server profile so OAuth tokens are stored
    /// under the per-profile vault key. Issue #214.
    /// Keep token refresh in the account selected by the CLI or MCP profile.
    pub fn with_credential_user(mut self, user_id: Option<i64>) -> Self {
        self.oauth_manager = self.oauth_manager.with_credential_user(user_id);
        self
    }

    pub fn with_profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = profile_id.into();
        self
    }

    /// Discover the privatespace (My Folders) id rclone needs as
    /// `root_folder_id`. Without it rclone lists `GET /files//files` and Zoho
    /// answers F6016. Uses the same team/privatespace walk as `connect`.
    pub async fn rclone_root_folder_id_for_export(
        profile_id: &str,
        client_id: &str,
        client_secret: &str,
        region: &str,
    ) -> Option<String> {
        if profile_id.is_empty() {
            return None;
        }
        let mut provider = Self::new(ZohoWorkdriveConfig::new(client_id, client_secret, region))
            .with_profile_id(profile_id);
        if provider.discover_team().await.is_err() {
            return None;
        }
        rclone_root_folder_id_from_discovery(
            provider.privatespace_id.as_deref(),
            &provider.current_folder_id,
            provider.team_id.as_deref(),
        )
    }

    /// Get OAuth config for token operations
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::zoho(
            &self.config.client_id,
            &self.config.client_secret,
            &self.config.region,
        )
        .with_profile_id(&self.profile_id)
    }

    /// Get authorization header with valid token
    ///
    /// Every WorkDrive request asks for this header just before it goes out,
    /// so this is where the requests keep to [`ZOHO_CALLS_PER_SECOND`]. The
    /// turn is taken once the token is in hand: taken before, the requests
    /// that waited on a token refresh would all go out together after it.
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        #[cfg(test)]
        if let Some(token) = &self.test_access_token {
            let header = HeaderValue::from_str(&format!("Zoho-oauthtoken {token}"))
                .map_err(|_| ProviderError::Other("Invalid token characters".into()))?;
            self.pace.calls.acquire().await;
            return Ok(header);
        }
        let token = self
            .oauth_manager
            .get_valid_token(&self.oauth_config())
            .await?;
        // Zoho uses "Zoho-oauthtoken" format, but "Bearer" also works with newer APIs
        let header = HeaderValue::from_str(&format!("Zoho-oauthtoken {}", token.expose_secret()))
            .map_err(|_| ProviderError::Other("Invalid token characters".into()))?;
        self.pace.calls.acquire().await;
        Ok(header)
    }

    /// API base URL
    fn api_base(&self) -> String {
        #[cfg(test)]
        if let Some(base) = &self.endpoint_override {
            return format!("{base}/workdrive/api/v1");
        }
        self.config.api_base()
    }

    /// Stream upload endpoint on the region's upload server.
    fn stream_upload_url(&self, host: &str) -> String {
        #[cfg(test)]
        if let Some(base) = &self.endpoint_override {
            return format!("{base}/workdrive-api/v1/stream/upload");
        }
        format!("https://{host}/workdrive-api/v1/stream/upload")
    }

    /// Upload through the stream API of the upload server (Upload Large File
    /// API): the file is the raw request body, and name, folder and mode go in
    /// headers. The response lists only the files that were stored, so a
    /// success status with no file in it is a failure, not an upload.
    /// https://www.zoho.com/workdrive/developer/docs/api/v1/upload-large-file-250mb.html
    async fn stream_upload(
        &self,
        host: &str,
        parent_id: &str,
        file_name: &str,
        total_size: u64,
        body: reqwest::Body,
    ) -> Result<(), ProviderError> {
        let url = self.stream_upload_url(host);
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(reqwest::header::CONTENT_LENGTH, total_size)
            // "The name of the file should be URL encoded with UTF-8 Charset."
            .header("x-filename", upload_filename_field(file_name))
            .header("x-parent_id", parent_id)
            .header("upload-id", uuid::Uuid::new_v4().to_string())
            .header("x-streammode", "1")
            .header("x-override-name-exist", "true")
            .body(body)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let text = resp.text().await;
        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "Upload failed ({}): {}",
                status,
                sanitize_api_error(&text.unwrap_or_default())
            )));
        }
        let text = text.map_http_err(|e| body_broke_off("Upload failed", e))?;
        debug!(
            "Zoho WorkDrive stream upload response ({}): {}",
            status,
            log_preview(&text, 300)
        );
        if !stream_upload_stored(&text, file_name) {
            return Err(ProviderError::Other(format!(
                "Upload failed: Zoho WorkDrive accepted the stream upload of {file_name} but did not report it as stored"
            )));
        }
        Ok(())
    }

    // ── Zoho-specific trash management ─────────────────────────────────

    /// Helper: PATCH a single file/folder status (1=active, 51=trash, 61=delete)
    async fn patch_file_status(&self, file_id: &str, status: &str) -> Result<(), ProviderError> {
        let url = format!("{}/files/{}", self.api_base(), file_id);
        let body = serde_json::json!({
            "data": {
                "attributes": { "status": status },
                "type": "files"
            }
        });

        let resp = self
            .client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .body(body.to_string())
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status_code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "PATCH status {} failed ({}): {}",
                status,
                status_code,
                sanitize_api_error(&text)
            )));
        }
        Ok(())
    }

    /// Helper: PATCH multiple files/folders status in batch (max 200 per Zoho API)
    async fn patch_files_status_batch(
        &self,
        file_ids: &[String],
        status: &str,
    ) -> Result<(), ProviderError> {
        if file_ids.is_empty() {
            return Ok(());
        }

        // Zoho allows max 200 objects per batch
        for chunk in file_ids.chunks(200) {
            let items: Vec<serde_json::Value> = chunk
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "attributes": { "status": status },
                        "id": id,
                        "type": "files"
                    })
                })
                .collect();

            let body = serde_json::json!({ "data": items });
            let url = format!("{}/files", self.api_base());

            let resp = self
                .client
                .patch(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.api+json"),
                )
                .body(body.to_string())
                .send()
                .await
                .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !resp.status().is_success() {
                let status_code = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Batch PATCH status {} failed ({}): {}",
                    status,
                    status_code,
                    sanitize_api_error(&text)
                )));
            }
        }
        Ok(())
    }

    /// Permanently delete a file/folder (must be in trash first)
    pub async fn permanent_delete(&self, file_id: &str) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive permanent delete: {}", file_id);
        self.patch_file_status(file_id, "61").await?;
        info!("Permanently deleted: {}", file_id);
        Ok(())
    }

    /// Permanently delete multiple files/folders in batch (max 200 per request)
    pub async fn permanent_delete_batch(&self, file_ids: &[String]) -> Result<(), ProviderError> {
        info!(
            "Zoho WorkDrive permanent delete batch: {} items",
            file_ids.len()
        );
        self.patch_files_status_batch(file_ids, "61").await?;
        info!("Permanently deleted {} items", file_ids.len());
        Ok(())
    }

    /// Restore a file/folder from trash to its original location
    pub async fn restore_from_trash(&self, file_id: &str) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive restore from trash: {}", file_id);
        self.patch_file_status(file_id, "1").await?;
        info!("Restored from trash: {}", file_id);
        Ok(())
    }

    /// Restore multiple files/folders from trash in batch (max 200 per request)
    pub async fn restore_from_trash_batch(&self, file_ids: &[String]) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive restore batch: {} items", file_ids.len());
        self.patch_files_status_batch(file_ids, "1").await?;
        info!("Restored {} items from trash", file_ids.len());
        Ok(())
    }

    /// List trashed files from privatespace and team folders (with pagination)
    pub async fn list_trash(&self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut results = Vec::new();
        let page_limit = 50;

        // List privatespace trash (paginated)
        if let Some(ref ps_id) = self.privatespace_id {
            let mut offset = 0;
            loop {
                let url = format!(
                    "{}/privatespace/{}/trashedfiles?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(),
                    ps_id,
                    page_limit,
                    offset
                );
                info!("Zoho WorkDrive list trash: GET {}", url);

                let resp = self
                    .client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .send()
                    .await
                    .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                if !resp.status().is_success() {
                    break;
                }

                let body = resp.text().await.unwrap_or_default();
                if let Ok(parsed) = serde_json::from_str::<JsonApiListResponse<FileResource>>(&body)
                {
                    let count = parsed.data.len();
                    info!(
                        "Zoho WorkDrive trash (privatespace): {} items at offset {}",
                        count, offset
                    );
                    for file in &parsed.data {
                        results.push(self.to_remote_entry(file, "[Trash]"));
                    }
                    if count < page_limit {
                        break; // Last page
                    }
                    offset += count;
                } else {
                    break;
                }
            }
        }

        // List team folder trash (paginated)
        for tf in &self.team_folders.clone() {
            let mut offset = 0;
            loop {
                let url = format!(
                    "{}/teamfolders/{}/trashedfiles?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(),
                    tf.0,
                    page_limit,
                    offset
                );

                let resp = self
                    .client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .send()
                    .await;

                let resp = match resp {
                    Ok(r) if r.status().is_success() => r,
                    _ => break,
                };

                let body = resp.text().await.unwrap_or_default();
                if let Ok(parsed) = serde_json::from_str::<JsonApiListResponse<FileResource>>(&body)
                {
                    let count = parsed.data.len();
                    info!(
                        "Zoho WorkDrive trash (team folder {}): {} items at offset {}",
                        tf.1, count, offset
                    );
                    for file in &parsed.data {
                        results.push(self.to_remote_entry(file, &format!("[Trash/{}]", tf.1)));
                    }
                    if count < page_limit {
                        break; // Last page
                    }
                    offset += count;
                } else {
                    break;
                }
            }
        }

        info!("Zoho WorkDrive total trash items: {}", results.len());
        Ok(results)
    }

    // ── Zoho-specific label management ─────────────────────────────────

    // ── MCP-parity tools ──────────────────────────────────────────────────

    /// List all share links for a file/folder (MCP parity: getFileShareLinks)
    pub async fn get_file_share_links(
        &mut self,
        path: &str,
    ) -> Result<Vec<ZohoShareLink>, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;

        let url = format!("{}/files/{}/links", self.api_base(), file_id);
        info!(
            "Zoho WorkDrive get share links: GET {} for file {}",
            url, file_id
        );

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let resp_body = resp.text().await;

        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "Get share links failed ({}): {}",
                status,
                sanitize_api_error(&resp_body.unwrap_or_default())
            )));
        }
        let resp_body = resp_body.map_http_err(|e| body_broke_off("Get share links failed", e))?;

        let parsed: JsonApiListResponse<ZohoShareLink> = serde_json::from_str(&resp_body)
            .map_err(|e| ProviderError::ParseError(format!("Parse share links: {}", e)))?;

        info!("Found {} share link(s) for {}", parsed.data.len(), path);
        Ok(parsed.data)
    }

    /// Delete an external share link by ID (MCP parity: deleteExternalShareLink)
    pub async fn delete_share_link(&self, link_id: &str) -> Result<(), ProviderError> {
        let url = format!("{}/links/{}", self.api_base(), link_id);
        info!("Zoho WorkDrive delete share link: DELETE {}", url);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status_code = resp.status().as_u16();
        if status_code != 200 && status_code != 204 {
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Delete share link failed ({}): {}",
                status_code,
                sanitize_api_error(&resp_body)
            )));
        }

        info!("Deleted share link {}", link_id);
        Ok(())
    }

    /// Create a native Zoho document (MCP parity: createNativeDocument)
    /// doc_type: "zw" (Writer), "zs" (Sheet), "zp" (Show/Presentation)
    pub async fn create_native_document(
        &mut self,
        name: &str,
        doc_type: &str,
        folder_path: &str,
    ) -> Result<String, ProviderError> {
        // Validate doc_type
        let zoho_type = match doc_type {
            "writer" | "zw" => "zw",
            "sheet" | "zs" => "zs",
            "show" | "presentation" | "zp" => "zp",
            _ => {
                return Err(ProviderError::Other(format!(
                    "Invalid document type '{}'. Use: writer/zw, sheet/zs, show/zp",
                    doc_type
                )))
            }
        };

        // Resolve parent folder
        let parent_id = self
            .parent_folder_id(folder_path.starts_with('/'), folder_path.trim_matches('/'))
            .await?;

        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "name": name,
                    "parent_id": parent_id,
                    "service_type": zoho_type
                },
                "type": "files"
            }
        });

        let url = format!("{}/files", self.api_base());
        info!(
            "Zoho WorkDrive create native doc '{}' (type {}): POST {}",
            name, zoho_type, url
        );

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .body(body.to_string())
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let resp_body = resp.text().await;

        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "Create native document failed ({}): {}",
                status,
                sanitize_api_error(&resp_body.unwrap_or_default())
            )));
        }
        let resp_body =
            resp_body.map_http_err(|e| body_broke_off("Create native document failed", e))?;

        // Parse response to get the created file's permalink/URL
        let parsed: serde_json::Value = serde_json::from_str(&resp_body)
            .map_err(|e| ProviderError::ParseError(format!("Parse create doc response: {}", e)))?;

        let permalink = parsed["data"]["attributes"]["permalink"]
            .as_str()
            .unwrap_or("");

        let file_id = parsed["data"]["id"].as_str().unwrap_or("unknown");

        info!(
            "Created native Zoho document '{}' (id: {}, type: {})",
            name, file_id, zoho_type
        );
        Ok(if permalink.is_empty() {
            file_id.to_string()
        } else {
            permalink.to_string()
        })
    }

    /// Resolve a file path to its Zoho ID (helper that splits parent/file)
    async fn resolve_file_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let path_is_absolute = path.starts_with('/');
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        Ok(file.id)
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager
            .has_tokens(OAuthProvider::ZohoWorkdrive, &self.profile_id)
    }

    /// Discover team ID and privatespace root folder
    async fn discover_team(&mut self) -> Result<(), ProviderError> {
        // Step 1: Get user info and user ID
        let user_url = format!("{}/users/me", self.api_base());
        info!("Zoho WorkDrive: GET {}", user_url);
        let resp = self
            .client
            .get(&user_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let mut user_id: Option<String> = None;
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<JsonApiResponse<UserResource>>().await {
                self.account_email = body.data.attributes.email_id.clone();
                user_id = Some(body.data.id.clone());
                info!(
                    "Zoho WorkDrive user: {} ({}), edition: {}",
                    body.data.attributes.email_id.as_deref().unwrap_or("?"),
                    body.data.id,
                    body.data.attributes.edition.as_deref().unwrap_or("?")
                );
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /users/me failed ({}): {}", status, text);
        }

        // Step 2: Get teams: try /teams first, then /users/{id}/teams as fallback
        let teams_url = format!("{}/teams", self.api_base());
        info!("Zoho WorkDrive: GET {}", teams_url);
        let resp = self
            .client
            .get(&teams_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let team = if resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive /teams response: {}",
                log_preview(&body, 500)
            );
            match serde_json::from_str::<JsonApiListResponse<TeamResource>>(&body) {
                Ok(teams) if !teams.data.is_empty() => teams.data.into_iter().next(),
                Ok(_) => {
                    info!(
                        "Zoho WorkDrive /teams returned empty list, trying user-specific endpoint"
                    );
                    None
                }
                Err(e) => {
                    info!("Zoho WorkDrive /teams parse error: {}", e);
                    None
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive /teams failed ({}): {}",
                status,
                log_preview(&text, 500)
            );
            None
        };

        // Fallback: try /users/{user_id}/teams
        let team: Option<TeamResource> = if team.is_some() {
            team
        } else if let Some(ref uid) = user_id {
            let user_teams_url = format!("{}/users/{}/teams", self.api_base(), uid);
            info!("Zoho WorkDrive: GET {} (fallback)", user_teams_url);
            let resp = self
                .client
                .get(&user_teams_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                debug!(
                    "Zoho WorkDrive /users/{}/teams response: {}",
                    uid,
                    log_preview(&body, 500)
                );
                match serde_json::from_str::<JsonApiListResponse<TeamResource>>(&body) {
                    Ok(teams) => teams.data.into_iter().next(),
                    Err(e) => {
                        info!("Zoho WorkDrive /users/{}/teams parse error: {}", uid, e);
                        None
                    }
                }
            } else {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                debug!(
                    "Zoho WorkDrive /users/{}/teams failed ({}): {}",
                    uid,
                    status,
                    log_preview(&text, 500)
                );
                None
            }
        } else {
            None
        };

        // No team unit at all: personal / no-team account (rclone PERSONAL path).
        // The privatespace (My Folders) of the ZUID is then the whole account root,
        // so discovery continues there instead of failing.
        let team = match team {
            Some(team) => team,
            None => {
                let uid = user_id.clone().ok_or_else(|| {
                    ProviderError::Other(NO_TEAM_NO_PRIVATESPACE_ERROR.to_string())
                })?;
                info!(
                    "Zoho WorkDrive: no teams; trying personal privatespace for ZUID {}",
                    uid
                );
                self.team_id = None;
                // Personal accounts have no team-scoped user id: the ZUID plays
                // that role for the endpoints that need one (labels, privatespace).
                self.team_user_id = Some(uid.clone());
                if self.discover_privatespace(&uid).await? {
                    info!("Zoho WorkDrive: connected as personal account (no team)");
                    return Ok(());
                }
                return Err(ProviderError::Other(
                    NO_TEAM_NO_PRIVATESPACE_ERROR.to_string(),
                ));
            }
        };

        self.team_id = Some(team.id.clone());
        info!(
            "Zoho WorkDrive team: {} ({})",
            team.attributes.name, team.id
        );

        // Step 3: Get team-specific user ID (different from ZUID)
        // rclone flow: /teams/{teamID}/currentuser → teamUserID
        let cu_url = format!("{}/teams/{}/currentuser", self.api_base(), team.id);
        info!("Zoho WorkDrive: GET {}", cu_url);
        let resp = self
            .client
            .get(&cu_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let team_user_id = if resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive /currentuser response: {}",
                log_preview(&body_text, 500)
            );
            match serde_json::from_str::<JsonApiResponse<TeamCurrentUserResource>>(&body_text) {
                Ok(cu) => {
                    info!("Zoho WorkDrive teamUserID: {}", cu.data.id);
                    Some(cu.data.id)
                }
                Err(e) => {
                    info!("Zoho WorkDrive /currentuser parse error: {}", e);
                    None
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive /currentuser failed ({}): {}",
                status,
                log_preview(&text, 300)
            );
            None
        };
        self.team_user_id = team_user_id.clone();

        // Step 4: Get privatespace (My Folders) root using teamUserID
        // rclone flow: /users/{teamUserID}/privatespace → root folder ID
        let ps_user_id = team_user_id
            .clone()
            .or_else(|| user_id.clone())
            .unwrap_or_else(|| team.id.clone());
        if self.discover_privatespace(&ps_user_id).await? {
            self.discover_team_folders(&team.id).await;
            return Ok(());
        }

        // Last fallback: use team ID as root
        self.current_folder_id = team.id.clone();
        self.folder_cache.insert("/".to_string(), team.id.clone());

        // Continue to fetch team folders (don't return early)
        self.discover_team_folders(&team.id).await;

        Ok(())
    }

    /// Resolve the privatespace (My Folders) root for `user_id` and make it the
    /// current root. Returns `false` when the endpoint fails or returns no
    /// workspace, so the caller can fall back (team accounts) or report a
    /// precise error (personal accounts).
    async fn discover_privatespace(&mut self, user_id: &str) -> Result<bool, ProviderError> {
        let private_url = format!("{}/users/{}/privatespace", self.api_base(), user_id);
        info!("Zoho WorkDrive: GET {}", private_url);
        let resp = self
            .client
            .get(&private_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive /users/{}/privatespace failed ({}): {}",
                user_id,
                status,
                log_preview(&text, 300)
            );
            return Ok(false);
        }

        let body_text = resp.text().await.unwrap_or_default();
        debug!(
            "Zoho WorkDrive privatespace response: {}",
            log_preview(&body_text, 500)
        );

        match parse_privatespace_id(&body_text) {
            Some(id) => {
                self.privatespace_id = Some(id.clone());
                self.current_folder_id = id.clone();
                self.folder_cache.insert("/".to_string(), id);
                info!(
                    "Zoho WorkDrive privatespace root: {}",
                    self.current_folder_id
                );
                Ok(true)
            }
            None => {
                info!("Zoho WorkDrive privatespace parse failed for both array and object formats");
                Ok(false)
            }
        }
    }

    /// Storage quota for a personal account (no team unit): `/teams/{id}` does
    /// not exist there, so quota is read from the user record and falls back to
    /// the free WorkDrive quota when Zoho omits the fields.
    async fn personal_storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/users/me", self.api_base());
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let mut used = 0u64;
        let mut total = 0u64;
        if resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
                used = find_byte_amount_by_keys(&raw, ZOHO_USED_QUOTA_KEYS).unwrap_or(0);
                total = find_byte_amount_by_keys(&raw, ZOHO_TOTAL_QUOTA_KEYS).unwrap_or(0);
            }
        } else {
            debug!(
                "Zoho WorkDrive personal storage_info /users/me failed ({})",
                resp.status()
            );
        }

        if total == 0 {
            debug!(
                "Zoho WorkDrive personal storage_info: no quota in /users/me, using 5 GB fallback"
            );
            total = ZOHO_WORKDRIVE_FALLBACK_QUOTA_BYTES;
        }
        if used > total {
            total = used;
        }

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    /// Discover team folders.
    /// Primary: GET /teams/{teamID}/teamfolders (documented endpoint)
    /// Fallback: GET /teams/{teamID}/workspaces (workspaces as virtual folders)
    async fn discover_team_folders(&mut self, team_id: &str) {
        // Primary: GET /teams/{teamId}/teamfolders
        let tf_url = format!("{}/teams/{}/teamfolders", self.api_base(), team_id);
        info!("Zoho WorkDrive: GET {}", tf_url);

        let auth = match self.auth_header().await {
            Ok(h) => h,
            Err(e) => {
                info!("Zoho WorkDrive: auth error for team folders: {}", e);
                return;
            }
        };

        let resp = match self
            .client
            .get(&tf_url)
            .header(AUTHORIZATION, auth)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                info!("Zoho WorkDrive: team folders request failed: {}", e);
                return;
            }
        };

        if resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive teamfolders response: {}",
                log_preview(&body, 500)
            );

            if let Ok(tfs) = serde_json::from_str::<JsonApiListResponse<TeamFolderResource>>(&body)
            {
                for tf in tfs.data {
                    info!(
                        "Zoho WorkDrive team folder: {} ({})",
                        tf.attributes.name, tf.id
                    );
                    self.folder_cache
                        .insert(format!("/{}", tf.attributes.name), tf.id.clone());
                    self.team_folders.push((tf.id, tf.attributes.name));
                }
                if !self.team_folders.is_empty() {
                    info!(
                        "Zoho WorkDrive discovered {} team folders",
                        self.team_folders.len()
                    );
                    return;
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive teamfolders failed ({}): {}",
                status,
                log_preview(&text, 300)
            );
        }

        // Fallback: use workspaces as team folders (they appear as "Cartelle Team" in the UI)
        let ws_url = format!("{}/teams/{}/workspaces", self.api_base(), team_id);
        info!(
            "Zoho WorkDrive: GET {} (fallback: treating workspaces as team folders)",
            ws_url
        );

        let auth = match self.auth_header().await {
            Ok(h) => h,
            Err(_) => return,
        };

        let resp = match self
            .client
            .get(&ws_url)
            .header(AUTHORIZATION, auth)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                info!("Zoho WorkDrive: workspaces request failed: {}", e);
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive workspaces failed ({}): {}",
                status,
                log_preview(&text, 300)
            );
            return;
        }

        let body = resp.text().await.unwrap_or_default();
        debug!(
            "Zoho WorkDrive workspaces response: {}",
            log_preview(&body, 500)
        );

        if let Ok(ws_list) =
            serde_json::from_str::<JsonApiListResponse<WorkspaceListResource>>(&body)
        {
            for ws in ws_list.data {
                info!(
                    "Zoho WorkDrive workspace as team folder: {} ({})",
                    ws.attributes.name, ws.id
                );
                self.folder_cache
                    .insert(format!("/{}", ws.attributes.name), ws.id.clone());
                self.team_folders.push((ws.id, ws.attributes.name));
            }
        }

        info!(
            "Zoho WorkDrive discovered {} team folders (via workspaces)",
            self.team_folders.len()
        );
    }

    /// Send one WorkDrive request at the pace WorkDrive accepts. `listing` is
    /// the folder a listing reads, which takes a turn in [`FolderListings`].
    /// `build` gets the authorization header, asked again for every attempt.
    /// A 429, and a 5xx when `resend` says so, is sent again up to three
    /// times, after [`zoho_retry_wait`].
    async fn send_api(
        &self,
        listing: Option<&str>,
        resend: Resend,
        build: impl Fn(HeaderValue) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let config = self.retry.clone();
        // This loop owns the retries; the shared sender still pays the CLI's
        // tpslimit and records each attempt's time to first byte.
        let single_attempt = HttpRetryConfig {
            max_retries: 0,
            ..config.clone()
        };
        let mut retries = 0;
        loop {
            let auth = self.auth_header().await?;
            // A listing takes its folder's turn as it goes out, after the
            // account's pace and the token, so the window counts sends.
            if let Some(folder) = listing {
                let listings = &self.pace.listings;
                if let Some(wait) = listings.reserve(folder, tokio::time::Instant::now()) {
                    debug!(
                        "Zoho WorkDrive: folder {} listed {} times in {:?}, next listing in {:?}",
                        folder, listings.limit, listings.window, wait
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
            let response =
                send_with_retry_replayable(&self.client, || build(auth.clone()), &single_attempt)
                    .await
                    .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
            let status = response.status();
            let sent_again = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || (resend == Resend::RefusedOrFailed && is_server_failure(status));
            if retries >= config.max_retries || !sent_again {
                return Ok(response);
            }
            let retry_after = zoho_retry_after(&response);
            let wait = zoho_retry_wait(status, retry_after, retries, &config);
            info!(
                "Zoho WorkDrive answered {} to {} (Retry-After {:?}); retry {}/{} in {:?}",
                status,
                response.url().path(),
                response.headers().get(reqwest::header::RETRY_AFTER),
                retries + 1,
                config.max_retries,
                wait
            );
            retries += 1;
            // Release the connection before a wait that can last minutes.
            drop(response);
            tokio::time::sleep(wait).await;
        }
    }

    /// List files in a folder by ID.
    /// - Privatespace root: GET /privatespace/{id}/files
    /// - All other folders:  GET /files/{id}/files (universal endpoint)
    async fn list_folder(&self, folder_id: &str) -> Result<Vec<FileResource>, ProviderError> {
        let mut all_files = Vec::new();
        let mut cursor = "0".to_string();
        // A page that names an earlier one as the next would list forever.
        let mut sent = std::collections::HashSet::new();

        // Privatespace root needs its own endpoint; everything else uses /files/{id}/files
        let collection = if self.privatespace_id.as_deref() == Some(folder_id) {
            "privatespace"
        } else {
            "files"
        };

        loop {
            sent.insert(cursor.clone());
            let url = format!(
                "{}/{}/{}/files?page%5Blimit%5D={}&page%5Bnext%5D={}",
                self.api_base(),
                collection,
                folder_id,
                LIST_PAGE_LIMIT,
                urlencoding::encode(&cursor)
            );
            let page = self.list_page(&url, folder_id).await?;
            let count = page.data.len();
            info!("Zoho WorkDrive list parsed {} items", count);
            debug!(
                "Zoho WorkDrive list page cursor (folder_id={}): has_next {:?}",
                folder_id,
                page.links
                    .as_ref()
                    .and_then(|links| links.cursor.as_ref())
                    .map(|cursor| cursor.has_next)
            );
            all_files.extend(page.data);
            match next_page_cursor(page.links.as_ref(), count) {
                Ok(Some(next)) if sent.contains(&next) => {
                    return Err(ProviderError::Other(format!(
                        "List files error (folder_id={folder_id}): a page named one already listed as the next"
                    )))
                }
                Ok(Some(next)) => cursor = next,
                Ok(None) => break,
                Err(why) => {
                    return Err(ProviderError::Other(format!(
                        "List files error (folder_id={folder_id}): {why}"
                    )))
                }
            }
        }

        Ok(all_files)
    }

    /// One page of a folder listing. A body that breaks off or comes empty
    /// under a success status is asked for once more, then is an error that
    /// says so: read as empty text it failed as `Parse files error: EOF while
    /// parsing a value at line 1 column 0`, naming neither cause.
    async fn list_page(&self, url: &str, folder_id: &str) -> Result<FolderPage, ProviderError> {
        let mut asked_again = false;
        loop {
            let resp = self
                .send_api(Some(folder_id), Resend::RefusedOrFailed, |auth| {
                    self.client.get(url).header(AUTHORIZATION, auth)
                })
                .await?;

            let status = resp.status();
            // Logged once answered: the turn of a folder can wait a minute.
            info!("Zoho WorkDrive list: GET {} answered {}", url, status);
            let body = resp.text().await;

            if !status.is_success() {
                let body_text = body.unwrap_or_default();
                debug!(
                    "Zoho WorkDrive list error ({}): {}",
                    status,
                    log_preview(&body_text, 300)
                );
                return Err(list_folder_failure(status, folder_id, &body_text));
            }

            let body_text = match body {
                Ok(text) if !text.trim().is_empty() => text,
                unread if !asked_again => {
                    info!(
                        "Zoho WorkDrive list (folder_id={}): {}, asking again",
                        folder_id,
                        unread.err().map_or("empty body".to_string(), |e| e.to_string())
                    );
                    asked_again = true;
                    continue;
                }
                Ok(_) => {
                    return Err(ProviderError::NetworkError(format!(
                        "List files error (folder_id={folder_id}): WorkDrive answered {status} with an empty body, twice"
                    )))
                }
                Err(e) => {
                    return Err(ProviderError::NetworkError(format!(
                        "List files error (folder_id={folder_id}): the answer broke off before its body was read, twice: {e}"
                    )))
                }
            };

            debug!(
                "Zoho WorkDrive list response ({} bytes): {}",
                body_text.len(),
                log_preview(&body_text, 500)
            );

            return serde_json::from_str(&body_text).map_err(|e| {
                ProviderError::Other(format!(
                    "Parse files error: {}: body: {}",
                    e,
                    sanitize_api_error(&body_text)
                ))
            });
        }
    }

    /// Convert a FileResource to RemoteEntry
    fn to_remote_entry(&self, file: &FileResource, parent_path: &str) -> RemoteEntry {
        let is_dir = file.is_folder();
        let size = file
            .attributes
            .storage_info
            .as_ref()
            .and_then(|s| s.size_in_bytes)
            .unwrap_or(0);

        let path = if parent_path == "/" {
            format!("/{}", file.attributes.name)
        } else {
            format!(
                "{}/{}",
                parent_path.trim_end_matches('/'),
                file.attributes.name
            )
        };

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), file.id.clone());

        // Include labels in metadata if present (serialized as JSON array of {name, color})
        if let Some(labels) = &file.attributes.labels {
            if !labels.is_empty() {
                let label_data: Vec<serde_json::Value> = labels
                    .iter()
                    .filter_map(|lv| {
                        // Labels can be full objects or just ID strings
                        if let Some(obj) = lv.as_object() {
                            let attrs = obj.get("attributes").and_then(|a| a.as_object());
                            let name = attrs
                                .and_then(|a| a.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let color = attrs
                                .and_then(|a| a.get("color"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !name.is_empty() {
                                Some(serde_json::json!({"name": name, "color": color}))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                if !label_data.is_empty() {
                    metadata.insert(
                        "zoho_labels".to_string(),
                        serde_json::to_string(&label_data).unwrap_or_default(),
                    );
                }
            }
        }

        // Store file_type and extn for debugging / native doc detection
        if !file.attributes.file_type.is_empty() {
            metadata.insert("file_type".to_string(), file.attributes.file_type.clone());
        }
        if let Some(ref extn) = file.attributes.extn {
            metadata.insert("extn".to_string(), extn.clone());
        }

        // Detect Zoho native documents by extn OR file_type
        let native_export = file
            .attributes
            .extn
            .as_deref()
            .and_then(zoho_native_export_info)
            .or_else(|| zoho_native_export_info(&file.attributes.file_type));
        if let Some((_, format, office_ext)) = native_export {
            metadata.insert("isZohoNativeDoc".to_string(), "true".to_string());
            metadata.insert("exportFormat".to_string(), format.to_string());
            metadata.insert("exportExtension".to_string(), office_ext.to_string());
        }

        RemoteEntry {
            name: file.attributes.name.clone(),
            path,
            is_dir,
            size,
            modified: file
                .attributes
                .modified_time_i18
                .clone()
                .or_else(|| file.attributes.modified_time.clone()),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        }
    }

    /// List root directory: team folders (as directories) + privatespace files
    async fn list_root(&self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut entries = Vec::new();

        // 1. Team folders as virtual directory entries
        for (id, name) in &self.team_folders {
            let mut metadata = HashMap::new();
            metadata.insert("id".to_string(), id.clone());
            metadata.insert("resource_type".to_string(), "teamfolder".to_string());
            entries.push(RemoteEntry {
                name: name.clone(),
                path: format!("/{}", name),
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata,
            });
        }

        // 2. Privatespace files (My Folders contents): merge at root level
        if let Some(ref ps_id) = self.privatespace_id {
            match self.list_folder(ps_id).await {
                Ok(files) => {
                    for f in &files {
                        entries.push(self.to_remote_entry(f, "/"));
                    }
                    info!(
                        "Zoho WorkDrive root: {} team folders + {} privatespace items",
                        self.team_folders.len(),
                        files.len()
                    );
                }
                Err(e) => {
                    info!("Zoho WorkDrive privatespace list error: {}", e);
                    // Cached team folders do not make a failed privatespace
                    // listing complete. A missing private file is unverified.
                    return Err(e);
                }
            }
        } else {
            info!("Zoho WorkDrive: no privatespace_id set");
        }

        if entries.is_empty() {
            info!(
                "Zoho WorkDrive root listing is empty: no team folders and no privatespace files"
            );
        }

        Ok(entries)
    }

    /// Find a file/folder by name in a parent folder
    async fn find_by_name(
        &self,
        name: &str,
        parent_id: &str,
    ) -> Result<Option<FileResource>, ProviderError> {
        let files = self.list_folder(parent_id).await?;
        Ok(files.into_iter().find(|f| f.attributes.name == name))
    }

    /// Refuse to undo a first step onto `name` in the folder `parent_id`
    /// when an item other than `file_id` took that name since: what
    /// WorkDrive does with a taken name is not documented. `path` names the
    /// way back in the error.
    async fn way_back_is_free(
        &self,
        name: &str,
        parent_id: &str,
        file_id: &str,
        path: &str,
    ) -> Result<(), ProviderError> {
        match self.find_by_name(name, parent_id).await? {
            Some(found) if found.id != file_id => Err(ProviderError::AlreadyExists(format!(
                "{path} was taken by another item, so the first step was not undone"
            ))),
            _ => Ok(()),
        }
    }

    /// Move the item `file_id` into the folder `to_parent_id` under its name.
    async fn move_file_into(
        &self,
        file_id: &str,
        to_parent_id: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        let move_body = move_request_body(file_id, to_parent_id);
        let url = format!("{}/files", self.api_base());
        let payload = move_body.to_string();
        let resp = self
            .send_api(None, Resend::RefusedOrFailed, |auth| {
                self.client
                    .patch(&url)
                    .header(AUTHORIZATION, auth)
                    .header(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/vnd.api+json"),
                    )
                    .body(payload.clone())
            })
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::CONFLICT {
                return Err(ProviderError::AlreadyExists(to.to_string()));
            }
            return Err(ProviderError::Other(format!(
                "Move failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }
        Ok(())
    }

    /// Rename the item `file_id` to `new_name` in the folder it is in.
    async fn rename_file_in_place(
        &self,
        file_id: &str,
        new_name: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        let rename_body = serde_json::json!({
            "data": {
                "attributes": {
                    "name": new_name
                },
                "type": "files"
            }
        });
        let url = format!("{}/files/{}", self.api_base(), file_id);
        let payload = rename_body.to_string();
        let resp = self
            .send_api(None, Resend::RefusedOrFailed, |auth| {
                self.client
                    .patch(&url)
                    .header(AUTHORIZATION, auth)
                    .header(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/vnd.api+json"),
                    )
                    .body(payload.clone())
            })
            .await?;
        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::CONFLICT {
                return Err(ProviderError::AlreadyExists(to.to_string()));
            }
            return Err(ProviderError::Other(format!(
                "Rename failed: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    /// Move `from` into the folder of `to` and/or rename it: two calls when
    /// both change. The folder cache is `rename`'s to clean up.
    async fn move_then_rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path_is_absolute = from.starts_with('/');
        let from_path = from.trim_matches('/');
        let (from_parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let from_parent_id = self
            .parent_folder_id(from_path_is_absolute, from_parent_path)
            .await?;

        let file = self
            .find_by_name(file_name, &from_parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        let to_path_is_absolute = to.starts_with('/');
        let to_path = to.trim_matches('/');
        let (to_parent_path, new_name) = if let Some(pos) = to_path.rfind('/') {
            (&to_path[..pos], &to_path[pos + 1..])
        } else {
            ("", to_path)
        };

        let to_parent_id = self
            .parent_folder_id(to_path_is_absolute, to_parent_path)
            .await?;

        let is_cross_folder = from_parent_id != to_parent_id;
        let is_rename = file_name != new_name;

        // The move keeps the old name. When the destination folder already
        // holds it, the rename goes first, in the source folder, so neither
        // step puts two items under one name; when the source folder also
        // holds the new name, either order would, and nothing is changed.
        let rename_first = is_cross_folder
            && is_rename
            && self.find_by_name(file_name, &to_parent_id).await?.is_some();
        if rename_first
            && self
                .find_by_name(new_name, &from_parent_id)
                .await?
                .is_some()
        {
            return Err(ProviderError::Other(format!(
                "Cannot move {from} to {to} in two steps without two items sharing a name: \
                 the destination folder holds {file_name} and the source folder holds {new_name}"
            )));
        }
        // Two steps when both the folder and the name change. If the second
        // fails, the first is undone, and if that fails too the error says
        // where the item is.
        let at = |absolute: bool, parent: &str, name: &str| {
            let slash = if absolute { "/" } else { "" };
            if parent.is_empty() {
                format!("{slash}{name}")
            } else {
                format!("{slash}{parent}/{name}")
            }
        };
        if !is_cross_folder {
            if is_rename {
                self.rename_file_in_place(&file.id, new_name, to).await?;
            }
        } else if rename_first {
            self.rename_file_in_place(&file.id, new_name, to).await?;
            if let Err(e) = self.move_file_into(&file.id, &to_parent_id, to).await {
                let undone = match self
                    .way_back_is_free(file_name, &from_parent_id, &file.id, from)
                    .await
                {
                    Ok(()) => self.rename_file_in_place(&file.id, file_name, from).await,
                    Err(e) => Err(e),
                };
                let now_at = at(from_path_is_absolute, from_parent_path, new_name);
                return Err(super::second_step_failed(from, to, &now_at, e, undone));
            }
        } else {
            self.move_file_into(&file.id, &to_parent_id, to).await?;
            info!("Moved {} to folder {}", from, to_parent_path);
            if is_rename {
                if let Err(e) = self.rename_file_in_place(&file.id, new_name, to).await {
                    let undone = match self
                        .way_back_is_free(file_name, &from_parent_id, &file.id, from)
                        .await
                    {
                        Ok(()) => self.move_file_into(&file.id, &from_parent_id, from).await,
                        Err(e) => Err(e),
                    };
                    let now_at = at(to_path_is_absolute, to_parent_path, file_name);
                    return Err(super::second_step_failed(from, to, &now_at, e, undone));
                }
            }
        }

        info!("Renamed {} to {}", from, to);
        Ok(())
    }

    /// Drop the cached ids of the folder at `path` (absolute, or relative to
    /// the current folder) and of every folder under it. A rename, move or
    /// delete leaves them pointing at a folder that is no longer there: after
    /// `/a` moved to `/b`, a new `/a` still resolved to the moved folder, and
    /// deleting `/a/x` deleted `/b/x`.
    fn forget_folder(&mut self, path: &str) {
        let trimmed = path.trim_matches('/');
        let path = if path.starts_with('/') {
            format!("/{trimmed}")
        } else {
            format!("{}/{trimmed}", self.current_path.trim_end_matches('/'))
        };
        let below = format!("{}/", path.trim_end_matches('/'));
        self.folder_cache
            .retain(|cached, _| *cached != path && !cached.starts_with(&below));
    }

    /// The key [`Self::resolve_path`] caches `path` under: absolute, one slash
    /// between names. A relative path is read from the current folder.
    fn cache_key(&self, path: &str) -> String {
        let absolute = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{path}", self.current_path)
        };
        let names: Vec<&str> = absolute.split('/').filter(|s| !s.is_empty()).collect();
        format!("/{}", names.join("/"))
    }

    /// Cache the ids of the folders the listing of `folder` (a
    /// [`Self::cache_key`]) shows, so listing one of them next does not list
    /// `folder` again to find it. A tree walk listed each folder once more
    /// per subfolder, and WorkDrive refuses the 20th listing of one folder
    /// within a minute. The listing is what `folder` holds now: a child cached
    /// under a name it shows for no folder, or for another folder, is
    /// forgotten with what was cached below it. A name shown twice is left to
    /// [`Self::resolve_path`], which takes the first.
    fn remember_listed_folders(&mut self, folder: &str, entries: &[RemoteEntry]) {
        let prefix = if folder == "/" {
            "/".to_string()
        } else {
            format!("{folder}/")
        };
        let mut shown: HashMap<&str, usize> = HashMap::new();
        for entry in entries {
            *shown.entry(entry.name.as_str()).or_default() += 1;
        }
        let listed: HashMap<String, String> = entries
            .iter()
            .filter(|e| e.is_dir && shown[e.name.as_str()] == 1)
            .filter(|e| !e.name.is_empty() && !e.name.contains('/'))
            .filter_map(|e| Some((format!("{prefix}{}", e.name), e.metadata.get("id")?.clone())))
            .collect();
        let stale: Vec<String> = self
            .folder_cache
            .iter()
            .filter(|(path, id)| {
                path.strip_prefix(prefix.as_str())
                    .is_some_and(|name| !name.is_empty() && !name.contains('/'))
                    && listed.get(path.as_str()) != Some(*id)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            self.forget_folder(&path);
        }
        self.folder_cache.extend(listed);
    }

    /// The id of `parent`, the folder part of a path already stripped of
    /// its slashes. An absolute path resolves from the root (the cached
    /// `/`) and a relative one from the current folder, as on every other
    /// provider. Until 2026-09-25 a one-segment path went to the current
    /// folder even with a leading slash, and a longer relative path went to
    /// the root.
    async fn parent_folder_id(
        &mut self,
        absolute: bool,
        parent: &str,
    ) -> Result<String, ProviderError> {
        if absolute {
            return self.resolve_path(parent).await;
        }
        if parent.is_empty() {
            return Ok(self.current_folder_id.clone());
        }
        let joined = format!("{}/{parent}", self.current_path.trim_end_matches('/'));
        self.resolve_path(&joined).await
    }

    /// Resolve a path like "/docs/file.txt" to a folder/file ID.
    /// First path component is checked against team folder names, then privatespace.
    async fn resolve_path(&mut self, path: &str) -> Result<String, ProviderError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(self
                .folder_cache
                .get("/")
                .cloned()
                .unwrap_or_else(|| self.current_folder_id.clone()));
        }

        // Check cache
        let full_path = format!("/{}", path);
        if let Some(id) = self.folder_cache.get(&full_path) {
            return Ok(id.clone());
        }

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        // First component: check team folders by name, then privatespace contents
        let first = parts[0];
        let mut current_path = format!("/{}", first);

        let mut current_id = if let Some(cached) = self.folder_cache.get(&current_path) {
            cached.clone()
        } else if let Some((tf_id, _)) = self
            .team_folders
            .iter()
            .find(|(_, name)| name.as_str() == first)
        {
            let id = tf_id.clone();
            self.folder_cache.insert(current_path.clone(), id.clone());
            id
        } else {
            // Try privatespace listing
            let root_id = self
                .privatespace_id
                .as_deref()
                .unwrap_or(&self.current_folder_id);
            let file = self
                .find_by_name(first, root_id)
                .await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;
            self.folder_cache
                .insert(current_path.clone(), file.id.clone());
            file.id
        };

        // Remaining components: normal walk via /files/{id}/files listing
        for part in &parts[1..] {
            current_path = format!("{}/{}", current_path, part);

            if let Some(cached) = self.folder_cache.get(&current_path) {
                current_id = cached.clone();
                continue;
            }

            let file = self
                .find_by_name(part, &current_id)
                .await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;

            current_id = file.id.clone();
            self.folder_cache.insert(current_path.clone(), file.id);
        }

        Ok(current_id)
    }
}

#[async_trait]
impl StorageProvider for ZohoWorkdriveProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::ZohoWorkdrive
    }

    fn display_name(&self) -> String {
        "Zoho WorkDrive".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.is_authenticated() {
            return Err(ProviderError::AuthenticationFailed(
                "Not authenticated. Complete OAuth2 flow first.".to_string(),
            ));
        }

        // Discover team and privatespace
        self.discover_team().await?;

        self.connected = true;
        self.current_path = "/".to_string();
        info!(
            "Connected to Zoho WorkDrive ({})",
            self.account_email.as_deref().unwrap_or("unknown")
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.folder_cache.clear();
        self.team_id = None;
        self.privatespace_id = None;
        self.team_folders.clear();
        info!("Disconnected from Zoho WorkDrive");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Determine effective path
        let effective_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            path.to_string()
        };

        // Root listing: team folders + privatespace files combined
        if effective_path == "/" {
            let entries = self.list_root().await?;
            self.remember_listed_folders("/", &entries);
            return Ok(entries);
        }

        let folder_id = if path == "." || path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.parent_folder_id(path.starts_with('/'), path.trim_matches('/'))
                .await?
        };

        let files = self.list_folder(&folder_id).await?;

        let entries: Vec<RemoteEntry> = files
            .iter()
            .map(|f| self.to_remote_entry(f, &effective_path))
            .collect();
        let listed = self.cache_key(&effective_path);
        self.remember_listed_folders(&listed, &entries);
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self
                .current_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let folder_id = self.resolve_path(&new_path).await?;
        self.current_folder_id = folder_id;
        self.current_path = if new_path.is_empty() {
            "/".to_string()
        } else {
            new_path
        };
        Ok(())
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
        let path_is_absolute = remote_path.starts_with('/');
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // WorkDrive download URL (same for regular files and native docs: auto-converts)
        let download_url = format!(
            "https://{}/v1/workdrive/download/{}",
            self.config.download_domain(),
            file.id
        );

        // For native Zoho docs, append Office extension to local path
        let actual_local_path = if let Some((_, _, office_ext)) = file
            .attributes
            .extn
            .as_deref()
            .and_then(zoho_native_export_info)
            .or_else(|| zoho_native_export_info(&file.attributes.file_type))
        {
            if !local_path.ends_with(office_ext) {
                format!("{}{}", local_path, office_ext)
            } else {
                local_path.to_string()
            }
        } else {
            local_path.to_string()
        };

        info!("Zoho WorkDrive download: GET {}", download_url);
        let auth = self.auth_header().await?;

        let resp = self
            .client
            .get(&download_url)
            .header(AUTHORIZATION, auth)
            .header(ACCEPT, HeaderValue::from_static("*/*"))
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let final_url = resp.url().to_string();
        debug!(
            "Zoho WorkDrive download response: {} (content-length: {:?}, final_url: {})",
            status,
            resp.content_length(),
            final_url
        );

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive download error body: {}",
                log_preview(&text, 1000)
            );
            return Err(ProviderError::Other(format!(
                "Download failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        // Streaming download: write chunks to file as they arrive
        use futures_util::StreamExt;

        let total_size = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(&actual_local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Create file error: {}", e)))?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        info!(
            "Downloaded {} to {} ({} bytes)",
            remote_path, actual_local_path, downloaded
        );
        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let path_is_absolute = remote_path.starts_with('/');
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // Skip resume for native Zoho docs (exports are generated on-the-fly)
        if file
            .attributes
            .extn
            .as_deref()
            .and_then(zoho_native_export_info)
            .or_else(|| zoho_native_export_info(&file.attributes.file_type))
            .is_some()
        {
            return self.download(remote_path, local_path, on_progress).await;
        }

        let download_url = format!(
            "https://{}/v1/workdrive/download/{}",
            self.config.download_domain(),
            file.id
        );
        let this: &Self = self;
        super::resume_http_download(
            local_path,
            local_path,
            &super::atomic_write::ResumeSource::new(
                "zohoworkdrive",
                self.config.api_domain(),
                &super::atomic_write::ResumeSource::account(
                    &self.profile_id,
                    self.team_user_id
                        .as_deref()
                        .or(self.account_email.as_deref()),
                ),
                &file.id,
            ),
            // Each request, the restart after a resume too, takes its own
            // header and with it the account's pace.
            |range_header| {
                let download_url = &download_url;
                async move {
                    let mut req = this
                        .client
                        .get(download_url)
                        .header(AUTHORIZATION, this.auth_header().await?)
                        .header(ACCEPT, HeaderValue::from_static("*/*"));
                    if let Some(range) = range_header {
                        req = req.header("Range", range);
                    }
                    req.send()
                        .await
                        .map_http_err(|e| ProviderError::NetworkError(e.to_string()))
                }
            },
            on_progress,
        )
        .await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path_is_absolute = remote_path.starts_with('/');
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // Check if this is a Zoho native doc: use same download URL (WorkDrive auto-converts)
        let download_url = format!(
            "https://{}/v1/workdrive/download/{}",
            self.config.download_domain(),
            file.id
        );
        info!("Zoho WorkDrive download_to_bytes: GET {}", download_url);

        let resp = self
            .client
            .get(&download_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("*/*"))
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!(
                "Zoho WorkDrive download_to_bytes error: {}: {}",
                status,
                log_preview(&text, 500)
            );
            return Err(ProviderError::Other(format!(
                "Download failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let path_is_absolute = remote_path.starts_with('/');
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        // Streaming upload: read file as a stream instead of loading into memory
        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Metadata error: {}", e)))?;
        let total_size = file_meta.len();
        info!(
            "Zoho WorkDrive upload: parent_id={}, filename={}, size={}",
            parent_id, file_name, total_size
        );

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Open file error: {}", e)))?;
        // Both routes stream the file and report the bytes as they go out;
        // 100 percent waits for Zoho's answer (see `UploadProgress`).
        let progress = super::upload_progress::UploadProgress::new(on_progress, total_size);
        let body = progress.file_body(file);

        // #347: above 250 MB the single-request endpoint answers 413, so a
        // large file goes to the upload server's stream API instead.
        if let Some(host) = large_upload_host(&self.config, total_size) {
            self.stream_upload(host, &parent_id, file_name, total_size, body)
                .await?;
            progress.complete();
            info!(
                "Uploaded {} to {} via stream upload (parent_id={})",
                local_path, remote_path, parent_id
            );
            return Ok(());
        }

        // Zoho WorkDrive upload uses multipart/form-data
        // parent_id and override-name-exist go in the form body (per Zoho docs)
        let url = format!("{}/upload", self.api_base());

        let file_part = reqwest::multipart::Part::stream(body)
            .file_name(file_name.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::Other(format!("MIME error: {}", e)))?;

        // WorkDrive URL-decodes `filename`: sent as it was, `a+b.txt` was
        // stored as `a b.txt`, over a file of that name, and `a%2Bb.txt` as
        // `a+b.txt` (measured 2026-11-01). Encoded, every name decodes back
        // to itself.
        let form = reqwest::multipart::Form::new()
            .text("parent_id", parent_id.clone())
            .text("filename", upload_filename_field(file_name))
            .text("override-name-exist", "true")
            .part("content", file_part);

        // Override Accept header: upload response is NOT JSON:API format
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .multipart(form)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Upload failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let resp_text = resp.text().await.unwrap_or_default();
        debug!(
            "Zoho WorkDrive upload response ({}): {}",
            status,
            log_preview(&resp_text, 300)
        );

        progress.complete();

        info!(
            "Uploaded {} to {} (parent_id={})",
            local_path, remote_path, parent_id
        );
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let path_is_absolute = path.starts_with('/');
        let path = path.trim_matches('/');
        let (parent_path, folder_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        // `mkdir` is not documented as idempotent. An existing child of this
        // name is AlreadyExists, the refusal the other providers give, rather
        // than a second folder of the same name.
        if self.find_by_name(folder_name, &parent_id).await?.is_some() {
            return Err(ProviderError::AlreadyExists(path.to_string()));
        }

        // JSON:API format for folder creation
        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "name": folder_name,
                    "parent_id": parent_id
                },
                "type": "files"
            }
        });

        let url = format!("{}/files", self.api_base());
        let payload = body.to_string();
        let config = self.retry.clone();
        let mut failures = 0;
        let resp = loop {
            let resp = self
                .send_api(None, Resend::RefusedOnly, |auth| {
                    self.client
                        .post(&url)
                        .header(AUTHORIZATION, auth)
                        .header(
                            CONTENT_TYPE,
                            HeaderValue::from_static("application/vnd.api+json"),
                        )
                        .body(payload.clone())
                })
                .await?;
            let status = resp.status();
            if !is_server_failure(status) {
                break resp;
            }
            // A 5xx can come after the folder was made: look for it before
            // sending the create again, or it is made twice, and before saying
            // the create failed, the last time too.
            let wait = zoho_retry_wait(status, zoho_retry_after(&resp), failures, &config);
            let text = resp.text().await.unwrap_or_default();
            info!(
                "Zoho WorkDrive answered {} to the create of {}; looking for it in {:?}",
                status, path, wait
            );
            tokio::time::sleep(wait).await;
            failures += 1;
            match self.find_by_name(folder_name, &parent_id).await? {
                Some(found) if found.is_folder() => {
                    info!("Created folder: {} (found after a {} answer)", path, status);
                    return Ok(());
                }
                Some(_) => return Err(ProviderError::AlreadyExists(path.to_string())),
                None => {}
            }
            if failures > config.max_retries {
                return Err(ProviderError::Other(format!(
                    "mkdir failed: {}",
                    sanitize_api_error(&text)
                )));
            }
        };

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "mkdir failed: {}",
                sanitize_api_error(&text)
            )));
        }

        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let original_path = path;
        let path_is_absolute = path.starts_with('/');
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // Move to trash via PATCH with status "51" (Zoho JSON:API convention)
        let url = format!("{}/files/{}", self.api_base(), file.id);
        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "status": "51"
                },
                "type": "files"
            }
        });

        info!("Zoho WorkDrive move to trash: PATCH {} id={}", url, file.id);
        let payload = body.to_string();
        let resp = self
            .send_api(None, Resend::RefusedOrFailed, |auth| {
                self.client
                    .patch(&url)
                    .header(AUTHORIZATION, auth)
                    .header(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/vnd.api+json"),
                    )
                    .body(payload.clone())
            })
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Delete failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        info!("Moved to trash: {}", path);
        self.forget_folder(original_path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        self.refuse_non_empty_dir(path).await?;
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Zoho trash handles recursive deletion
        self.delete(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // Resolve basename → trashed file id by scanning the trash listing
        // (privatespace + team folders). The id is stored in metadata["id"]
        // by `to_remote_entry`. If multiple trashed entries share the name
        // we delete the first match (the JSON:API listing returns newest
        // first per Zoho convention).
        let basename = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        if basename.is_empty() {
            return Ok(false);
        }
        let trashed = self.list_trash().await?;
        let id = trashed
            .iter()
            .find(|e| e.name == basename)
            .and_then(|e| e.metadata.get("id").cloned());
        match id {
            Some(file_id) => {
                self.permanent_delete(&file_id).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// WorkDrive's move and rename name no conflict behaviour, so the
    /// destination is looked up first and a taken one refused.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if from.trim_end_matches('/') == to.trim_end_matches('/') {
            return Ok(());
        }
        super::refuse_occupied_destination(self, from, to).await?;
        let outcome = self.move_then_rename(from, to).await;
        // After a failure too, whichever step and however it failed (an HTTP
        // status, or a `?` on the auth header, the request or the transport):
        // a move that went through has changed paths, and a cache entry that
        // is merely stale costs one lookup, where a wrong one deletes the
        // wrong folder's files.
        self.forget_folder(from);
        self.forget_folder(to);
        outcome
    }

    /// No: WorkDrive's rename and move have no documented overwrite, so
    /// there is no one-step replace, and the callers that need one refuse
    /// before they write anything.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if path.starts_with('/') && path.trim_matches('/').is_empty() {
            if !self.connected {
                return Err(ProviderError::NotConnected);
            }
            // The virtual root is not a child named "" in its own listing.
            // Verify the listing so authentication/server failures stay errors.
            self.list("/").await?;
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }
        let path_str_is_absolute = path.starts_with('/');
        let path_str = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path_str.rfind('/') {
            (&path_str[..pos], &path_str[pos + 1..])
        } else {
            ("", path_str)
        };

        let parent_id = self
            .parent_folder_id(path_str_is_absolute, parent_path)
            .await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(path_str.to_string()))?;

        Ok(self.to_remote_entry(&file, parent_path))
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

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // Zoho REST API doesn't need keep-alive
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let team_name = if let Some(ref team_id) = self.team_id {
            let url = format!("{}/teams/{}", self.api_base(), team_id);
            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if let Ok(body) = resp.json::<JsonApiResponse<TeamResource>>().await {
                body.data.attributes.name
            } else {
                "Unknown".to_string()
            }
        } else {
            // Personal account: no team unit exists, the privatespace is the root.
            "Personal".to_string()
        };

        Ok(format!(
            "Zoho WorkDrive: Team: {}: Region: {}",
            team_name,
            self.config.region.to_uppercase()
        ))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `/files/<id>/copy` implementation lives on
        // `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path_is_absolute = from.starts_with('/');
        let from_path = from.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let parent_id = self
            .parent_folder_id(from_path_is_absolute, parent_path)
            .await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        // Resolve destination folder
        let to_path = to.trim_matches('/');
        let dest_parent = if let Some(pos) = to_path.rfind('/') {
            &to_path[..pos]
        } else {
            ""
        };

        let dest_id = self
            .parent_folder_id(to.starts_with('/'), dest_parent)
            .await?;

        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "parent_id": dest_id
                },
                "type": "files"
            }
        });

        let url = format!("{}/files/{}/copy", self.api_base(), file.id);
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .body(body.to_string())
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Copy failed: {}",
                resp.status()
            )));
        }

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
            supports_permissions: true,
            available_permissions: vec!["view".into(), "edit".into()],
            supports_list_links: true,
            // Revoked one link at a time through `remove_share_link_by_id`:
            // a file can carry several.
            supports_revoke: true,
        }
    }

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        let zoho_links = self.get_file_share_links(path).await?;
        let links = zoho_links
            .into_iter()
            .map(|zl| ShareLinkInfo {
                id: zl.id,
                url: zl.attributes.link.unwrap_or_default(),
                created_at: zl.attributes.created_time,
                expires_at: zl.attributes.expiry_date,
                password_protected: zl.attributes.is_password_protected.unwrap_or(false),
                permissions: zl.attributes.link_type,
            })
            .collect();
        Ok(links)
    }

    async fn remove_share_link_by_id(
        &mut self,
        _path: &str,
        link_id: &str,
    ) -> Result<(), ProviderError> {
        // The id becomes a URL path segment.
        if !super::is_link_id(link_id) {
            return Err(ProviderError::InvalidPath(format!(
                "Not a Zoho WorkDrive link id: {link_id}"
            )));
        }
        self.delete_share_link(link_id).await
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        // Resolve path to file/folder ID
        let path_is_absolute = path.starts_with('/');
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = self.parent_folder_id(path_is_absolute, parent_path).await?;

        let file = self
            .find_by_name(file_name, &parent_id)
            .await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // Create a download link via Zoho WorkDrive Links API
        // POST /api/v1/links
        // role_id: "34" = download, "33" = view only, "32" = edit
        let role_id = match options.permissions.as_deref() {
            Some("edit") => "32",
            Some("view") => "33",
            _ => "34", // download (default)
        };
        let mut attrs = serde_json::json!({
            "resource_id": file.id,
            "link_name": file_name,
            "request_user_data": false,
            "allow_download": true,
            "role_id": role_id,
            "link_type": "download"
        });
        if let Some(ref pw) = options.password {
            attrs["password_text"] = serde_json::json!(pw);
        }
        if let Some(secs) = options.expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            attrs["expiration_date"] = serde_json::json!(expires_at.format("%Y-%m-%d").to_string());
        }
        let body = serde_json::json!({
            "data": {
                "attributes": attrs,
                "type": "links"
            }
        });

        let url = format!("{}/links", self.api_base());
        info!(
            "Zoho WorkDrive create share link: POST {} for resource {}",
            url, file.id
        );

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .body(body.to_string())
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let resp_text = resp
            .text()
            .await
            .map_http_err(|e| ProviderError::Other(format!("Read response: {}", e)))?;

        if !status.is_success() {
            info!(
                "Zoho WorkDrive create share link failed: {} - {}",
                status, resp_text
            );
            return Err(ProviderError::Other(format!(
                "Create share link failed: {} - {}",
                status,
                sanitize_api_error(&resp_text)
            )));
        }

        debug!("Zoho WorkDrive create share link response: {}", resp_text);

        // Parse response to extract the link URL
        // Response shape: { "data": { "attributes": { "link": "https://..." } } }
        let parsed: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| ProviderError::Other(format!("Parse share link response: {}", e)))?;

        let link = parsed["data"]["attributes"]["link"]
            .as_str()
            .or_else(|| parsed["data"]["attributes"]["permalink"].as_str())
            .ok_or_else(|| {
                ProviderError::Other(format!(
                    "No link URL in response: {}",
                    sanitize_api_error(&resp_text)
                ))
            })?;

        info!("Zoho WorkDrive share link created: {}", link);
        Ok(ShareLinkResult {
            url: link.to_string(),
            password: None,
            expires_at: None,
        })
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // Personal accounts have no team unit: quota comes from the user record.
        let owned_team_id = match self.team_id.clone() {
            Some(id) => id,
            None => return self.personal_storage_info().await,
        };
        let team_id = &owned_team_id;

        let url = format!("{}/teams/{}", self.api_base(), team_id);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Storage info failed: {}",
                resp.status()
            )));
        }

        let body = resp
            .text()
            .await
            .map_http_err(|e| ProviderError::Other(format!("Read team response: {}", e)))?;

        let team: JsonApiResponse<TeamResource> = serde_json::from_str(&body)
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let mut used = team.data.attributes.storage_used.unwrap_or(0);
        let mut total = team.data.attributes.storage_quota.unwrap_or(0);
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
            used = find_byte_amount_by_keys(&raw, ZOHO_USED_QUOTA_KEYS).unwrap_or(used);
            total = find_byte_amount_by_keys(&raw, ZOHO_TOTAL_QUOTA_KEYS).unwrap_or(total);
        }
        if total == 0 {
            let teams_url = format!("{}/teams", self.api_base());
            let teams_resp = self
                .client
                .get(&teams_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
            if teams_resp.status().is_success() {
                let body = teams_resp.text().await.map_http_err(|e| {
                    ProviderError::Other(format!("Read teams response: {}", e))
                })?;
                if let Ok(teams) = serde_json::from_str::<JsonApiListResponse<TeamResource>>(&body)
                {
                    if let Some(found) = teams.data.into_iter().find(|team| team.id == *team_id) {
                        used = found.attributes.storage_used.unwrap_or(used);
                        total = found.attributes.storage_quota.unwrap_or(total);
                    }
                } else {
                    debug!(
                        "Zoho WorkDrive storage_info /teams fallback parse failed: {}",
                        sanitize_api_error(&body)
                    );
                }
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
                    let matched_team =
                        raw.get("data")
                            .and_then(|data| data.as_array())
                            .and_then(|teams| {
                                teams.iter().find(|team| {
                                    team.get("id")
                                        .and_then(|id| id.as_str())
                                        .map(|id| id == team_id)
                                        .unwrap_or(false)
                                })
                            });
                    let quota_source = matched_team.unwrap_or(&raw);
                    used = find_byte_amount_by_keys(quota_source, ZOHO_USED_QUOTA_KEYS)
                        .unwrap_or(used);
                    total = find_byte_amount_by_keys(quota_source, ZOHO_TOTAL_QUOTA_KEYS)
                        .unwrap_or(total);
                }
            }
        }
        if total == 0 {
            debug!(
                "Zoho WorkDrive storage_info response did not include quota; using 5 GB fallback"
            );
            total = ZOHO_WORKDRIVE_FALLBACK_QUOTA_BYTES;
        }
        if used > total {
            total = used;
        }
        let free = total.saturating_sub(used);

        Ok(StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // The search API answers outside the folder or without the hit's real
        // path, and a caller acts on that path.
        super::find_by_walk(self, path, pattern).await
    }

    // ── Versioning ────────────────────────────────────────────────────────

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;

        let url = format!("{}/files/{}/versions", self.api_base(), file_id);
        info!("Zoho WorkDrive list versions: GET {}", url);

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let body = resp.text().await;

        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "List versions failed ({}): {}",
                status,
                sanitize_api_error(&body.unwrap_or_default())
            )));
        }
        let body = body.map_http_err(|e| body_broke_off("List versions failed", e))?;

        debug!(
            "Zoho WorkDrive versions response ({} bytes): {}",
            body.len(),
            log_preview(&body, 500)
        );

        let parsed: JsonApiListResponse<VersionResource> =
            serde_json::from_str(&body).map_err(|e| {
                ProviderError::ParseError(format!(
                    "Parse versions: {}: body: {}",
                    e,
                    sanitize_api_error(&body)
                ))
            })?;

        let versions = parsed
            .data
            .into_iter()
            .map(|v| {
                let modified = v.attributes.modified_time.or_else(|| {
                    v.attributes.modified_time_in_millisecond.map(|ms| {
                        chrono::DateTime::from_timestamp_millis(ms)
                            .unwrap_or_default()
                            .format("%Y-%m-%d %H:%M:%SZ")
                            .to_string()
                    })
                });
                let size = v
                    .attributes
                    .storage_info
                    .and_then(|si| si.size_in_bytes)
                    .unwrap_or(0);
                let modified_by = v
                    .attributes
                    .author_name
                    .or(v.attributes.modified_by)
                    .or(v.attributes.version_number.map(|n| format!("v{}", n)));

                FileVersion {
                    id: v.id,
                    modified,
                    size,
                    modified_by,
                }
            })
            .collect();

        Ok(versions)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;

        // Download specific version via download domain with version parameter
        let download_url = format!(
            "https://{}/v1/workdrive/download/{}?version={}",
            self.config.download_domain(),
            file_id,
            version_id
        );
        info!("Zoho WorkDrive download version: GET {}", download_url);

        let resp = self
            .client
            .get(&download_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("*/*"))
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Version download failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        use futures_util::StreamExt;

        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Create file error: {}", e)))?;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        info!(
            "Downloaded version {} of {} to {}",
            version_id, path, local_path
        );
        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;

        // Promote version to current via POST
        let url = format!(
            "{}/files/{}/versions/{}/promote",
            self.api_base(),
            file_id,
            version_id
        );
        info!("Zoho WorkDrive restore version: POST {}", url);

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .body("{}")
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Restore version failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        info!("Restored version {} of {}", version_id, path);
        Ok(())
    }

    fn supports_versions(&self) -> bool {
        true
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on Zoho WorkDrive.
        //
        // `upload()` streams the whole file in one request: `POST /upload` up
        // to 250 MB, and the upload server's stream API above that (#347).
        // Zoho also documents a Chunk File Upload API, recommended above
        // 1 GB, built on upload sessions; it is not wired here, so there is
        // no per-part primitive to advertise, and a fake multipart wrapper
        // would buffer chunks in memory without adding real resumability.
        // Wiring the chunk API is the place to re-evaluate this.
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search skips a folder that is gone and stops on what may speak of the
    /// session, and a 403 is not told apart from that here.
    #[test]
    fn a_failed_listing_is_the_folders_only_when_zoho_says_it_is_not_there() {
        let failure = |status| list_folder_failure(status, "id", "{}");
        assert!(matches!(
            failure(reqwest::StatusCode::NOT_FOUND),
            ProviderError::NotFound(_)
        ));
        for status in [
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(
                matches!(failure(status), ProviderError::Other(_)),
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn probe_contract_zoho_root_is_not_a_missing_child() {
        let app = axum::Router::new().fallback(axum::routing::get(|| async { r#"{"data":[]}"# }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.current_folder_id = "ROOT".into();
        provider.folder_cache.insert("/".into(), "ROOT".into());
        provider.team_folders.push(("T".into(), "Team".into()));
        assert_eq!(provider.list("/").await.unwrap().len(), 1);
        let outcome = provider.stat("/").await;
        server.abort();
        assert!(
            matches!(outcome, Ok(ref root) if root.is_dir),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn probe_contract_zoho_cannot_hide_a_refused_privatespace() {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = hits.clone();
        let app = axum::Router::new().fallback(axum::routing::get(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { (axum::http::StatusCode::FORBIDDEN, "refused") }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.team_folders.push(("T".into(), "Team".into()));
        provider.privatespace_id = Some("PS".into());
        let outcome = provider.list("/").await;
        server.abort();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            outcome.is_err(),
            "a partial root cannot prove absence: {outcome:?}"
        );
    }

    fn config(region: &str) -> ZohoWorkdriveConfig {
        ZohoWorkdriveConfig::new("cid", "csec", region)
    }

    /// A listing body: `(id, name, is_folder)` entries, and the cursor's
    /// `has_next` and `next` when given.
    fn listing_json(
        entries: &[(&str, &str, bool)],
        cursor: Option<(bool, Option<&str>)>,
    ) -> String {
        let data: Vec<serde_json::Value> = entries
            .iter()
            .map(|(id, name, folder)| {
                serde_json::json!({"id": id, "type": "files", "attributes": {
                    "name": name, "type": if *folder { "folder" } else { "file" }, "is_folder": folder
                }})
            })
            .collect();
        let mut body = serde_json::json!({ "data": data });
        if let Some((has_next, next)) = cursor {
            body["links"] = serde_json::json!({"cursor": {"has_next": has_next, "next": next}});
        }
        body.to_string()
    }

    type Requests = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

    /// A provider over a local WorkDrive stand-in that answers a listing of
    /// folder `id` with query `query` by `answer(id, query, nth)`, `nth`
    /// counting that folder's listings from 0. Every listing is recorded.
    async fn listing_fixture(
        answer: impl Fn(&str, &str, usize) -> (u16, Vec<(&'static str, String)>, String)
            + Clone
            + Send
            + Sync
            + 'static,
    ) -> (ZohoWorkdriveProvider, Requests, tokio::task::JoinHandle<()>) {
        let requests: Requests = Default::default();
        let seen = requests.clone();
        let app = axum::Router::new().fallback(axum::routing::get(move |uri: axum::http::Uri| {
            let answer = answer.clone();
            let seen = seen.clone();
            async move {
                let parts: Vec<&str> = uri.path().split('/').collect();
                let id = parts.get(5).copied().unwrap_or_default().to_string();
                let query = uri.query().unwrap_or_default().to_string();
                let nth = {
                    let mut seen = seen.lock().unwrap();
                    let nth = seen.iter().filter(|(seen_id, _)| *seen_id == id).count();
                    seen.push((id.clone(), query.clone()));
                    nth
                };
                let (status, headers, body) = answer(&id, &query, nth);
                let mut response = axum::http::Response::builder().status(status);
                for (name, value) in headers {
                    response = response.header(name, value);
                }
                response.body(axum::body::Body::from(body)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.current_folder_id = "ROOT".into();
        provider.folder_cache.insert("/".into(), "ROOT".into());
        (provider, requests, server)
    }

    fn listings_of(requests: &Requests, id: &str) -> usize {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| seen == id)
            .count()
    }

    /// A tree walk lists every folder once. It listed a folder again before
    /// each of its subfolders, and WorkDrive refuses the 20th listing of one
    /// folder within a minute: a folder with 25 subfolders stopped the walk.
    #[tokio::test]
    async fn a_walk_lists_each_folder_once() {
        let children: Vec<(String, String)> = (1..=25)
            .map(|n| (format!("C{n:02}"), format!("c{n:02}")))
            .collect();
        let listed = children.clone();
        let (mut provider, requests, server) = listing_fixture(move |id, _, _| {
            let body = if id == "TOP" {
                let entries: Vec<(&str, &str, bool)> = listed
                    .iter()
                    .map(|(id, name)| (id.as_str(), name.as_str(), true))
                    .collect();
                listing_json(&entries, Some((false, None)))
            } else {
                listing_json(&[], Some((false, None)))
            };
            (200, vec![], body)
        })
        .await;
        provider.folder_cache.insert("/top".into(), "TOP".into());

        let top = provider.list("/top").await.unwrap();
        assert_eq!(top.len(), 25);
        for (_, name) in &children {
            provider.list(&format!("/top/{name}")).await.unwrap();
        }
        server.abort();

        assert_eq!(listings_of(&requests, "TOP"), 1);
        for (id, _) in &children {
            assert_eq!(listings_of(&requests, id), 1, "{id}");
        }
    }

    /// A listing is what the folder holds now. A child cached under a name it
    /// shows for another folder, or for none, is forgotten with what was
    /// cached below it; a name shown twice is not cached, so the first match
    /// is still looked up the way `resolve_path` looks it up.
    #[test]
    fn a_listing_replaces_what_was_cached_for_the_folders_children() {
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        for (path, id) in [
            ("/", "ROOT"),
            ("/moved", "OLD"),
            ("/moved/x", "OLD_X"),
            ("/kept", "KEPT"),
            ("/kept/y", "KEPT_Y"),
            ("/gone", "GONE"),
            ("/gone/z", "GONE_Z"),
            ("/twice", "TWICE"),
            ("/elsewhere/deep", "DEEP"),
        ] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        let entry = |id: &str, name: &str, is_dir: bool| {
            let mut entry = RemoteEntry::directory(name.to_string(), format!("/{name}"));
            entry.is_dir = is_dir;
            entry.metadata.insert("id".to_string(), id.to_string());
            entry
        };
        let listing = [
            entry("NEW", "moved", true),
            entry("KEPT", "kept", true),
            entry("TWICE_1", "twice", true),
            entry("TWICE_2", "twice", false),
            entry("FILE", "plain.txt", false),
            entry("FRESH", "fresh", true),
        ];
        p.remember_listed_folders("/", &listing);

        let cached = |p: &ZohoWorkdriveProvider, path: &str| p.folder_cache.get(path).cloned();
        assert_eq!(cached(&p, "/").as_deref(), Some("ROOT"));
        assert_eq!(cached(&p, "/moved").as_deref(), Some("NEW"));
        assert_eq!(cached(&p, "/moved/x"), None);
        assert_eq!(cached(&p, "/kept").as_deref(), Some("KEPT"));
        assert_eq!(cached(&p, "/kept/y").as_deref(), Some("KEPT_Y"));
        assert_eq!(cached(&p, "/gone"), None);
        assert_eq!(cached(&p, "/gone/z"), None);
        assert_eq!(cached(&p, "/twice"), None);
        assert_eq!(cached(&p, "/plain.txt"), None);
        assert_eq!(cached(&p, "/fresh").as_deref(), Some("FRESH"));
        assert_eq!(cached(&p, "/elsewhere/deep").as_deref(), Some("DEEP"));
    }

    /// A relative listing caches under the path `resolve_path` resolves.
    #[tokio::test]
    async fn a_relative_listing_caches_under_the_absolute_path() {
        let (mut provider, requests, server) = listing_fixture(|id, _, _| {
            let body = match id {
                "SUB" => listing_json(&[("LEAF", "leaf", true)], Some((false, None))),
                _ => listing_json(&[], Some((false, None))),
            };
            (200, vec![], body)
        })
        .await;
        provider.folder_cache.insert("/docs".into(), "DOCS".into());
        provider
            .folder_cache
            .insert("/docs/sub".into(), "SUB".into());
        provider.current_folder_id = "DOCS".into();
        provider.current_path = "/docs".into();

        provider.list("sub").await.unwrap();
        provider.list("/docs/sub/leaf").await.unwrap();
        server.abort();

        assert_eq!(
            provider
                .folder_cache
                .get("/docs/sub/leaf")
                .map(String::as_str),
            Some("LEAF")
        );
        assert_eq!(listings_of(&requests, "SUB"), 1);
        assert_eq!(listings_of(&requests, "LEAF"), 1);
    }

    /// A listing asks for 1000 entries a page and follows the cursor until
    /// the answer says no page follows.
    #[tokio::test]
    async fn a_listing_follows_the_cursor_to_its_last_page() {
        let (mut provider, requests, server) = listing_fixture(|_, query, _| {
            let body = if query.contains("page%5Bnext%5D=0") {
                listing_json(
                    &[("A", "a", false), ("B", "b", false)],
                    Some((
                        true,
                        Some("https://workdrive.example/api/v1/files/TOP/files?page%5Blimit%5D=1000&page%5Bnext%5D=c%2B1%3D"),
                    )),
                )
            } else if query.contains("page%5Bnext%5D=c%2B1%3D") {
                listing_json(&[("C", "c", false)], Some((false, None)))
            } else {
                "unexpected".to_string()
            };
            (200, vec![], body)
        })
        .await;
        provider.folder_cache.insert("/top".into(), "TOP".into());

        let names: Vec<String> = provider
            .list("/top")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        server.abort();

        assert_eq!(names, ["a", "b", "c"]);
        let queries: Vec<String> = requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, q)| q.clone())
            .collect();
        assert_eq!(queries.len(), 2);
        assert!(
            queries.iter().all(|q| q.contains("page%5Blimit%5D=1000")),
            "{queries:?}"
        );
    }

    /// The end of a listing: the cursor's word when there is one, a short page
    /// when there is none. A full page without a cursor, or a cursor that says
    /// more follows without saying where, is an error, never the end.
    #[test]
    fn a_listing_ends_only_where_the_answer_says_it_does() {
        let cursor = |has_next: bool, next: Option<&str>| PageLinks {
            cursor: Some(PageCursor {
                has_next,
                next: next.map(str::to_string),
            }),
        };
        let next = "https://x/api/v1/files/ID/files?page%5Blimit%5D=1000&page%5Bnext%5D=k%2F2";
        assert_eq!(
            next_page_cursor(Some(&cursor(true, Some(next))), 3),
            Ok(Some("k/2".to_string()))
        );
        assert_eq!(
            next_page_cursor(Some(&cursor(false, Some(next))), 3),
            Ok(None)
        );
        assert_eq!(
            next_page_cursor(Some(&cursor(true, Some(next))), 0),
            Ok(Some("k/2".to_string()))
        );
        assert_eq!(next_page_cursor(None, 0), Ok(None));
        assert!(next_page_cursor(Some(&cursor(true, None)), 3).is_err());
        let empty = "https://x/api/v1/files/ID/files?page%5Blimit%5D=1000&page%5Bnext%5D=";
        assert!(next_page_cursor(Some(&cursor(true, Some(empty))), 3).is_err());
        assert!(next_page_cursor(Some(&cursor(true, Some("not a url"))), 3).is_err());
        assert_eq!(next_page_cursor(None, LIST_PAGE_LIMIT - 1), Ok(None));
        assert_eq!(
            next_page_cursor(Some(&PageLinks { cursor: None }), 5),
            Ok(None)
        );
        assert!(next_page_cursor(None, LIST_PAGE_LIMIT).is_err());
    }

    /// A page that names an earlier page as the next, itself or the first,
    /// ends the listing with an error instead of listing forever.
    #[tokio::test]
    async fn a_listing_whose_next_page_was_already_listed_is_an_error() {
        let next = |id: &str, cursor: &str| {
            format!("https://workdrive.example/api/v1/files/{id}/files?page%5Blimit%5D=1000&page%5Bnext%5D={cursor}")
        };
        let (mut provider, requests, server) = listing_fixture(move |id, query, _| {
            let first = query.ends_with("page%5Bnext%5D=0");
            let body = match (id, first) {
                ("SELF", true) => {
                    listing_json(&[("A", "a", false)], Some((true, Some(&next(id, "s")))))
                }
                ("SELF", false) => {
                    listing_json(&[("B", "b", false)], Some((true, Some(&next(id, "s")))))
                }
                (_, true) => listing_json(&[("C", "c", false)], Some((true, Some(&next(id, "t"))))),
                (_, false) => {
                    listing_json(&[("D", "d", false)], Some((true, Some(&next(id, "0")))))
                }
            };
            (200, vec![], body)
        })
        .await;
        provider.folder_cache.insert("/self".into(), "SELF".into());
        provider.folder_cache.insert("/back".into(), "BACK".into());
        let limit = std::time::Duration::from_secs(10);

        for path in ["/self", "/back"] {
            let listed = tokio::time::timeout(limit, provider.list(path))
                .await
                .expect("the listing ended");
            let error = listed.unwrap_err().to_string();
            assert!(
                error.contains("already listed as the next"),
                "{path}: {error}"
            );
        }
        server.abort();
        assert_eq!(listings_of(&requests, "SELF"), 2);
        assert_eq!(listings_of(&requests, "BACK"), 2);
    }

    /// A listing that answers success with an empty body is asked for once
    /// more. Read as text and parsed, it failed as `Parse files error: EOF
    /// while parsing a value at line 1 column 0`.
    #[tokio::test]
    async fn an_empty_listing_body_is_asked_for_once_more() {
        let (mut provider, requests, server) = listing_fixture(|_, _, nth| {
            let body = if nth == 0 {
                String::new()
            } else {
                listing_json(&[("A", "a", false)], Some((false, None)))
            };
            (200, vec![], body)
        })
        .await;
        provider.folder_cache.insert("/top".into(), "TOP".into());
        let listed = provider.list("/top").await;
        server.abort();
        assert_eq!(listed.unwrap().len(), 1);
        assert_eq!(listings_of(&requests, "TOP"), 2);
    }

    #[tokio::test]
    async fn a_listing_body_empty_twice_is_an_error_that_says_so() {
        let (mut provider, requests, server) =
            listing_fixture(|_, _, _| (200, vec![], String::new())).await;
        provider.folder_cache.insert("/top".into(), "TOP".into());
        let listed = provider.list("/top").await;
        server.abort();
        let error = listed.unwrap_err().to_string();
        assert!(error.contains("empty body, twice"), "{error}");
        assert!(!error.contains("EOF"), "{error}");
        assert_eq!(listings_of(&requests, "TOP"), 2);
    }

    /// A server that writes `answer(request line)` on each connection and
    /// closes it, so an answer can break off; it counts the connections.
    async fn raw_fixture(
        answer: fn(&str) -> Vec<u8>,
    ) -> (
        ZohoWorkdriveProvider,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = accepted.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut request = vec![0u8; 4096];
                let read = socket.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..read]).to_string();
                let line = request.lines().next().unwrap_or_default().to_string();
                let _ = socket.write_all(&answer(&line)).await;
                let _ = socket.shutdown().await;
            }
        });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.folder_cache.insert("/".into(), "ROOT".into());
        (provider, accepted, server)
    }

    /// The head of an answer that promises 100 bytes and sends 9.
    fn broken_answer() -> Vec<u8> {
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"data\":[".to_vec()
    }

    /// A listing whose answer breaks off halfway is asked for once more, then
    /// fails saying the body broke off.
    #[tokio::test]
    async fn a_listing_body_that_breaks_off_twice_is_an_error_that_says_so() {
        let (mut provider, accepted, server) = raw_fixture(|_| broken_answer()).await;
        provider.folder_cache.insert("/top".into(), "TOP".into());
        let listed = provider.list("/top").await;
        server.abort();
        let error = listed.unwrap_err().to_string();
        assert!(
            error.contains("broke off before its body was read, twice"),
            "{error}"
        );
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// The same for the other answers read for their content, here the
    /// versions of a file: a body that breaks off is said to, where it was
    /// parsed as empty text and failed as `EOF while parsing a value`.
    #[tokio::test]
    async fn a_versions_body_that_breaks_off_is_an_error_that_says_so() {
        let (mut provider, _, server) = raw_fixture(|line| {
            if line.contains("/versions") {
                return broken_answer();
            }
            let body = listing_json(&[("F", "f.txt", false)], Some((false, None)));
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .into_bytes()
        })
        .await;
        let versions = provider.list_versions("/f.txt").await;
        server.abort();
        let error = versions.unwrap_err().to_string();
        assert!(
            error.contains("List versions failed: the answer broke off before its body was read"),
            "{error}"
        );
    }

    /// A 429 is sent again once its Retry-After has passed, and a second
    /// later: measured, a listing sent when the Retry-After had reached 0
    /// was refused again.
    #[tokio::test]
    async fn a_refused_listing_is_sent_again_a_second_after_its_retry_after() {
        let (mut provider, requests, server) = listing_fixture(|_, _, nth| {
            if nth == 0 {
                (
                    429,
                    vec![("retry-after", "0".to_string())],
                    r#"{"errors":[{"id":"F7008","title":"Request rate limit exceeded. Please retry again later."}]}"#.to_string(),
                )
            } else {
                (200, vec![], listing_json(&[("A", "a", false)], Some((false, None))))
            }
        })
        .await;
        provider.folder_cache.insert("/top".into(), "TOP".into());
        let started = std::time::Instant::now();
        let listed = provider.list("/top").await;
        let waited = started.elapsed();
        server.abort();
        assert_eq!(listed.unwrap().len(), 1);
        assert_eq!(listings_of(&requests, "TOP"), 2);
        assert!(waited >= RETRY_AFTER_MARGIN, "{waited:?}");
    }

    /// A listing answered with a 5xx is sent again: reading a folder twice
    /// changes nothing.
    #[tokio::test]
    async fn a_listing_answered_503_is_sent_again() {
        let (mut provider, requests, server) = listing_fixture(|_, _, nth| {
            if nth == 0 {
                (503, vec![("retry-after", "0".to_string())], String::new())
            } else {
                (
                    200,
                    vec![],
                    listing_json(&[("A", "a", false)], Some((false, None))),
                )
            }
        })
        .await;
        provider.folder_cache.insert("/top".into(), "TOP".into());
        let listed = provider.list("/top").await;
        server.abort();
        assert_eq!(listed.unwrap().len(), 1);
        assert_eq!(listings_of(&requests, "TOP"), 2);
    }

    #[test]
    fn a_rate_refusal_waits_what_zoho_asks_and_a_second_or_a_minute() {
        use std::time::Duration;
        let config = HttpRetryConfig::default();
        let too_many = reqwest::StatusCode::TOO_MANY_REQUESTS;
        let unavailable = reqwest::StatusCode::SERVICE_UNAVAILABLE;
        assert_eq!(
            zoho_retry_wait(too_many, Some(Duration::from_secs(600)), 0, &config),
            Duration::from_secs(601)
        );
        assert_eq!(
            zoho_retry_wait(too_many, Some(Duration::ZERO), 2, &config),
            Duration::from_secs(1)
        );
        assert_eq!(
            zoho_retry_wait(too_many, None, 0, &config),
            Duration::from_secs(61)
        );
        assert_eq!(
            zoho_retry_wait(unavailable, Some(Duration::from_secs(5)), 0, &config),
            Duration::from_secs(5)
        );
        let backoff = zoho_retry_wait(unavailable, None, 0, &config);
        assert!(backoff < Duration::from_secs(2), "{backoff:?}");
    }

    /// WorkDrive's Retry-After is read in whole seconds past the shared
    /// parser's 300 s, up to [`RETRY_AFTER_CAP`].
    #[test]
    fn a_retry_after_of_ten_minutes_is_read_as_ten_minutes() {
        use std::time::Duration;
        let with = |value: &str| -> reqwest::Response {
            http::Response::builder()
                .status(429)
                .header("Retry-After", value)
                .body(reqwest::Body::from(""))
                .unwrap()
                .into()
        };
        assert_eq!(
            zoho_retry_after(&with("600")),
            Some(Duration::from_secs(600))
        );
        assert_eq!(zoho_retry_after(&with(" 7 ")), Some(Duration::from_secs(7)));
        assert_eq!(zoho_retry_after(&with("100000")), Some(RETRY_AFTER_CAP));
        assert_eq!(zoho_retry_after(&with("soon")), None);
        let none: reqwest::Response = http::Response::builder()
            .status(429)
            .body(reqwest::Body::from(""))
            .unwrap()
            .into();
        assert_eq!(zoho_retry_after(&none), None);
    }

    /// At most 19 listings of one folder in any 61 s: the 20th waits until
    /// the first leaves the window. Other folders do not wait for it.
    #[test]
    fn the_twentieth_listing_of_a_folder_waits_for_the_window() {
        use std::time::Duration;
        let listings = FolderListings::new(FOLDER_LISTINGS_PER_WINDOW, FOLDER_LISTING_WINDOW);
        let start = tokio::time::Instant::now();
        for _ in 0..19 {
            assert_eq!(listings.reserve("F", start), None);
        }
        assert_eq!(listings.reserve("F", start), Some(Duration::from_secs(61)));
        assert_eq!(listings.reserve("G", start), None);
        assert_eq!(
            listings.reserve("F", start + Duration::from_secs(30)),
            Some(Duration::from_secs(31))
        );

        // The first 19 leave the window together, so the next goes at once.
        assert_eq!(listings.reserve("F", start + Duration::from_secs(61)), None);
    }

    /// A listing takes its folder's turn: past the folder's limit it waits
    /// for the window and is not sent, while a listing of another folder
    /// goes out at once.
    #[tokio::test]
    async fn a_listing_past_its_folders_limit_waits_and_others_do_not() {
        use std::time::Duration;
        let (mut provider, requests, server) =
            listing_fixture(|_, _, _| (200, vec![], listing_json(&[], Some((false, None))))).await;
        let mut pace = ZohoPace::new();
        pace.listings = FolderListings::new(1, Duration::from_secs(60));
        provider.pace = std::sync::Arc::new(pace);
        provider.folder_cache.insert("/top".into(), "TOP".into());
        provider
            .folder_cache
            .insert("/other".into(), "OTHER".into());

        provider.list("/top").await.unwrap();
        let again = tokio::time::timeout(Duration::from_millis(500), provider.list("/top")).await;
        assert!(again.is_err(), "the second listing of /top did not wait");
        let other = tokio::time::timeout(Duration::from_secs(10), provider.list("/other")).await;
        server.abort();

        other
            .expect("the listing of /other waited for /top")
            .unwrap();
        assert_eq!(listings_of(&requests, "TOP"), 1);
        assert_eq!(listings_of(&requests, "OTHER"), 1);
    }

    /// Requests reach WorkDrive at the account's pace, a sixth of a second
    /// apart, and a folder's window counts its listings as they were sent: a
    /// listing that took its turn and then waited for the pace would let the
    /// next one in early. Measured on the requests the server receives.
    #[tokio::test]
    async fn sends_keep_the_account_pace_and_the_folder_window() {
        use std::time::{Duration, Instant};
        let arrivals: std::sync::Arc<std::sync::Mutex<Vec<Instant>>> = Default::default();
        let seen = arrivals.clone();
        let (mut provider, _, server) = listing_fixture(move |_, _, _| {
            seen.lock().unwrap().push(Instant::now());
            (200, vec![], listing_json(&[], Some((false, None))))
        })
        .await;
        provider.folder_cache.insert("/a".into(), "A".into());
        provider.folder_cache.insert("/b".into(), "B".into());
        provider.list("/a").await.unwrap();
        provider.list("/b").await.unwrap();

        // Half a second a request, one listing of a folder in 600 ms, and a
        // turn of the pace already taken: the first listing waits for the
        // pace before it goes out.
        provider.pace = std::sync::Arc::new(ZohoPace {
            calls: super::super::tpslimit::TpsLimiter::new(2.0, 1.0),
            listings: FolderListings::new(1, Duration::from_millis(600)),
        });
        provider.auth_header().await.unwrap();
        provider.list("/a").await.unwrap();
        provider.list("/a").await.unwrap();
        server.abort();

        let arrivals = arrivals.lock().unwrap();
        assert_eq!(arrivals.len(), 4);
        let pace = arrivals[1] - arrivals[0];
        assert!(pace >= Duration::from_millis(150), "{pace:?}");
        let window = arrivals[3] - arrivals[2];
        assert!(window >= Duration::from_millis(600), "{window:?}");
    }

    /// WorkDrive URL-decodes an upload's `filename`, so the field carries the
    /// name encoded and every name decodes back to itself. Sent as it was,
    /// `a+b.txt` was stored as `a b.txt`, over the file of that name.
    #[test]
    fn an_upload_name_decodes_back_to_itself() {
        for name in [
            "a+b.txt",
            "a%20b.txt",
            "a%2Bb.txt",
            "100% done.txt",
            "a b.txt",
            "a&b=c;d.txt",
            "été ☁.txt",
        ] {
            let field = format!("filename={}", upload_filename_field(name));
            let decoded: Vec<(String, String)> = url::form_urlencoded::parse(field.as_bytes())
                .into_owned()
                .collect();
            assert_eq!(
                decoded,
                [("filename".to_string(), name.to_string())],
                "{name}"
            );
        }
    }

    /// The single-request upload sends the encoded name in `filename`.
    #[tokio::test]
    async fn the_upload_request_carries_the_encoded_name() {
        let bodies: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Default::default();
        let seen = bodies.clone();
        let app =
            axum::Router::new().fallback(axum::routing::post(move |body: axum::body::Bytes| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body.to_vec());
                    "{}"
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("payload.txt");
        std::fs::write(&local, b"a+b").unwrap();
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.folder_cache.insert("/".into(), "ROOT".into());
        provider
            .upload(local.to_str().unwrap(), "/a+b.txt", None)
            .await
            .unwrap();
        server.abort();
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let body = String::from_utf8_lossy(&bodies[0]);
        assert!(
            body.contains("name=\"filename\"\r\n\r\na%2Bb.txt\r\n"),
            "{body}"
        );
    }

    /// WorkDrive's rename and move have no documented overwrite, so there is no
    /// one-step replace. The answer is no, so the callers that need one (CLI
    /// `edit`, MCP `remote_edit`, the crypt marker paths) refuse before they
    /// write.
    #[tokio::test]
    async fn zoho_does_not_claim_an_atomic_replace() {
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        assert!(!p.supports_atomic_replace().await.unwrap());
        assert!(!p.replace_sets_aside());
    }

    /// After `cd /docs`: `/x` and `x` name different folders, `/x` the
    /// root's and `x` the current one's, and a longer path follows the same
    /// rule. Resolved from the cache alone, so no request is made.
    #[tokio::test]
    async fn absolute_paths_resolve_from_the_root_and_relative_ones_from_the_current_folder() {
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        for (path, id) in [
            ("/", "ROOT"),
            ("/docs", "DOCS"),
            ("/docs/sub", "DOCS_SUB"),
            ("/sub", "ROOT_SUB"),
        ] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        p.current_folder_id = "DOCS".to_string();
        p.current_path = "/docs".to_string();

        assert_eq!(p.parent_folder_id(true, "").await.unwrap(), "ROOT");
        assert_eq!(p.parent_folder_id(false, "").await.unwrap(), "DOCS");
        assert_eq!(p.parent_folder_id(true, "sub").await.unwrap(), "ROOT_SUB");
        assert_eq!(p.parent_folder_id(false, "sub").await.unwrap(), "DOCS_SUB");
    }

    /// Upload `file` to a local fixture that answers `status` and `body` on
    /// both upload routes; returns the outcome and every progress update.
    async fn upload_against_fixture(
        file: &std::path::Path,
        remote: &str,
        status: u16,
        body: &str,
    ) -> (Result<(), ProviderError>, Vec<(u64, u64)>) {
        use crate::providers::upload_progress::fixture::{recorder, serve, Route};
        let (base, server) = serve(vec![
            Route::post("/workdrive/api/v1/upload", status, body),
            Route::post("/workdrive-api/v1/stream/upload", status, body),
        ])
        .await;
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(base);
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        let (callback, updates) = recorder();
        let outcome = provider
            .upload(file.to_str().unwrap(), remote, Some(callback))
            .await;
        server.abort();
        let updates = updates.lock().unwrap().clone();
        (outcome, updates)
    }

    /// `POST /upload`, the single-request route: the file goes out in the
    /// reader's chunks and the bar follows them. Before this, the callback ran
    /// once, `(total, total)`, after the response.
    #[tokio::test]
    async fn single_request_upload_reports_real_progress() {
        use crate::providers::upload_progress::fixture::assert_real_progress;
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![3u8; 300 * 1024]).unwrap();
        let (outcome, updates) =
            upload_against_fixture(file.path(), "/small.bin", 200, r#"{"data":[]}"#).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_real_progress(&updates, 300 * 1024, true);

        let (outcome, updates) =
            upload_against_fixture(file.path(), "/small.bin", 500, r#"{"errors":[]}"#).await;
        assert!(outcome.is_err());
        assert_real_progress(&updates, 300 * 1024, false);
    }

    /// The stream route above 250 MB (#933), on a sparse file (no disk blocks,
    /// read as zeros). 100 percent waits for the answer that lists the file:
    /// a success status without it is a failed upload and must not show a
    /// completed one.
    #[tokio::test]
    async fn stream_upload_over_250_mb_reports_real_progress() {
        use crate::providers::upload_progress::fixture::assert_real_progress;
        let size = ZOHO_SINGLE_UPLOAD_MAX_BYTES + 1;
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(size).unwrap();
        let (outcome, updates) = upload_against_fixture(
            file.path(),
            "/big.bin",
            200,
            r#"{"data":[{"attributes":{"file_name":"big.bin"},"type":"files"}]}"#,
        )
        .await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_real_progress(&updates, size, true);

        let (outcome, updates) =
            upload_against_fixture(file.path(), "/big.bin", 200, r#"{"data":[]}"#).await;
        assert!(outcome.is_err());
        assert_real_progress(&updates, size, false);
    }

    /// A WorkDrive double whose root `ROOT` holds the folder `a` (`A`):
    /// listings answer that, every PATCH succeeds. Returns a provider on it
    /// whose cache maps `/` to `ROOT`, `/a` to `A` and `/a/sub` to `SUB`.
    async fn provider_with_cached_folder_a() -> ZohoWorkdriveProvider {
        use crate::providers::upload_progress::fixture::{serve, Route};
        let listing = r#"{"data":[{"id":"A","attributes":{"name":"a","type":"folder"}}]}"#;
        let (base, _server) = serve(vec![
            Route::get("/workdrive/api/v1/files/ROOT/files", 200, listing),
            Route::get("/workdrive/api/v1/files/A/files", 200, r#"{"data":[]}"#),
            Route {
                method: axum::http::Method::PATCH,
                path: "/workdrive/api/v1/files/A",
                status: 200,
                body: "{}".to_string(),
                busy_first: false,
            },
        ])
        .await;
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(base);
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        for (path, id) in [("/", "ROOT"), ("/a", "A"), ("/a/sub", "SUB"), ("/ab", "AB")] {
            provider
                .folder_cache
                .insert(path.to_string(), id.to_string());
        }
        provider.current_folder_id = "ROOT".to_string();
        provider
    }

    /// `mkdir` does not say it is idempotent. A name the parent already holds
    /// is AlreadyExists, and the create request is not sent.
    #[tokio::test]
    async fn mkdir_refuses_a_name_the_parent_already_holds() {
        use std::sync::{Arc, Mutex};
        let posts = Arc::new(Mutex::new(0u32));
        let seen = Arc::clone(&posts);
        let listing = r#"{"data":[{"id":"A","attributes":{"name":"taken","type":"folder"}}]}"#;
        let app = axum::Router::new()
            .route(
                "/workdrive/api/v1/files/ROOT/files",
                axum::routing::get(move || async move { listing }),
            )
            .route(
                "/workdrive/api/v1/files",
                axum::routing::post(move || {
                    let seen = Arc::clone(&seen);
                    async move {
                        *seen.lock().unwrap() += 1;
                        (axum::http::StatusCode::CREATED, r#"{"data":{"id":"N"}}"#)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider
            .folder_cache
            .insert("/".to_string(), "ROOT".to_string());
        provider.current_folder_id = "ROOT".to_string();

        let err = provider.mkdir("/taken").await.unwrap_err();
        assert!(matches!(err, ProviderError::AlreadyExists(_)), "{err:?}");
        assert_eq!(*posts.lock().unwrap(), 0, "an existing name is not created");

        provider.mkdir("/fresh").await.expect("a free name");
        assert_eq!(*posts.lock().unwrap(), 1);
    }

    /// A provider over a WorkDrive stand-in for `mkdir` in the root: the
    /// `nth` listing of the root answers `listing(nth)` and the `nth` create
    /// the status `create(nth)`. Both are counted.
    async fn mkdir_fixture(
        listing: fn(usize) -> String,
        create: fn(usize) -> u16,
    ) -> (
        ZohoWorkdriveProvider,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let listings = Arc::new(AtomicUsize::new(0));
        let creates = Arc::new(AtomicUsize::new(0));
        let (listed, created) = (listings.clone(), creates.clone());
        let app = axum::Router::new()
            .route(
                "/workdrive/api/v1/files/ROOT/files",
                axum::routing::get(move || {
                    let nth = listed.fetch_add(1, Ordering::SeqCst);
                    async move { listing(nth) }
                }),
            )
            .route(
                "/workdrive/api/v1/files",
                axum::routing::post(move || {
                    let nth = created.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let status = axum::http::StatusCode::from_u16(create(nth)).unwrap();
                        (status, r#"{"data":{"id":"N"}}"#)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;
        provider.folder_cache.insert("/".into(), "ROOT".into());
        provider.current_folder_id = "ROOT".into();
        provider.retry = HttpRetryConfig {
            base_delay_ms: 1,
            max_delay_ms: 5,
            ..HttpRetryConfig::default()
        };
        (provider, listings, creates, server)
    }

    /// A 5xx to a create can come after the folder was made, so the folder is
    /// looked for before the create is sent again: found, it is not made a
    /// second time; not found, the create goes again; a file of that name is
    /// AlreadyExists. It is looked for after the last 5xx too, before the
    /// create is said to have failed.
    #[tokio::test]
    async fn a_create_answered_with_a_5xx_is_looked_for_before_it_goes_again() {
        use std::sync::atomic::Ordering::SeqCst;
        let (mut provider, listings, creates, server) = mkdir_fixture(
            |nth| match nth {
                0 => listing_json(&[], None),
                _ => listing_json(&[("N", "fresh", true)], None),
            },
            |_| 502,
        )
        .await;
        provider.mkdir("/fresh").await.unwrap();
        server.abort();
        assert_eq!((listings.load(SeqCst), creates.load(SeqCst)), (2, 1));

        let (mut provider, listings, creates, server) = mkdir_fixture(
            |_| listing_json(&[], None),
            |nth| if nth == 0 { 503 } else { 201 },
        )
        .await;
        provider.mkdir("/fresh").await.unwrap();
        server.abort();
        assert_eq!((listings.load(SeqCst), creates.load(SeqCst)), (2, 2));

        let (mut provider, _, creates, server) = mkdir_fixture(
            |nth| match nth {
                0 => listing_json(&[], None),
                _ => listing_json(&[("F", "fresh", false)], None),
            },
            |_| 500,
        )
        .await;
        let error = provider.mkdir("/fresh").await.unwrap_err();
        server.abort();
        assert!(
            matches!(error, ProviderError::AlreadyExists(_)),
            "{error:?}"
        );
        assert_eq!(creates.load(SeqCst), 1);

        let (mut provider, listings, creates, server) = mkdir_fixture(
            |nth| match nth {
                0..=3 => listing_json(&[], None),
                _ => listing_json(&[("N", "fresh", true)], None),
            },
            |_| 502,
        )
        .await;
        provider.mkdir("/fresh").await.unwrap();
        server.abort();
        assert_eq!((listings.load(SeqCst), creates.load(SeqCst)), (5, 4));

        let (mut provider, listings, creates, server) =
            mkdir_fixture(|_| listing_json(&[], None), |_| 503).await;
        let error = provider.mkdir("/fresh").await.unwrap_err();
        server.abort();
        assert!(error.to_string().contains("mkdir failed"), "{error}");
        assert_eq!((listings.load(SeqCst), creates.load(SeqCst)), (5, 4));
    }

    /// After `/a` moved to `/b`, the cache still mapped `/a` (and `/a/sub`)
    /// to the moved folder: a new `/a` resolved to `/b`, and deleting `/a/x`
    /// deleted `/b/x`.
    #[tokio::test]
    async fn renaming_a_folder_forgets_its_cached_path_and_everything_under_it() {
        let mut p = provider_with_cached_folder_a().await;
        p.rename("/a", "/b").await.expect("rename");
        assert!(!p.folder_cache.contains_key("/a"));
        assert!(!p.folder_cache.contains_key("/a/sub"));
        assert!(
            p.folder_cache.contains_key("/ab"),
            "a sibling sharing the prefix stays"
        );
    }

    /// A move under a new name is two calls. When the move went through and
    /// the rename then failed before any answer (the connection dropped), the
    /// error returned past the cache cleanup, which ran only on an HTTP error
    /// status: `/a` still resolved to the folder now in `/d`.
    #[tokio::test]
    async fn a_move_whose_rename_fails_in_transport_still_forgets_the_cache() {
        let listing = r#"{"data":[{"id":"A","attributes":{"name":"a","type":"folder"}}]}"#;
        let app = axum::Router::new()
            .route(
                "/workdrive/api/v1/files/ROOT/files",
                axum::routing::get(move || async move { listing }),
            )
            .route(
                // `/d` is empty: the destination is free.
                "/workdrive/api/v1/files/D/files",
                axum::routing::get(|| async { r#"{"data":[]}"# }),
            )
            .route(
                "/workdrive/api/v1/files",
                axum::routing::patch(|| async { "{}" }),
            )
            .route(
                // A handler that panics drops the connection without an
                // answer: a transport failure, as when the network goes.
                "/workdrive/api/v1/files/A",
                axum::routing::patch(|| async {
                    if std::hint::black_box(true) {
                        panic!("connection dropped");
                    }
                    ""
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        p.endpoint_override = Some(format!("http://{addr}"));
        p.test_access_token = Some("test-token".into());
        p.connected = true;
        for (path, id) in [("/", "ROOT"), ("/a", "A"), ("/a/sub", "SUB"), ("/d", "D")] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        p.current_folder_id = "ROOT".to_string();
        let outcome = p.rename("/a", "/d/b").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!p.folder_cache.contains_key("/a"), "{:?}", p.folder_cache);
        assert!(
            !p.folder_cache.contains_key("/a/sub"),
            "{:?}",
            p.folder_cache
        );
    }

    /// WorkDrive's rename and move name no conflict behaviour, so the
    /// destination is looked up first and a taken one refused before any
    /// call that changes something.
    #[tokio::test]
    async fn a_rename_onto_a_taken_name_is_refused_before_any_change() {
        use crate::providers::upload_progress::fixture::{serve_logged, Route};
        let listing = r#"{"data":[{"id":"A","attributes":{"name":"a","type":"folder"}},{"id":"B","attributes":{"name":"b.txt","type":"file"}}]}"#;
        let (base, _server, received) = serve_logged(vec![
            Route::get("/workdrive/api/v1/files/ROOT/files", 200, listing),
            Route {
                method: axum::http::Method::PATCH,
                path: "/workdrive/api/v1/files/A",
                status: 200,
                body: "{}".to_string(),
                busy_first: false,
            },
        ])
        .await;
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        p.endpoint_override = Some(base);
        p.test_access_token = Some("test-token".into());
        p.connected = true;
        p.folder_cache.insert("/".to_string(), "ROOT".to_string());
        p.current_folder_id = "ROOT".to_string();
        let outcome = p.rename("/a", "/b.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            !received
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| *path == "/workdrive/api/v1/files/A"),
            "{:?}",
            received.lock().unwrap()
        );
    }

    /// The move keeps the old name: with `/dst/a.txt` there it put a second
    /// `a.txt` in `/dst` until the rename. The rename goes first, in the
    /// source folder.
    #[tokio::test]
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        use crate::providers::upload_progress::fixture::{serve_logged, Route};
        let a = |id: &str| {
            format!(r#"{{"data":[{{"id":"{id}","attributes":{{"name":"a.txt","type":"file"}}}}]}}"#)
        };
        let patch = |path: &'static str| Route {
            method: axum::http::Method::PATCH,
            path,
            status: 200,
            body: "{}".to_string(),
            busy_first: false,
        };
        let (base, _server, received) = serve_logged(vec![
            Route::get("/workdrive/api/v1/files/S/files", 200, a("FA")),
            Route::get("/workdrive/api/v1/files/D/files", 200, a("FA2")),
            patch("/workdrive/api/v1/files/FA"),
            patch("/workdrive/api/v1/files"),
        ])
        .await;
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        p.endpoint_override = Some(base);
        p.test_access_token = Some("test-token".into());
        p.connected = true;
        for (path, id) in [("/", "ROOT"), ("/src", "S"), ("/dst", "D")] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        p.current_folder_id = "ROOT".to_string();
        p.rename("/src/a.txt", "/dst/c.txt")
            .await
            .expect("rename then move");
        let changes: Vec<&str> = received
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| *path)
            .filter(|path| !path.ends_with("/files") || *path == "/workdrive/api/v1/files")
            .collect();
        assert_eq!(
            changes,
            ["/workdrive/api/v1/files/FA", "/workdrive/api/v1/files"]
        );
    }

    /// A move to another folder under a new name is two calls. When the
    /// rename after the move was refused, the item stayed in the new folder
    /// under its old name while the error said nothing of it. The move is
    /// undone.
    #[tokio::test]
    async fn a_move_whose_rename_is_refused_is_moved_back() {
        use crate::providers::upload_progress::fixture::{serve_logged, Route};
        let patch = |path: &'static str, status: u16| Route {
            method: axum::http::Method::PATCH,
            path,
            status,
            body: "{}".to_string(),
            busy_first: false,
        };
        let (base, _server, received) = serve_logged(vec![
            Route::get(
                "/workdrive/api/v1/files/S/files",
                200,
                r#"{"data":[{"id":"FA","attributes":{"name":"a.txt","type":"file"}}]}"#,
            ),
            Route::get("/workdrive/api/v1/files/D/files", 200, r#"{"data":[]}"#),
            patch("/workdrive/api/v1/files/FA", 403),
            patch("/workdrive/api/v1/files", 200),
        ])
        .await;
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        p.endpoint_override = Some(base);
        p.test_access_token = Some("test-token".into());
        p.connected = true;
        for (path, id) in [("/", "ROOT"), ("/src", "S"), ("/dst", "D")] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        p.current_folder_id = "ROOT".to_string();
        let outcome = p.rename("/src/a.txt", "/dst/c.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        let changes: Vec<&str> = received
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| *path)
            .filter(|path| !path.ends_with("/files") || *path == "/workdrive/api/v1/files")
            .collect();
        assert_eq!(
            changes,
            [
                "/workdrive/api/v1/files",
                "/workdrive/api/v1/files/FA",
                "/workdrive/api/v1/files"
            ]
        );
    }

    /// When the name the undo would take back was taken meanwhile (here
    /// the source folder lists another `a.txt` after the move), moving back
    /// would meet it: what WorkDrive does then is not documented. The move
    /// stays, and the error says where the item is.
    #[tokio::test]
    async fn an_undo_whose_way_back_is_taken_is_not_made() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        let source_listings = Arc::new(AtomicUsize::new(0));
        let moves: Arc<Mutex<usize>> = Arc::default();
        let (listed, moved) = (Arc::clone(&source_listings), Arc::clone(&moves));
        let app = axum::Router::new()
            .route(
                "/workdrive/api/v1/files/S/files",
                axum::routing::get(move || {
                    // The source first, then another item under its name.
                    let id = if listed.fetch_add(1, Ordering::SeqCst) == 0 { "FA" } else { "FX" };
                    async move {
                        format!(
                            r#"{{"data":[{{"id":"{id}","attributes":{{"name":"a.txt","type":"file"}}}}]}}"#
                        )
                    }
                }),
            )
            .route(
                "/workdrive/api/v1/files/D/files",
                axum::routing::get(|| async { r#"{"data":[]}"# }),
            )
            .route(
                "/workdrive/api/v1/files",
                axum::routing::patch(move || {
                    *moved.lock().unwrap() += 1;
                    async { "{}" }
                }),
            )
            .route(
                "/workdrive/api/v1/files/FA",
                axum::routing::patch(|| async { (axum::http::StatusCode::FORBIDDEN, "{}") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut p = ZohoWorkdriveProvider::new(config("com"));
        p.endpoint_override = Some(format!("http://{addr}"));
        p.test_access_token = Some("test-token".into());
        p.connected = true;
        for (path, id) in [("/", "ROOT"), ("/src", "S"), ("/dst", "D")] {
            p.folder_cache.insert(path.to_string(), id.to_string());
        }
        p.current_folder_id = "ROOT".to_string();
        let outcome = p.rename("/src/a.txt", "/dst/c.txt").await;
        match outcome {
            Err(ProviderError::Other(message)) => {
                assert!(message.contains("now at /dst/a.txt"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(*moves.lock().unwrap(), 1, "no move back");
    }

    #[tokio::test]
    async fn deleting_a_folder_forgets_its_cached_path_and_everything_under_it() {
        let mut p = provider_with_cached_folder_a().await;
        p.rmdir("/a").await.expect("rmdir");
        assert!(!p.folder_cache.contains_key("/a"));
        assert!(!p.folder_cache.contains_key("/a/sub"));
    }

    /// #347: above the documented 250 MB of `POST /upload` (which answered a
    /// 287 MB file with 413), a file goes to the region's upload server.
    #[test]
    fn a_file_over_250_mb_takes_the_stream_upload() {
        let us = config("us");
        assert_eq!(large_upload_host(&us, 250 * 1000 * 1000), None);
        assert_eq!(
            large_upload_host(&us, 250 * 1000 * 1000 + 1),
            Some("upload.zoho.com")
        );
        assert_eq!(
            large_upload_host(&us, 287 * 1024 * 1024),
            Some("upload.zoho.com")
        );
        assert_eq!(large_upload_host(&us, 1024), None);
    }

    #[test]
    fn a_log_preview_never_splits_a_character() {
        // "è" is two bytes; put its first byte at index 299 of a 300-byte cut.
        let text = format!("{}è rest", "a".repeat(299));
        assert_eq!(log_preview(&text, 300), "a".repeat(299));
        assert_eq!(log_preview("short", 300), "short");
    }

    /// A 200 that does not list the file is not an upload: the API leaves
    /// failed files out of its answer.
    #[test]
    fn a_stream_upload_counts_only_when_the_file_is_listed() {
        let stored =
            r#"{"data":[{"attributes":{"file_name":"big.bin","parent_id":"p1"},"type":"files"}]}"#;
        assert!(stream_upload_stored(stored, "big.bin"));
        assert!(!stream_upload_stored(stored, "other.bin"));
        assert!(!stream_upload_stored(r#"{"data":[]}"#, "big.bin"));
        assert!(!stream_upload_stored("<html>", "big.bin"));
    }

    /// The GUI reaches `upload()` through the DAG engine (`provider_upload_file`
    /// -> `run_dag_upload_leaf`, and `upload_files_batch`): the stream route is
    /// only taken if the graph for a large Zoho file is ONE whole-file node,
    /// whose executor calls `upload()`. A multipart fan-out would bypass it.
    #[test]
    fn the_gui_dag_sends_a_large_file_to_upload_whole() {
        use crate::providers::StorageProvider;
        let provider = ZohoWorkdriveProvider::new(config("eu"));
        let caps = provider.transfer_capabilities();
        let built = crate::transfer_dag::TransferDagBuilder::shaped_file(
            crate::transfer_dag::TransferDirection::Upload,
            &caps,
            300 * 1024 * 1024,
        );
        assert_eq!(built.transfer.len(), 1, "one whole-file node");
        assert_eq!(
            built.profile.upload_parts, 1,
            "one UploadFile, no UploadPart fan-out"
        );
    }

    /// The hosts the Upload Large File API page lists; the UK data centre has
    /// none there, so it keeps the single request rather than a guessed host.
    #[test]
    fn upload_server_per_region_is_the_documented_one() {
        for (region, host) in [
            ("us", Some("upload.zoho.com")),
            ("eu", Some("upload.zoho.eu")),
            ("in", Some("upload.zoho.in")),
            ("au", Some("upload.zoho.com.au")),
            ("jp", Some("upload.zoho.jp")),
            ("cn", Some("upload.zoho.com.cn")),
            ("ae", Some("files.zoho.ae")),
            ("ca", Some("upload.zohocloud.ca")),
            ("sa", Some("files.zoho.sa")),
            ("uk", None),
        ] {
            assert_eq!(config(region).upload_domain(), host, "{region}");
        }
        assert_eq!(large_upload_host(&config("uk"), 1 << 30), None);
    }

    #[test]
    fn api_domain_maps_each_region_to_correct_host() {
        assert_eq!(config("us").api_domain(), "www.zohoapis.com");
        assert_eq!(config("eu").api_domain(), "www.zohoapis.eu");
        assert_eq!(config("in").api_domain(), "www.zohoapis.in");
        assert_eq!(config("au").api_domain(), "www.zohoapis.com.au");
        assert_eq!(config("jp").api_domain(), "www.zohoapis.jp");
        assert_eq!(config("uk").api_domain(), "www.zohoapis.uk");
        assert_eq!(config("ca").api_domain(), "www.zohoapis.ca");
        assert_eq!(config("sa").api_domain(), "www.zohoapis.sa");
        // unknown region falls back to US
        assert_eq!(config("unknown").api_domain(), "www.zohoapis.com");
    }

    #[test]
    fn download_domain_differs_from_api_domain() {
        assert_eq!(config("us").download_domain(), "download.zoho.com");
        assert_eq!(config("eu").download_domain(), "download.zoho.eu");
        assert_eq!(config("ae").download_domain(), "files.zoho.ae");
        assert_eq!(config("sa").download_domain(), "files.zoho.sa");
        // API domain and download domain must not be equal
        assert_ne!(config("us").api_domain(), config("us").download_domain());
    }

    #[test]
    fn api_base_builds_workdrive_v1_url() {
        let c = config("eu");
        assert_eq!(c.api_base(), "https://www.zohoapis.eu/workdrive/api/v1");
        let c = config("us");
        assert_eq!(c.api_base(), "https://www.zohoapis.com/workdrive/api/v1");
    }

    #[test]
    fn move_request_body_targets_the_collection_endpoint_shape() {
        // Regression for #451: a per-file `/files/{id}/move` PATCH is refused by
        // the gateway (F6016). The relocation is a bulk write, so `data` must be
        // an array whose element carries the id — not a bare object.
        let body = move_request_body("file-123", "folder-456");
        let items = body["data"].as_array().expect("data must be an array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "file-123");
        assert_eq!(items[0]["type"], "files");
        assert_eq!(items[0]["attributes"]["parent_id"], "folder-456");
        // The id belongs to the element, never to the envelope.
        assert!(body.get("id").is_none());
    }

    #[test]
    fn zoho_native_export_info_maps_zoho_extensions() {
        let (product, format, office_ext) = zoho_native_export_info("zw").unwrap();
        assert_eq!(product, "writer");
        assert_eq!(format, "docx");
        assert_eq!(office_ext, ".docx");

        let (product, format, _) = zoho_native_export_info("ZohoSheet").unwrap();
        assert_eq!(product, "sheet");
        assert_eq!(format, "xlsx");

        let (product, _, _) = zoho_native_export_info("zp").unwrap();
        assert_eq!(product, "show");

        // non-zoho extensions return None
        assert!(zoho_native_export_info("pdf").is_none());
        assert!(zoho_native_export_info("jpg").is_none());
        assert!(zoho_native_export_info("").is_none());
    }

    #[test]
    fn parse_byte_amount_handles_units_separators_and_rejects_garbage() {
        // Binary units (1024 base), both spaced and glued, with thousands commas.
        assert_eq!(parse_byte_amount("0").unwrap(), 0);
        assert_eq!(parse_byte_amount("").unwrap(), 0);
        assert_eq!(parse_byte_amount("512").unwrap(), 512); // bare number = bytes
        assert_eq!(parse_byte_amount("1 KB").unwrap(), 1024);
        assert_eq!(parse_byte_amount("1kb").unwrap(), 1024);
        assert_eq!(parse_byte_amount("1 MiB").unwrap(), 1024 * 1024);
        assert_eq!(parse_byte_amount("5 GB").unwrap(), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_byte_amount("2,048 KB").unwrap(), 2048 * 1024);
        assert_eq!(parse_byte_amount("1.5 KB").unwrap(), 1536);
        // Unknown unit and negative/non-finite numbers are errors.
        assert!(parse_byte_amount("10 ZB").is_err());
        assert!(parse_byte_amount("-1 KB").is_err());
        assert!(parse_byte_amount("abc").is_err());
    }

    #[test]
    fn parse_byte_amount_value_reads_number_string_and_object_shapes() {
        use serde_json::json;
        assert_eq!(parse_byte_amount_value(&json!(4096)), Some(4096));
        assert_eq!(parse_byte_amount_value(&json!(1.0)), Some(1));
        assert_eq!(parse_byte_amount_value(&json!("2 KB")), Some(2048));
        assert_eq!(
            parse_byte_amount_value(&json!({"bytes": "1 MB"})),
            Some(1024 * 1024)
        );
        assert_eq!(parse_byte_amount_value(&json!({"value": 100})), Some(100));
        assert_eq!(parse_byte_amount_value(&json!(true)), None);
        assert_eq!(parse_byte_amount_value(&json!("garbage")), None);
    }

    #[test]
    fn parse_privatespace_id_accepts_array_and_object_shapes() {
        // Team accounts answer with a JSON:API list.
        let array = r#"{"data":[{"id":"fmip966f979e195e64ec78e6846976861eed5","attributes":{"name":"My Folders"}}]}"#;
        assert_eq!(
            parse_privatespace_id(array).as_deref(),
            Some("fmip966f979e195e64ec78e6846976861eed5")
        );

        // Personal accounts may answer with a single object.
        let object = r#"{"data":{"id":"fmip9personal0000000000000000000000","attributes":{}}}"#;
        assert_eq!(
            parse_privatespace_id(object).as_deref(),
            Some("fmip9personal0000000000000000000000")
        );

        // First element wins when several workspaces are returned.
        let many = r#"{"data":[{"id":"first"},{"id":"second"}]}"#;
        assert_eq!(parse_privatespace_id(many).as_deref(), Some("first"));
    }

    #[test]
    fn parse_privatespace_id_rejects_empty_and_malformed_bodies() {
        assert_eq!(parse_privatespace_id(r#"{"data":[]}"#), None);
        assert_eq!(parse_privatespace_id(r#"{"data":{}}"#), None);
        assert_eq!(
            parse_privatespace_id(r#"{"errors":[{"title":"F6016"}]}"#),
            None
        );
        assert_eq!(parse_privatespace_id(""), None);
        assert_eq!(parse_privatespace_id("not json"), None);
    }

    #[test]
    fn rclone_root_folder_id_from_discovery_rejects_team_id_fallback() {
        assert_eq!(
            rclone_root_folder_id_from_discovery(
                Some("fmip966f979e195e64ec78e6846976861eed5"),
                "team-id",
                Some("team-id"),
            )
            .as_deref(),
            Some("fmip966f979e195e64ec78e6846976861eed5")
        );
        assert_eq!(
            rclone_root_folder_id_from_discovery(None, "team-id", Some("team-id")),
            None,
            "team id is not a folder id"
        );
        assert_eq!(
            rclone_root_folder_id_from_discovery(None, "  ", Some("team-id")),
            None
        );
        assert_eq!(
            rclone_root_folder_id_from_discovery(None, "folder-not-team", Some("team-id"))
                .as_deref(),
            Some("folder-not-team")
        );
        assert_eq!(
            rclone_root_folder_id_from_discovery(Some(""), "", None),
            None
        );
    }

    #[test]
    fn no_team_error_does_not_demand_creating_a_team() {
        // #451: personal accounts cannot create a team, so the error must point
        // at region and WorkDrive availability, not at "create a team".
        let msg = NO_TEAM_NO_PRIVATESPACE_ERROR.to_lowercase();
        assert!(!msg.contains("create a team"));
        assert!(msg.contains("privatespace"));
        assert!(msg.contains("region"));
    }

    #[test]
    fn find_byte_amount_by_keys_walks_nested_objects_and_arrays() {
        use serde_json::json;
        let body = json!({
            "data": {
                "attributes": { "storage_quota": "5 GB", "storage_used": 1048576 }
            }
        });
        assert_eq!(
            find_byte_amount_by_keys(&body, ZOHO_TOTAL_QUOTA_KEYS),
            Some(5 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            find_byte_amount_by_keys(&body, ZOHO_USED_QUOTA_KEYS),
            Some(1048576)
        );
        // Array recursion + absent keys -> None.
        let arr = json!([{"noise": 1}, {"quota": "10 MB"}]);
        assert_eq!(
            find_byte_amount_by_keys(&arr, ZOHO_TOTAL_QUOTA_KEYS),
            Some(10 * 1024 * 1024)
        );
        assert_eq!(
            find_byte_amount_by_keys(&json!({"x": 1}), ZOHO_TOTAL_QUOTA_KEYS),
            None
        );
    }

    /// A WorkDrive file can carry several external links, and the Manage
    /// list offers each one: revoking deletes the one picked, by its id.
    /// There was no `remove_share_link` at all, so the button only failed.
    #[tokio::test]
    async fn revoking_a_listed_link_deletes_that_link_id() {
        use std::sync::{Arc, Mutex};
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&calls);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                seen.lock().unwrap().push(format!(
                    "{method} {}{}",
                    uri.path(),
                    uri.query().map(|q| format!("?{q}")).unwrap_or_default()
                ));
                async { (axum::http::StatusCode::NO_CONTENT, r#""#) }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = ZohoWorkdriveProvider::new(config("com"));
        provider.endpoint_override = Some(format!("http://{addr}"));
        provider.test_access_token = Some("test-token".into());
        provider.connected = true;

        provider
            .remove_share_link_by_id("/a.txt", "abc123")
            .await
            .expect("deleted");
        let refused = provider.remove_share_link_by_id("/a.txt", "abc/../x").await;
        assert!(
            matches!(refused, Err(ProviderError::InvalidPath(_))),
            "{refused:?}"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            ["DELETE /workdrive/api/v1/links/abc123"]
        );
    }
}
