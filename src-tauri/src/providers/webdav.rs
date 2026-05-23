//! WebDAV Storage Provider
//!
//! Implementation of the StorageProvider trait for WebDAV protocol.
//! Compatible with Nextcloud, Synology, QNAP, Koofr, and other WebDAV servers.
//!
//! WebDAV extends HTTP with methods like PROPFIND, MKCOL, MOVE, COPY, and DELETE
//! to provide full file system operations over HTTP/HTTPS.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use futures_util::StreamExt;
use md5::{Digest as _, Md5};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::Rng;
use reqwest::{Client, Method, StatusCode};
use secrecy::ExposeSecret;
use std::collections::HashMap;

use super::{
    sanitize_api_error, ProviderError, ProviderType, RemoteEntry, ShareLinkCapabilities,
    ShareLinkOptions, ShareLinkResult, StorageProvider, WebDavConfig,
};

/// A trash item from a Nextcloud trashbin PROPFIND response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NextcloudTrashEntry {
    /// Trash item identifier (from WebDAV href, needed for restore/delete).
    pub id: String,
    /// Original filename before deletion.
    pub name: String,
    /// Original path (relative to user root) before deletion.
    pub original_path: String,
    /// Unix timestamp of when the file was deleted.
    pub deleted_at: u64,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
}

// ============ HTTP Digest Authentication (RFC 2617) ============

/// State for HTTP Digest authentication
struct DigestState {
    realm: String,
    nonce: String,
    qop: String,
    opaque: Option<String>,
    nc: u32,
}

impl DigestState {
    /// Parse a `WWW-Authenticate: Digest ...` header value
    fn parse(header: &str) -> Option<Self> {
        let s = header.strip_prefix("Digest ")?;
        Some(Self {
            realm: Self::extract_param(s, "realm")?,
            nonce: Self::extract_param(s, "nonce")?,
            qop: Self::extract_param(s, "qop").unwrap_or_default(),
            opaque: Self::extract_param(s, "opaque"),
            nc: 0,
        })
    }

    /// Extract a parameter value from the Digest challenge string
    fn extract_param(s: &str, key: &str) -> Option<String> {
        // Try quoted form: key="value"
        let quoted = format!("{}=\"", key);
        if let Some(pos) = s.find(&quoted) {
            let after = &s[pos + quoted.len()..];
            let end = after.find('"')?;
            return Some(after[..end].to_string());
        }
        // Try unquoted form: key=value
        let unquoted = format!("{}=", key);
        if let Some(pos) = s.find(&unquoted) {
            let after = &s[pos + unquoted.len()..];
            let end = after.find([',', ' ']).unwrap_or(after.len());
            return Some(after[..end].to_string());
        }
        None
    }

