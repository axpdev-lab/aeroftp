//! Jottacloud Storage Provider
//!
//! Implements StorageProvider for Jottacloud using the JFS REST API.
//! Authentication: Personal Login Token → OIDC discovery → OAuth2 token exchange.
//! API reference: rclone Jottacloud backend (no official docs available).
//!
//! JFS Base: https://jfs.jottacloud.com/jfs/
//! API Base: https://api.jottacloud.com/
//! Path: /{username}/{device}/{mountpoint}/{path}
//! Upload: two-phase (allocate → upload)
//! Listing: XML format parsed with quick-xml

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Instant;
use tracing::info;

use super::{
    sanitize_api_error, send_with_retry, HttpRetryConfig, JottacloudConfig, ProviderError,
    ProviderType, RemoteEntry, ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider,
};

const JFS_BASE: &str = "https://jfs.jottacloud.com/jfs";
const API_BASE: &str = "https://api.jottacloud.com";
/// The recycle bin is a mountpoint of the device, alongside Archive and Sync.
const TRASH_MOUNTPOINT: &str = "Trash";

fn jotta_log(msg: &str) {
    info!("[JOTTACLOUD] {}", msg);
}

fn mask_credential(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    if let Some(at) = value.find('@') {
        let local = &value[..at];
        let domain = &value[at..];
        let visible = local.len().min(3);
        format!("{}***{}", &local[..visible], domain)
    } else if value.len() <= 3 {
        "***".to_string()
    } else {
        format!("{}***", &value[..3])
    }
}

// ─── Auth Types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LoginToken {
    username: Option<String>,
    auth_token: Option<String>,
    #[serde(alias = "wellKnownLink")]
    well_known_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OidcConfig {
    token_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

// ─── API Types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CustomerInfo {
    username: Option<String>,
    #[serde(default)]
    usage: i64,
    #[serde(default)]
    quota: i64,
}

// ─── Provider ───────────────────────────────────────────────────────────

/// Outcome of a trash restore. Every count is server-confirmed: a file is
/// "restored" only when its cphash POST 2xx'd against a tombstone, and a
/// directory is "restored" only when mkDir's answer came back without the
/// `deleted` attribute. Children found already live are counted apart and
/// never as work we produced.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrashRestoreReport {
    pub files_restored: u32,
    pub files_already_present: u32,
    pub dirs_restored: u32,
    /// `path: reason` for every entry that could not be restored.
    pub failed: Vec<String>,
}

/// One child of a folder listing, tombstone-aware. `deleted` mirrors the
/// `deleted` attribute: tombstoned children still sit in Trash; live ones
/// were already restored (by us earlier or by someone else).
#[derive(Debug, Clone, PartialEq)]
enum TombstoneChild {
    Folder {
        name: String,
        deleted: bool,
    },
    /// `revision` is the tombstone's (size, md5, created, modified).
    File {
        name: String,
        deleted: bool,
        revision: Option<(u64, String, String, String)>,
    },
}

/// A file collected by the restore walk: path under the mountpoint, whether
/// it still carries the `deleted` stamp, and its tombstone revision.
struct WalkedFile {
    path: String,
    deleted: bool,
    revision: Option<(u64, String, String, String)>,
}

pub struct JottacloudProvider {
    config: JottacloudConfig,
    client: reqwest::Client,
    connected: bool,
    username: String,
    /// OAuth2 access token (SecretString for memory zeroization)
    access_token: SecretString,
    /// OAuth2 refresh token (SecretString for memory zeroization)
    refresh_token: SecretString,
    token_endpoint: String,
    token_expiry: Instant,
    /// Vault key the live refresh chain was loaded from, when it was not the
    /// key this provider is bound to. A rotation is written back to BOTH, so
    /// whichever client reads either key next finds the current value; see
    /// `refresh_persist_accounts`.
    refresh_source_account: Option<String>,
    current_path: String,
    /// Server profile identifier owning the persisted refresh token. Empty
    /// when the caller has not bound a profile (legacy singleton key path).
    /// Issue #214.
    profile_id: String,
}