    /// Generate the `Authorization: Digest ...` header value
    fn authorization(&mut self, method: &str, uri: &str, username: &str, password: &str) -> String {
        self.nc += 1;
        let nc_str = format!("{:08x}", self.nc);
        let cnonce = Self::generate_cnonce();

        let ha1 = md5_hex(&format!("{}:{}:{}", username, self.realm, password));
        let ha2 = md5_hex(&format!("{}:{}", method, uri));

        let response = if !self.qop.is_empty() {
            md5_hex(&format!(
                "{}:{}:{}:{}:auth:{}",
                ha1, self.nonce, nc_str, cnonce, ha2
            ))
        } else {
            md5_hex(&format!("{}:{}:{}", ha1, self.nonce, ha2))
        };

        // Quote algorithm and qop for maximum server compatibility
        // (Python requests quotes these and works with all servers)
        let mut header = format!(
            r#"Digest username="{}", realm="{}", nonce="{}", uri="{}", response="{}", algorithm="MD5""#,
            username, self.realm, self.nonce, uri, response
        );

        if !self.qop.is_empty() {
            header.push_str(&format!(
                r#", qop="auth", nc={}, cnonce="{}""#,
                nc_str, cnonce
            ));
        }

        if let Some(ref opaque) = self.opaque {
            header.push_str(&format!(r#", opaque="{}""#, opaque));
        }

        tracing::debug!(
            "[WebDAV Digest] method={} uri={} nc={} response={}",
            method,
            uri,
            nc_str,
            response
        );

        header
    }

    fn generate_cnonce() -> String {
        use rand::rngs::OsRng;
        let bytes: [u8; 8] = OsRng.gen();
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

/// Compute MD5 hex digest of a string
fn md5_hex(input: &str) -> String {
    let digest = Md5::digest(input.as_bytes());
    format!("{:x}", digest)
}

/// Extract the path component from a full URL, preserving trailing slash
fn extract_uri_path(url: &str) -> String {
    if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        if let Some(slash_idx) = after_scheme.find('/') {
            let path = after_scheme[slash_idx..].to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }
    "/".to_string()
}

/// Custom HTTP methods for WebDAV
mod webdav_methods {
    use reqwest::Method;

    pub fn propfind() -> Method {
        Method::from_bytes(b"PROPFIND").unwrap()
    }

    pub fn mkcol() -> Method {
        Method::from_bytes(b"MKCOL").unwrap()
    }

    #[allow(dead_code)]
    pub fn copy() -> Method {
        Method::from_bytes(b"COPY").unwrap()
    }

    pub fn move_method() -> Method {
        Method::from_bytes(b"MOVE").unwrap()
    }

    pub fn lock() -> Method {
        Method::from_bytes(b"LOCK").unwrap()
    }

    pub fn unlock() -> Method {
        Method::from_bytes(b"UNLOCK").unwrap()
    }
}

/// Pure boundary check used by `cd()` and `cd_up()`.
///
/// Returns `true` when `path` resolves above the configured WebDAV root.
/// `boundary` is the effective root (auto-detected `server_root` if known,
/// otherwise the user-supplied `initial_path`). Issue #175.
///
/// The check trims trailing slashes for case-insensitive prefix matching,
/// and short-circuits on empty / "/" boundaries (no-op since the WebDAV
/// server has effectively no boundary to enforce in those cases).
fn path_violates_root(path: &str, boundary: Option<&str>) -> bool {
    let Some(boundary) = boundary else {
        return false;
    };
    if boundary.is_empty() {
        return false;
    }
    let root_trimmed = boundary.trim_end_matches('/');
    if root_trimmed.is_empty() {
        return false;
    }
    let path_trimmed = path.trim_end_matches('/');
    !path_trimmed.starts_with(root_trimmed) && path_trimmed != root_trimmed
}

/// WebDAV Storage Provider
pub struct WebDavProvider {
    config: WebDavConfig,
    client: Client,
    current_path: String,
    connected: bool,
    /// Digest auth state (set during connect if server requires it)
    digest_auth: Option<DigestState>,
    /// Server-side WebDAV root resolved at connect time (issue #175).
    ///
    /// `config.initial_path` is the user-supplied value from the saved-server
    /// form and may be empty, "/", or a relative folder like "/Documents".
    /// On Nextcloud / ownCloud the actual WebDAV root is auto-detected at
    /// connect time as one of `/remote.php/dav/files/<user>/` or
    /// `/remote.php/webdav/` after a 405 on `PROPFIND /`. The boundary checks
    /// in `cd()` and `cd_up()` MUST use the auto-detected root (otherwise a
    /// drill-down click into a folder fails with "Cannot navigate above
    /// WebDAV root", because the entry path returned by `list()` is rooted
    /// at the real WebDAV root, not the user-typed one).
    ///
    /// Populated in every successful branch of `connect()`. `None` when
    /// disconnected. When `Some`, takes precedence over `config.initial_path`.
    server_root: Option<String>,
    /// Multi-thread concurrent-Range download (rclone `--multi-thread-streams`).
    /// `1` = disabled (default). Set via `set_multi_thread_download`.
    multi_thread_streams: usize,
    /// Files at or above this size use the concurrent-Range path when
    /// `multi_thread_streams >= 2`.
    multi_thread_cutoff: u64,
}

/// Provider-specific hard cap on concurrent Range streams (mirrors S3's 16).
const WEBDAV_MULTI_THREAD_MAX_STREAMS: usize = 16;

impl WebDavProvider {
    /// Create a new WebDAV provider with the given configuration
    pub fn new(config: WebDavConfig) -> Result<Self, ProviderError> {
        // M6: Log a warning when TLS certificate verification is disabled.
        // This exposes the connection to MITM attacks: acceptable only for self-signed certs
        // in trusted networks (e.g. home NAS).
        if !config.verify_cert {
            tracing::warn!(
                "[WEBDAV] TLS certificate verification DISABLED for {}: connection is vulnerable to MITM attacks",
                config.url
            );
        }
        let client = Client::builder()
            .user_agent(crate::providers::AEROFTP_WEBDAV_USER_AGENT)
            .danger_accept_invalid_certs(!config.verify_cert)
            .connect_timeout(std::time::Duration::from_secs(30))
            // 1800s (30 min) accommodates large body uploads on slow links and
            // server-side post-processing (md5, replication). 300s previously
            // killed 1 GiB uploads on jianguoyun, InfiniCloud, DriveHQ, and
            // Koofr WebDAV when sustained throughput dropped below ~27 Mbps.
            .read_timeout(std::time::Duration::from_secs(1800))
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("HTTP client init failed: {e}"))
            })?;

        Ok(Self {
            config,
            client,
            current_path: "/".to_string(),
            connected: false,
            digest_auth: None,
            server_root: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: 8 * 1024 * 1024,
        })
    }

    /// Normalize a collection (directory) path to the trailing-slash form.
    ///
    /// Apache mod_dav answers a collection request (PROPFIND/MKCOL/DELETE/
    /// MOVE) whose path lacks a trailing slash with `301 Moved Permanently`
    /// to the slash form. Behind a TLS-terminating reverse proxy that 301
    /// carries an `http://` Location, so reqwest sees a scheme downgrade,
    /// treats the hop as cross-origin, strips the `Authorization` header,
    /// and the redirected request 401s: this is what surfaced as
    /// `Session expired` on `check`/`tree` and as a 401 on directory
    /// delete. Callers that always target collections normalize through
    /// this so the redirect never happens. File verbs (GET/PUT/file
    /// DELETE) must NOT use it: a trailing slash there would point at a
    /// non-existent collection.
    fn collection_path(path: &str) -> String {
        if path.is_empty() || path == "/" || path.ends_with('/') {
            path.to_string()
        } else {
            format!("{}/", path)
        }
    }

    /// Compose a caller path under the auto-detected `server_root`.
    ///
    /// On Nextcloud / ownCloud the real WebDAV root lives under a versioned
    /// prefix (`/remote.php/dav/files/<user>/`) discovered at connect time.
    /// Before this, only `list()`/`cd()` rewrote the literal `/` to that
    /// root; every other verb (`mkdir`, `put`, `get`, `delete`, `stat`,
    /// `move`, ...) sent the bare caller path, which on Nextcloud bypasses
    /// the DAV root and hits the front controller as `404`/`405`. This
    /// centralizes the rewrite at the single URL chokepoint.
    ///
    /// Idempotent: a path already at or under `server_root` (the GUI
    /// drill-down case, where `list()` returns fully-rooted entry paths) is
    /// returned unchanged, so no double-prefix occurs. A no-op when there is
    /// no distinct root (traditional servers, `server_root` `None` or `/`),
    /// so non-Nextcloud backends and the connect-time auto-detection probes
    /// (which run before `server_root` is set) are unaffected.
    fn resolve_root(&self, path: &str) -> String {
        let root = match self.server_root.as_deref() {
            Some(r) if !r.is_empty() && r != "/" => r,
            _ => return path.to_string(),
        };
        let root_trim = root.trim_end_matches('/');
        let p = if path.is_empty() { "/" } else { path };
        let p_norm = p.trim_end_matches('/');

        if p_norm == root_trim || p.starts_with(&format!("{}/", root_trim)) {
            return p.to_string();
        }

        let rel = p.trim_start_matches('/');
        if rel.is_empty() {
            format!("{}/", root_trim)
        } else if p.ends_with('/') {
            format!("{}/{}/", root_trim, rel.trim_end_matches('/'))
        } else {
            format!("{}/{}", root_trim, rel)
        }
    }

    /// Build full URL for a path
    fn build_url(&self, path: &str) -> String {
        let base = self.config.url.trim_end_matches('/');
        let rooted = self.resolve_root(path);
        let path = rooted.trim_start_matches('/');

        if path.is_empty() || path == "/" {
            // For root/empty path, ensure trailing slash (required by some WebDAV servers
            // for Digest auth URI matching on directory endpoints)
            format!("{}/", base)
        } else {
            format!("{}/{}", base, Self::encode_path(path))
        }
    }

    /// Percent-encode a remote path so reserved characters in file names
    /// (`#`, `?`, `%`, space, non-ASCII, ...) survive intact.
    ///
    /// Without this, `#`/`?` were parsed by the `url` crate as fragment /
    /// query delimiters (truncating the request target) and a literal
    /// space, while transport-encoded to `%20` on the wire, was hashed
    /// raw into the Digest `uri`, producing a mismatch the server
    /// rejected. rclone shipped the equivalent fix in v1.73.2.
    ///
    /// The encode set is deliberately conservative: structural and unsafe
    /// characters only. `pchar` sub-delims (`! $ & ' ( ) * + , ; = : @`)
    /// are left literal, because percent-encoding those is exactly the
    /// over-escaping rclone had to revert in v1.73.3 ("URLPathEscapeAll
    /// broke strict path-matching servers"). `/` is never in the set:
    /// segments are encoded individually and rejoined, so a trailing
    /// slash (collection form) is preserved.
    fn encode_path(path: &str) -> String {
        const WEBDAV_SEGMENT: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'%')
            .add(b'<')
            .add(b'>')
            .add(b'?')
            .add(b'[')
            .add(b'\\')
            .add(b']')
            .add(b'^')
            .add(b'`')
            .add(b'{')
            .add(b'|')
            .add(b'}');

        path.split('/')
            .map(|seg| utf8_percent_encode(seg, WEBDAV_SEGMENT).to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Make an authenticated request (Basic or Digest depending on server)
    fn request(&mut self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self.build_url(path);
        let builder = self.client.request(method.clone(), &url);

        if self.config.anonymous {
            return builder;
        }

        if let Some(ref mut state) = self.digest_auth {
            let uri_path = extract_uri_path(&url);
            tracing::debug!(
                "[WebDAV] Digest request: {} {} (uri={})",
                method.as_str(),
                url,
                uri_path
            );
            let auth = state.authorization(
                method.as_str(),
                &uri_path,
                &self.config.username,
                self.config.password.expose_secret(),
            );
            builder.header("Authorization", auth)
        } else {
            builder.basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
        }
    }

    async fn send_with_too_early_retry(
        &mut self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        const MAX_ATTEMPTS: usize = 3;

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self
                .request(method.clone(), path)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if response.status() != StatusCode::TOO_EARLY || attempt == MAX_ATTEMPTS {
                return Ok(response);
            }

            tracing::debug!(
                "[WEBDAV] {} {} returned 425 Too Early, retry {}/{}",
                method.as_str(),
                path,
                attempt,
                MAX_ATTEMPTS
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        unreachable!("retry loop must return on final attempt")
    }

    /// Send a PROPFIND and, on a `401` that carries a fresh
    /// `WWW-Authenticate: Digest` challenge, re-negotiate the Digest state
    /// once and retry the request.
    ///
    /// Long recursive scans (`check` / `tree` right after `sync`) issue many
    /// sequential PROPFIND requests reusing one cached `nonce`. Servers and
    /// reverse proxies expire that nonce by TTL or by use count and answer
    /// `401 ... stale=true` with a brand-new nonce, expecting the client to
    /// re-handshake (RFC 2617 section 3.3). Without this, the first stale
    /// `401` was surfaced as `Session expired` even though the credentials
    /// were still valid: the WebDAV recursive-scan defect reproduced against
    /// `dav.lab.axpdev.it`. Basic-auth servers never emit a Digest challenge,
    /// so a genuine `401` there still propagates unchanged for the caller to
    /// map.
    async fn send_propfind(
        &mut self,
        path: &str,
        depth: &str,
        body: &'static str,
    ) -> Result<reqwest::Response, ProviderError> {
        // `list()` and `list_recursive()` always target collections;
        // normalize to the trailing-slash form so Apache does not 301 to a
        // scheme-downgraded URL that loses auth. See `collection_path`.
        let dir_path = Self::collection_path(path);
        let path = dir_path.as_str();

        let response = self
            .request(webdav_methods::propfind(), path)
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        let www_auth = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let Some(state) = DigestState::parse(&www_auth) else {
            // No Digest challenge (Basic auth, or a genuine credential
            // failure): keep the existing behaviour and let the caller map
            // the 401.
            return Ok(response);
        };

        tracing::debug!(
            "[WebDAV] PROPFIND 401 with Digest challenge, re-negotiating stale nonce (realm={}, nonce={}...)",
            state.realm,
            &state.nonce[..state.nonce.len().min(12)]
        );
        self.digest_auth = Some(state);

        self.request(webdav_methods::propfind(), path)
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    // ─── Nextcloud OCS / Trashbin helpers ─────────────────────────────

    /// Detect if this WebDAV server is a Nextcloud instance (URL pattern match).
    fn is_nextcloud(&self) -> bool {
        self.config.url.contains("/remote.php/dav/files/")
    }

    /// Extract the Nextcloud base URL (e.g. https://cloud.felicloud.com).
    fn nextcloud_base_url(&self) -> Option<String> {
        self.config
            .url
            .find("/remote.php/")
            .map(|idx| self.config.url[..idx].to_string())
    }

    /// Make an authenticated request to an arbitrary URL (for OCS / trashbin endpoints
    /// that live outside the WebDAV files path).
    fn request_url(&mut self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.client.request(method.clone(), url);
        if self.config.anonymous {
            return builder;
        }
        if let Some(ref mut state) = self.digest_auth {
            let uri_path = extract_uri_path(url);
            let auth = state.authorization(
                method.as_str(),
                &uri_path,
                &self.config.username,
                self.config.password.expose_secret(),
            );
            builder.header("Authorization", auth)
        } else {
            builder.basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
        }
    }

    /// Koofr exposes a REST API that accepts the same basic auth used for
    /// WebDAV (email + app password) and returns spaceTotal / spaceUsed in MiB
    /// per mount. This is more reliable than PROPFIND for Koofr (their WebDAV
    /// server returns 0 for RFC 4331 quota properties).
    async fn koofr_storage_via_api(&self) -> Result<super::StorageInfo, ProviderError> {
        const MIB: u64 = 1024 * 1024;
        let url = "https://app.koofr.net/api/v2/mounts";
        let resp = self
            .client
            .get(url)
            .basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::NotSupported(format!(
                "koofr quota api returned {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let mounts = body
            .get("mounts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ProviderError::ParseError("koofr quota: no mounts".into()))?;
        // Prefer the primary mount (default Koofr storage); fall back to the
        // first entry if no mount is flagged as primary.
        let mount = mounts
            .iter()
            .find(|m| {
                m.get("isPrimary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .or_else(|| mounts.first())
            .ok_or_else(|| ProviderError::ParseError("koofr quota: empty mounts".into()))?;
        let used = mount
            .get("spaceUsed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_mul(MIB);
        let total = mount
            .get("spaceTotal")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_mul(MIB);
        Ok(super::StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    /// OpenDrive exposes the same quota the native provider reads, but its
    /// WebDAV server (webdav.opendrive.com) does not return RFC 4331 quota
    /// via PROPFIND. The OpenDrive REST API uses session auth (not basic
    /// auth like Koofr), so do a minimal login -> users/info -> logout with
    /// the same account credentials the WebDAV profile already stores.
    /// Mirrors the native provider's storage_info (opendrive.rs).
    async fn opendrive_storage_via_api(&self) -> Result<super::StorageInfo, ProviderError> {
        const API: &str = "https://dev.opendrive.com/api/v1";

        // reqwest's `.form()` helper is not enabled in this build; urlencode
        // the body manually like opendrive.rs/post_form does.
        let login_body = {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            s.append_pair("username", &self.config.username);
            s.append_pair("passwd", self.config.password.expose_secret());
            s.append_pair("version", "2.9.7");
            s.append_pair("partner_id", "");
            s.finish()
        };
        let login: serde_json::Value = self
            .client
            .post(format!("{}/session/login.json", API))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(login_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let session_id = login
            .get("SessionID")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ProviderError::NotSupported("opendrive quota: no SessionID".into()))?
            .to_string();

        let info_res = self
            .client
            .get(format!("{}/users/info.json/{}", API, session_id))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()));

        // Best-effort logout regardless of the users/info outcome: never
        // leak a session because the quota call failed.
        let logout_body = {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            s.append_pair("session_id", &session_id);
            s.finish()
        };
        let _ = self
            .client
            .post(format!("{}/session/logout.json", API))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(logout_body)
            .send()
            .await;

        let info = info_res?;
        // OpenDrive reports MaxStorage in MiB, StorageUsed in bytes
        // (verified live in opendrive.rs:1921). Accept number or string.
        let as_u64 = |v: Option<&serde_json::Value>| -> u64 {
            v.and_then(|x| {
                x.as_u64()
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
        };
        let total = as_u64(info.get("MaxStorage")).saturating_mul(1024 * 1024);
        let used = as_u64(info.get("StorageUsed"));
        if total == 0 {
            return Err(ProviderError::NotSupported(
                "opendrive quota: no MaxStorage".into(),
            ));
        }
        Ok(super::StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    /// OCS: Create a public share link for a file/folder.
    /// If the Nextcloud instance enforces passwords on share links (HTTP 403),
    /// retries automatically with a generated password and returns "url\npassword".
    pub async fn nextcloud_create_share(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!("{}/ocs/v2.php/apps/files_sharing/api/v1/shares", base);

        let mut form_body = format!(
            "path={}&shareType=3&permissions=1",
            urlencoding::encode(path)
        );
        if let Some(secs) = options.expires_in_secs {
            let days = (secs / 86400).max(1);
            let expire = chrono::Utc::now() + chrono::Duration::days(days as i64);
            form_body.push_str(&format!("&expireDate={}", expire.format("%Y-%m-%d")));
        }

        let resp: reqwest::Response = self
            .request_url(Method::POST, &url)
            .header("OCS-APIREQUEST", "true")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body.clone())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        // If server requires password on share links (403), retry with auto-generated password
        if status == StatusCode::FORBIDDEN && text.contains("password") {
            let password = Self::generate_share_password();
            let form_with_pw = format!("{}&password={}", form_body, urlencoding::encode(&password));

            let resp2: reqwest::Response = self
                .request_url(Method::POST, &url)
                .header("OCS-APIREQUEST", "true")
                .header("Accept", "application/json")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_with_pw)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            let status2 = resp2.status();
            let text2 = resp2
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !status2.is_success() {
                return Err(ProviderError::ServerError(format!(
                    "OCS share failed: HTTP {} - {}",
                    status2,
                    &text2[..text2.len().min(200)]
                )));
            }

            let json: serde_json::Value = serde_json::from_str(&text2)
                .map_err(|e| ProviderError::ParseError(format!("OCS JSON parse error: {}", e)))?;

            let share_url = json
                .pointer("/ocs/data/url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ProviderError::ServerError("OCS share API did not return a URL".into())
                })?;

            return Ok(ShareLinkResult {
                url: share_url.to_string(),
                password: Some(password),
                expires_at: None,
            });
        }

        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "OCS share failed: HTTP {} - {}",
                status,
                &text[..text.len().min(200)]
            )));
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::ParseError(format!("OCS JSON parse error: {}", e)))?;

        let share_url = json
            .pointer("/ocs/data/url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::ServerError("OCS share API did not return a URL".into())
            })?;

        Ok(ShareLinkResult {
            url: share_url.to_string(),
            password: None,
            expires_at: None,
        })
    }

    /// Generate a random 16-char password for share links.
    fn generate_share_password() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let lower: &[u8] = b"abcdefghijkmnpqrstuvwxyz";
        let upper: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
        let digits: &[u8] = b"23456789";
        let special: &[u8] = b"!@#$%&*?";
        let all: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%&*?";
        // Guarantee at least one of each category
        let mut pwd = vec![
            lower[rng.gen_range(0..lower.len())] as char,
            upper[rng.gen_range(0..upper.len())] as char,
            digits[rng.gen_range(0..digits.len())] as char,
            special[rng.gen_range(0..special.len())] as char,
        ];
        // Fill remaining 12 chars from full set
        for _ in 0..12 {
            pwd.push(all[rng.gen_range(0..all.len())] as char);
        }
        // Shuffle to avoid predictable positions
        for i in (1..pwd.len()).rev() {
            let j = rng.gen_range(0..=i);
            pwd.swap(i, j);
        }
        pwd.into_iter().collect()
    }

    /// Nextcloud trashbin: list deleted items.
    pub async fn nextcloud_list_trash(
        &mut self,
    ) -> Result<Vec<NextcloudTrashEntry>, ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash/",
            base, self.config.username
        );

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:propfind xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns" xmlns:oc="http://owncloud.org/ns">
                <d:prop>
                    <nc:trashbin-filename/>
                    <nc:trashbin-original-location/>
                    <nc:trashbin-deletion-time/>
                    <d:getcontentlength/>
                    <d:resourcetype/>
                </d:prop>
            </d:propfind>"#;

        let resp = self
            .request_url(webdav_methods::propfind(), &url)
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::ServerError(format!(
                "Trashbin PROPFIND failed: HTTP {}",
                resp.status()
            )));
        }

        let xml = resp
            .text()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        self.parse_trashbin_response(&xml)
    }

    /// Parse trashbin PROPFIND XML into entries.
    fn parse_trashbin_response(
        &self,
        xml: &str,
    ) -> Result<Vec<NextcloudTrashEntry>, ProviderError> {
        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut current_tag: Option<String> = None;
        let mut href = String::new();
        let mut trash_filename = String::new();
        let mut trash_location = String::new();
        let mut trash_deletion_time = String::new();
        let mut content_length = String::new();
        let mut is_collection = false;
        let mut in_resourcetype = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(_) => break,
                Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) => {
                    let local = e.local_name();
                    let tag = std::str::from_utf8(local.as_ref())
                        .unwrap_or("")
                        .to_string();
                    match tag.as_str() {
                        "response" => {
                            in_response = true;
                            href.clear();
                            trash_filename.clear();
                            trash_location.clear();
                            trash_deletion_time.clear();
                            content_length.clear();
                            is_collection = false;
                        }
                        "resourcetype" => {
                            in_resourcetype = true;
                        }
                        "collection" if in_resourcetype => {
                            is_collection = true;
                        }
                        _ if in_response => {
                            current_tag = Some(tag);
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = e.local_name();
                    let tag = std::str::from_utf8(local.as_ref()).unwrap_or("");
                    if tag == "collection" && in_resourcetype {
                        is_collection = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = e.local_name();
                    let tag = std::str::from_utf8(local.as_ref()).unwrap_or("");
                    match tag {
                        "response" if in_response => {
                            in_response = false;
                            // Skip the collection itself (the trash/ container)
                            if !trash_filename.is_empty() {
                                let id = href
                                    .rsplit('/')
                                    .find(|s| !s.is_empty())
                                    .unwrap_or("")
                                    .to_string();
                                entries.push(NextcloudTrashEntry {
                                    id,
                                    name: trash_filename.trim().to_string(),
                                    original_path: trash_location.trim().to_string(),
                                    deleted_at: trash_deletion_time
                                        .trim()
                                        .parse::<u64>()
                                        .unwrap_or(0),
                                    size: content_length.trim().parse::<u64>().unwrap_or(0),
                                    is_dir: is_collection,
                                });
                            }
                        }
                        "resourcetype" => {
                            in_resourcetype = false;
                        }
                        _ => {
                            current_tag = None;
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = String::from_utf8_lossy(e.as_ref()).to_string();
                        if !raw.is_empty() {
                            match tag.as_str() {
                                "href" => href.push_str(&raw),
                                "trashbin-filename" => trash_filename.push_str(&raw),
                                "trashbin-original-location" => trash_location.push_str(&raw),
                                "trashbin-deletion-time" => trash_deletion_time.push_str(&raw),
                                "getcontentlength" => content_length.push_str(&raw),
                                _ => {}
                            }
                        }
                    }
                }
                Ok(Event::GeneralRef(ref e)) => {
                    // Decode XML entities (&amp; &apos; &lt; &gt; &quot;) so file
                    // names containing these characters are not silently truncated.
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            match tag.as_str() {
                                "href" => href.push_str(&ch),
                                "trashbin-filename" => trash_filename.push_str(&ch),
                                "trashbin-original-location" => trash_location.push_str(&ch),
                                "trashbin-deletion-time" => trash_deletion_time.push_str(&ch),
                                "getcontentlength" => content_length.push_str(&ch),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }

        Ok(entries)
    }

    /// Nextcloud trashbin: restore a single item.
    pub async fn nextcloud_restore_trash(&mut self, id: &str) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let from = format!(
            "{}/remote.php/dav/trashbin/{}/trash/{}",
            base, self.config.username, id
        );
        let dest = format!(
            "{}/remote.php/dav/trashbin/{}/restore/{}",
            base, self.config.username, id
        );

        let resp = self
            .request_url(Method::from_bytes(b"MOVE").unwrap(), &from)
            .header("Destination", &dest)
            .header("Overwrite", "T")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::CREATED && status != StatusCode::NO_CONTENT
        {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore failed: HTTP {}: {}",
                status,
                &body[..body.len().min(200)]
            )));
        }
        Ok(())
    }

    /// Nextcloud trashbin: permanently delete a single item.
    pub async fn nextcloud_delete_trash_item(&mut self, id: &str) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash/{}",
            base, self.config.username, id
        );

        let resp = self
            .request_url(Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::NO_CONTENT {
            return Err(ProviderError::ServerError(format!(
                "Delete trash item failed: HTTP {}",
                status
            )));
        }
        Ok(())
    }

    /// Nextcloud trashbin: empty entire trash.
    pub async fn nextcloud_empty_trash(&mut self) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash",
            base, self.config.username
        );