impl JottacloudProvider {
    pub fn new(config: JottacloudConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            username: String::new(),
            access_token: SecretString::from(String::new()),
            refresh_token: SecretString::from(String::new()),
            token_endpoint: String::new(),
            token_expiry: Instant::now(),
            refresh_source_account: None,
            current_path: "/".to_string(),
            profile_id: String::new(),
        }
    }

    /// Bind this provider to a server profile so the persisted Jotta refresh
    /// token is stored under `jottacloud_refresh_<profile_id>` instead of the
    /// legacy singleton key. Issue #214.
    pub fn with_profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = profile_id.into();
        self
    }

    /// Vault key for the Jottacloud refresh token bound to this provider's
    /// profile (legacy singleton when `profile_id` is empty). Issue #214.
    fn refresh_token_account(&self) -> String {
        if self.profile_id.is_empty() {
            "jottacloud_refresh".to_string()
        } else {
            format!("jottacloud_refresh_{}", self.profile_id)
        }
    }

    /// Every vault key that may hold a live refresh chain for this profile,
    /// most specific first. A station can carry TWO chains for one profile:
    /// the per-profile key written by a bound provider (the GUI) and the
    /// legacy singleton written by an unbound one (the CLI up to v4.1.8).
    /// Jotta rotates each independently, so one can be refused while the
    /// other still works; a refusal on the first must try the second before
    /// falling through to the one-shot login token, which is usually spent.
    /// Measured 2026-09-01: per-profile refused with `invalid_grant`, the
    /// singleton accepted, and the fallthrough reported the login token as
    /// "expired or already used" on a station that could still connect.
    fn refresh_chain_candidates(profile_id: &str) -> Vec<String> {
        if profile_id.is_empty() {
            vec!["jottacloud_refresh".to_string()]
        } else {
            vec![
                format!("jottacloud_refresh_{}", profile_id),
                "jottacloud_refresh".to_string(),
            ]
        }
    }

    /// Keys a rotated token is written to: the provider's own key, plus the
    /// key the chain was loaded from when that was a different one. Writing
    /// only the own key would leave the source key holding a consumed token,
    /// and the next client reading it (an older CLI on the singleton) would
    /// fall through to the spent login token. Both keys, one chain.
    fn refresh_persist_accounts(profile_id: &str, source: Option<&str>) -> Vec<String> {
        let own = if profile_id.is_empty() {
            "jottacloud_refresh".to_string()
        } else {
            format!("jottacloud_refresh_{}", profile_id)
        };
        let mut out = vec![own.clone()];
        if let Some(src) = source {
            if src != own {
                out.push(src.to_string());
            }
        }
        out
    }

    // ─── Auth Helpers ───────────────────────────────────────────────────

    fn decode_login_token(token_str: &str) -> Result<LoginToken, ProviderError> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(token_str.trim())
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(token_str.trim()))
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!(
                    "Invalid login token (Base64 decode failed): {}",
                    e
                ))
            })?;
        let token: LoginToken = serde_json::from_slice(&decoded).map_err(|e| {
            ProviderError::AuthenticationFailed(format!(
                "Invalid login token (JSON parse failed): {}",
                e
            ))
        })?;
        if token.username.as_ref().is_none_or(|u| u.is_empty()) {
            return Err(ProviderError::AuthenticationFailed(
                "Login token missing username".to_string(),
            ));
        }
        if token.auth_token.as_ref().is_none_or(|t| t.is_empty()) {
            return Err(ProviderError::AuthenticationFailed(
                "Login token missing auth_token".to_string(),
            ));
        }
        if token.well_known_link.as_ref().is_none_or(|l| l.is_empty()) {
            return Err(ProviderError::AuthenticationFailed(
                "Login token missing wellKnownLink".to_string(),
            ));
        }
        Ok(token)
    }

    async fn discover_oidc(&self, well_known_url: &str) -> Result<String, ProviderError> {
        // Validate URL scheme
        if !well_known_url.starts_with("https://") {
            return Err(ProviderError::AuthenticationFailed(
                "OIDC well-known URL must use HTTPS".to_string(),
            ));
        }
        let resp = self.client.get(well_known_url).send().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("OIDC discovery failed: {}", e))
        })?;
        if !resp.status().is_success() {
            return Err(ProviderError::AuthenticationFailed(format!(
                "OIDC discovery returned {}",
                resp.status()
            )));
        }
        let config: OidcConfig = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("OIDC config parse failed: {}", e))
        })?;
        config.token_endpoint.ok_or_else(|| {
            ProviderError::AuthenticationFailed("OIDC config missing token_endpoint".to_string())
        })
    }

    async fn exchange_token(
        &self,
        token_endpoint: &str,
        username: &str,
        auth_token: &str,
    ) -> Result<TokenResponse, ProviderError> {
        let form_body = format!(
            "grant_type=password&username={}&password={}&scope={}&client_id=jottacli",
            urlencoding::encode(username),
            urlencoding::encode(auth_token),
            urlencoding::encode("openid offline_access"),
        );
        let resp = self
            .client
            .post(token_endpoint)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Token exchange failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(format!(
                "Token exchange failed: {}",
                sanitize_api_error(&body)
            )));
        }
        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Token response parse failed: {}", e))
        })?;
        if token_resp.access_token.is_none() {
            return Err(ProviderError::AuthenticationFailed(
                "Token exchange returned no access_token".to_string(),
            ));
        }
        Ok(token_resp)
    }

    async fn refresh_if_needed(&mut self) -> Result<(), ProviderError> {
        // Refresh 60 seconds before expiry
        if Instant::now() < self.token_expiry - std::time::Duration::from_secs(60) {
            return Ok(());
        }
        if self.refresh_token.expose_secret().is_empty() || self.token_endpoint.is_empty() {
            return Err(ProviderError::AuthenticationFailed(
                "Cannot refresh: no refresh token available".to_string(),
            ));
        }
        jotta_log("Refreshing access token");
        // RFC 6749 §6 `grant_type=refresh_token` (lowercase). The prior
        // "Jottacloud quirk: uppercase REFRESH_TOKEN" is no longer
        // accepted by Jotta's OIDC server: the endpoint now returns
        // `unsupported_grant_type` for the uppercase form, which forced
        // every second invocation to fall back to the single-use login
        // token (J1 finding part 2).
        let form_body = format!(
            "grant_type=refresh_token&refresh_token={}&client_id=jottacli",
            urlencoding::encode(self.refresh_token.expose_secret()),
        );
        let resp = self
            .client
            .post(&self.token_endpoint)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Token refresh failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(format!(
                "Token refresh failed: {}",
                sanitize_api_error(&body)
            )));
        }
        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Refresh response parse failed: {}", e))
        })?;
        if let Some(ref at) = token_resp.access_token {
            self.access_token = SecretString::from(at.clone());
        }
        let mut rt_rotated = false;
        if let Some(ref rt) = token_resp.refresh_token {
            if rt != self.refresh_token.expose_secret() {
                rt_rotated = true;
            }
            self.refresh_token = SecretString::from(rt.clone());
        }
        let expires_in = token_resp.expires_in.unwrap_or(3600);
        self.token_expiry = Instant::now() + std::time::Duration::from_secs(expires_in);
        // Re-persist if refresh token was rotated by the server
        if rt_rotated {
            self.persist_refresh_token();
        }
        jotta_log("Access token refreshed");
        Ok(())
    }

    fn auth_header(&self) -> HeaderValue {
        HeaderValue::from_str(&format!("Bearer {}", self.access_token.expose_secret()))
            .unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    // ─── Token Persistence ───────────────────────────────────────────────

    /// Persist refresh token + metadata in credential vault for reconnection
    /// without requiring a new single-use login token.
    fn persist_refresh_token(&self) {
        let data = serde_json::json!({
            "refresh_token": self.refresh_token.expose_secret(),
            "token_endpoint": self.token_endpoint,
            "username": self.username,
        });
        let json = data.to_string();
        let accounts = Self::refresh_persist_accounts(
            &self.profile_id,
            self.refresh_source_account.as_deref(),
        );
        let write_all = |store: &crate::credential_store::CredentialStore| -> bool {
            let mut ok = true;
            for account in &accounts {
                if store.store(account, &json).is_err() {
                    ok = false;
                    continue;
                }
                // MUV-4: mirror into the active user's partition (per-profile only).
                if account != "jottacloud_refresh" {
                    crate::user_partitions::mirror_active_credential(
                        store,
                        account,
                        "jottacloud_refresh",
                        &json,
                    );
                }
            }
            ok
        };
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if write_all(&store) {
                jotta_log(&format!(
                    "Refresh token persisted to vault ({} key(s))",
                    accounts.len()
                ));
                return;
            }
        }
        // Try auto-init vault
        if crate::credential_store::CredentialStore::init().is_ok() {
            if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
                let _ = write_all(&store);
                jotta_log("Refresh token persisted to auto-initialized vault");
            }
        }
    }

    /// Read one refresh chain from the vault: the active user's partition
    /// first (vault fallback inside), then the raw vault key.
    fn load_refresh_chain(account: &str) -> Option<(String, String, String)> {
        let store = crate::credential_store::CredentialStore::from_cache()?;
        let parse = |json: String| -> Option<(String, String, String)> {
            let v: serde_json::Value = serde_json::from_str(&json).ok()?;
            let rt = v["refresh_token"].as_str()?.to_string();
            let te = v["token_endpoint"].as_str()?.to_string();
            let un = v["username"].as_str()?.to_string();
            if rt.is_empty() || te.is_empty() || un.is_empty() {
                None
            } else {
                Some((rt, te, un))
            }
        };
        if let Ok(Some(json)) = crate::user_partitions::resolve_active_credential(&store, account) {
            if let Some(parsed) = parse(json.to_string()) {
                return Some(parsed);
            }
        }
        store.get(account).ok().and_then(parse)
    }

    /// Try to connect using a persisted refresh token (no login token needed).
    async fn try_connect_with_refresh(&mut self) -> Result<bool, ProviderError> {
        let own = self.refresh_token_account();
        for account in Self::refresh_chain_candidates(&self.profile_id) {
            let Some((rt, te, un)) = Self::load_refresh_chain(&account) else {
                continue;
            };
            jotta_log(&format!(
                "Found persisted refresh token ({} chars) for {} under {}, attempting reconnection at {}",
                rt.len(),
                mask_credential(&un),
                account,
                te
            ));

            // J1 root cause: Jotta's OIDC rejects uppercase `REFRESH_TOKEN`
            // as `unsupported_grant_type`. Use the RFC 6749 lowercase form.
            let form_body = format!(
                "grant_type=refresh_token&refresh_token={}&client_id=jottacli",
                urlencoding::encode(&rt),
            );
            let resp = self
                .client
                .post(&te)
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(form_body)
                .send()
                .await
                .map_err(|e| {
                    ProviderError::AuthenticationFailed(format!(
                        "Refresh token exchange failed: {}",
                        e
                    ))
                })?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                jotta_log(&format!(
                    "Persisted refresh token under {} rejected by Jotta OIDC (status={}, body={}): trying the next chain",
                    account,
                    status,
                    sanitize_api_error(&body)
                ));
                // A refused chain is dead for everyone: clear it so no client
                // reads it again and falls through to the login token. Both
                // copies must go: the raw vault key AND the active user's
                // partition mirror, which `resolve_active_credential` reads
                // FIRST. Deleting only the vault key left the mirror in place,
                // so every later read (this provider, the profile export)
                // found the same dead chain again. Measured 2026-09-02: three
                // exports in a row carried a 707-char chain Jotta refused
                // with the same `invalid_grant` on the other station.
                if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
                    let _ = store.delete(&account);
                }
                crate::user_partitions::unmirror_active_credential(&account);
                continue;
            }

            let token_resp: TokenResponse = resp.json().await.map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Refresh response parse failed: {}", e))
            })?;
            if token_resp.access_token.is_none() {
                continue;
            }

            self.username = un;
            self.access_token = SecretString::from(token_resp.access_token.unwrap_or_default());
            // Jotta rotates the refresh token at every exchange. Persisting
            // the rotated value is mandatory: if we keep the old one in the
            // on-disk slot, the next invocation reads an already-consumed
            // token and falls through to the (one-shot) login token, which
            // returns "Login token expired or already used". The prior
            // behaviour only updated the in-memory field (J1 finding).
            self.refresh_token = SecretString::from(token_resp.refresh_token.unwrap_or(rt));
            self.token_endpoint = te;
            let expires_in = token_resp.expires_in.unwrap_or(3600);
            self.token_expiry = Instant::now() + std::time::Duration::from_secs(expires_in);
            self.refresh_source_account = if account == own { None } else { Some(account) };

            self.persist_refresh_token();
            jotta_log("Reconnected using persisted refresh token; rotated token saved");
            return Ok(true);
        }
        jotta_log("No usable persisted refresh token in vault: will use login token");
        Ok(false)
    }

    // ─── HTTP Helpers ───────────────────────────────────────────────────

    async fn get_with_retry(&mut self, url: &str) -> Result<reqwest::Response, ProviderError> {
        self.refresh_if_needed().await?;
        let request = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// POST with no body and no `Content-Type`.
    ///
    /// JFS routes a POST on a *file* URL by content type: an
    /// `application/octet-stream` POST is an upload, so the delete/trash
    /// parameters in the query string were never reached and the call came
    /// back `404 Not Found` with an XML error body (#397). Command-style POSTs
    /// (`?dl`, `?rm`, `?dlDir`, `?rmDir`, `?mv`, `?restore`) must therefore go
    /// out bare, exactly as the reference JFS clients send them.
    async fn post_command_with_retry(
        &mut self,
        url: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        self.refresh_if_needed().await?;
        let request = self
            .client
            .post(url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_LENGTH, "0")
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    // ─── Path Helpers ───────────────────────────────────────────────────

    /// Build full JFS URL: /jfs/{username}/{device}/{mountpoint}/{path}
    fn jfs_url(&self, path: &str) -> String {
        let clean = path.trim_start_matches('/');
        let user = urlencoding::encode(&self.username);
        let device = urlencoding::encode(&self.config.device);
        let mount = urlencoding::encode(&self.config.mountpoint);
        if clean.is_empty() {
            format!("{}/{}/{}/{}", JFS_BASE, user, device, mount)
        } else {
            // Reversible restricted-character encoding goes BEFORE URL
            // percent-encoding on the way out (decoding happens after the name
            // is read back from the response XML). Names a user could not
            // otherwise store on Jottacloud round-trip via the encoded form.
            let encoded_path: String = clean
                .split('/')
                .map(|s| {
                    let enc = crate::restricted_chars::encode_leaf(ProviderType::Jottacloud, s);
                    urlencoding::encode(&enc).into_owned()
                })
                .collect::<Vec<_>>()
                .join("/");
            format!(
                "{}/{}/{}/{}/{}",
                JFS_BASE, user, device, mount, encoded_path
            )
        }
    }

    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim().replace('\\', "/");
        if trimmed.is_empty() || trimmed == "/" {
            return "/".to_string();
        }
        let p = if trimmed.starts_with('/') {
            trimmed
        } else {
            format!("/{}", trimmed)
        };
        p.trim_end_matches('/').to_string()
    }

    fn resolve_path(&self, path: &str) -> String {
        let path = path.trim();
        if path.is_empty() || path == "." {
            return self.current_path.clone();
        }
        if path.starts_with('/') {
            return Self::normalize_path(path);
        }
        let base = self.current_path.trim_end_matches('/');
        Self::normalize_path(&format!("{}/{}", base, path))
    }

    fn split_path(path: &str) -> (String, String) {
        let normalized = Self::normalize_path(path);
        if let Some(pos) = normalized.rfind('/') {
            let parent = if pos == 0 {
                "/".to_string()
            } else {
                normalized[..pos].to_string()
            };
            let name = normalized[pos + 1..].to_string();
            (parent, name)
        } else {
            ("/".to_string(), normalized)
        }
    }

    // ─── Discovery Helpers ────────────────────────────────────────────

    /// Parse device names from /jfs/{username} XML response.
    /// Looks for <device><name>text</name></device> under <devices>.
    fn parse_device_names(xml: &str) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut names = Vec::new();
        let mut reader = Reader::from_str(xml);
        // No trim_text: name fragments around XML entities must survive
        // intact; the name accumulates and is trimmed once at element end.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        let mut in_devices = false;
        let mut in_device = false;
        let mut in_name = false;
        let mut current_name = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "devices" => in_devices = true,
                        "device" if in_devices => in_device = true,
                        "name" if in_device => {
                            in_name = true;
                            current_name.clear();
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "devices" => {
                            in_devices = false;
                            in_device = false;
                        }
                        "device" => in_device = false,
                        "name" => {
                            in_name = false;
                            let trimmed = current_name.trim().to_string();
                            if !trimmed.is_empty() {
                                names.push(trimmed);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(ref e)) if in_name => {
                    current_name.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
                Ok(Event::GeneralRef(ref e)) if in_name => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        current_name.push_str(&ch);
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        names
    }

    /// Parse mountpoint names from /jfs/{username}/{device} XML response.
    /// Looks for <mountPoint name="..."> elements.
    fn parse_mountpoint_names(xml: &str) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut names = Vec::new();
        let mut reader = Reader::from_str(xml);
        // No trim_text: name fragments around XML entities must survive
        // intact; the name accumulates and is trimmed once at element end.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        // JFS nests the name as a child element:
        //   <mountPoints><mountPoint><name>Archive</name>...
        // Reading only a `name` attribute made discovery return an empty list
        // against the live API, silently falling back to the configured
        // mountpoint (#397). Both shapes are accepted now.
        let mut in_mount_point = false;
        let mut in_name = false;
        let mut current_name = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if tag == "mountPoint" {
                        in_mount_point = true;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                // Account-level mountpoint name: an identifier,
                                // not a user filename, so it is NOT decoded.
                                let name = super::xml_text::attr_value(&attr);
                                if !name.is_empty() {
                                    names.push(name);
                                }
                            }
                        }
                    } else if tag == "name" && in_mount_point {
                        in_name = true;
                        current_name.clear();
                    }
                }
                Ok(Event::Text(ref e)) if in_name => {
                    current_name.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
                Ok(Event::GeneralRef(ref e)) if in_name => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        current_name.push_str(&ch);
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if tag == "mountPoint" {
                        in_mount_point = false;
                    } else if tag == "name" {
                        in_name = false;
                        let trimmed = current_name.trim().to_string();
                        if !trimmed.is_empty() && !names.contains(&trimmed) {
                            names.push(trimmed);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        names
    }

    /// Auto-discover device and mountpoint from the user's account.
    /// Queries /jfs/{username} for devices, then /jfs/{username}/{device} for mountpoints.
    /// Falls back to configured defaults if discovery fails.
    async fn discover_device_mountpoint(&mut self) -> Result<(), ProviderError> {
        // Step 1: Query user root to find devices
        let user_url = format!("{}/{}", JFS_BASE, self.username);
        let resp = self.get_with_retry(&user_url).await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let xml = resp.text().await.unwrap_or_default();
                jotta_log(&format!(
                    "Device discovery XML ({} bytes): {}",
                    xml.len(),
                    &xml[..xml.len().min(500)]
                ));
                let devices = Self::parse_device_names(&xml);
                jotta_log(&format!("Available devices: {:?}", devices));

                // Pick device: prefer configured, then "Jotta", then first available
                if !devices.is_empty() && !devices.contains(&self.config.device) {
                    if devices.contains(&"Jotta".to_string()) {
                        self.config.device = "Jotta".to_string();
                    } else {
                        self.config.device = devices[0].clone();
                    }
                }
            }
        }

        // Step 2: Query device to find mountpoints
        let device_url = format!("{}/{}/{}", JFS_BASE, self.username, self.config.device);
        let resp = self.get_with_retry(&device_url).await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let xml = resp.text().await.unwrap_or_default();
                jotta_log(&format!(
                    "Mountpoint discovery XML ({} bytes): {}",
                    xml.len(),
                    &xml[..xml.len().min(500)]
                ));
                let mountpoints = Self::parse_mountpoint_names(&xml);
                jotta_log(&format!(
                    "Available mountpoints on {}: {:?}",
                    self.config.device, mountpoints
                ));

                // Pick mountpoint: prefer configured, then "Archive", then "Sync", then first
                if !mountpoints.is_empty() && !mountpoints.contains(&self.config.mountpoint) {
                    if mountpoints.contains(&"Archive".to_string()) {
                        self.config.mountpoint = "Archive".to_string();
                    } else if mountpoints.contains(&"Sync".to_string()) {
                        self.config.mountpoint = "Sync".to_string();
                    } else {
                        self.config.mountpoint = mountpoints[0].clone();
                    }
                }
            }
        }

        jotta_log(&format!(
            "Using device={}, mountpoint={}",
            self.config.device, self.config.mountpoint
        ));
        Ok(())
    }

    // ─── XML Parsing ────────────────────────────────────────────────────

    /// Parse JFS XML folder listing into RemoteEntry items.
    /// Handles both `<folder>` and `<mountPoint>` as root elements.
    /// Handles both full `<folder name="X">...</folder>` and self-closing `<folder name="X"/>`.
    /// Only includes files with state=COMPLETED (skips INCOMPLETE, CORRUPT, ADDED).
    fn parse_folder_xml(xml: &str, base_path: &str) -> Vec<RemoteEntry> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        // trim_text(true) is SAFE here: entry names arrive as attributes
        // (xml_text::attr_value), never as Text events; only scalars do.
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut depth: u32 = 0;
        let mut root_depth: Option<u32> = None;

        // Folder section: <folders> wrapper inside root
        let mut in_folders_section = false;
        let mut folders_section_depth: u32 = 0;
        let mut child_folder_depth: Option<u32> = None; // skip nested content

        // File parsing state
        let mut in_file = false;
        let mut in_revision = false;
        let mut current_name = String::new();
        let mut current_size: u64 = 0;
        let mut current_modified = String::new();
        let mut current_mime = String::new();
        let mut current_md5 = String::new();
        let mut current_state = String::new();
        let mut current_deleted = false; // skip trashed files
        let mut current_tag = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    depth += 1;

                    match tag.as_str() {
                        "folder" | "mountPoint" if root_depth.is_none() => {
                            root_depth = Some(depth);
                        }
                        "folders"
                            if root_depth == Some(depth - 1) && child_folder_depth.is_none() =>
                        {
                            in_folders_section = true;
                            folders_section_depth = depth;
                        }
                        "folder"
                            if child_folder_depth.is_none()
                                && (in_folders_section
                                    || (root_depth.is_some()
                                        && depth == root_depth.unwrap() + 1)) =>
                        {
                            // Direct child folder (full element): inside <folders> or direct child of root
                            let mut name = String::new();
                            let mut is_deleted = false;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    name = crate::restricted_chars::decode_leaf(
                                        ProviderType::Jottacloud,
                                        &super::xml_text::attr_value(&attr),
                                    );
                                }
                                if attr.key.as_ref() == b"deleted" {
                                    is_deleted = true;
                                }
                            }
                            if !name.is_empty() && !is_deleted {
                                let entry_path = if base_path == "/" {
                                    format!("/{}", name)
                                } else {
                                    format!("{}/{}", base_path, name)
                                };
                                entries.push(RemoteEntry {
                                    name,
                                    path: entry_path,
                                    is_dir: true,
                                    size: 0,
                                    modified: None,
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                            child_folder_depth = Some(depth);
                        }
                        "file" if !in_file && child_folder_depth.is_none() => {
                            in_file = true;
                            current_name.clear();
                            current_size = 0;
                            current_modified.clear();
                            current_mime.clear();
                            current_md5.clear();
                            current_state.clear();
                            current_deleted = false;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    current_name = crate::restricted_chars::decode_leaf(
                                        ProviderType::Jottacloud,
                                        &super::xml_text::attr_value(&attr),
                                    );
                                }
                                // A trashed file stays in its folder listing as a
                                // tombstone carrying `deleted="<timestamp>"`, the
                                // same marker the folder branch already honours.
                                // Only the `<deleted>` *element* form was checked
                                // here, so a file moved to the recycle bin kept
                                // showing as live and the delete looked like a
                                // no-op (#397).
                                if attr.key.as_ref() == b"deleted" {
                                    current_deleted = true;
                                }
                            }
                        }
                        "currentRevision" if in_file => {
                            in_revision = true;
                        }
                        _ => {
                            current_tag = tag;
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();

                    if tag == "folder"
                        && child_folder_depth.is_none()
                        && (in_folders_section
                            || (root_depth.is_some() && depth == root_depth.unwrap()))
                    {
                        // Self-closing <folder name="X"/>: direct child
                        let mut name = String::new();
                        let mut is_deleted = false;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                name = crate::restricted_chars::decode_leaf(
                                    ProviderType::Jottacloud,
                                    &super::xml_text::attr_value(&attr),
                                );
                            }
                            if attr.key.as_ref() == b"deleted" {
                                is_deleted = true;
                            }
                        }
                        if !name.is_empty() && !is_deleted {
                            let entry_path = if base_path == "/" {
                                format!("/{}", name)
                            } else {
                                format!("{}/{}", base_path, name)
                            };
                            entries.push(RemoteEntry {
                                name,
                                path: entry_path,
                                is_dir: true,
                                size: 0,
                                modified: None,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                metadata: HashMap::new(),
                                mime_type: None,
                            });
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();

                    match tag.as_str() {
                        "folders" if in_folders_section && depth == folders_section_depth => {
                            in_folders_section = false;
                        }
                        "folder" if child_folder_depth == Some(depth) => {
                            child_folder_depth = None;
                        }
                        "file" if in_file => {
                            in_file = false;
                            in_revision = false;
                            if current_state == "COMPLETED"
                                && !current_deleted
                                && !current_name.is_empty()
                            {
                                let entry_path = if base_path == "/" {
                                    format!("/{}", current_name)
                                } else {
                                    format!("{}/{}", base_path, current_name)
                                };
                                let mut metadata = HashMap::new();
                                if !current_md5.is_empty() {
                                    metadata.insert("md5".to_string(), current_md5.clone());
                                }
                                entries.push(RemoteEntry {
                                    name: current_name.clone(),
                                    path: entry_path,
                                    is_dir: false,
                                    size: current_size,
                                    modified: if current_modified.is_empty() {
                                        None
                                    } else {
                                        Some(current_modified.clone())
                                    },
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata,
                                    mime_type: if current_mime.is_empty() {
                                        None
                                    } else {
                                        Some(current_mime.clone())
                                    },
                                });
                            }
                        }
                        "currentRevision" => {
                            in_revision = false;
                        }
                        _ => {}
                    }

                    depth = depth.saturating_sub(1);
                    current_tag.clear();
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if in_file {
                        // <deleted> tag at file level (outside revision) marks trashed files
                        if current_tag == "deleted" && !text.is_empty() {
                            current_deleted = true;
                        }
                        if in_revision {
                            match current_tag.as_str() {
                                "size" => {
                                    current_size = text.parse().unwrap_or(0);
                                }
                                "mime" => {
                                    current_mime = text;
                                }
                                "md5" => {
                                    current_md5 = text;
                                }
                                "state" => {
                                    current_state = text;
                                }
                                "modified" | "updated" if current_modified.is_empty() => {
                                    current_modified = Self::parse_jotta_time(&text);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        entries
    }

    /// Parse Jottacloud time format "2006-01-02-T15:04:05Z0700" into ISO 8601
    fn parse_jotta_time(s: &str) -> String {
        // Jottacloud uses a non-standard format with an extra dash before T
        // "2023-01-15-T10:30:45Z0100" → "2023-01-15T10:30:45+01:00"
        let cleaned = s.replace("-T", "T");
        // Try to parse and format nicely, or return as-is
        if let Ok(dt) = chrono::DateTime::parse_from_str(&cleaned, "%Y-%m-%dT%H:%M:%S%z") {
            return dt.format("%Y-%m-%d %H:%M:%SZ").to_string();
        }
        // Try RFC3339
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&cleaned) {
            return dt.format("%Y-%m-%d %H:%M:%SZ").to_string();
        }
        // Return cleaned version
        cleaned
    }
}

// ─── StorageProvider Implementation ──────────────────────────────────────

#[async_trait]
impl StorageProvider for JottacloudProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Jottacloud
    }

    fn display_name(&self) -> String {
        format!("Jottacloud ({})", self.username)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        jotta_log("Connecting to Jottacloud");

        // Strategy: try persisted refresh token first (avoids consuming the single-use login token).
        // If that fails, fall back to the login token from config.
        let used_refresh = self.try_connect_with_refresh().await.unwrap_or(false);

        if !used_refresh {
            // Step 1: Decode login token
            let login_token = Self::decode_login_token(self.config.login_token.expose_secret())?;
            let username = login_token.username.unwrap_or_default();
            let auth_token = login_token.auth_token.unwrap_or_default();
            let well_known_link = login_token.well_known_link.unwrap_or_default();

            jotta_log(&format!(
                "Username: {}, discovering OIDC from well-known URL",
                username
            ));

            // Step 2: OIDC discovery
            let token_endpoint = self.discover_oidc(&well_known_link).await?;
            jotta_log(&format!("Token endpoint discovered: {}", token_endpoint));

            // Step 3: Exchange credentials for access token
            // Login tokens are single-use: if already consumed, this returns invalid_grant.
            let token_resp = self.exchange_token(&token_endpoint, &username, &auth_token).await
                .map_err(|e| {
                    let msg = format!("{}", e);
                    if msg.contains("invalid_grant") || msg.contains("Invalid user credentials") {
                        ProviderError::AuthenticationFailed(
                            "Login token expired or already used. Generate a new Personal Login Token at jottacloud.com → Settings → Security.".to_string()
                        )
                    } else {
                        e
                    }
                })?;

            self.username = username;
            self.access_token = SecretString::from(token_resp.access_token.unwrap_or_default());
            self.refresh_token = SecretString::from(token_resp.refresh_token.unwrap_or_default());
            self.token_endpoint = token_endpoint;
            let expires_in = token_resp.expires_in.unwrap_or(3600);
            self.token_expiry = Instant::now() + std::time::Duration::from_secs(expires_in);
        }

        // Step 4: Verify by fetching customer info
        let url = format!("{}/account/v1/customer", API_BASE);
        let resp = self.get_with_retry(&url).await?;

        if resp.status().as_u16() == 401 {
            return Err(ProviderError::AuthenticationFailed(
                "Invalid credentials. Regenerate your Personal Login Token at jottacloud.com → Settings → Security".to_string()
            ));
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ConnectionFailed(format!(
                "Jottacloud connection failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let customer: CustomerInfo = resp.json().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to parse customer info: {}", e))
        })?;

        // Use customer info username for JFS paths (may differ from login token username)
        if let Some(ref u) = customer.username {
            if !u.is_empty() && *u != self.username {
                jotta_log(&format!(
                    "JFS username from customer info: {} (token had: {})",
                    mask_credential(u),
                    mask_credential(&self.username)
                ));
                self.username = u.clone();
            } else {
                jotta_log(&format!("Authenticated as: {}", mask_credential(u)));
            }
        }

        // Step 5: Auto-discover device and mountpoint
        self.discover_device_mountpoint().await?;

        // Step 6: Navigate to initial path
        self.current_path = "/".to_string();
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                self.current_path = Self::normalize_path(&initial);
            }
        }

        self.connected = true;

        // Persist refresh token for future reconnections (login token is single-use)
        self.persist_refresh_token();

        jotta_log(&format!(
            "Connected (device={}, mountpoint={})",
            self.config.device, self.config.mountpoint
        ));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.current_path = "/".to_string();
        self.access_token = SecretString::from(String::new());
        self.refresh_token = SecretString::from(String::new());
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
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
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        };

        // Verify directory exists by listing it
        let url = self.jfs_url(&new_path);
        let resp = self.get_with_retry(&url).await?;
        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(format!(
                "Directory not found: {}",
                new_path
            )));
        }

        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = self.jfs_url(&resolved);

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Err(ProviderError::NotFound(format!(
                    "Path not found: {}",
                    resolved
                )));
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List {} failed ({}): {}",
                resolved,
                status,
                sanitize_api_error(&body)
            )));
        }

        let xml = resp
            .text()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to read response: {}", e)))?;

        jotta_log(&format!(
            "List XML for '{}' ({} bytes): {}",
            resolved,
            xml.len(),
            &xml[..xml.len().min(2000)]
        ));

        let entries = Self::parse_folder_xml(&xml, &resolved);
        jotta_log(&format!(
            "Parsed {} entries (dirs={}, files={})",
            entries.len(),
            entries.iter().filter(|e| e.is_dir).count(),
            entries.iter().filter(|e| !e.is_dir).count(),
        ));
        Ok(entries)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let url = format!("{}?mode=bin", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Download {} failed ({}): {}",
                resolved,
                status,
                sanitize_api_error(&body)
            )));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| {
                ProviderError::TransferFailed(format!("Create local file failed: {}", e))
            })?;

        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {}", e))
            })?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write failed: {}", e)))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = progress {
                cb(downloaded, total_size);
            }
        }

        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        jotta_log(&format!("Downloaded {} ({} bytes)", resolved, downloaded));
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
        let resolved = self.resolve_path(remote_path);
        let url = format!("{}?mode=bin", self.jfs_url(&resolved));

        // Ensure token is fresh before building the request closure
        self.refresh_if_needed().await?;
        let auth = self.auth_header();

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&url).header(AUTHORIZATION, auth.clone());
                if let Some(range) = range_header {
                    req = req.header("Range", range);
                }
                req
            },
            on_progress,
        )
        .await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        // M9: Full file read into memory: Jottacloud's upload API requires the complete body
        // with an MD5 hash for deduplication. This limits practical upload size to available RAM.
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Read local file failed: {}", e)))?;

        let total_size = data.len() as u64;

        // Calculate MD5 for deduplication
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(&data);
        let md5_hash = format!("{:x}", hasher.finalize());

        // Get file modification time in Jottacloud format: "2006-01-02-T15:04:05Z" (extra dash before T)
        let modified_time = tokio::fs::metadata(local_path)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.format("%Y-%m-%d-T%H:%M:%SZ").to_string()
            })
            .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d-T%H:%M:%SZ").to_string());

        // Direct upload to up.jottacloud.com (rclone-compatible method)
        // POST https://up.jottacloud.com/jfs/{user}/{device}/{mountpoint}/{path}
        let clean = resolved.trim_start_matches('/');
        // Restricted-character encoding BEFORE URL percent-encoding (see jfs_url).
        let encoded_path: String = clean
            .split('/')
            .map(|s| {
                let enc = crate::restricted_chars::encode_leaf(ProviderType::Jottacloud, s);
                urlencoding::encode(&enc).into_owned()
            })
            .collect::<Vec<_>>()
            .join("/");
        let upload_url = format!(
            "https://up.jottacloud.com/jfs/{}/{}/{}/{}",
            urlencoding::encode(&self.username),
            urlencoding::encode(&self.config.device),
            urlencoding::encode(&self.config.mountpoint),
            encoded_path
        );

        jotta_log(&format!("Upload URL: {}", upload_url));

        // Extract filename for multipart
        let filename = resolved.rsplit('/').next().unwrap_or("file").to_string();

        self.refresh_if_needed().await?;

        // Upload as multipart/form-data with "file" field (rclone-compatible)
        let file_part = reqwest::multipart::Part::bytes(data)
            .file_name(filename)
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::TransferFailed(format!("Multipart error: {}", e)))?;
        let form = reqwest::multipart::Form::new().part("file", file_part);

        let resp = self
            .client
            .post(&upload_url)
            .header(AUTHORIZATION, self.auth_header())
            .header("JMd5", &md5_hash)
            .header("JSize", total_size.to_string())
            .header("JCreated", &modified_time)
            .header("JModified", &modified_time)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            jotta_log(&format!(
                "Upload error response: {}",
                &body[..body.len().min(1000)]
            ));
            return Err(ProviderError::TransferFailed(format!(
                "Upload failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        if let Some(ref cb) = progress {
            cb(total_size, total_size);
        }

        jotta_log(&format!("Uploaded {} ({} bytes)", resolved, total_size));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}?mkDir=true", self.jfs_url(&resolved));

        let resp = self.post_command_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Mkdir {} failed ({}): {}",
                resolved,
                status,
                sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Created directory: {}", resolved));
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        // Soft-delete into Jottacloud Trash (recoverable via View Trash).
        // Hard delete used to fire `?rm=true` / `?rmDir=true` which skipped the
        // recycle bin entirely — unlike OpenDrive/Google Drive — so folders
        // disappeared permanently from AeroFTP. Permanent purge stays on
        // `permanent_delete_from_trash` / `delete_permanent` only (#397).
        self.move_to_trash(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);

        // Use move operation for rename. Encode the destination path segments
        // (not the account identifiers) before building the move target.
        let encoded_to_path: String = resolved_to
            .trim_start_matches('/')
            .split('/')
            .map(|s| crate::restricted_chars::encode_leaf(ProviderType::Jottacloud, s))
            .collect::<Vec<_>>()
            .join("/");
        let to_jfs = format!(
            "/{}/{}/{}/{}",
            self.username, self.config.device, self.config.mountpoint, encoded_to_path
        );
        // JFS: files take `mv`, directories take `mvDir` (and a trailing slash
        // on the source). `?mv=` on a folder 404s (#397).
        let is_dir = self
            .stat(&resolved_from)
            .await
            .map(|e| e.is_dir)
            .unwrap_or(false);
        let mut from_url = self.jfs_url(&resolved_from);
        if is_dir && !from_url.ends_with('/') {
            from_url.push('/');
        }
        let url = format!(
            "{}?{}={}",
            from_url,
            if is_dir { "mvDir" } else { "mv" },
            urlencoding::encode(&to_jfs)
        );

        let resp = self.post_command_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            // A live 404 is "not found here", not "exists only in Trash".
            // Retrying as a Trash-to-Trash rename can move a homonym in
            // Trash and return Ok while the live object was never touched
            // (F-652-2), so the status is reported as it came.
            //
            // A Trash-to-Trash rename exists as `rename_in_trash`, but
            // NOTHING calls it yet: there is no Tauri command and no button
            // in JottacloudTrashManager, so it must not be described as
            // reachable.
            //
            // Keeping it is a DECLARED exception to the house rule "remove
            // dead code immediately" (CLAUDE.md). The reason: issue #397
            // reports this exact operation failing for a user, and this is
            // its implementation with its tests. Deleting it would throw away
            // the only answer to that report and make whoever takes #397
            // write it again. The exception lasts as long as the reason: if
            // #397 does not wire it during this release cycle, it goes.
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Rename {} → {} failed ({}): {}",
                resolved_from,
                resolved_to,
                status,
                sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Renamed {} → {}", resolved_from, resolved_to));
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = self.jfs_url(&resolved);

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(format!(
                "Path not found: {}",
                resolved
            )));
        }

        let xml = resp
            .text()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to read response: {}", e)))?;

        // Check if response is a folder or file
        let (_, name) = Self::split_path(&resolved);
        let is_dir = xml.contains("<folders>") || xml.contains("<folder ");

        if is_dir {
            Ok(RemoteEntry {
                name,
                path: resolved,
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                metadata: HashMap::new(),
                mime_type: None,
            })
        } else {
            // Try to parse as file listing (parent folder containing the file)
            let entries = Self::parse_folder_xml(&xml, &resolved);
            entries.into_iter().next().ok_or_else(|| {
                // Return basic entry if parsing yields nothing
                ProviderError::NotFound(format!("Could not stat: {}", resolved))
            })
        }
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        // Use recursive listing and filter by pattern
        let url = format!("{}?mode=list", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let xml = resp.text().await.unwrap_or_default();
        let all_entries = Self::parse_folder_xml(&xml, &resolved);

        Ok(all_entries
            .into_iter()
            .filter(|e| super::matches_find_pattern(&e.name, pattern))
            .collect())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/account/v1/customer", API_BASE);
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(
                "Failed to get storage info".to_string(),
            ));
        }

        let customer: CustomerInfo = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse customer info failed: {}", e))
        })?;

        let used = customer.usage.max(0) as u64;
        let total = customer.quota.max(0) as u64;
        let free = total.saturating_sub(used);

        Ok(StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    async fn create_share_link(
        &mut self,
        _path: &str,
        _options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        Err(ProviderError::NotSupported(
            "share links for Jottacloud are not yet verified against the live API".to_string(),
        ))
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let url = format!("{}?mode=bin", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Download {} failed ({}): {}",
                resolved,
                status,
                sanitize_api_error(&body)
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Jottacloud delete with dlDir=true removes recursively
        self.delete(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // `delete()` now soft-deletes into Trash (#397). Permanent purge of an
        // already-trashed entry (or a hard wipe when the caller opts in) goes
        // through the trash path with `?rm=true`.
        let basename = path.rsplit('/').next().unwrap_or(path);
        match self.permanent_delete_from_trash(basename).await {
            Ok(()) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
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

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // REST API doesn't need keep-alive
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Jottacloud: User: {}, Device: {}, Mountpoint: {}",
            self.username, self.config.device, self.config.mountpoint
        ))
    }

    fn supports_find(&self) -> bool {
        true
    }
    fn supports_share_links(&self) -> bool {
        false
    }
    fn supports_versions(&self) -> bool {
        false
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

// ─── Jottacloud-specific methods (trash management) ──────────────────────

impl JottacloudProvider {
    /// Build JFS URL for the trash: `/jfs/{username}/{device}/Trash/{path}`.
    ///
    /// Trash is a **mountpoint of the device**, a sibling of Archive and Sync,
    /// exactly like the mountpoint the profile browses. The URL used to omit
    /// the device segment, which makes JFS read `Trash` as a *device* name: no
    /// such device exists, the request 404s, and `list_trash` reports the 404
    /// as an empty bin, so the trash always looked empty however many items
    /// were in it (#397).
    fn trash_url(&self, path: &str) -> String {
        let clean = path.trim_start_matches('/');
        let user = urlencoding::encode(&self.username);
        let device = urlencoding::encode(&self.config.device);
        if clean.is_empty() {
            format!("{}/{}/{}/{}", JFS_BASE, user, device, TRASH_MOUNTPOINT)
        } else {
            let encoded_path: String = clean
                .split('/')
                .map(|s| {
                    let enc = crate::restricted_chars::encode_leaf(ProviderType::Jottacloud, s);
                    urlencoding::encode(&enc).into_owned()
                })
                .collect::<Vec<_>>()
                .join("/");
            format!(
                "{}/{}/{}/{}/{}",
                JFS_BASE, user, device, TRASH_MOUNTPOINT, encoded_path
            )
        }
    }

    /// Query parameter that deletes a JFS entry.
    ///
    /// The file and the directory forms are *not* interchangeable: firing the
    /// file form at a folder used to take it out of the account without ever
    /// putting it in the recycle bin (#397, "Move to Trash hard-deletes
    /// folders"), which is why the type decides the parameter instead of a
    /// blind retry with the other one.
    fn delete_param(is_dir: bool, permanent: bool) -> &'static str {
        match (is_dir, permanent) {
            (false, false) => "dl",
            (false, true) => "rm",
            (true, false) => "dlDir",
            (true, true) => "rmDir",
        }
    }

    /// Move a file/folder to Jottacloud Trash (soft delete).
    /// POST /jfs/{...}/{path}?dl=true (file) or ?dlDir=true (directory)
    pub async fn move_to_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        // `resolve_path`, not `normalize_path`: a bare name coming from the CLI
        // or the agent must resolve against the working directory, otherwise it
        // silently targets a same-named entry in the account root (#397).
        let resolved = self.resolve_path(path);
        // Ask what it is first — see `delete_param`.
        let is_dir = self.stat(&resolved).await?.is_dir;
        let url = format!(
            "{}?{}=true",
            self.jfs_url(&resolved),
            Self::delete_param(is_dir, false)
        );
        jotta_log(&format!("Moving to trash: {} (dir={})", resolved, is_dir));

        let resp = self.post_command_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Move to trash {} failed ({}): {}",
                resolved,
                status,
                sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Moved to trash: {}", resolved));
        Ok(())
    }

    /// List items in Jottacloud Trash.
    /// Trash is at /jfs/{username}/{device}/Trash: it is a mountpoint of the
    /// device, so the device segment is required (see `trash_url`).
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let url = self.trash_url("");
        jotta_log(&format!("Listing trash: {}", url));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Ok(Vec::new()); // Empty trash
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List trash failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let xml = resp.text().await.unwrap_or_default();
        jotta_log(&format!(
            "Trash XML ({} bytes): {}",
            xml.len(),
            &xml[..xml.len().min(2000)]
        ));

        // Parse trash listing: include ALL items (even "deleted" ones, since they ARE trash)
        let entries = Self::parse_trash_xml(&xml);
        jotta_log(&format!("Trash: {} items", entries.len()));
        Ok(entries)
    }

    /// File restore URL: rclone's `cphash` on the original mountpoint object.
    /// The Trash listing is a virtual view; GET of the live path still 200s
    /// with a deleted tombstone. `?mv=` against `/Trash/name` 404s;
    /// `?restore=true` against `/Trash/name` and against the original path
    /// both 500 (Ehud, #397).
    fn restore_cphash_url(&self, from_in_trash: &str) -> String {
        format!("{}?cphash=true", self.jfs_url(from_in_trash))
    }

    /// POST `?cphash=true` on the original mountpoint object with the
    /// TOMBSTONE's size/md5/timestamps — rclone's restore of a trashed file.
    /// Callers must source the revision from the tombstone, never from a live
    /// object at the same path (F-652-3). A 2xx answer carries the revived
    /// `<file>` (no `deleted` attribute), so success here is server-confirmed.
    async fn post_cphash_restore(
        &mut self,
        clean_path: &str,
        size: u64,
        md5: &str,
        created: &str,
        modified: &str,
    ) -> Result<(), ProviderError> {
        let url = self.restore_cphash_url(clean_path);
        jotta_log(&format!(
            "Restoring from trash via cphash: {} (size={} md5={})",
            url, size, md5
        ));
        self.refresh_if_needed().await?;
        let request = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_LENGTH, "0")
            .header("JSize", size.to_string())
            .header("JMd5", md5)
            .header("JCreated", created)
            .header("JModified", modified)
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Build restore request failed: {}", e))
            })?;
        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Restore request failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "cphash restore of {} failed ({}): {}",
                clean_path,
                status,
                sanitize_api_error(&body)
            )));
        }
        Ok(())
    }

    /// Name of the first element in a JFS response (`file`, `folder`, …).
    /// A substring test cannot be used: a folder listing contains `<files>`
    /// and a `<file>` child per entry, which made non-empty folders take the
    /// cphash branch with a child's JSize/JMd5.
    fn jfs_root_element(xml: &str) -> Option<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    return Some(String::from_utf8_lossy(e.name().as_ref()).to_string());
                }
                Ok(Event::Eof) | Err(_) => return None,
                _ => {}
            }
            buf.clear();
        }
    }

    /// True when the root `<file>` carries a non-empty `deleted` attribute.
    /// A live object at the original path 200s without it; using that
    /// object's size/md5 for cphash would restore the wrong bytes (F-652-3).
    fn jfs_root_file_is_tombstone(xml: &str) -> bool {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.name().as_ref() != b"file" {
                        return false;
                    }
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"deleted"
                            && !super::xml_text::attr_value(&attr).is_empty()
                        {
                            return true;
                        }
                    }
                    return false;
                }
                Ok(Event::Eof) | Err(_) => return false,
                _ => {}
            }
            buf.clear();
        }
    }

    /// Relative path under the live mountpoint: strip
    /// `/{user}/{device}/{mount}` from the trash `<abspath>` (the original
    /// parent) and append the entry name. Root-level items stay `/{name}`.
    fn trash_relative_path(abspath: &str, name: &str) -> String {
        let parts: Vec<&str> = abspath
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        // 0 user, 1 device, 2 mountpoint, 3… original parent under that mount.
        let parent = if parts.len() > 3 {
            parts[3..].join("/")
        } else {
            String::new()
        };
        if parent.is_empty() {
            format!("/{name}")
        } else {
            format!("/{parent}/{name}")
        }
    }

    /// size, md5, created, modified from a JFS `<file>` tombstone.
    /// Only the root `<file>`'s `currentRevision` counts: a folder listing
    /// carries child files whose revisions must not leak into cphash.
    fn parse_jfs_file_revision(xml: &str) -> Option<(u64, String, String, String)> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut depth: u32 = 0;
        let mut root_is_file = false;
        let mut in_revision = false;
        let mut tag = String::new();
        let mut size: u64 = 0;
        let mut md5 = String::new();
        let mut created = String::new();
        let mut modified = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    depth += 1;
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if depth == 1 {
                        root_is_file = name == "file";
                    }
                    if root_is_file && depth == 2 && name == "currentRevision" {
                        in_revision = true;
                    }
                    tag = name;
                }
                Ok(Event::End(ref e)) => {
                    if e.name().as_ref() == b"currentRevision" {
                        in_revision = false;
                    }
                    depth = depth.saturating_sub(1);
                    tag.clear();
                }
                Ok(Event::Text(ref e)) if in_revision => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    match tag.as_str() {
                        "size" => size = text.parse().unwrap_or(0),
                        "md5" => md5 = text,
                        "created" if created.is_empty() => created = text,
                        "modified" if modified.is_empty() => modified = text,
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        if !root_is_file || md5.is_empty() {
            return None;
        }
        if created.is_empty() {
            created = modified.clone();
        }
        if modified.is_empty() {
            modified = created.clone();
        }
        Some((size, md5, created, modified))
    }

    /// Parse a folder listing (live or tombstoned) into tombstone-aware
    /// children. Unlike `parse_folder_xml` nothing is filtered out: the
    /// restore walk needs the tombstoned entries, each with its own
    /// `currentRevision` — reading size/md5 from here is what keeps cphash
    /// away from a live object at the same path (F-652-3).
    fn parse_folder_tombstone_children(xml: &str) -> Vec<TombstoneChild> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut children = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut depth: u32 = 0;
        let mut in_file = false;
        let mut in_revision = false;
        let mut tag = String::new();
        let mut cur_name = String::new();
        let mut cur_deleted = false;
        let mut cur_size: u64 = 0;
        let mut cur_md5 = String::new();
        let mut cur_created = String::new();
        let mut cur_modified = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    depth += 1;
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    // Children live exactly one level below <folders>/<files>.
                    if depth == 3 && name == "folder" {
                        let mut fname = String::new();
                        let mut deleted = false;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                fname = crate::restricted_chars::decode_leaf(
                                    ProviderType::Jottacloud,
                                    &super::xml_text::attr_value(&attr),
                                );
                            }
                            if attr.key.as_ref() == b"deleted"
                                && !super::xml_text::attr_value(&attr).is_empty()
                            {
                                deleted = true;
                            }
                        }
                        if !fname.is_empty() {
                            children.push(TombstoneChild::Folder {
                                name: fname,
                                deleted,
                            });
                        }
                    } else if depth == 3 && name == "file" {
                        in_file = true;
                        cur_name.clear();
                        cur_deleted = false;
                        cur_size = 0;
                        cur_md5.clear();
                        cur_created.clear();
                        cur_modified.clear();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                cur_name = crate::restricted_chars::decode_leaf(
                                    ProviderType::Jottacloud,
                                    &super::xml_text::attr_value(&attr),
                                );
                            }
                            if attr.key.as_ref() == b"deleted"
                                && !super::xml_text::attr_value(&attr).is_empty()
                            {
                                cur_deleted = true;
                            }
                        }
                    } else if in_file && depth == 4 && name == "currentRevision" {
                        in_revision = true;
                    }
                    tag = name;
                }
                Ok(Event::Empty(ref e)) => {
                    // Live folders arrive as `<folder name="x"/>`.
                    if depth == 2 && e.name().as_ref() == b"folder" {
                        let mut fname = String::new();
                        let mut deleted = false;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                fname = crate::restricted_chars::decode_leaf(
                                    ProviderType::Jottacloud,
                                    &super::xml_text::attr_value(&attr),
                                );
                            }
                            if attr.key.as_ref() == b"deleted"
                                && !super::xml_text::attr_value(&attr).is_empty()
                            {
                                deleted = true;
                            }
                        }
                        if !fname.is_empty() {
                            children.push(TombstoneChild::Folder {
                                name: fname,
                                deleted,
                            });
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    match e.name().as_ref() {
                        b"currentRevision" => in_revision = false,
                        b"file" if in_file => {
                            in_file = false;
                            in_revision = false;
                            if !cur_name.is_empty() {
                                let revision = if cur_md5.is_empty() {
                                    None
                                } else {
                                    let c = if cur_created.is_empty() {
                                        cur_modified.clone()
                                    } else {
                                        cur_created.clone()
                                    };
                                    let m = if cur_modified.is_empty() {
                                        cur_created.clone()
                                    } else {
                                        cur_modified.clone()
                                    };
                                    Some((cur_size, cur_md5.clone(), c, m))
                                };
                                children.push(TombstoneChild::File {
                                    name: cur_name.clone(),
                                    deleted: cur_deleted,
                                    revision,
                                });
                            }
                        }
                        _ => {}
                    }
                    depth = depth.saturating_sub(1);
                    tag.clear();
                }
                Ok(Event::Text(ref e)) if in_revision => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    match tag.as_str() {
                        "size" => cur_size = text.parse().unwrap_or(0),
                        "md5" => cur_md5 = text,
                        "created" if cur_created.is_empty() => cur_created = text,
                        "modified" if cur_modified.is_empty() => cur_modified = text,
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        children
    }

    /// True when the root `<folder>` carries a non-empty `deleted` attribute.
    fn jfs_root_folder_is_tombstone(xml: &str) -> bool {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    if e.name().as_ref() != b"folder" {
                        return false;
                    }
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"deleted"
                            && !super::xml_text::attr_value(&attr).is_empty()
                        {
                            return true;
                        }
                    }
                    return false;
                }
                Ok(Event::Eof) | Err(_) => return false,
                _ => {}
            }
            buf.clear();
        }
    }

    /// Composed folder restore (#397): no JFS verb restores a directory —
    /// `?restore=true` 500s on both the Trash view and the original path, and
    /// `?mv=`/`?mvDir=` out of Trash 404 because the view is virtual. What
    /// works is reviving each descendant with the primitives that already
    /// carry file restore: walk the original-path tombstone (JFS still serves
    /// the deleted tree, children `deleted`-stamped, revisions intact),
    /// cphash every tombstoned file — JFS revives the ancestor chain along
    /// with it — and mkDir only the directories that stay tombstoned because
    /// no file lives beneath them.
    ///
    /// Partial failure is reported, never hidden: every entry is attempted,
    /// `failed` lists what did not come back, and the caller gets `Err` with
    /// the confirmed counts. Re-running is safe: live children are counted as
    /// `files_already_present`, not restored again, and cphash/mkDir are
    /// server-side no-ops on live objects.
    async fn restore_folder_from_trash(
        &mut self,
        root: &str,
    ) -> Result<TrashRestoreReport, ProviderError> {
        let mut report = TrashRestoreReport::default();
        // dir path -> tombstoned?
        let mut dirs: Vec<(String, bool)> = Vec::new();
        let mut files: Vec<WalkedFile> = Vec::new();

        // Breadth-first so parents are scanned before their children.
        let mut queue = vec![root.to_string()];
        let mut cursor = 0;
        while cursor < queue.len() {
            let dir = queue[cursor].clone();
            cursor += 1;
            let url = self.jfs_url(&dir);
            // A listing that cannot be fetched is a failed entry, not an
            // abort: the rest of the tree is still restorable, and the
            // report must carry everything that did not come back.
            let resp = match self.get_with_retry(&url).await {
                Ok(resp) => resp,
                Err(e) => {
                    report
                        .failed
                        .push(format!("/{}: tombstone listing failed: {}", dir, e));
                    continue;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                report.failed.push(format!(
                    "/{}: tombstone listing failed ({}): {}",
                    dir,
                    status,
                    sanitize_api_error(&body)
                ));
                continue;
            }
            let xml = resp.text().await.unwrap_or_default();
            let dir_deleted = Self::jfs_root_folder_is_tombstone(&xml);
            dirs.push((dir.clone(), dir_deleted));
            for child in Self::parse_folder_tombstone_children(&xml) {
                match child {
                    // The dir's own GET (when the walk reaches it) records
                    // its tombstone state; recording it here too would count
                    // it twice in the empty-dir pass.
                    TombstoneChild::Folder { name, .. } => {
                        queue.push(format!("{dir}/{name}"));
                    }
                    TombstoneChild::File {
                        name,
                        deleted,
                        revision,
                    } => {
                        files.push(WalkedFile {
                            path: format!("{dir}/{name}"),
                            deleted,
                            revision,
                        });
                    }
                }
            }
        }

        // Files first: each cphash revives its ancestor chain, so directory
        // creation must not run ahead of it.
        for file in &files {
            if !file.deleted {
                // Already live: not our work, never counted as restored.
                report.files_already_present += 1;
                continue;
            }
            let Some((size, md5, created, modified)) = &file.revision else {
                report
                    .failed
                    .push(format!("/{}: tombstone carries no md5/size", file.path));
                continue;
            };
            match self
                .post_cphash_restore(&file.path, *size, md5, created, modified)
                .await
            {
                Ok(()) => report.files_restored += 1,
                Err(e) => report.failed.push(format!("/{}: {e}", file.path)),
            }
        }

        // Directories still tombstoned after the file pass are the ones no
        // file could revive (empty subtrees). mkDir them top-down; the answer
        // must come back without `deleted`, otherwise the dir is a failure,
        // not a silent success.
        let tombstoned_files: Vec<&str> = files
            .iter()
            .filter(|f| f.deleted)
            .map(|f| f.path.as_str())
            .collect();
        let mut empty_dirs: Vec<&String> = dirs
            .iter()
            .filter(|(d, deleted)| {
                *deleted
                    && !tombstoned_files
                        .iter()
                        .any(|f| f.starts_with(&format!("{d}/")))
            })
            .map(|(d, _)| d)
            .collect();
        empty_dirs.sort_by_key(|d| d.matches('/').count());
        for dir in empty_dirs {
            let url = format!("{}?mkDir=true", self.jfs_url(dir));
            match self.post_command_with_retry(&url).await {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.unwrap_or_default();
                    if Self::jfs_root_folder_is_tombstone(&body) {
                        report.failed.push(format!(
                            "/{dir}: mkDir answered 2xx but the folder is still tombstoned"
                        ));
                    } else {
                        report.dirs_restored += 1;
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    report.failed.push(format!(
                        "/{dir}: mkDir failed ({}): {}",
                        status,
                        sanitize_api_error(&body)
                    ));
                }
                Err(e) => report.failed.push(format!("/{dir}: mkDir failed: {e}")),
            }
        }

        if !report.failed.is_empty() {
            return Err(ProviderError::ServerError(format!(
                "Folder restore of /{} incomplete: {} file(s) restored, {} already present, {} empty folder(s) revived; {} failure(s): {}",
                root,
                report.files_restored,
                report.files_already_present,
                report.dirs_restored,
                report.failed.len(),
                report.failed.join("; ")
            )));
        }
        Ok(report)
    }

    /// JFS destination path for a rename that stays inside Trash.
    fn rename_in_trash_dest_jfs(&self, to_clean: &str) -> String {
        let encoded_to: String = to_clean
            .split('/')
            .map(|s| crate::restricted_chars::encode_leaf(ProviderType::Jottacloud, s))
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "/{}/{}/{}/{}",
            self.username, self.config.device, TRASH_MOUNTPOINT, encoded_to
        )
    }

    /// `(file_mv_url, dir_mvdir_url)` for a move whose source lives in Trash.
    /// Directory form has a trailing slash on the source, matching `rename()`
    /// and rclone's Jottacloud backend; without it `mvDir` 404s (#397).
    fn trash_move_urls(&self, from_in_trash: &str, dest_jfs: &str) -> (String, String) {
        let dest_q = urlencoding::encode(dest_jfs);
        let src = self.trash_url(from_in_trash);
        let file_url = format!("{}?mv={}", src, dest_q);
        let dir_src = if src.ends_with('/') {
            src.clone()
        } else {
            format!("{src}/")
        };
        let dir_url = format!("{dir_src}?mvDir={dest_q}");
        (file_url, dir_url)
    }

    /// POST `?mv=`, and only on HTTP 404 retry `?mvDir=` with the trailing
    /// slash. Any other failure is the real error; retrying 500s as a
    /// directory move hid that.
    async fn post_trash_move(
        &mut self,
        file_url: &str,
        dir_url: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let resp = self.post_command_with_retry(file_url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            jotta_log(&format!(
                "Trash file-move 404, retrying as dir: {}",
                dir_url
            ));
            return self.post_command_with_retry(dir_url).await;
        }
        Ok(resp)
    }

    /// Restore an item from trash to its original location.
    ///
    /// Files: GET the original-mountpoint tombstone, then POST `?cphash=true`
    /// with JSize/JMd5/JCreated/JModified, which is rclone's restore of a trashed
    /// destination. `?restore=true` 500s on both the Trash view and the
    /// original path; `?mv=` against `/Trash/name` 404s.
    /// Folders: no JFS verb restores a directory (same probes, #397), so the
    /// restore is composed — see `restore_folder_from_trash`.
    ///
    /// The report counts only server-confirmed work; an incomplete folder
    /// restore is an `Err` carrying the confirmed counts, never a quiet
    /// success.
    pub async fn restore_from_trash(
        &mut self,
        path: &str,
    ) -> Result<TrashRestoreReport, ProviderError> {
        let clean = path.trim_start_matches('/');
        let src = self.jfs_url(clean);
        let get = self.get_with_retry(&src).await?;
        if !get.status().is_success() {
            let status = get.status();
            let body = get.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore GET tombstone failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }
        let xml = get.text().await.unwrap_or_default();
        let looks_like_file = Self::jfs_root_element(&xml).as_deref() == Some("file");

        if looks_like_file {
            if !Self::jfs_root_file_is_tombstone(&xml) {
                return Err(ProviderError::ServerError(format!(
                    "Restore refused for {}: original path is a live object, not a deleted tombstone",
                    clean
                )));
            }
            let (size, md5, created, modified) =
                Self::parse_jfs_file_revision(&xml).ok_or_else(|| {
                    ProviderError::ServerError(format!(
                        "Restore tombstone for {} has no md5/size",
                        clean
                    ))
                })?;
            self.post_cphash_restore(clean, size, &md5, &created, &modified)
                .await?;
            jotta_log(&format!("Restored from trash: {}", clean));
            return Ok(TrashRestoreReport {
                files_restored: 1,
                ..Default::default()
            });
        }

        let report = self.restore_folder_from_trash(clean).await?;
        jotta_log(&format!(
            "Restored folder from trash: {} ({} file(s), {} already present, {} empty dir(s))",
            clean, report.files_restored, report.files_already_present, report.dirs_restored
        ));
        Ok(report)
    }

    /// Rename an item that already lives in Trash.
    ///
    /// Regular `rename` posts `?mv=` against the live mountpoint URL, so a
    /// folder that only exists in Trash 404s (#397). Keep the destination
    /// inside the Trash mountpoint.
    pub async fn rename_in_trash(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_clean = from.trim_start_matches('/');
        let to_clean = to.trim_start_matches('/');
        let dest = self.rename_in_trash_dest_jfs(to_clean);
        let (file_url, dir_url) = self.trash_move_urls(from_clean, &dest);
        jotta_log(&format!("Rename in trash: {}", file_url));

        let resp = self.post_trash_move(&file_url, &dir_url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Rename in trash {} → {} failed ({}): {}",
                from_clean,
                to_clean,
                status,
                sanitize_api_error(&body)
            )));
        }
        Ok(())
    }

    /// Permanently delete an item from trash.
    /// POST /jfs/{username}/{device}/Trash/{path}?rm=true (file) or `?rmDir=true` (folder)
    /// Hard-delete form for a trashed FOLDER: `?rmDir=true` on the ORIGINAL
    /// path, not on the Trash view. Measured on 2026-09-01 against a real
    /// account: the Trash view answers 404 to `rm`/`rmDir` because folders are
    /// not addressable there, while this form on the tombstone answers 200 and
    /// the folder is gone from the trash on re-listing. It is the same request
    /// rclone's hard delete sends at a live folder, and it purges a tombstone
    /// as well, which is why `trashed_folder_purge_decision` must run first.
    fn purge_trashed_dir_url(&self, clean: &str) -> String {
        format!(
            "{}?{}=true",
            self.jfs_url(clean),
            Self::delete_param(true, true)
        )
    }

    /// Whether the object at the ORIGINAL path may be purged with the
    /// hard-delete form. Only a root `<folder>` carrying the `deleted` stamp
    /// qualifies: the same POST at a LIVE folder destroys live data, and a
    /// trash manager must never be able to do that through a stale listing or
    /// a name that was re-created after being trashed.
    fn trashed_folder_purge_decision(xml: &str) -> Result<(), &'static str> {
        match Self::jfs_root_element(xml).as_deref() {
            Some("folder") if Self::jfs_root_folder_is_tombstone(xml) => Ok(()),
            Some("folder") => Err(
                "the original path holds a LIVE folder, not a trash tombstone; hard-deleting it from the trash manager would destroy live data",
            ),
            Some("file") => Err("the original path is a file and the file form already failed"),
            _ => Err("the original path did not answer with a folder"),
        }
    }

    /// The Trash view can return 404 for files too (#397 follow-up). The
    /// original-path fallback must never hard-delete a re-created live file.
    fn trashed_entry_purge_param(xml: &str) -> Result<&'static str, &'static str> {
        if Self::jfs_root_element(xml).as_deref() == Some("file") {
            return if Self::jfs_root_file_is_tombstone(xml) {
                Ok("rm")
            } else {
                Err("the original path holds a LIVE file, not a trash tombstone")
            };
        }
        Self::trashed_folder_purge_decision(xml).map(|()| "rmDir")
    }

    /// Permanently delete one trashed entry. Try the Trash view first; it can
    /// return 404 for both files and folders (#397 follow-up). Fall back to
    /// `rm` (file) or `rmDir` (folder) on the original path only after reading
    /// it back and confirming the root object is a deleted tombstone.
    ///
    /// Declared limit: the tombstone check and the delete are two requests,
    /// and JFS offers no conditional delete (no revision or ETag precondition
    /// on `rm`/`rmDir`), so a session that restores or recreates the same name
    /// between them would see its live object purged. The window is one
    /// round trip, the same window every list-then-purge trash manager has
    /// on this provider, and it cannot be closed from the client side. The
    /// check exists so that the ordinary case, a live object that was never
    /// the one selected, is refused rather than deleted.
    pub async fn permanent_delete_from_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        let clean = path.trim_start_matches('/');
        let url = format!(
            "{}?{}=true",
            self.trash_url(clean),
            Self::delete_param(false, true)
        );
        jotta_log(&format!("Permanent delete from trash: {}", url));

        let resp = self.post_command_with_retry(&url).await?;
        if resp.status().is_success() {
            jotta_log(&format!("Permanently deleted from trash: {}", clean));
            return Ok(());
        }
        let file_status = resp.status();
        let file_body = resp.text().await.unwrap_or_default();

        // Read the ORIGINAL path and require a tombstone of the actual type.
        let original = self.jfs_url(clean);
        let get = self.get_with_retry(&original).await?;
        if get.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound(format!(
                "{} is neither in the trash ({}: {}) nor at its original path",
                clean,
                file_status,
                sanitize_api_error(&file_body)
            )));
        }
        if !get.status().is_success() {
            let status = get.status();
            let body = get.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Permanent delete failed: reading the original path of {} answered {}: {}",
                clean,
                status,
                sanitize_api_error(&body)
            )));
        }
        let xml = get.text().await.unwrap_or_default();
        let param = Self::trashed_entry_purge_param(&xml).map_err(|reason| {
            ProviderError::ServerError(format!(
                "Permanent delete refused for {}: {}",
                clean, reason
            ))
        })?;

        let purge_url = if param == "rmDir" {
            self.purge_trashed_dir_url(clean)
        } else {
            format!("{original}?{param}=true")
        };
        jotta_log(&format!(
            "Purging trash tombstone via original path: {}",
            purge_url
        ));
        let resp = self.post_command_with_retry(&purge_url).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Permanent delete failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }
        jotta_log(&format!("Permanently deleted trash tombstone: {}", clean));
        Ok(())
    }

    /// The counts `purge_trash` reports, or a parse error. A malformed body
    /// must not read as "0 files, 0 folders": that number is shown to the user
    /// as what the server confirmed, and a silent zero would say the trash was
    /// already empty when the answer was in fact unreadable.
    fn parse_purge_trash_counts(body: &str) -> Result<(u64, u64), ProviderError> {
        let counts: serde_json::Value = serde_json::from_str(body).map_err(|e| {
            ProviderError::ParseError(format!(
                "purge_trash answered 200 with a body that is not JSON ({}): {}",
                e,
                sanitize_api_error(body)
            ))
        })?;
        let field = |key: &str| -> Result<u64, ProviderError> {
            counts.get(key).and_then(|v| v.as_u64()).ok_or_else(|| {
                ProviderError::ParseError(format!(
                    "purge_trash answered without a numeric `{}`: {}",
                    key,
                    sanitize_api_error(body)
                ))
            })
        };
        Ok((field("files")?, field("folders")?))
    }

    /// Empty the whole trash: `POST {API_BASE}/files/v1/purge_trash`, the
    /// request behind rclone's `cleanup`. Measured on 2026-09-01: 200 with
    /// `{"files":N,"folders":M}` and the trash re-lists empty. Returns the
    /// counts the server reports, so the caller shows what was confirmed.
    pub async fn empty_trash(&mut self) -> Result<(u64, u64), ProviderError> {
        self.refresh_if_needed().await?;
        let url = format!("{}/files/v1/purge_trash", API_BASE);
        jotta_log(&format!("Emptying trash: {}", url));
        let request = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_LENGTH, "0")
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Build purge_trash request failed: {}", e))
            })?;
        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("purge_trash failed: {}", e)))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "Empty trash failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }
        let (files, folders) = Self::parse_purge_trash_counts(&body)?;
        jotta_log(&format!(
            "Trash emptied: {} file(s), {} folder(s) purged",
            files, folders
        ));
        Ok((files, folders))
    }

    /// Whether this provider supports trash operations.
    #[allow(dead_code)]
    pub fn supports_trash(&self) -> bool {
        true
    }

    /// Parse trash XML listing. Unlike regular listing, we include all items
    /// regardless of deleted status (they ARE trash items).
    fn parse_trash_xml(xml: &str) -> Vec<RemoteEntry> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        // Names arrive as attributes. `<abspath>` is Text + GeneralRef and
        // is accumulated (then trimmed once) rather than assigned per chunk.
        //
        // Trimming is left OFF here on purpose: quick-xml trims every Text
        // EVENT, not the node, so an entity splits `Photos &amp; Videos` into
        // three events whose own edges carry the spaces. Trimming per event
        // welds the pieces into `Photos&Videos`, and the single trim applied
        // when the path is derived cannot put back what was already dropped.
        // The scalars that need it (`size`, `state`, `modified`) trim
        // themselves below, where a whitespace-only event is also harmless
        // because it never matches their tag.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut depth: u32 = 0;
        let mut root_depth: Option<u32> = None;
        let mut in_folders_section = false;
        let mut folders_section_depth: u32 = 0;
        let mut child_folder_depth: Option<u32> = None;

        let mut in_file = false;
        let mut in_revision = false;
        let mut current_name = String::new();
        let mut current_size: u64 = 0;
        let mut current_modified = String::new();
        let mut current_deleted_at = String::new();
        let mut current_abspath = String::new();
        let mut current_state = String::new();
        let mut current_tag = String::new();
        let mut pending_folder_name = String::new();
        let mut pending_folder_deleted = String::new();
        let mut pending_folder_abspath = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    depth += 1;

                    match tag.as_str() {
                        "folder" | "mountPoint" | "trashcan" if root_depth.is_none() => {
                            root_depth = Some(depth);
                        }
                        "folders"
                            if root_depth == Some(depth - 1) && child_folder_depth.is_none() =>
                        {
                            in_folders_section = true;
                            folders_section_depth = depth;
                        }
                        "folder"
                            if child_folder_depth.is_none()
                                && (in_folders_section
                                    || (root_depth.is_some()
                                        && depth == root_depth.unwrap() + 1)) =>
                        {
                            pending_folder_name.clear();
                            pending_folder_deleted.clear();
                            pending_folder_abspath.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    pending_folder_name = crate::restricted_chars::decode_leaf(
                                        ProviderType::Jottacloud,
                                        &super::xml_text::attr_value(&attr),
                                    );
                                }
                                if attr.key.as_ref() == b"deleted" {
                                    pending_folder_deleted =
                                        Self::parse_jotta_time(&super::xml_text::attr_value(&attr));
                                }
                            }
                            child_folder_depth = Some(depth);
                        }
                        "file" if !in_file && child_folder_depth.is_none() => {
                            in_file = true;
                            current_name.clear();
                            current_size = 0;
                            current_modified.clear();
                            current_deleted_at.clear();
                            current_abspath.clear();
                            current_state.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    current_name = crate::restricted_chars::decode_leaf(
                                        ProviderType::Jottacloud,
                                        &super::xml_text::attr_value(&attr),
                                    );
                                }
                                if attr.key.as_ref() == b"deleted" {
                                    current_deleted_at =
                                        Self::parse_jotta_time(&super::xml_text::attr_value(&attr));
                                }
                            }
                        }
                        "currentRevision" if in_file => {
                            in_revision = true;
                        }
                        _ => {
                            current_tag = tag;
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if tag == "folder"
                        && child_folder_depth.is_none()
                        && (in_folders_section
                            || (root_depth.is_some() && depth == root_depth.unwrap()))
                    {
                        let mut name = String::new();
                        let mut deleted_at = String::new();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                name = crate::restricted_chars::decode_leaf(
                                    ProviderType::Jottacloud,
                                    &super::xml_text::attr_value(&attr),
                                );
                            }
                            if attr.key.as_ref() == b"deleted" {
                                deleted_at =
                                    Self::parse_jotta_time(&super::xml_text::attr_value(&attr));
                            }
                        }
                        if !name.is_empty() {
                            entries.push(RemoteEntry {
                                name: name.clone(),
                                path: Self::trash_relative_path("", &name),
                                is_dir: true,
                                size: 0,
                                modified: if deleted_at.is_empty() {
                                    None
                                } else {
                                    Some(deleted_at)
                                },
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                metadata: HashMap::new(),
                                mime_type: None,
                            });
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "folders" if in_folders_section && depth == folders_section_depth => {
                            in_folders_section = false;
                        }
                        "folder" if child_folder_depth == Some(depth) => {
                            if !pending_folder_name.is_empty() {
                                entries.push(RemoteEntry {
                                    name: pending_folder_name.clone(),
                                    path: Self::trash_relative_path(
                                        pending_folder_abspath.trim(),
                                        &pending_folder_name,
                                    ),
                                    is_dir: true,
                                    size: 0,
                                    modified: if pending_folder_deleted.is_empty() {
                                        None
                                    } else {
                                        Some(pending_folder_deleted.clone())
                                    },
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                            pending_folder_name.clear();
                            pending_folder_deleted.clear();
                            pending_folder_abspath.clear();
                            child_folder_depth = None;
                        }
                        "file" if in_file => {
                            in_file = false;
                            in_revision = false;
                            // In trash, show all files (not just COMPLETED)
                            if !current_name.is_empty() {
                                let deleted_or_modified = if !current_deleted_at.is_empty() {
                                    Some(current_deleted_at.clone())
                                } else if current_modified.is_empty() {
                                    None
                                } else {
                                    Some(current_modified.clone())
                                };
                                entries.push(RemoteEntry {
                                    name: current_name.clone(),
                                    path: Self::trash_relative_path(
                                        current_abspath.trim(),
                                        &current_name,
                                    ),
                                    is_dir: false,
                                    size: current_size,
                                    modified: deleted_or_modified,
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                        }
                        "currentRevision" => {
                            in_revision = false;
                        }
                        _ => {}
                    }
                    depth = depth.saturating_sub(1);
                    current_tag.clear();
                }
                Ok(Event::Text(ref e)) => {
                    if current_tag.as_str() == "abspath" {
                        // Accumulate, do not assign: an `<abspath>` with an
                        // entity arrives as Text + GeneralRef + Text, and
                        // assigning the last chunk silently truncates the
                        // original parent (the shape we just closed).
                        let chunk = String::from_utf8_lossy(e.as_ref());
                        if in_file && !in_revision {
                            current_abspath.push_str(&chunk);
                        } else if child_folder_depth == Some(depth.saturating_sub(1)) {
                            pending_folder_abspath.push_str(&chunk);
                        }
                    } else if in_revision && in_file {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        match current_tag.as_str() {
                            "size" => {
                                current_size = text.parse().unwrap_or(0);
                            }
                            "state" => {
                                current_state = text;
                            }
                            "modified" | "updated" if current_modified.is_empty() => {
                                current_modified = Self::parse_jotta_time(&text);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::GeneralRef(ref e)) if current_tag.as_str() == "abspath" => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if in_file && !in_revision {
                            current_abspath.push_str(&ch);
                        } else if child_folder_depth == Some(depth.saturating_sub(1)) {
                            pending_folder_abspath.push_str(&ch);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> JottacloudProvider {
        let config = JottacloudConfig {
            login_token: SecretString::from("test-token".to_string()),
            device: "Jotta".to_string(),
            mountpoint: "Archive".to_string(),
            initial_path: None,
        };
        let mut p = JottacloudProvider::new(config);
        p.username = "user123".to_string();
        p
    }

    #[test]
    fn delete_param_never_sends_the_file_form_at_a_folder() {
        // #397: the file form fired at a folder took it out of the account
        // without ever filling the recycle bin.
        assert_eq!(JottacloudProvider::delete_param(false, false), "dl");
        assert_eq!(JottacloudProvider::delete_param(true, false), "dlDir");
        assert_eq!(JottacloudProvider::delete_param(false, true), "rm");
        assert_eq!(JottacloudProvider::delete_param(true, true), "rmDir");
    }

    #[test]
    fn normalize_path_trims_and_normalizes_separators() {
        assert_eq!(JottacloudProvider::normalize_path(""), "/");
        assert_eq!(JottacloudProvider::normalize_path("/"), "/");
        assert_eq!(JottacloudProvider::normalize_path("   "), "/");
        assert_eq!(JottacloudProvider::normalize_path("foo"), "/foo");
        assert_eq!(JottacloudProvider::normalize_path("/foo/bar/"), "/foo/bar");
        assert_eq!(JottacloudProvider::normalize_path(r"\foo\bar"), "/foo/bar");
    }

    #[test]
    fn split_path_handles_root_nested_and_bare() {
        assert_eq!(
            JottacloudProvider::split_path("/file.txt"),
            ("/".to_string(), "file.txt".to_string())
        );
        assert_eq!(
            JottacloudProvider::split_path("/a/b/file"),
            ("/a/b".to_string(), "file".to_string())
        );
        assert_eq!(
            JottacloudProvider::split_path("/onlyone"),
            ("/".to_string(), "onlyone".to_string())
        );
    }

    #[test]
    fn jfs_url_builds_canonical_path_with_device_and_mountpoint() {
        let p = test_provider();
        assert_eq!(p.jfs_url(""), format!("{}/user123/Jotta/Archive", JFS_BASE));
        assert_eq!(
            p.jfs_url("/folder/file.txt"),
            format!("{}/user123/Jotta/Archive/folder/file.txt", JFS_BASE)
        );
        assert_eq!(
            p.jfs_url("no-slash/path"),
            format!("{}/user123/Jotta/Archive/no-slash/path", JFS_BASE)
        );
    }

    #[test]
    fn trash_url_keeps_the_device_segment() {
        // Trash is a mountpoint of the device, a sibling of Archive. Without the
        // device segment JFS reads `Trash` as a device name, 404s, and the bin
        // reads as empty however many items are in it (#397).
        let p = test_provider();
        assert_eq!(p.trash_url(""), format!("{}/user123/Jotta/Trash", JFS_BASE));
        assert_eq!(
            p.trash_url("/photo.jpg"),
            format!("{}/user123/Jotta/Trash/photo.jpg", JFS_BASE)
        );
        // The mountpoint the profile browses never appears in a trash URL.
        assert!(!p.trash_url("/photo.jpg").contains("Archive"));
    }

    #[test]
    fn restore_from_trash_posts_cphash_on_the_original_mountpoint() {
        // File restore is rclone cphash on the original object, not a
        // move out of the Trash view and not ?restore=true (both live-failed).
        let p = test_provider();
        let url = p.restore_cphash_url("ehud-397-0420.txt");
        assert!(
            url.contains("/Jotta/Archive/ehud-397-0420.txt?cphash=true"),
            "cphash: {url}"
        );
        assert!(
            !url.contains("/Trash/"),
            "must not post at Trash view: {url}"
        );
        assert!(!url.contains("?mv="));
        assert!(!url.contains("?restore="));
        let xml = r#"<file name="ehud-397-0420.txt" deleted="2026-08-29-T13:45:33Z">
            <currentRevision>
                <size>28</size>
                <md5>9a6924f78982f7e759f371f08fb5d57e</md5>
                <created>2026-08-29-T13:44:54Z</created>
                <modified>2026-08-29-T13:44:54Z</modified>
            </currentRevision>
        </file>"#;
        let (size, md5, created, modified) =
            JottacloudProvider::parse_jfs_file_revision(xml).expect("revision");
        assert_eq!(size, 28);
        assert_eq!(md5, "9a6924f78982f7e759f371f08fb5d57e");
        assert!(created.contains("13:44:54"), "{created}");
        assert!(modified.contains("13:44:54"), "{modified}");

        let folder_xml = r#"<folder name="aeroftp-scratch-397">
            <files>
                <file name="child.txt">
                    <currentRevision>
                        <size>99</size>
                        <md5>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</md5>
                    </currentRevision>
                </file>
            </files>
        </folder>"#;
        assert_eq!(
            JottacloudProvider::jfs_root_element(folder_xml).as_deref(),
            Some("folder")
        );
        assert!(
            JottacloudProvider::parse_jfs_file_revision(folder_xml).is_none(),
            "child revision must not leak into a folder restore"
        );
        assert_eq!(
            JottacloudProvider::jfs_root_element(xml).as_deref(),
            Some("file")
        );
        assert!(
            JottacloudProvider::jfs_root_file_is_tombstone(xml),
            "Ehud tombstone carries deleted="
        );
        let live = r#"<file name="ehud-397-0420.txt">
            <currentRevision>
                <size>99</size>
                <md5>bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</md5>
                <created>2026-08-29-T14:00:00Z</created>
                <modified>2026-08-29-T14:00:00Z</modified>
            </currentRevision>
        </file>"#;
        assert!(
            !JottacloudProvider::jfs_root_file_is_tombstone(live),
            "live object at the original path must not feed cphash"
        );
        assert!(JottacloudProvider::parse_jfs_file_revision(live).is_some());
    }

    #[test]
    fn parse_trash_xml_uses_the_deleted_attribute_not_revision_mtime() {
        let xml = r#"<mountPoint name="Trash">
  <folders>
    <folder name="aeroftp-scratch-397" deleted="2026-08-29-T08:02:08Z" contextType="TRASH">
      <abspath>/user123/Jotta/Archive</abspath>
    </folder>
  </folders>
  <files>
    <file name="ehud-397-0420.txt" uuid="b363c7f6-53aa-49a3-a587-9cd346666a52" deleted="2026-08-29-T13:45:33Z" contextType="TRASH">
      <abspath>/user123/Jotta/Archive</abspath>
      <currentRevision>
        <modified>2026-08-29-T13:44:54Z</modified>
        <size>28</size>
      </currentRevision>
    </file>
  </files>
</mountPoint>"#;
        let entries = JottacloudProvider::parse_trash_xml(xml);
        assert_eq!(entries.len(), 2, "{entries:?}");
        let folder = entries.iter().find(|e| e.is_dir).expect("folder");
        assert_eq!(folder.name, "aeroftp-scratch-397");
        assert_eq!(folder.path, "/aeroftp-scratch-397");
        let folder_deleted = folder.modified.as_deref().expect("folder deleted-at");
        assert!(
            folder_deleted.contains("2026-08-29") && folder_deleted.contains("08:02"),
            "folder deleted attr, not missing: {folder_deleted}"
        );
        let file = entries.iter().find(|e| !e.is_dir).expect("file");
        assert_eq!(file.name, "ehud-397-0420.txt");
        assert_eq!(file.path, "/ehud-397-0420.txt");
        assert_eq!(file.size, 28);
        let file_deleted = file.modified.as_deref().expect("file deleted-at");
        assert!(
            file_deleted.contains("13:45"),
            "deleted attr wins over revision mtime 13:44: {file_deleted}"
        );
        assert!(!file_deleted.contains("13:44"), "{file_deleted}");
    }

    #[test]
    fn parse_trash_xml_keeps_the_original_parent_from_abspath() {
        let xml = r#"<mountPoint name="Trash">
  <folders>
    <folder name="vacation" deleted="2026-08-29-T08:02:08Z">
      <abspath>/user123/Jotta/Archive/photos</abspath>
    </folder>
  </folders>
  <files>
    <file name="img.jpg" deleted="2026-08-29-T13:45:33Z">
      <abspath>/user123/Jotta/Archive/photos/vacation</abspath>
      <currentRevision><size>1</size></currentRevision>
    </file>
  </files>
</mountPoint>"#;
        let entries = JottacloudProvider::parse_trash_xml(xml);
        let folder = entries.iter().find(|e| e.is_dir).expect("folder");
        assert_eq!(folder.path, "/photos/vacation");
        let file = entries.iter().find(|e| !e.is_dir).expect("file");
        assert_eq!(file.path, "/photos/vacation/img.jpg");
        assert_eq!(
            JottacloudProvider::trash_relative_path("/user123/Jotta/Archive", "root.txt"),
            "/root.txt"
        );
    }

    #[test]
    fn parse_trash_xml_accumulates_and_decodes_abspath_entities() {
        // `<abspath>…a&amp;b…</abspath>` is Text("…a") + GeneralRef("amp") +
        // Text("b…"). Assigning each Text overwrites the prefix and drops
        // the entity, so restore would hit `/b/x.txt` instead of `/a&b/x.txt`.
        let xml = r#"<mountPoint name="Trash">
  <folders>
    <folder name="vacation">
      <abspath>/user123/Jotta/Archive/photos/a&amp;b</abspath>
    </folder>
  </folders>
  <files>
    <file name="x.txt">
      <abspath>/user123/Jotta/Archive/photos/a&amp;b</abspath>
      <currentRevision><size>1</size></currentRevision>
    </file>
  </files>
</mountPoint>"#;
        let entries = JottacloudProvider::parse_trash_xml(xml);
        let folder = entries.iter().find(|e| e.is_dir).expect("folder");
        assert_eq!(folder.path, "/photos/a&b/vacation");
        let file = entries.iter().find(|e| !e.is_dir).expect("file");
        assert_eq!(file.path, "/photos/a&b/x.txt");
    }

    #[test]
    fn parse_trash_xml_keeps_spaces_around_an_entity_in_abspath() {
        // The accumulation is not enough on its own: quick-xml trims every
        // Text EVENT, not the node, so `Photos &amp; Videos` arrives as
        // Text("...Photos ") trimmed to "...Photos", GeneralRef -> "&",
        // Text(" Videos") trimmed to "Videos", and the pieces weld into
        // "Photos&Videos". The folder exists under its real name, so a
        // restore would GET a path that is not there.
        let xml = r#"<mountPoint name="Trash">
  <files>
    <file name="x.txt">
      <abspath>/user123/Jotta/Archive/Photos &amp; Videos</abspath>
      <currentRevision><size>1</size></currentRevision>
    </file>
  </files>
</mountPoint>"#;
        let entries = JottacloudProvider::parse_trash_xml(xml);
        let file = entries.iter().find(|e| !e.is_dir).expect("file");
        assert_eq!(file.path, "/Photos & Videos/x.txt");
    }

    #[test]
    fn rename_in_trash_builds_urls_that_stay_on_trash() {
        let p = test_provider();
        let dest = p.rename_in_trash_dest_jfs("renamed");
        assert_eq!(dest, "/user123/Jotta/Trash/renamed");
        let (file_url, dir_url) = p.trash_move_urls("old", &dest);
        assert!(file_url.contains("/Jotta/Trash/old?mv="), "{file_url}");
        assert!(dir_url.contains("/Jotta/Trash/old/?mvDir="), "{dir_url}");
        assert!(
            file_url.contains("Jotta%2FTrash%2Frenamed")
                || file_url.contains("%2Fuser123%2FJotta%2FTrash%2Frenamed"),
            "dest stays in Trash: {file_url}"
        );
        assert!(!file_url.contains("Archive"));
    }

    #[test]
    fn jfs_url_url_encodes_segments_for_special_chars() {
        // Names with `+`, `#`, `%`, ` `, `&` must be percent-encoded so the
        // server doesn't see `+` as space, `#` as fragment, etc.
        let p = test_provider();
        assert_eq!(
            p.jfs_url("/folder/a+b=c.txt"),
            format!("{}/user123/Jotta/Archive/folder/a%2Bb%3Dc.txt", JFS_BASE)
        );
        assert_eq!(
            p.jfs_url("/folder/cool#1.txt"),
            format!("{}/user123/Jotta/Archive/folder/cool%231.txt", JFS_BASE)
        );
        // `%` IS in Jottacloud's restricted table, so it is first reversibly
        // encoded to fullwidth `％` (U+FF05) and only then URL percent-encoded
        // (UTF-8 of U+FF05 = EF BC 85). `+`, `#`, `&`, ` ` above are NOT in the
        // table, so they are only URL-encoded.
        assert_eq!(
            p.jfs_url("/folder/100%.txt"),
            format!("{}/user123/Jotta/Archive/folder/100%EF%BC%85.txt", JFS_BASE)
        );
        // Slash separators must NOT be encoded; only segment contents.
        assert_eq!(
            p.jfs_url("/a&b/c d.txt"),
            format!("{}/user123/Jotta/Archive/a%26b/c%20d.txt", JFS_BASE)
        );
    }

    #[test]
    fn resolve_path_joins_relative_against_current_path() {
        let mut p = test_provider();
        p.current_path = "/pictures".to_string();
        assert_eq!(p.resolve_path("/abs"), "/abs");
        assert_eq!(p.resolve_path(""), "/pictures");
        assert_eq!(p.resolve_path("."), "/pictures");
        assert_eq!(p.resolve_path("child"), "/pictures/child");
    }

    #[test]
    fn parse_device_names_extracts_all_devices() {
        let xml = r#"<?xml version="1.0"?>
            <user>
              <devices>
                <device><name>Jotta</name></device>
                <device><name>LAPTOP-01</name></device>
                <device><name>iPhone</name></device>
              </devices>
            </user>"#;
        let names = JottacloudProvider::parse_device_names(xml);
        assert_eq!(names, vec!["Jotta", "LAPTOP-01", "iPhone"]);
    }

    /// LIVE-1 regression: a device/mountpoint name whose XML text is split
    /// around an entity must come back whole, not as trimmed fragments
    /// ("a &amp; b" must not become two names "a" and "b").
    #[test]
    fn parse_names_preserve_entity_adjacent_whitespace() {
        let xml = r#"<?xml version="1.0"?>
            <user>
              <devices>
                <device><name>Mum &amp; Dad</name></device>
              </devices>
            </user>"#;
        assert_eq!(
            JottacloudProvider::parse_device_names(xml),
            vec!["Mum & Dad"]
        );

        let xml_mp = r#"<device>
            <mountPoints><mountPoint><name>Tom &amp; Jerry</name></mountPoint></mountPoints>
        </device>"#;
        assert_eq!(
            JottacloudProvider::parse_mountpoint_names(xml_mp),
            vec!["Tom & Jerry"]
        );
    }

    #[test]
    fn parse_device_names_returns_empty_on_malformed_xml() {
        assert!(JottacloudProvider::parse_device_names("not-xml-at-all").is_empty());
        assert!(JottacloudProvider::parse_device_names("<empty/>").is_empty());
    }

    #[test]
    fn parse_mountpoint_names_extracts_name_attribute() {
        let xml = r#"<device>
            <mountPoint name="Archive"/>
            <mountPoint name="Sync"/>
            <mountPoint name="Shared"></mountPoint>
        </device>"#;
        let names = JottacloudProvider::parse_mountpoint_names(xml);
        assert_eq!(names, vec!["Archive", "Sync", "Shared"]);
    }

    #[test]
    fn parse_mountpoint_names_reads_the_live_child_element_shape() {
        // Captured from the live API (#397): the name is a child element, not an
        // attribute, and the previous attribute-only reader returned an empty
        // list here while the account clearly had mountpoints.
        let xml = r#"<device time="2026-07-27-T07:19:53Z">
          <name xml:space="preserve">Jotta</name>
          <user>4smczzcts33jjedcplkqrorq</user>
          <mountPoints>
            <mountPoint>
              <name xml:space="preserve">Archive</name>
              <size></size>
            </mountPoint>
            <mountPoint>
              <name xml:space="preserve">Sync</name>
            </mountPoint>
            <mountPoint>
              <name xml:space="preserve">Trash</name>
            </mountPoint>
          </mountPoints>
        </device>"#;
        let names = JottacloudProvider::parse_mountpoint_names(xml);
        assert_eq!(names, vec!["Archive", "Sync", "Trash"]);
        // The device's own <name> sits outside <mountPoints> and must not leak in.
        assert!(!names.contains(&"Jotta".to_string()));
    }

    #[test]
    fn parse_jotta_time_normalizes_nonstandard_format() {
        // Jottacloud uses "-T" separator: parse_jotta_time should strip it
        let out = JottacloudProvider::parse_jotta_time("2023-01-15-T10:30:45+0100");
        assert!(out.starts_with("2023-01-15"));
        // Fallback: string not parseable is returned cleaned (no "-T")
        let fallback = JottacloudProvider::parse_jotta_time("not-a-time");
        assert_eq!(fallback, "not-a-time");
    }

    #[test]
    fn parse_folder_xml_lists_folders_and_completed_files_skipping_deleted() {
        // Realistic JFS folder listing: a live subfolder, a deleted subfolder, a COMPLETED
        // file, an INCOMPLETE file (upload in progress), and a trashed file.
        let xml = r#"<folder name="Backup">
            <path>/Backup</path>
            <folders>
                <folder name="Photos"/>
                <folder name="Old" deleted="2020-01-01-T00:00:00Z"/>
            </folders>
            <files>
                <file name="report.pdf">
                    <currentRevision>
                        <number>1</number>
                        <state>COMPLETED</state>
                        <size>2048</size>
                        <md5>deadbeef</md5>
                        <mime>application/pdf</mime>
                        <modified>2024-03-04-T08:09:10Z</modified>
                    </currentRevision>
                </file>
                <file name="uploading.bin">
                    <currentRevision>
                        <state>INCOMPLETE</state>
                        <size>999</size>
                    </currentRevision>
                </file>
                <file name="trashed.txt">
                    <deleted>2020-01-01-T00:00:00Z</deleted>
                    <currentRevision>
                        <state>COMPLETED</state>
                        <size>5</size>
                    </currentRevision>
                </file>
                <file name="trashed-attr.jpg" uuid="88cec155-fbb0-41eb-82f1-341547d39526" deleted="2026-07-27-T07:23:35Z">
                    <currentRevision>
                        <state>COMPLETED</state>
                        <size>7</size>
                    </currentRevision>
                </file>
            </files>
        </folder>"#;

        let entries = JottacloudProvider::parse_folder_xml(xml, "/Backup");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Live folder present, deleted folder skipped.
        assert!(names.contains(&"Photos"), "live folder missing: {names:?}");
        assert!(!names.contains(&"Old"), "deleted folder must be skipped");
        // COMPLETED file present; INCOMPLETE and trashed files skipped.
        assert!(names.contains(&"report.pdf"), "completed file missing");
        assert!(
            !names.contains(&"uploading.bin"),
            "INCOMPLETE must be skipped"
        );
        assert!(
            !names.contains(&"trashed.txt"),
            "trashed file must be skipped"
        );
        // The live API tombstones a trashed file with a `deleted` *attribute*,
        // which only the folder branch used to honour (#397).
        assert!(
            !names.contains(&"trashed-attr.jpg"),
            "file tombstoned by attribute must be skipped"
        );

        let folder = entries.iter().find(|e| e.name == "Photos").unwrap();
        assert!(folder.is_dir);
        assert_eq!(folder.path, "/Backup/Photos");
        assert_eq!(folder.size, 0);

        let file = entries.iter().find(|e| e.name == "report.pdf").unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.path, "/Backup/report.pdf");
        assert_eq!(file.size, 2048);
        assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(
            file.metadata.get("md5").map(|s| s.as_str()),
            Some("deadbeef")
        );
        assert!(file.modified.is_some());
    }

    #[test]
    fn parse_folder_xml_joins_root_base_path_and_tolerates_garbage() {
        // base_path "/" must not double the leading slash.
        let xml = r#"<folder name="root"><folders><folder name="Docs"/></folders></folder>"#;
        let entries = JottacloudProvider::parse_folder_xml(xml, "/");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/Docs");

        // Garbage / empty bodies yield an empty list, never a panic.
        assert!(JottacloudProvider::parse_folder_xml("not xml at all", "/").is_empty());
        assert!(JottacloudProvider::parse_folder_xml("", "/").is_empty());
    }

    /// A station can hold two refresh chains for one profile (GUI per-profile,
    /// CLI singleton); a bound provider must try both, most specific first,
    /// and an unbound one only the singleton. A rotated token goes back to
    /// the key it came from as well as the provider's own key, so neither
    /// client is left reading a consumed value.
    #[test]
    fn refresh_chains_are_tried_in_order_and_rotations_reach_both_keys() {
        assert_eq!(
            JottacloudProvider::refresh_chain_candidates("srv_1"),
            vec![
                "jottacloud_refresh_srv_1".to_string(),
                "jottacloud_refresh".to_string()
            ]
        );
        assert_eq!(
            JottacloudProvider::refresh_chain_candidates(""),
            vec!["jottacloud_refresh".to_string()]
        );
        // Loaded from its own key: one write.
        assert_eq!(
            JottacloudProvider::refresh_persist_accounts("srv_1", None),
            vec!["jottacloud_refresh_srv_1".to_string()]
        );
        assert_eq!(
            JottacloudProvider::refresh_persist_accounts("srv_1", Some("jottacloud_refresh_srv_1")),
            vec!["jottacloud_refresh_srv_1".to_string()]
        );
        // Loaded from the singleton by a bound provider: both keys.
        assert_eq!(
            JottacloudProvider::refresh_persist_accounts("srv_1", Some("jottacloud_refresh")),
            vec![
                "jottacloud_refresh_srv_1".to_string(),
                "jottacloud_refresh".to_string()
            ]
        );
        // Unbound provider: only the singleton, whatever it was loaded from.
        assert_eq!(
            JottacloudProvider::refresh_persist_accounts("", Some("jottacloud_refresh")),
            vec!["jottacloud_refresh".to_string()]
        );
    }

    /// A credential mirrored into the user partition has TWO copies, and
    /// `resolve_active_credential` reads the partition one first. A file that
    /// mirrors a key must therefore also unmirror it wherever it deletes it,
    /// or every later read finds the dead value again (three Jotta exports
    /// in a row carried a refused chain this way). Pinned on the callers of
    /// `mirror_active_credential`, which is the property; MEGA and 4shared
    /// store and delete raw and are outside it.
    #[test]
    fn every_file_that_mirrors_a_credential_also_unmirrors_it() {
        let files: [(&str, &str); 2] = [
            ("jottacloud.rs", include_str!("jottacloud.rs")),
            ("oauth2.rs", include_str!("oauth2.rs")),
        ];
        for (name, src) in files {
            let mirrors = src.matches("mirror_active_credential(").count()
                - src.matches("unmirror_active_credential(").count();
            assert!(mirrors > 0, "{name}: expected at least one mirror call");
            assert!(
                src.contains("unmirror_active_credential("),
                "{name} mirrors a credential and never unmirrors it: a raw delete leaves the partition copy alive"
            );
        }
    }

    /// #397 / tracker #701 item 9: a trashed folder is purged with the
    /// hard-delete form on its ORIGINAL path, never on the Trash view, and
    /// only when that path answers with a tombstone. A live folder at the
    /// same path is refused, because the same POST would destroy it.
    #[test]
    fn trashed_folder_purge_goes_to_the_original_path_and_only_for_a_tombstone() {
        let p = test_provider();
        let url = p.purge_trashed_dir_url("aeroftp-397-purge");
        assert!(
            url.ends_with("/Jotta/Archive/aeroftp-397-purge?rmDir=true"),
            "{url}"
        );
        assert!(!url.contains("/Trash/"), "never the Trash view: {url}");

        let tombstone = r#"<folder name="x" deleted="2026-09-01-T10:41:34Z"><files/></folder>"#;
        assert_eq!(
            JottacloudProvider::trashed_folder_purge_decision(tombstone),
            Ok(())
        );
        let live = r#"<folder name="x"><files/></folder>"#;
        let refused = JottacloudProvider::trashed_folder_purge_decision(live).unwrap_err();
        assert!(refused.contains("LIVE"), "{refused}");
        let file = r#"<file name="x" deleted="2026-09-01-T10:41:34Z"/>"#;
        assert!(JottacloudProvider::trashed_folder_purge_decision(file).is_err());
        assert!(JottacloudProvider::trashed_folder_purge_decision("garbage").is_err());
    }

    #[test]
    fn file_purge_fallback_requires_the_root_file_to_be_a_tombstone() {
        assert_eq!(
            JottacloudProvider::trashed_entry_purge_param(
                r#"<file name="x" deleted="2026-09-06-T22:00:00Z"/>"#
            ),
            Ok("rm")
        );
        assert!(JottacloudProvider::trashed_entry_purge_param(
            r#"<file name="x"><revision deleted="2026-09-06-T22:00:00Z"/></file>"#
        )
        .unwrap_err()
        .contains("LIVE"));
        assert_eq!(
            JottacloudProvider::trashed_entry_purge_param(
                r#"<folder name="x" deleted="2026-09-06-T22:00:00Z"/>"#
            ),
            Ok("rmDir")
        );
        assert!(JottacloudProvider::trashed_entry_purge_param(r#"<folder name="x"/>"#).is_err());
        assert!(JottacloudProvider::trashed_entry_purge_param("garbage").is_err());
    }

    /// `purge_trash` answers `{"files":N,"folders":M}`; anything else is a
    /// parse error and never a confirmed zero.
    #[test]
    fn purge_trash_counts_are_parsed_strictly() {
        assert_eq!(
            JottacloudProvider::parse_purge_trash_counts(r#"{"files":0,"folders":2}"#).unwrap(),
            (0, 2)
        );
        for bad in [
            "not json",
            "",
            r#"{"files":0}"#,
            r#"{"files":"0","folders":2}"#,
            r#"{"files":-1,"folders":2}"#,
            r#"[]"#,
        ] {
            let err = JottacloudProvider::parse_purge_trash_counts(bad).unwrap_err();
            assert!(
                matches!(err, ProviderError::ParseError(_)),
                "{bad:?} -> {err:?}"
            );
        }
    }

    #[test]
    fn folder_tombstone_children_classify_live_and_deleted() {
        // Shape captured from the live API (#397): a trashed folder's listing
        // on the ORIGINAL mountpoint still serves every child, each stamped
        // with its own `deleted` attribute and, for files, the tombstone
        // revision carrying size/md5. Restored (live) children lose the
        // attribute; live folders collapse to an empty element.
        let xml = r#"<folder name="aeroftp-397-restore" deleted="2026-09-01-T10:41:34Z">
  <path>/user123/Jotta/Archive</path>
  <folders>
    <folder name="sub1" deleted="2026-09-01-T10:41:34Z">
      <abspath>/user123/Jotta/Archive/aeroftp-397-restore</abspath>
    </folder>
    <folder name="already-back"/>
  </folders>
  <files>
    <file name="root-file.txt" uuid="02d5df8e-b376-4f34-84f5-749925faf0e0" deleted="2026-09-01-T10:41:34Z">
      <abspath>/user123/Jotta/Archive/aeroftp-397-restore</abspath>
      <currentRevision>
        <number>1</number>
        <state>COMPLETED</state>
        <created>2026-09-01-T10:40:48Z</created>
        <modified>2026-09-01-T10:40:48Z</modified>
        <mime>text/plain</mime>
        <size>30</size>
        <md5>d84d3caf340bd500193971c9eecb7255</md5>
        <updated>2026-09-01-T10:41:11Z</updated>
      </currentRevision>
    </file>
    <file name="live.txt" uuid="7dd16aac-df5c-49e0-b4cc-b851e28f2493">
      <currentRevision>
        <size>24</size>
        <md5>c665afba7ca3625a6544034ca2341c19</md5>
        <created>2026-09-01-T10:40:48Z</created>
        <modified>2026-09-01-T10:40:48Z</modified>
      </currentRevision>
    </file>
  </files>
</folder>"#;
        let children = JottacloudProvider::parse_folder_tombstone_children(xml);
        assert_eq!(children.len(), 4, "{children:?}");
        assert_eq!(
            children[0],
            TombstoneChild::Folder {
                name: "sub1".to_string(),
                deleted: true
            }
        );
        assert_eq!(
            children[1],
            TombstoneChild::Folder {
                name: "already-back".to_string(),
                deleted: false
            },
            "live folders arrive as empty elements and must not be re-created"
        );
        match &children[2] {
            TombstoneChild::File {
                name,
                deleted,
                revision,
            } => {
                assert_eq!(name, "root-file.txt");
                assert!(deleted, "tombstoned file keeps its deleted stamp");
                let (size, md5, created, modified) = revision.clone().expect("tombstone revision");
                assert_eq!(size, 30);
                assert_eq!(md5, "d84d3caf340bd500193971c9eecb7255");
                assert_eq!(created, "2026-09-01-T10:40:48Z");
                assert_eq!(modified, "2026-09-01-T10:40:48Z");
            }
            other => panic!("expected file, got {other:?}"),
        }
        match &children[3] {
            TombstoneChild::File {
                name,
                deleted,
                revision,
            } => {
                assert_eq!(name, "live.txt");
                assert!(!deleted, "live child must land in already-present");
                assert!(revision.is_some());
            }
            other => panic!("expected file, got {other:?}"),
        }

        // Garbage yields no children, never a panic.
        assert!(JottacloudProvider::parse_folder_tombstone_children("junk").is_empty());
        assert!(JottacloudProvider::parse_folder_tombstone_children("").is_empty());
    }

    #[test]
    fn root_folder_tombstone_detection() {
        let tombstoned = r#"<folder name="x" deleted="2026-09-01-T10:41:34Z"><files/></folder>"#;
        let live = r#"<folder name="x"><files/></folder>"#;
        let not_a_folder = r#"<file name="x" deleted="2026-09-01-T10:41:34Z"/>"#;
        assert!(JottacloudProvider::jfs_root_folder_is_tombstone(tombstoned));
        assert!(!JottacloudProvider::jfs_root_folder_is_tombstone(live));
        assert!(
            !JottacloudProvider::jfs_root_folder_is_tombstone(not_a_folder),
            "a file root is not a folder tombstone"
        );
        assert!(!JottacloudProvider::jfs_root_folder_is_tombstone("garbage"));
    }

    // ─── Live end-to-end regression for #397 folder restore ────────────
    //
    // This is the red→green proof: on v4.1.9 the folder branch of
    // `restore_from_trash` POSTs `?restore=true`, JFS answers 500, and this
    // test fails at the restore step. With the composed restore it passes.
    //
    // Run explicitly (never in CI):
    //   cargo test --release --lib live_restore_folder_tree_end_to_end -- --ignored --nocapture
    // Env: JOTTA_TEST_PROFILE (default "Jotta").
    //
    // The test builds a tree (root file, nested subfolders with one file per
    // level, one EMPTY subfolder), trashes it, restores it, byte-compares
    // every file, then restores a SECOND time to prove idempotence (nothing
    // re-restored, nothing duplicated, no error). It cleans up after itself.
    #[tokio::test]
    #[ignore = "live Jottacloud test; run explicitly"]
    async fn live_restore_folder_tree_end_to_end() {
        use crate::providers::{ProviderConfig, ProviderFactory, ProviderType};

        let profile_query =
            std::env::var("JOTTA_TEST_PROFILE").unwrap_or_else(|_| "Jotta".to_string());
        let status = crate::credential_store::CredentialStore::init().expect("vault init failed");
        if status == "MASTER_PASSWORD_REQUIRED" {
            let password = zeroize::Zeroizing::new(
                std::env::var("AEROFTP_MASTER_PASSWORD")
                    .expect("set AEROFTP_MASTER_PASSWORD for this master-mode test vault"),
            );
            crate::credential_store::CredentialStore::unlock_with_master(&password, None)
                .expect("test vault unlock failed");
        }
        let store = crate::credential_store::CredentialStore::from_cache().expect("vault not open");
        let profiles = crate::user_partitions::mcp_list_active_server_profiles(&store)
            .expect("profile listing failed");
        let matched = profiles
            .iter()
            .find(|p| {
                p.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&profile_query))
            })
            .cloned()
            .expect("test profile not found");
        let profile_id = matched
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let raw = crate::user_partitions::resolve_active_credential(
            &store,
            &format!("server_{profile_id}"),
        )
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .unwrap_or_default();
        let token = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            v.get("password")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("access_token").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string()
        } else {
            raw.trim_matches('"').to_string()
        };
        assert!(!token.is_empty(), "no credential resolved for test profile");

        // Bind the profile id: the per-profile refresh-token key is the
        // partition-aware chain the GUI and the current CLI maintain, so it
        // stays valid across runs. The legacy singleton is a separate chain
        // (used by older installed binaries) and sharing values across
        // chains makes one rotation invalidate the other.
        let mut extra = std::collections::HashMap::new();
        extra.insert("profile_id".to_string(), profile_id);
        let config = ProviderConfig {
            name: "jotta-e2e".to_string(),
            provider_type: ProviderType::Jottacloud,
            host: "jfs.jottacloud.com".to_string(),
            port: Some(443),
            username: Some("token".to_string()),
            password: Some(token),
            initial_path: None,
            extra,
        };
        let mut boxed = ProviderFactory::create(&config).expect("provider create failed");
        // The GUI and CLI on this machine hold their own Jotta token chains
        // and refresh on their own schedule; a rotation race fails one
        // connect with invalid_grant. A fresh provider re-reads the newest
        // persisted token, so one retry absorbs the race.
        if let Err(e) = boxed.connect().await {
            eprintln!("first connect failed ({e}); retrying with a fresh provider");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            boxed = ProviderFactory::create(&config).expect("provider create failed");
            boxed.connect().await.expect("connect failed after retry");
        }
        let p = boxed
            .as_any_mut()
            .downcast_mut::<JottacloudProvider>()
            .expect("downcast failed");

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let root = format!("aeroftp-397-e2e-{stamp}");
        let files: Vec<(String, Vec<u8>)> = vec![
            (format!("{root}/root.txt"), b"e2e root level\n".to_vec()),
            (
                format!("{root}/sub1/sub1.txt"),
                b"e2e sub1 level\n".to_vec(),
            ),
            (
                format!("{root}/sub1/sub2/deep.txt"),
                b"e2e deepest level\n".to_vec(),
            ),
        ];

        // Build the fixture.
        p.mkdir(&root).await.expect("mkdir root");
        p.mkdir(&format!("{root}/empty"))
            .await
            .expect("mkdir empty");
        p.mkdir(&format!("{root}/sub1")).await.expect("mkdir sub1");
        p.mkdir(&format!("{root}/sub1/sub2"))
            .await
            .expect("mkdir sub2");
        let tmp = std::env::temp_dir().join(format!("jotta-e2e-{stamp}"));
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        for (remote, bytes) in &files {
            let local = tmp.join(remote.rsplit('/').next().unwrap());
            tokio::fs::write(&local, bytes).await.unwrap();
            p.upload(local.to_str().unwrap(), remote, None)
                .await
                .unwrap_or_else(|e| panic!("upload {remote} failed: {e}"));
        }

        // Trash the whole tree, then confirm the bin holds it.
        p.move_to_trash(&root).await.expect("move to trash");
        let trash = p.list_trash().await.expect("list trash");
        assert!(
            trash.iter().any(|e| e.name == root),
            "fixture must appear in trash: {trash:?}"
        );

        // THE restore under test.
        let report = p.restore_from_trash(&root).await.expect("FOLDER RESTORE");
        assert_eq!(
            report.files_restored, 3,
            "one file per level must be restored: {report:?}"
        );
        assert_eq!(report.files_already_present, 0, "{report:?}");
        assert_eq!(
            report.dirs_restored, 1,
            "the empty dir needs mkDir: {report:?}"
        );
        assert!(report.failed.is_empty(), "{report:?}");

        // Every file must be back with the exact bytes it had before.
        for (remote, bytes) in &files {
            let got = p
                .download_to_bytes(remote)
                .await
                .unwrap_or_else(|e| panic!("download restored {remote} failed: {e}"));
            assert_eq!(&got, bytes, "restored bytes differ for {remote}");
        }
        // The empty dir must be back too.
        let root_entries = p.list(&root).await.expect("list restored root");
        assert!(
            root_entries.iter().any(|e| e.is_dir && e.name == "empty"),
            "empty dir missing after restore: {root_entries:?}"
        );
        // The bin must no longer hold the fixture.
        let trash = p.list_trash().await.expect("list trash after restore");
        assert!(
            !trash.iter().any(|e| e.name == root),
            "fully restored folder must leave the bin: {trash:?}"
        );

        // Idempotence: a second restore changes nothing and fails nothing.
        let second = p
            .restore_from_trash(&root)
            .await
            .expect("second restore must not fail");
        assert_eq!(second.files_restored, 0, "{second:?}");
        assert_eq!(
            second.files_already_present, 3,
            "live children are counted apart, not re-restored: {second:?}"
        );
        assert_eq!(second.dirs_restored, 0, "{second:?}");
        assert!(second.failed.is_empty(), "{second:?}");

        // The follow-up also reports a file purge 404. Exercise both a root
        // child and a deeply nested child, and confirm file restore bytes
        // before purging. Never call empty_trash on the owner's account.
        for (remote, bytes) in &files {
            p.move_to_trash(remote)
                .await
                .expect("trash individual file");
            let report = p
                .restore_from_trash(remote)
                .await
                .expect("restore individual file");
            assert_eq!(report.files_restored, 1);
            assert_eq!(&p.download_to_bytes(remote).await.unwrap(), bytes);
            p.move_to_trash(remote)
                .await
                .expect("re-trash individual file");
            p.permanent_delete_from_trash(remote)
                .await
                .unwrap_or_else(|e| panic!("purge individual fixture {remote}: {e}"));
            assert!(
                !p.list_trash()
                    .await
                    .unwrap()
                    .iter()
                    .any(|entry| { entry.path.trim_start_matches('/') == remote }),
                "purged file still in trash: {remote}"
            );
        }

        // Cleanup: trash the tree again, then purge it for real. A trashed
        // FOLDER is purged with `?rmDir=true` on its ORIGINAL path (the Trash
        // view still 404s), measured 2026-09-01, so the fixture must leave
        // the bin and a leftover is a failure, not a log line.
        p.move_to_trash(&root).await.expect("cleanup move to trash");
        p.permanent_delete_from_trash(&root)
            .await
            .expect("purge of the trashed fixture folder");
        let trash = p.list_trash().await.expect("list trash after purge");
        assert!(
            !trash.iter().any(|e| e.name.trim_start_matches('/') == root),
            "purged fixture must be gone from the bin: {trash:?}"
        );
        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    // ─── Live check of the purge guard and of empty_trash (#397, item 9) ──
    //
    // Run explicitly (never in CI):
    //   cargo test --release --lib live_purge_trashed_folder_and_guard -- --ignored --nocapture
    // Env: JOTTA_TEST_PROFILE (default "Jotta"). Test account only: the last
    // step empties the WHOLE trash of that account.
    #[tokio::test]
    #[ignore = "live Jottacloud test; run explicitly"]
    async fn live_purge_trashed_folder_and_guard() {
        use crate::providers::{ProviderConfig, ProviderFactory, ProviderType};

        let profile_query =
            std::env::var("JOTTA_TEST_PROFILE").unwrap_or_else(|_| "Jotta".to_string());
        crate::credential_store::CredentialStore::init().expect("vault init failed");
        let store = crate::credential_store::CredentialStore::from_cache().expect("vault not open");
        let profiles = crate::user_partitions::mcp_list_active_server_profiles(&store)
            .expect("profile listing failed");
        let matched = profiles
            .iter()
            .find(|p| {
                p.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&profile_query))
            })
            .cloned()
            .expect("test profile not found");
        let profile_id = matched
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let raw = crate::user_partitions::resolve_active_credential(
            &store,
            &format!("server_{profile_id}"),
        )
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .unwrap_or_default();
        let token = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            v.get("password")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("access_token").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string()
        } else {
            raw.trim_matches('"').to_string()
        };
        // The refresh chain to use is selectable: the installed CLI rotates
        // the legacy singleton (`jottacloud_refresh`), a GUI-bound provider
        // the per-profile key. Default to the singleton; set
        // JOTTA_TEST_BIND_PROFILE=1 to bind the profile id instead.
        let mut extra = std::collections::HashMap::new();
        if std::env::var("JOTTA_TEST_BIND_PROFILE").is_ok() {
            extra.insert("profile_id".to_string(), profile_id);
        }
        let config = ProviderConfig {
            name: "jotta-purge".to_string(),
            provider_type: ProviderType::Jottacloud,
            host: "jfs.jottacloud.com".to_string(),
            port: Some(443),
            username: Some("token".to_string()),
            password: Some(token),
            initial_path: None,
            extra,
        };
        let mut boxed = ProviderFactory::create(&config).expect("provider create failed");
        boxed.connect().await.expect("connect failed");
        let p = boxed
            .as_any_mut()
            .downcast_mut::<JottacloudProvider>()
            .expect("downcast failed");

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let trashed = format!("aeroftp-397-purge-{stamp}");
        let live = format!("aeroftp-397-live-{stamp}");
        p.mkdir(&trashed).await.expect("mkdir trashed fixture");
        p.mkdir(&format!("{trashed}/sub")).await.expect("mkdir sub");
        p.mkdir(&live).await.expect("mkdir live fixture");

        // 1. A trashed folder purges through the original-path form.
        p.move_to_trash(&trashed).await.expect("move to trash");
        assert!(p
            .list_trash()
            .await
            .expect("list")
            .iter()
            .any(|e| e.name.trim_start_matches('/') == trashed));
        p.permanent_delete_from_trash(&trashed)
            .await
            .expect("PURGE OF A TRASHED FOLDER");
        assert!(
            !p.list_trash()
                .await
                .expect("list")
                .iter()
                .any(|e| e.name.trim_start_matches('/') == trashed),
            "the purged folder must leave the bin"
        );

        // 2. The same call at a LIVE folder is refused and the folder survives.
        let refused = p.permanent_delete_from_trash(&live).await;
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("LIVE")),
            "a live folder must be refused, got {refused:?}"
        );
        assert!(
            p.list("")
                .await
                .expect("list root")
                .iter()
                .any(|e| e.name == live),
            "the live folder must survive the refused purge"
        );

        // 3. empty_trash reports what it removed and the bin re-lists empty.
        // This step purges EVERY entry in the account's trash, not only the
        // fixtures, so it needs its own opt-in on top of the ignored test:
        // without JOTTA_TEST_ALLOW_EMPTY_TRASH=1 the fixture is purged one
        // by one instead and the whole-bin step is reported as skipped.
        p.move_to_trash(&live)
            .await
            .expect("trash the live fixture for cleanup");
        if std::env::var("JOTTA_TEST_ALLOW_EMPTY_TRASH").as_deref() == Ok("1") {
            let (files, folders) = p.empty_trash().await.expect("EMPTY TRASH");
            eprintln!("empty_trash: {files} file(s), {folders} folder(s)");
            assert!(
                folders >= 1,
                "the trashed fixture must be counted: {folders}"
            );
            assert!(
                p.list_trash().await.expect("list after empty").is_empty(),
                "bin must be empty"
            );
        } else {
            p.permanent_delete_from_trash(&live)
                .await
                .expect("purge the second fixture");
            eprintln!(
                "empty_trash step SKIPPED: set JOTTA_TEST_ALLOW_EMPTY_TRASH=1 to purge the whole bin"
            );
        }
    }
}