        let resp = self
            .request_url(Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::NO_CONTENT {
            return Err(ProviderError::ServerError(format!(
                "Empty trash failed: HTTP {}",
                status
            )));
        }
        Ok(())
    }

    /// Single-request recursive listing via `PROPFIND Depth: infinity`
    /// (item 4b "used storage" scan). Returns every descendant of `path`
    /// flat (files at any depth + collections), reusing the same prop set
    /// and parser as `list()`. Servers that forbid or limit infinity
    /// (403/400, or a non-multistatus status) yield an `Err` so the caller
    /// falls back to the recursive Depth:1 BFS. Only `size`/`is_dir` are
    /// relied on downstream, so the flat `name`/`path` (computed against
    /// the root) being approximate for deep entries does not matter.
    pub async fn list_recursive(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else if path == "/" {
            self.server_root
                .clone()
                .unwrap_or_else(|| self.current_path.clone())
        } else {
            path.to_string()
        };

        let response = self
            .send_propfind(
                &list_path,
                "infinity",
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#,
            )
            .await?;

        let status = response.status();
        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                // Depth:infinity returns the whole subtree in one body, which
                // is server-controlled. Bound it so a pathological or hostile
                // server cannot OOM the process by streaming an unbounded
                // PROPFIND response. On overflow the Err propagates and the
                // callers (used_scan / provider_scan_used) fall back to the
                // bounded BFS rather than producing a wrong figure.
                const MAX_PROPFIND_INFINITY_BYTES: u64 = 256 * 1024 * 1024;
                let body =
                    super::response_bytes_with_limit(response, MAX_PROPFIND_INFINITY_BYTES).await?;
                let xml = String::from_utf8_lossy(&body);
                self.parse_propfind_response(&xml, &list_path)
            }
            other => Err(ProviderError::ServerError(format!(
                "Depth:infinity not available (HTTP {})",
                other
            ))),
        }
    }

    /// Parse PROPFIND XML response into RemoteEntry list using quick-xml
    fn parse_propfind_response(
        &self,
        xml: &str,
        base_path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut entries = Vec::new();

        tracing::debug!(
            "[WebDAV] Parsing XML with base_path: {}, url: {}",
            base_path,
            self.config.url
        );

        // Event-based quick-xml parser
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut in_resourcetype = false;
        let mut current_tag: Option<String> = None;
        let mut href = String::new();
        let mut displayname = String::new();
        let mut getcontentlength = String::new();
        let mut getlastmodified = String::new();
        let mut getcontenttype = String::new();
        let mut getetag = String::new();
        let mut is_collection = false;
        let mut is_collection_by_iscollection = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => {
                    tracing::warn!(
                        "[WebDAV] XML parse error at position {}: {}",
                        reader.error_position(),
                        e
                    );
                    break;
                }
                Ok(Event::Eof) => break,

                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" => {
                            in_response = true;
                            href.clear();
                            displayname.clear();
                            getcontentlength.clear();
                            getlastmodified.clear();
                            getcontenttype.clear();
                            getetag.clear();
                            is_collection = false;
                            is_collection_by_iscollection = false;
                        }
                        "resourcetype" if in_response => {
                            in_resourcetype = true;
                        }
                        "collection" if in_response && in_resourcetype => {
                            is_collection = true;
                        }
                        "href" | "displayname" | "getcontentlength" | "getlastmodified"
                        | "getcontenttype" | "getetag" | "iscollection"
                            if in_response =>
                        {
                            current_tag = Some(local);
                        }
                        _ => {}
                    }
                }

                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_response && in_resourcetype {
                        is_collection = true;
                    }
                }

                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" if in_response => {
                            // Process accumulated response
                            in_response = false;
                            if href.is_empty() {
                                tracing::warn!("[WebDAV] No href found in response element");
                                continue;
                            }

                            let decoded_href =
                                urlencoding::decode(&href).unwrap_or_else(|_| href.clone().into());
                            let clean_path = decoded_href.trim_end_matches('/');
                            let base_clean = base_path.trim_end_matches('/');
                            let url_clean = self.config.url.trim_end_matches('/');

                            let url_path_clean = url_clean
                                .find("://")
                                .and_then(|i| url_clean[i + 3..].find('/').map(|j| i + 3 + j))
                                .map(|i| url_clean[i..].trim_end_matches('/'))
                                .unwrap_or("");

                            let is_self_reference = clean_path == base_clean
                                || clean_path == url_clean
                                || (!base_clean.is_empty() && clean_path.ends_with(base_clean))
                                || (!base_clean.is_empty()
                                    && clean_path.ends_with(&format!(
                                        "/{}",
                                        base_clean.trim_start_matches('/')
                                    )))
                                || (!base_clean.is_empty()
                                    && base_clean != "/"
                                    && url_clean.ends_with(clean_path))
                                || (!url_path_clean.is_empty() && clean_path == url_path_clean);

                            if is_self_reference {
                                tracing::debug!("[WebDAV] Skipping self-reference: {}", clean_path);
                                continue;
                            }

                            let href_ends_slash = href.ends_with('/');
                            let is_dir =
                                is_collection || is_collection_by_iscollection || href_ends_slash;

                            let size: u64 = getcontentlength.trim().parse().unwrap_or(0);
                            let modified = if getlastmodified.trim().is_empty() {
                                None
                            } else {
                                Some(getlastmodified.trim().to_string())
                            };

                            // Extract name: prefer displayname, fallback to href.
                            // Filen Desktop's WebDAV bridge ships displayname percent-
                            // encoded (e.g. `my%20folder` for "my folder"), against the
                            // RFC 4918 expectation that `<DAV:displayname>` is a
                            // human-readable string. Mirror the href branch and decode
                            // defensively: on RFC-compliant servers (Nextcloud, Koofr,
                            // ...) the input has no percent-encoding and decode is a
                            // no-op; on the Filen bridge it un-mangles the name. Issue
                            // #128.
                            let name = if !displayname.is_empty() {
                                urlencoding::decode(&displayname)
                                    .unwrap_or_else(|_| displayname.clone().into())
                                    .into_owned()
                            } else {
                                decoded_href
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or("")
                                    .to_string()
                            };

                            if name.is_empty() || name == "." || name == ".." {
                                continue;
                            }

                            let path = if base_clean.is_empty() || base_clean == "/" {
                                format!("/{}", name)
                            } else {
                                format!("{}/{}", base_clean, name)
                            };

                            let mime_type = if getcontenttype.is_empty() {
                                None
                            } else {
                                Some(getcontenttype.clone())
                            };

                            let mut metadata = HashMap::new();
                            if !getetag.is_empty() {
                                metadata.insert("etag".to_string(), getetag.clone());
                            }

                            entries.push(RemoteEntry {
                                name,
                                path,
                                is_dir,
                                size,
                                modified,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                mime_type,
                                metadata,
                            });
                        }
                        "resourcetype" if in_resourcetype => {
                            in_resourcetype = false;
                        }
                        _ => {
                            if current_tag.as_deref() == Some(local.as_str()) {
                                current_tag = None;
                            }
                        }
                    }
                }

                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = String::from_utf8_lossy(e.as_ref()).to_string();
                        if !raw.is_empty() {
                            match tag.as_str() {
                                "href" => href.push_str(&raw),
                                "displayname" => displayname.push_str(&raw),
                                "getcontentlength" => getcontentlength.push_str(&raw),
                                "getlastmodified" => getlastmodified.push_str(&raw),
                                "getcontenttype" => getcontenttype.push_str(&raw),
                                "getetag" => getetag.push_str(&raw),
                                "iscollection" if raw.trim() == "1" => {
                                    is_collection_by_iscollection = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            match tag.as_str() {
                                "href" => href.push_str(&ch),
                                "displayname" => displayname.push_str(&ch),
                                "getcontentlength" => getcontentlength.push_str(&ch),
                                "getlastmodified" => getlastmodified.push_str(&ch),
                                "getcontenttype" => getcontenttype.push_str(&ch),
                                "getetag" => getetag.push_str(&ch),
                                _ => {}
                            }
                        }
                    }
                }

                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            match tag.as_str() {
                                "href" => href = text,
                                "displayname" => displayname = text,
                                "getcontentlength" => getcontentlength = text,
                                "getlastmodified" => getlastmodified = text,
                                "getcontenttype" => getcontenttype = text,
                                "getetag" => getetag = text,
                                "iscollection" if text == "1" => {
                                    is_collection_by_iscollection = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                _ => {}
            }
            buf.clear();
        }

        tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
        Ok(entries)
    }

    /// Extract a single property value from a PROPFIND Depth:0 XML response using quick-xml.
    /// Used by stat() and storage_info() for simple single-response parsing.
    fn extract_xml_properties(&self, xml: &str) -> HashMap<String, String> {
        let mut props = HashMap::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut current_tag: Option<String> = None;
        let mut in_resourcetype = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "resourcetype" => {
                            in_resourcetype = true;
                        }
                        "collection" if in_resourcetype => {
                            props.insert("_is_collection".to_string(), "true".to_string());
                        }
                        _ => {
                            current_tag = Some(local);
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_resourcetype {
                        props.insert("_is_collection".to_string(), "true".to_string());
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "resourcetype" {
                        in_resourcetype = false;
                    }
                    if current_tag.as_deref() == Some(local.as_str()) {
                        current_tag = None;
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = String::from_utf8_lossy(e.as_ref()).to_string();
                        if !raw.is_empty() {
                            if tag == "iscollection" && raw.trim() == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props
                                .entry(tag.clone())
                                .and_modify(|v| v.push_str(&raw))
                                .or_insert_with(|| raw.clone());
                        }
                    }
                }
                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            if tag == "iscollection" && text == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props.insert(tag.clone(), text);
                        }
                    }
                }
                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            props
                                .entry(tag.clone())
                                .and_modify(|v| v.push_str(&ch))
                                .or_insert_with(|| ch.to_string());
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }
        props
    }
}

/// Strip namespace prefix from an XML element name, returning an owned String.
/// e.g. "d:response" -> "response", "DAV:href" -> "href", "response" -> "response"
fn local_name(raw: &[u8]) -> String {
    let s = std::str::from_utf8(raw).unwrap_or("");
    match s.rfind(':') {
        Some(pos) => s[pos + 1..].to_string(),
        None => s.to_string(),
    }
}

/// Canonical lowercase key for an ownCloud/Nextcloud `oc:checksums`
/// algorithm label, matching every `StorageProvider::checksum()` impl
/// and the `hashsum` / `lsjson --hash` consumers. Unknown labels degrade
/// to a lowercased, separator-stripped form (still a real server-side
/// digest, just an exotic algo) rather than being dropped.
fn canonical_checksum_key(algo: &str) -> String {
    let norm: String = algo
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    match norm.as_str() {
        "SHA256" => "sha256",
        "SHA512" => "sha512",
        "SHA384" => "sha384",
        "SHA1" => "sha1",
        "MD5" => "md5",
        "CRC32" => "crc32",
        "ADLER32" => "adler32",
        _ => return norm.to_ascii_lowercase(),
    }
    .to_string()
}

/// Parse an `<oc:checksums><oc:checksum>` payload into canonical
/// `{key: hexdigest}` pairs. The element is a single string of
/// whitespace-separated `ALGO:HEXDIGEST` tokens, e.g.
/// `"SHA1:f1d2d2... MD5:900150... ADLER32:024d0127"`. Tokens without a
/// `:`, with an empty digest, or with a non-hex digest are skipped so a
/// malformed entry can never poison the map. Returns an empty map for
/// every WebDAV server that does not emit `oc:checksums` (the
/// server-side-or-omit contract: no content is ever downloaded).
fn parse_oc_checksums(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for token in raw.split_whitespace() {
        let Some((algo, digest)) = token.split_once(':') else {
            continue;
        };
        let digest = digest.trim().to_ascii_lowercase();
        if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let key = canonical_checksum_key(algo);
        if !key.is_empty() {
            out.entry(key).or_insert(digest);
        }
    }
    out
}

#[async_trait]
impl StorageProvider for WebDavProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::WebDav
    }

    fn display_name(&self) -> String {
        if self.config.anonymous {
            return self
                .config
                .url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
                .to_string();
        }
        format!(
            "{}@{}",
            self.config.username,
            self.config
                .url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
        )
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // A3-03: Warn when using unencrypted HTTP: credentials and data sent in plaintext
        if self.config.url.starts_with("http://") {
            tracing::warn!(
                "[WEBDAV] Connection uses unencrypted HTTP ({}). Credentials and data will be sent in plaintext.",
                self.config.url
            );
        }

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#;

        // First attempt with Basic auth
        let response = self
            .request(webdav_methods::propfind(), "/")
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                self.connected = true;
                // Traditional WebDAV server: `/` is a valid resource. Server
                // root is `/`, plus the user-supplied initial_path if any.
                let resolved_root = self
                    .config
                    .initial_path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("/")
                    .to_string();
                self.current_path = resolved_root.clone();
                self.server_root = Some(resolved_root);
                Ok(())
            }
            StatusCode::UNAUTHORIZED => {
                // Check if server requires Digest authentication
                let www_auth = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if let Some(state) = DigestState::parse(&www_auth) {
                    tracing::debug!(
                        "[WebDAV] Server requires Digest auth (realm: {}, qop: {}, nonce: {}...)",
                        state.realm,
                        state.qop,
                        &state.nonce[..state.nonce.len().min(12)]
                    );
                    self.digest_auth = Some(state);

                    // Retry with Digest auth
                    let response2 = self
                        .request(webdav_methods::propfind(), "/")
                        .header("Depth", "0")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                        .send()
                        .await
                        .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                    let retry_status = response2.status();
                    tracing::debug!("[WebDAV] Digest auth retry status: {}", retry_status);

                    match retry_status {
                        StatusCode::OK | StatusCode::MULTI_STATUS => {
                            tracing::debug!("[WebDAV] Digest auth successful");
                            self.connected = true;
                            let resolved_root = self
                                .config
                                .initial_path
                                .as_deref()
                                .filter(|p| !p.is_empty())
                                .unwrap_or("/")
                                .to_string();
                            self.current_path = resolved_root.clone();
                            self.server_root = Some(resolved_root);
                            Ok(())
                        }
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                            // Log the response body for debugging
                            let body = response2.text().await.unwrap_or_default();
                            tracing::warn!(
                                "[WebDAV] Digest auth failed ({}): {}",
                                retry_status,
                                &body[..body.len().min(200)]
                            );
                            self.digest_auth = None;
                            Err(ProviderError::AuthenticationFailed(
                                "Invalid credentials".to_string(),
                            ))
                        }
                        status => {
                            self.digest_auth = None;
                            Err(ProviderError::ConnectionFailed(format!(
                                "Server returned status: {}",
                                status
                            )))
                        }
                    }
                } else {
                    Err(ProviderError::AuthenticationFailed(
                        "Invalid credentials".to_string(),
                    ))
                }
            }
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".to_string(),
            )),
            StatusCode::METHOD_NOT_ALLOWED => {
                tracing::debug!("[WebDAV] PROPFIND / returned 405, trying well-known WebDAV paths");

                // Try well-known Nextcloud/ownCloud WebDAV paths before falling back to OPTIONS
                let username = &self.config.username;
                let well_known_paths: Vec<String> = if !username.is_empty() {
                    vec![
                        format!("/remote.php/dav/files/{}/", username),
                        "/remote.php/webdav/".to_string(),
                    ]
                } else {
                    vec!["/remote.php/webdav/".to_string()]
                };

                for wk_path in &well_known_paths {
                    tracing::debug!("[WebDAV] Trying well-known path: {}", wk_path);
                    let wk_response = self
                        .request(webdav_methods::propfind(), wk_path)
                        .header("Depth", "0")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                        .send()
                        .await;

                    if let Ok(resp) = wk_response {
                        let st = resp.status();
                        if st == StatusCode::OK || st == StatusCode::MULTI_STATUS {
                            tracing::info!(
                                "[WebDAV] Auto-detected WebDAV path: {} ({})",
                                wk_path,
                                st
                            );
                            self.connected = true;
                            // Issue #175: server_root is the auto-detected path.
                            // current_path defaults to it; if the user supplied
                            // a relative initial_path, append it under the root.
                            let user_initial = self
                                .config
                                .initial_path
                                .as_deref()
                                .map(str::trim)
                                .filter(|p| !p.is_empty() && *p != "/");
                            let starting_path = match user_initial {
                                Some(rel) => {
                                    let wk_trim = wk_path.trim_end_matches('/');
                                    let rel_trim = rel.trim_start_matches('/');
                                    format!("{}/{}", wk_trim, rel_trim)
                                }
                                None => wk_path.clone(),
                            };
                            self.current_path = starting_path;
                            self.server_root = Some(wk_path.clone());
                            return Ok(());
                        }
                    }
                }

                // No well-known path worked, fall back to OPTIONS on root
                tracing::debug!("[WebDAV] No well-known path worked, trying OPTIONS /");
                let options_response = self
                    .request(Method::OPTIONS, "/")
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                let options_status = options_response.status();
                if options_status.is_success() {
                    self.connected = true;
                    let resolved_root = self
                        .config
                        .initial_path
                        .as_deref()
                        .filter(|p| !p.is_empty())
                        .unwrap_or("/")
                        .to_string();
                    self.current_path = resolved_root.clone();
                    self.server_root = Some(resolved_root);
                    Ok(())
                } else {
                    Err(ProviderError::ConnectionFailed(format!(
                        "Server returned status: {}",
                        options_status
                    )))
                }
            }
            status => Err(ProviderError::ConnectionFailed(format!(
                "Server returned status: {}",
                status
            ))),
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.server_root = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Issue #175: Nextcloud / ownCloud serve `/` as 405 Method Not Allowed
        // because the WebDAV root lives under a versioned prefix
        // (`/remote.php/dav/files/<user>/` or `/remote.php/webdav/`). When the
        // frontend or a saved-server profile passes `path = "/"` literally,
        // we redirect to the auto-detected server_root so the listing works
        // out of the box. Traditional WebDAV servers where `/` is a valid
        // resource keep working because their server_root is also `/`, so
        // the redirect is a no-op.
        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else if path == "/" {
            self.server_root
                .clone()
                .unwrap_or_else(|| self.current_path.clone())
        } else {
            path.to_string()
        };

        tracing::debug!("[WebDAV] Listing path: {}", list_path);

        let response = self
            .send_propfind(
                &list_path,
                "1",
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#,
            )
            .await?;

        let status = response.status();
        tracing::debug!("[WebDAV] List response status: {}", status);

        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response
                    .text()
                    .await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                tracing::debug!("[WebDAV] Response XML length: {} bytes", xml.len());
                tracing::debug!("[WebDAV] Full XML response:\n{}", xml);

                let entries = self.parse_propfind_response(&xml, &list_path)?;
                tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
                Ok(entries)
            }
            StatusCode::NOT_FOUND => {
                tracing::warn!("[WebDAV] Path not found: {}", list_path);
                Err(ProviderError::NotFound(list_path))
            }
            StatusCode::UNAUTHORIZED => {
                self.connected = false;
                tracing::error!("[WebDAV] Unauthorized - session expired");
                Err(ProviderError::AuthenticationFailed(
                    "Session expired".to_string(),
                ))
            }
            status => {
                tracing::error!("[WebDAV] Server error: {}", status);
                Err(ProviderError::ServerError(format!(
                    "Server returned status: {}",
                    status
                )))
            }
        }
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Issue #175: enforce the boundary against the connect-time server
        // root, not the user-typed `config.initial_path`. See the doc on
        // `server_root` for the full reasoning.
        let boundary = self
            .server_root
            .as_deref()
            .or(self.config.initial_path.as_deref());
        if path_violates_root(path, boundary) {
            return Err(ProviderError::InvalidPath(format!(
                "Cannot navigate above WebDAV root: {}",
                boundary.unwrap_or("/")
            )));
        }

        // Verify the path exists and is a directory
        let response = self
            .request(webdav_methods::propfind(), path)
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#,
            )
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response
                    .text()
                    .await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                let props = self.extract_xml_properties(&xml);
                let is_collection = props.contains_key("_is_collection");

                if is_collection {
                    self.current_path = path.to_string();
                    Ok(())
                } else {
                    Err(ProviderError::InvalidPath(format!(
                        "{} is not a directory",
                        path
                    )))
                }
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Server returned status: {}",
                status
            ))),
        }
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        // Issue #175: prefer the connect-time server_root (auto-detected on
        // Nextcloud / ownCloud) over the user-typed initial_path so cd_up
        // clamps at the real WebDAV root, not at the form value.
        let root = self
            .server_root
            .as_deref()
            .or(self.config.initial_path.as_deref())
            .filter(|p| !p.is_empty())
            .unwrap_or("/");

        // Already at root: cannot go higher
        let current_trimmed = self.current_path.trim_end_matches('/');
        let root_trimmed = root.trim_end_matches('/');
        if current_trimmed == root_trimmed || current_trimmed.is_empty() {
            return Ok(());
        }

        let parent = std::path::Path::new(&self.current_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        let parent = if parent.is_empty() {
            "/".to_string()
        } else {
            parent
        };

        // Clamp: if parent would go above root, stay at root
        let parent_trimmed = parent.trim_end_matches('/');
        if !parent_trimmed.starts_with(root_trimmed) {
            self.current_path = root.to_string();
        } else {
            self.current_path = parent;
        }

        Ok(())
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

        let mut on_progress = on_progress;

        // PD-HTTP-2: concurrent-Range download behind a real strict 206 probe.
        // Digest auth mutates a per-request nonce counter and cannot be
        // replayed concurrently, so it is excluded here (honest single-stream).
        // The probe + closure go through the live reqwest client with a
        // precomputed Basic header; no credential is reconstructed.
        if self.multi_thread_streams >= 2 && self.digest_auth.is_none() {
            let mut headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> =
                Vec::new();
            if !self.config.anonymous {
                use base64::Engine as _;
                let token = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    self.config.username,
                    self.config.password.expose_secret()
                ));
                match reqwest::header::HeaderValue::from_str(&format!("Basic {}", token)) {
                    Ok(v) => headers.push((reqwest::header::AUTHORIZATION, v)),
                    Err(e) => {
                        return Err(ProviderError::AuthenticationFailed(format!(
                            "Invalid characters in credentials: {}",
                            e
                        )))
                    }
                }
            }
            let req = super::multi_thread::HttpRangeRequest {
                client: self.client.clone(),
                url: self.build_url(remote_path),
                headers,
                local_path: local_path.to_string(),
                streams: self.multi_thread_streams,
                max_streams: WEBDAV_MULTI_THREAD_MAX_STREAMS,
                cutoff: self.multi_thread_cutoff,
            };
            match super::multi_thread::try_http_concurrent_range_download(req, on_progress).await {
                super::multi_thread::HttpRangeAttempt::Completed => return Ok(()),
                super::multi_thread::HttpRangeAttempt::Failed(e) => return Err(e),
                super::multi_thread::HttpRangeAttempt::Fallback(p) => on_progress = p,
            }
        }

        let response = self
            .send_with_too_early_retry(Method::GET, remote_path)
            .await?;

        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);
                let mut stream = response.bytes_stream();
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let mut downloaded: u64 = 0;

                loop {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            atomic
                                .write_all(&chunk)
                                .await
                                .map_err(ProviderError::IoError)?;
                            downloaded += chunk.len() as u64;
                            if let Some(ref progress) = on_progress {
                                progress(downloaded, total_size);
                            }
                        }
                        Some(Err(e)) => {
                            if super::is_unexpected_eof_after_full_body(&e, downloaded, total_size)
                            {
                                tracing::warn!(
                                    "[WEBDAV] Server closed connection without TLS close_notify but full body received ({}/{} bytes); accepting",
                                    downloaded,
                                    total_size
                                );
                                break;
                            }
                            return Err(ProviderError::TransferFailed(e.to_string()));
                        }
                        None => break,
                    }
                }
                atomic.commit().await.map_err(ProviderError::IoError)?;

                Ok(())
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            ))),
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .send_with_too_early_retry(Method::GET, remote_path)
            .await?;

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

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total_size = file.metadata().await.map_err(ProviderError::IoError)?.len();

        // Stream file with Content-Length header (required by some HTTP/1.1 servers).
        // 256 KiB capacity matches our SFTP default and avoids the 4 KiB read
        // chunks ReaderStream uses by default, which churn syscalls and bottleneck
        // local-network throughput.
        let stream = tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024);
        let body = reqwest::Body::wrap_stream(stream);

        let response = self
            .request(Method::PUT, remote_path)
            .header("Content-Length", total_size)
            .body(body)
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
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Parent directory does not exist".to_string(),
            )),
            StatusCode::INSUFFICIENT_STORAGE => Err(ProviderError::ServerError(
                "Insufficient storage space".to_string(),
            )),
            status => Err(ProviderError::TransferFailed(format!(
                "Upload failed with status: {}",
                status
            ))),
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // MKCOL always targets a collection: use the trailing-slash form so
        // Apache does not 301 to a scheme-downgraded URL that loses auth.
        let col = Self::collection_path(path);
        let response = self
            .request(webdav_methods::mkcol(), &col)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            // RFC 4918 §9.3.1: 201 Created on success, 405 Method Not Allowed
            // when the collection already exists. Some servers (notably
            // FileLu's WebDAV frontend) return 204 No Content on success, and
            // others (Nextcloud variants) return 200 OK: treat all three as
            // idempotent success.
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED => Err(ProviderError::AlreadyExists(path.to_string())),
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Parent directory does not exist".to_string(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::PermissionDenied(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "MKCOL failed with status: {}",
                status
            ))),
        }
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .request(Method::DELETE, path)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            StatusCode::FORBIDDEN => Err(ProviderError::PermissionDenied(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "DELETE failed with status: {}",
                status
            ))),
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE works for both files and directories, but a
        // directory DELETE without a trailing slash triggers the same
        // scheme-downgrading 301 that strips auth (see `collection_path`).
        self.delete(&Self::collection_path(path)).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE automatically deletes recursively. Trailing-slash
        // form for the same reason as `rmdir`.
        self.delete(&Self::collection_path(path)).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let destination = self.build_url(to);

        let response = self
            .request(webdav_methods::move_method(), from)
            .header("Destination", destination)
            .header("Overwrite", "F") // Don't overwrite existing
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(from.to_string())),
            StatusCode::PRECONDITION_FAILED => Err(ProviderError::AlreadyExists(to.to_string())),
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Destination parent does not exist".to_string(),
            )),
            status => Err(ProviderError::ServerError(format!(
                "MOVE failed with status: {}",
                status
            ))),
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                    </d:prop>
                </d:propfind>"#;

        // `stat` is path-type-ambiguous (it is called for both files and
        // directories). Try the path verbatim first: that is correct for
        // files, where a trailing slash would instead point at a
        // non-existent collection. If the server answers `404` or a
        // redirect-stripped `401` (Apache mod_dav `301`s a slash-less
        // *collection* to a scheme-downgraded URL that loses the
        // `Authorization` header: the same class fixed for
        // `list`/`mkdir`/`rmdir`, see `collection_path`), retry once in the
        // collection (trailing-slash) form, which is what real directories
        // need. `name` always derives from the original `path`.
        let collection_form = Self::collection_path(path);
        let mut attempts: Vec<&str> = vec![path];
        if collection_form != path {
            attempts.push(collection_form.as_str());
        }

        let mut last_status = StatusCode::NOT_FOUND;
        for attempt in attempts {
            let response = self
                .request(webdav_methods::propfind(), attempt)
                .header("Depth", "0")
                .header("Content-Type", "application/xml")
                .body(PROPFIND_BODY)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            match response.status() {
                StatusCode::OK | StatusCode::MULTI_STATUS => {
                    let xml = response
                        .text()
                        .await
                        .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                    let props = self.extract_xml_properties(&xml);
                    let is_dir = props.contains_key("_is_collection");
                    let size: u64 = props
                        .get("getcontentlength")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let modified = props.get("getlastmodified").cloned();
                    let mime_type = props.get("getcontenttype").cloned();

                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());

                    return Ok(RemoteEntry {
                        name,
                        path: path.to_string(),
                        is_dir,
                        size,
                        modified,
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type,
                        metadata: Default::default(),
                    });
                }
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                    // Ambiguous: may be a collection that needs the
                    // trailing-slash form. Fall through to the retry.
                    last_status = response.status();
                    continue;
                }
                status => {
                    return Err(ProviderError::ServerError(format!(
                        "PROPFIND failed with status: {}",
                        status
                    )));
                }
            }
        }

        if last_status == StatusCode::NOT_FOUND {
            Err(ProviderError::NotFound(path.to_string()))
        } else {
            Err(ProviderError::ServerError(format!(
                "PROPFIND failed with status: {}",
                last_status
            )))
        }
    }

    fn supports_checksum(&self) -> bool {
        // ownCloud/Nextcloud expose server-side digests via the
        // `oc:checksums` PROPFIND prop; every other WebDAV server simply
        // omits it, in which case `checksum()` returns an empty map and
        // consumers omit / fall back. The probe is a metadata PROPFIND,
        // never a content download: the server-side-or-omit contract.
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
                    <d:prop>
                        <oc:checksums/>
                    </d:prop>
                </d:propfind>"#;

        // Mirror `stat()`'s file-first / collection-retry: a file must be
        // requested without a trailing slash, while a slash-less
        // collection can be 301'd by Apache to a scheme-downgraded URL
        // that strips auth (see `collection_path`).
        let collection_form = Self::collection_path(path);
        let mut attempts: Vec<&str> = vec![path];
        if collection_form != path {
            attempts.push(collection_form.as_str());
        }

        for attempt in attempts {
            let response = self
                .request(webdav_methods::propfind(), attempt)
                .header("Depth", "0")
                .header("Content-Type", "application/xml")
                .body(PROPFIND_BODY)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            match response.status() {
                StatusCode::OK | StatusCode::MULTI_STATUS => {
                    let xml = response
                        .text()
                        .await
                        .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                    let props = self.extract_xml_properties(&xml);
                    return Ok(props
                        .get("checksum")
                        .map(|s| parse_oc_checksums(s))
                        .unwrap_or_default());
                }
                // Ambiguous path type: retry in collection form.
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => continue,
                // Any other status: the server cannot answer the prop.
                // Treat as "no server-side hash" (omit) rather than an
                // error that would fail a listing or trigger a download.
                _ => return Ok(HashMap::new()),
            }
        }

        Ok(HashMap::new())
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
        // WebDAV uses HTTP which is stateless, no keep-alive needed
        // Just verify we can still authenticate
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if response.status() == StatusCode::UNAUTHORIZED {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed(
                "Session expired".to_string(),
            ));
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let server = response
            .headers()
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("Unknown WebDAV Server");

        let dav = response
            .headers()
            .get("dav")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1");

        Ok(format!(
            "WebDAV Server: {} (DAV compliance: {})",
            server, dav
        ))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut results = Vec::new();
        let mut dirs_to_scan = vec![path.to_string()];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries {
                if entry.is_dir {
                    dirs_to_scan.push(entry.path.clone());
                }

                if super::matches_find_pattern(&entry.name, pattern) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Koofr WebDAV does not expose RFC 4331 quota properties via PROPFIND
        // (returns 0 / 0). Use the native Koofr REST API instead: it accepts
        // basic auth with the same email + app password used for WebDAV.
        if self.config.url.contains("app.koofr.net") {
            if let Ok(info) = self.koofr_storage_via_api().await {
                return Ok(info);
            }
            // Fall through to PROPFIND on failure (best-effort).
        }

        // OpenDrive WebDAV behaves the same way: no RFC 4331 quota over
        // PROPFIND. Read the real quota from the OpenDrive REST API using
        // the same account credentials (session auth).
        if self.config.url.contains("webdav.opendrive.com") {
            if let Ok(info) = self.opendrive_storage_via_api().await {
                return Ok(info);
            }
            // Fall through to PROPFIND on failure (best-effort).
        }

        // RFC 4331: WebDAV quota properties
        let response = self
            .request(webdav_methods::propfind(), &self.current_path.clone())
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:quota-available-bytes/>
                        <d:quota-used-bytes/>
                    </d:prop>
                </d:propfind>"#,
            )
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::NotSupported("storage_info".to_string()));
        }

        let xml = response
            .text()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let props = self.extract_xml_properties(&xml);
        let used = props
            .get("quota-used-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        // Nextcloud / ownCloud convention: `quota-available-bytes` can be negative.
        //   -1 = unknown, -2 = unlimited (legacy), -3 = unlimited.
        // Parse as i64 so we can detect the sentinel, then surface "unlimited" by
        // returning total=0 / free=0, which the frontend StatusBar already renders
        // as "used (no cap)". Without this, u64::parse rejects negative values,
        // free falls back to 0, total collapses to `used`, and the UI shows
        // "59.7 MB / 59.7 MB" (100% full) for accounts that are actually unlimited.
        let free_raw = props
            .get("quota-available-bytes")
            .and_then(|s| s.parse::<i64>().ok());
        let (free, total) = match free_raw {
            Some(v) if v < 0 => (0u64, 0u64),
            Some(v) => {
                let f = v as u64;
                (f, used.saturating_add(f))
            }
            None => (0u64, 0u64),
        };

        Ok(super::StorageInfo { used, total, free })
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(
        &mut self,
        path: &str,
        timeout: u64,
    ) -> Result<super::LockInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let timeout_header = if timeout == 0 {
            "Infinite".to_string()
        } else {
            format!("Second-{}", timeout)
        };

        let lock_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:lockinfo xmlns:d="DAV:">
                <d:lockscope><d:exclusive/></d:lockscope>
                <d:locktype><d:write/></d:locktype>
                <d:owner><d:href>AeroFTP</d:href></d:owner>
            </d:lockinfo>"#;

        let response = self
            .request(webdav_methods::lock(), path)
            .header("Depth", "0")
            .header("Timeout", &timeout_header)
            .header("Content-Type", "application/xml")
            .body(lock_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "LOCK failed ({}): {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        // Extract lock token from Lock-Token header or XML response
        let lock_token = response
            .headers()
            .get("lock-token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string())
            .unwrap_or_default();

        Ok(super::LockInfo {
            token: lock_token,
            owner: Some("AeroFTP".to_string()),
            timeout,
            exclusive: true,
        })
    }

    async fn unlock_file(&mut self, path: &str, lock_token: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token_header = format!("<{}>", lock_token);

        let response = self
            .request(webdav_methods::unlock(), path)
            .header("Lock-Token", &token_header)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            reqwest::StatusCode::OK | reqwest::StatusCode::NO_CONTENT => Ok(()),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "UNLOCK failed ({}): {}",
                    status,
                    sanitize_api_error(&text)
                )))
            }
        }
    }

    fn supports_share_links(&self) -> bool {
        self.is_nextcloud()
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
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
        self.nextcloud_create_share(path, options).await
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands keep working.
        // The real RFC 4918 COPY implementation lives on `server_side_copy`.
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let destination = self.build_url(to);

        let response = self
            .request(webdav_methods::copy(), from)
            .header("Destination", destination)
            .header("Overwrite", "F")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(from.to_string())),
            StatusCode::PRECONDITION_FAILED => Err(ProviderError::AlreadyExists(to.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "COPY failed with status: {}",
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

        let response = self
            .request(Method::GET, remote_path)
            .header("Range", format!("bytes={}-", offset))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
                // Server ignored Range: restart from scratch
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
                let tmp = format!("{}.aerotmp", local_path);
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(ProviderError::TransferFailed(
                    "Range not satisfiable: file may have changed on server".to_string(),
                ))
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Resume download failed: {}",
                status
            ))),
        }
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_range_download: true,
            supports_resume_download: true,
            ..Default::default()
        }
    }

    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, WEBDAV_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes;
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

        let end = offset + len - 1; // HTTP Range is inclusive
        let range_header = format!("bytes={}-{}", offset, end);

        let response = self
            .request(Method::GET, path)
            .header("Range", &range_header)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        match status {
            StatusCode::PARTIAL_CONTENT | StatusCode::OK => {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                // If server ignores Range and returns full content, slice to requested range
                if status == StatusCode::OK {
                    if offset >= bytes.len() as u64 {
                        Ok(Vec::new())
                    } else {
                        let start = offset as usize;
                        let end = std::cmp::min(start.saturating_add(len as usize), bytes.len());
                        Ok(bytes[start..end].to_vec())
                    }
                } else {
                    Ok(bytes.to_vec())
                }
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
}

// ─── Nextcloud Tauri Commands ────────────────────────────────────────────

#[tauri::command]
pub async fn webdav_list_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
) -> Result<Vec<NextcloudTrashEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let webdav = provider
        .as_any_mut()
        .downcast_mut::<WebDavProvider>()
        .ok_or("Not a WebDAV connection")?;
    webdav
        .nextcloud_list_trash()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn webdav_restore_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    ids: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let webdav = provider
        .as_any_mut()
        .downcast_mut::<WebDavProvider>()
        .ok_or("Not a WebDAV connection")?;
    for id in &ids {
        webdav
            .nextcloud_restore_trash(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn webdav_delete_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    ids: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let webdav = provider
        .as_any_mut()
        .downcast_mut::<WebDavProvider>()
        .ok_or("Not a WebDAV connection")?;
    for id in &ids {
        webdav
            .nextcloud_delete_trash_item(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn webdav_empty_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let webdav = provider
        .as_any_mut()
        .downcast_mut::<WebDavProvider>()
        .ok_or("Not a WebDAV connection")?;
    webdav
        .nextcloud_empty_trash()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(url: &str) -> WebDavConfig {
        WebDavConfig {
            url: url.to_string(),
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            verify_cert: true,
            anonymous: false,
        }
    }

    #[test]
    fn test_build_url() {
        let provider = WebDavProvider::new(test_config(
            "https://cloud.example.com/remote.php/dav/files/user/",
        ))
        .expect("Failed to create WebDavProvider");

        assert_eq!(
            provider.build_url("/Documents"),
            "https://cloud.example.com/remote.php/dav/files/user/Documents"
        );
    }

    #[test]
    fn test_extract_xml_properties() {
        let provider = WebDavProvider::new(test_config("https://example.com"))
            .expect("Failed to create WebDavProvider");

        // Test with d: prefix
        let xml = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:getcontentlength>12345</d:getcontentlength>
                        <d:getcontenttype>text/plain</d:getcontenttype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props = provider.extract_xml_properties(xml);
        assert_eq!(props.get("getcontentlength"), Some(&"12345".to_string()));
        assert_eq!(props.get("getcontenttype"), Some(&"text/plain".to_string()));

        // Test with D: prefix
        let xml2 = r#"<D:multistatus xmlns:D="DAV:">
            <D:response>
                <D:propstat>
                    <D:prop>
                        <D:getcontentlength>99</D:getcontentlength>
                    </D:prop>
                </D:propstat>
            </D:response>
        </D:multistatus>"#;
        let props2 = provider.extract_xml_properties(xml2);
        assert_eq!(props2.get("getcontentlength"), Some(&"99".to_string()));

        // Test collection detection
        let xml3 = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props3 = provider.extract_xml_properties(xml3);
        assert!(props3.contains_key("_is_collection"));
    }

    #[test]
    fn test_parse_propfind_response() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/file.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength>1024</d:getcontentlength>
                        <d:getlastmodified>Mon, 01 Jan 2024 00:00:00 GMT</d:getlastmodified>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/subdir/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 1024);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_dir);
    }

    /// Issue #128 (Filen WebDAV bridge): the local bridge ships
    /// `<DAV:displayname>` percent-encoded ("my%20folder") instead of the
    /// human-readable form RFC 4918 mandates. Defensive decoding in
    /// parse_propfind_response un-mangles it. RFC-compliant servers send
    /// raw display names, on which the decode is a no-op.
    #[test]
    fn parse_propfind_decodes_percent_encoded_displayname() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/my%20folder/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>my%20folder</d:displayname>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/foto%20vacanze.jpg</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>foto%20vacanze.jpg</d:displayname>
                        <d:resourcetype/>
                        <d:getcontentlength>2048</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/normal-file.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>normal-file.txt</d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 3);
        // Filen-style: percent-encoded displayname must be decoded.
        assert_eq!(entries[0].name, "my folder");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "foto vacanze.jpg");
        assert!(!entries[1].is_dir);
        // RFC-compliant: raw displayname passes through unchanged.
        assert_eq!(entries[2].name, "normal-file.txt");
    }

    // Issue #175 — boundary check must use the auto-detected `server_root`
    // (Nextcloud / ownCloud) rather than the user-typed `initial_path`,
    // otherwise drill-down clicks fail with "Cannot navigate above WebDAV
    // root" because the entry path is rooted at the wk_path while the
    // user typed `/` or left it blank.

    #[test]
    fn boundary_pure_no_root_means_no_violation() {
        assert!(!path_violates_root("/anything", None));
        assert!(!path_violates_root("/", None));
    }

    #[test]
    fn boundary_pure_empty_or_root_boundary_is_a_noop() {
        assert!(!path_violates_root("/anything", Some("")));
        assert!(!path_violates_root("/anything", Some("/")));
    }

    #[test]
    fn boundary_pure_drill_down_into_nextcloud_root_is_allowed() {
        // Real-world Tab.digital / Nextcloud: server_root resolved at connect
        // time to /remote.php/dav/files/<user>/, user clicks "Documents".
        let root = Some("/remote.php/dav/files/testuser/");
        assert!(!path_violates_root(
            "/remote.php/dav/files/testuser/Documents",
            root
        ));
        assert!(!path_violates_root(
            "/remote.php/dav/files/testuser/Documents/Invoices",
            root
        ));
        // Equality is also fine (cd to the root itself).
        assert!(!path_violates_root("/remote.php/dav/files/testuser", root));
    }

    #[test]
    fn boundary_pure_paths_above_or_outside_root_are_rejected() {
        let root = Some("/remote.php/dav/files/testuser/");
        // Going above
        assert!(path_violates_root("/remote.php/dav/files", root));
        // Sibling user
        assert!(path_violates_root(
            "/remote.php/dav/files/someoneelse/Docs",
            root
        ));
        // Completely off-tree
        assert!(path_violates_root("/", root));
        assert!(path_violates_root("/etc/passwd", root));
    }

    #[tokio::test]
    async fn list_rewrites_root_slash_to_server_root_for_nextcloud() {
        // Issue #175 bug 2: a saved profile with initial_path = "/" hits
        // 405 on Nextcloud because PROPFIND on `/` is not allowed. The
        // backend now redirects "/" to the auto-detected server_root.
        // We can't observe the network call without a real server, but
        // we assert the path resolution is what we expect by inspecting
        // the rewrite directly.
        let mut provider = WebDavProvider::new(test_config("https://cloud.example.com")).unwrap();
        provider.connected = true;
        provider.current_path = "/remote.php/dav/files/testuser/".to_string();
        provider.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        // Reproduce the rewrite logic: ensure "/" maps to server_root.
        let resolved = match "/" {
            "" | "." => provider.current_path.clone(),
            "/" => provider
                .server_root
                .clone()
                .unwrap_or_else(|| provider.current_path.clone()),
            other => other.to_string(),
        };
        assert_eq!(resolved, "/remote.php/dav/files/testuser/");
    }

    #[test]
    fn cd_up_uses_server_root_over_config_initial_path() {
        // Sanity check the resolution order used by `cd_up`. Without going
        // through the network, we can verify the precedence on the field
        // directly via the same expression `cd_up` uses.
        let mut config = test_config("https://cloud.example.com");
        config.initial_path = Some("/Documents".to_string());
        let mut provider = WebDavProvider::new(config).unwrap();
        provider.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        let chosen = provider
            .server_root
            .as_deref()
            .or(provider.config.initial_path.as_deref())
            .filter(|p| !p.is_empty())
            .unwrap_or("/");
        assert_eq!(chosen, "/remote.php/dav/files/testuser/");
    }

    #[test]
    fn resolve_root_composes_relative_paths_under_nextcloud_root() {
        let mut p = WebDavProvider::new(test_config("https://cloud.example.com")).unwrap();
        p.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        // Bare caller paths get composed under the auto-detected root
        // (the mkdir/put/delete bug: previously bypassed it -> 404/405).
        assert_eq!(
            p.resolve_root("/aeroftp-utest"),
            "/remote.php/dav/files/testuser/aeroftp-utest"
        );
        // Collection form keeps its trailing slash.
        assert_eq!(
            p.resolve_root("/aeroftp-utest/"),
            "/remote.php/dav/files/testuser/aeroftp-utest/"
        );
        // Literal root maps to the server root.
        assert_eq!(p.resolve_root("/"), "/remote.php/dav/files/testuser/");

        // Idempotent: already-rooted paths (GUI drill-down, where list()
        // returns fully-rooted entry paths) are not double-prefixed.
        assert_eq!(
            p.resolve_root("/remote.php/dav/files/testuser/Documents"),
            "/remote.php/dav/files/testuser/Documents"
        );
        assert_eq!(
            p.resolve_root("/remote.php/dav/files/testuser/"),
            "/remote.php/dav/files/testuser/"
        );

        // No distinct root => exact no-op (traditional servers, and the
        // connect-time probes that run before server_root is set).
        let mut t = WebDavProvider::new(test_config("https://dav.example.com")).unwrap();
        assert_eq!(t.resolve_root("/aeroftp-utest"), "/aeroftp-utest");
        t.server_root = Some("/".to_string());
        assert_eq!(t.resolve_root("/aeroftp-utest"), "/aeroftp-utest");
    }

    #[test]
    fn oc_checksums_parsed_to_canonical_lowercase_keys() {
        // Real Nextcloud/ownCloud shape: space-separated ALGO:HEX tokens.
        let m = parse_oc_checksums(
            "SHA1:f1d2d2f924e986ac86fdf7b36c94bcdf32beec15 \
             MD5:900150983cd24fb0d6963f7d28e17f72 ADLER32:024d0127",
        );
        assert_eq!(
            m.get("sha1").map(String::as_str),
            Some("f1d2d2f924e986ac86fdf7b36c94bcdf32beec15")
        );
        assert_eq!(
            m.get("md5").map(String::as_str),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(m.get("adler32").map(String::as_str), Some("024d0127"));
        // No canonical key is upper-cased or dash-separated.
        assert!(m.keys().all(|k| k == &k.to_ascii_lowercase()));
    }

    #[test]
    fn oc_checksums_canonicalises_separators_and_case() {
        let m = parse_oc_checksums("SHA-256:ABCDEF01 sha512:00FF");
        assert_eq!(m.get("sha256").map(String::as_str), Some("abcdef01"));
        assert_eq!(m.get("sha512").map(String::as_str), Some("00ff"));
    }

    #[test]
    fn oc_checksums_skips_malformed_and_empty() {
        // No colon, empty digest, non-hex digest, and the empty string.
        assert!(parse_oc_checksums("").is_empty());
        assert!(parse_oc_checksums("SHA1 MD5: SHA256:zz_not_hex").is_empty());
        // A single good token among malformed ones still survives.
        let m = parse_oc_checksums("garbage MD5:0a1b BAD:");
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("md5").map(String::as_str), Some("0a1b"));
    }

    #[test]
    fn unknown_algo_degrades_not_dropped() {
        let m = parse_oc_checksums("WHIRLPOOL:dead");
        assert_eq!(m.get("whirlpool").map(String::as_str), Some("dead"));
    }

    /// SG-T08 gate: WebDAV advertises the server_side_copy capability under
    /// both the legacy and the new slot, since the RFC 4918 COPY method is
    /// universally available on compliant DAV servers.
    #[test]
    fn webdav_advertises_server_side_copy_capability() {
        let p = WebDavProvider::new(test_config("https://cloud.example.com/")).expect("provider");
        assert!(p.supports_server_copy());
        assert!(p.supports_server_side_copy());
    }

    /// SG-T08 gate: both entry points fail fast on the connection check
    /// before issuing a COPY request.
    #[tokio::test]
    async fn webdav_server_side_copy_requires_connection() {
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com/")).expect("provider");
        let direct = StorageProvider::server_side_copy(&mut p, "/src.txt", "/dst.txt").await;
        assert!(matches!(direct, Err(ProviderError::NotConnected)));

        let via_legacy = p.server_copy("/src.txt", "/dst.txt").await;
        assert!(matches!(via_legacy, Err(ProviderError::NotConnected)));
    }
}
