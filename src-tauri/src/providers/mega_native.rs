// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! MEGA Native API Provider — full JSON-RPC implementation.
//! Connects directly to MEGA servers without MEGAcmd dependency.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, sleep};
use secrecy::ExposeSecret;
use zeroize::Zeroize;

use super::{
    mega_crypto::{
        aes_ctr_decrypt, aes_ctr_encrypt,
        aes_ecb_decrypt_block, aes_ecb_encrypt_block, aes_ecb_encrypt_multi,
        chunk_mac, compute_chunk_boundaries, decrypt_node_attrs, decrypt_node_key_xor,
        decrypt_rsa_privkey, encrypt_node_attrs, kdf_v1, kdf_v2,
        mega_base64_decode, mega_base64_encode, meta_mac, pack_node_key, rsa_decrypt_csid,
        unpack_node_key, username_hash_v1,
    },
    MegaConfig, ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
    ShareLinkOptions, ShareLinkResult,
};

const MEGA_API_BASE_URL: &str = "https://g.api.mega.co.nz/cs";
const MEGA_API_VERSION: &str = "2";
const NATIVE_MAX_RETRIES: u32 = 2;
const NATIVE_RETRY_DELAY_MS: u64 = 2000;

// ─── Wire types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MegaPreloginResponse {
    version: u8,
    salt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaPreloginWire {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "s")]
    salt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaLoginResponseWire {
    #[serde(rename = "k")]
    encrypted_master_key: String,
    #[serde(rename = "tsid")]
    temporary_session_id: Option<String>,
    #[serde(rename = "csid")]
    encrypted_session_id: Option<String>,
    #[serde(rename = "privk")]
    encrypted_rsa_private_key: Option<String>,
    #[serde(rename = "u")]
    user_handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MegaUserInfoWire {
    #[serde(rename = "u")]
    user_handle: String,
    email: Option<String>,
    #[serde(rename = "sn")]
    sequence_number: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MegaPersistedSession {
    session_id: String,
    master_key_b64: String,
    account_version: u8,
    user_handle: Option<String>,
    sequence_number: Option<String>,
    stored_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
struct FetchNodesResponseWire {
    f: Vec<RawMegaNodeWire>,
}

#[derive(Debug, Deserialize)]
struct RawMegaNodeWire {
    /// Node handle
    h: String,
    /// Parent handle
    p: String,
    /// Node type: 0=file, 1=folder, 2=root, 3=inbox, 4=trash
    t: u8,
    /// Encrypted key (format: "owner_handle:base64_key" or multiple separated by /)
    #[serde(default)]
    k: Option<String>,
    /// Encrypted attributes
    #[serde(default, rename = "a")]
    attrs: Option<String>,
    /// File size (0 for folders)
    #[serde(default)]
    s: u64,
    /// Timestamp (unix)
    #[serde(default)]
    ts: u64,
    /// User handle (owner)
    #[serde(default)]
    _u: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetDownloadUrlResponseWire {
    /// Download URL
    g: String,
    /// File size
    s: u64,
}

#[derive(Debug, Deserialize)]
struct RequestUploadUrlResponseWire {
    /// Upload URL
    p: String,
}

#[derive(Debug, Deserialize)]
struct PutNodesResponseWire {
    #[allow(dead_code)]
    f: Vec<RawMegaNodeWire>,
}

#[derive(Debug, Deserialize)]
struct GetQuotaResponseWire {
    /// Total storage in bytes
    #[serde(default)]
    mstrg: u64,
    /// Used storage in bytes
    #[serde(default)]
    cstrg: u64,
}

// ─── Internal node representation ─────────────────────────────────────────

#[derive(Debug, Clone)]
struct MegaNode {
    handle: String,
    parent: String,
    node_type: u8,
    name: String,
    size: u64,
    timestamp: u64,
    /// Decrypted key (16 bytes for folders, 32 bytes for files)
    key: Vec<u8>,
}

impl MegaNode {
    fn is_file(&self) -> bool { self.node_type == 0 }
    fn is_folder(&self) -> bool { self.node_type == 1 }
}

// ─── API Client ───────────────────────────────────────────────────────────

struct MegaApiClient {
    client: reqwest::Client,
    next_request_id: AtomicU64,
    session_id: Option<String>,
}

impl MegaApiClient {
    fn new(session_id: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            next_request_id: AtomicU64::new(1),
            session_id,
        }
    }

    fn set_session_id(&mut self, session_id: Option<String>) {
        self.session_id = session_id;
    }

    async fn command<T>(&self, command: Value) -> Result<T, ProviderError>
    where
        T: DeserializeOwned,
    {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let mut url = reqwest::Url::parse(MEGA_API_BASE_URL).map_err(|err| {
            ProviderError::InvalidConfig(format!("Invalid MEGA API base URL: {err}"))
        })?;

        {
            let mut query = url.query_pairs_mut();
            query.append_pair("id", &request_id.to_string());
            query.append_pair("v", MEGA_API_VERSION);
            if let Some(session_id) = self.session_id.as_deref() {
                query.append_pair("sid", session_id);
            }
        }

        let cmd_name = command.get("a").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        tracing::debug!("[MEGA Native] API: cmd={}, has_sid={}", cmd_name, self.session_id.is_some());

        let response = self
            .client
            .post(url)
            .json(&vec![command])
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            let status = response.status();
            if status.as_u16() == 402 {
                return Err(ProviderError::AuthenticationFailed(format!(
                    "MEGA API returned HTTP {} (session expired or invalid)", status
                )));
            }
            return Err(ProviderError::NetworkError(format!(
                "MEGA API returned HTTP {}", status
            )));
        }

        let payload: Vec<Value> = response.json().await.map_err(|err| {
            ProviderError::ParseError(format!("Invalid JSON response from MEGA API: {err}"))
        })?;

        let first = payload.into_iter().next().ok_or_else(|| {
            ProviderError::ParseError("MEGA API returned an empty response array".to_string())
        })?;

        decode_command_response(first)
    }
}

fn decode_command_response<T>(value: Value) -> Result<T, ProviderError>
where
    T: DeserializeOwned,
{
    // MEGA returns negative integers for errors, 0 for success on mutation commands
    if let Some(code) = value.as_i64() {
        if code < 0 {
            return Err(map_mega_error_code(code));
        }
        // 0 or positive = success. Try to deserialize; if T can't be built from
        // an integer (e.g. T = Value), just return the raw integer as Value.
    }
    serde_json::from_value(value)
        .map_err(|err| ProviderError::ParseError(format!("Invalid MEGA response payload: {err}")))
}

fn map_reqwest_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() { ProviderError::Timeout }
    else { ProviderError::NetworkError(format!("MEGA API request failed: {err}")) }
}

fn map_mega_error_code(code: i64) -> ProviderError {
    match code {
        -1 => ProviderError::ServerError("MEGA internal error".to_string()),
        -2 => ProviderError::InvalidConfig("MEGA rejected the request arguments".to_string()),
        -3 => ProviderError::ServerError("MEGA requested a retry (EAGAIN)".to_string()),
        -4 => ProviderError::ServerError("MEGA rate limit exceeded".to_string()),
        -7 | -11 => ProviderError::PermissionDenied("MEGA access denied".to_string()),
        -8 => ProviderError::AlreadyExists("MEGA resource already exists".to_string()),
        -9 => ProviderError::NotFound("MEGA resource not found".to_string()),
        -13 => ProviderError::AuthenticationFailed("MEGA session expired".to_string()),
        -14 => ProviderError::AuthenticationFailed("MEGA account not confirmed".to_string()),
        -16 => ProviderError::ServerError("MEGA over quota".to_string()),
        other => ProviderError::ServerError(format!("MEGA API error {other}")),
    }
}

// ─── Provider ─────────────────────────────────────────────────────────────

pub struct MegaNativeProvider {
    config: MegaConfig,
    api_client: MegaApiClient,
    connected: bool,
    current_path: String,
    account_version: Option<u8>,
    prelogin_salt: Option<String>,
    session_id: Option<String>,
    master_key: Option<[u8; 16]>,
    user_handle: Option<String>,
    sequence_number: Option<String>,
    /// RSA private key components (p, q, d, u) for csid decryption and share key ops
    rsa_components: Option<(num_bigint_dig::BigUint, num_bigint_dig::BigUint, num_bigint_dig::BigUint, num_bigint_dig::BigUint)>,
    /// Decrypted node tree (handle → node)
    nodes: HashMap<String, MegaNode>,
    /// Children index (parent_handle → child handles)
    children: HashMap<String, Vec<String>>,
    root_handle: Option<String>,
    trash_handle: Option<String>,
    nodes_loaded: bool,
}

impl MegaNativeProvider {
    pub fn new(config: MegaConfig) -> Self {
        Self {
            config,
            api_client: MegaApiClient::new(None),
            connected: false,
            current_path: "/".to_string(),
            account_version: None,
            prelogin_salt: None,
            session_id: None,
            master_key: None,
            user_handle: None,
            sequence_number: None,
            rsa_components: None,
            nodes: HashMap::new(),
            children: HashMap::new(),
            root_handle: None,
            trash_handle: None,
            nodes_loaded: false,
        }
    }

    fn session_vault_key(&self) -> String {
        format!("mega_native_session_{}", self.config.email.trim().to_lowercase())
    }

    fn now_unix_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    fn clear_runtime_session(&mut self) {
        self.connected = false;
        self.current_path = "/".to_string();
        self.api_client.set_session_id(None);
        self.session_id = None;
        self.account_version = None;
        self.prelogin_salt = None;
        self.user_handle = None;
        self.sequence_number = None;
        self.rsa_components = None;
        self.nodes.clear();
        self.children.clear();
        self.root_handle = None;
        self.trash_handle = None;
        self.nodes_loaded = false;
        if let Some(mut mk) = self.master_key.take() { mk.zeroize(); }
    }

    fn is_transient_error(err: &ProviderError) -> bool {
        matches!(err, ProviderError::ServerError(msg) if msg.contains("retry (EAGAIN)") || msg.contains("rate limit"))
    }

    async fn command_with_retry<T>(&self, command: Value) -> Result<T, ProviderError>
    where T: DeserializeOwned {
        let mut attempt = 0;
        loop {
            match self.api_client.command(command.clone()).await {
                Ok(r) => return Ok(r),
                Err(err) if attempt < NATIVE_MAX_RETRIES && Self::is_transient_error(&err) => {
                    attempt += 1;
                    tracing::warn!("[MEGA Native] transient error, retry {}/{}: {}", attempt, NATIVE_MAX_RETRIES, err);
                    sleep(Duration::from_millis(NATIVE_RETRY_DELAY_MS)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    // ─── Auth helpers ─────────────────────────────────────────────────

    fn verify_tsid(tsid: &str, master_key: &[u8; 16]) -> Result<(), ProviderError> {
        let decoded = mega_base64_decode(tsid)?;
        if decoded.len() != 32 {
            return Err(ProviderError::ParseError(format!(
                "Invalid MEGA tsid length: expected 32, got {}", decoded.len()
            )));
        }
        let first_half = <[u8; 16]>::try_from(&decoded[..16])
            .map_err(|_| ProviderError::ParseError("tsid slice error".into()))?;
        let expected = aes_ecb_encrypt_block(&first_half, master_key)?;
        if expected != decoded[16..32] {
            return Err(ProviderError::AuthenticationFailed("MEGA tsid verification failed".into()));
        }
        Ok(())
    }

    async fn get_user_info(&self) -> Result<MegaUserInfoWire, ProviderError> {
        self.command_with_retry(json!({ "a": "ug", "v": 1 })).await
    }

    fn ensure_expected_email(&self, returned: Option<&str>) -> Result<(), ProviderError> {
        if let Some(returned) = returned {
            let expected = self.config.email.trim().to_lowercase();
            let actual = returned.trim().to_lowercase();
            if actual != expected {
                return Err(ProviderError::AuthenticationFailed(format!(
                    "MEGA session belongs to {actual}, expected {expected}"
                )));
            }
        }
        Ok(())
    }

    fn persist_session(&self) -> Result<(), ProviderError> {
        if !self.config.save_session { return Ok(()); }
        let (Some(sid), Some(mk), Some(ver)) = (self.session_id.as_ref(), self.master_key.as_ref(), self.account_version) else { return Ok(()) };
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else { return Ok(()) };

        let persisted = MegaPersistedSession {
            session_id: sid.clone(),
            master_key_b64: mega_base64_encode(mk),
            account_version: ver,
            user_handle: self.user_handle.clone(),
            sequence_number: self.sequence_number.clone(),
            stored_at_unix_ms: Self::now_unix_ms(),
        };
        let serialized = serde_json::to_string(&persisted)
            .map_err(|e| ProviderError::ParseError(format!("Failed to serialize session: {e}")))?;
        store.store(&self.session_vault_key(), &serialized)
            .map_err(|e| ProviderError::Other(format!("Failed to persist session: {e}")))
    }

    fn clear_persisted_session(&self) -> Result<(), ProviderError> {
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else { return Ok(()) };
        match store.delete(&self.session_vault_key()) {
            Ok(()) | Err(crate::credential_store::CredentialError::NotFound(_)) => Ok(()),
            Err(e) => Err(ProviderError::Other(format!("Failed to clear session: {e}"))),
        }
    }

    async fn try_resume_session(&mut self) -> Result<bool, ProviderError> {
        if !self.config.save_session { return Ok(false); }
        let Some(store) = crate::credential_store::CredentialStore::from_cache() else { return Ok(false) };

        let serialized = match store.get(&self.session_vault_key()) {
            Ok(s) => s,
            Err(crate::credential_store::CredentialError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(ProviderError::Other(format!("Failed to load session: {e}"))),
        };

        let persisted: MegaPersistedSession = match serde_json::from_str(&serialized) {
            Ok(p) => p,
            Err(_) => { self.clear_persisted_session()?; return Ok(false); }
        };

        let master_key = match mega_base64_decode(&persisted.master_key_b64)
            .and_then(|bytes| {
                if bytes.len() != 16 { return Err(ProviderError::ParseError("bad key length".into())); }
                let mut arr = [0u8; 16]; arr.copy_from_slice(&bytes); Ok(arr)
            }) {
            Ok(mk) => mk,
            Err(_) => { self.clear_persisted_session()?; return Ok(false); }
        };

        self.api_client.set_session_id(Some(persisted.session_id.clone()));

        match self.get_user_info().await {
            Ok(info) => {
                self.ensure_expected_email(info.email.as_deref())?;
                self.connected = true;
                self.session_id = Some(persisted.session_id);
                self.account_version = Some(persisted.account_version);
                self.master_key = Some(master_key);
                self.user_handle = Some(info.user_handle);
                self.sequence_number = info.sequence_number;
                self.current_path = "/".to_string();
                Ok(true)
            }
            Err(ProviderError::AuthenticationFailed(_)) => {
                self.clear_runtime_session();
                self.clear_persisted_session()?;
                Ok(false)
            }
            Err(e) => { self.clear_runtime_session(); Err(e) }
        }
    }

    // ─── Login flows ──────────────────────────────────────────────────

    async fn login_v2(&mut self, prelogin: MegaPreloginResponse) -> Result<(), ProviderError> {
        let salt_b64 = prelogin.salt.ok_or_else(|| {
            ProviderError::ParseError("MEGA v2 pre-login did not include a salt".into())
        })?;
        let salt = mega_base64_decode(&salt_b64)?;
        let password = self.config.password.expose_secret();
        let (mut password_key, user_hash) = kdf_v2(password.as_bytes(), &salt)?;
        let user_hash_b64 = mega_base64_encode(&user_hash);

        let login_result: Result<(), ProviderError> = async {
            let response: MegaLoginResponseWire = self
                .command_with_retry(json!({
                    "a": "us",
                    "user": self.config.email.trim().to_lowercase(),
                    "uh": user_hash_b64,
                }))
                .await?;

            let enc_mk = mega_base64_decode(&response.encrypted_master_key)?;
            let enc_mk_arr = <[u8; 16]>::try_from(enc_mk.as_slice())
                .map_err(|_| ProviderError::ParseError("master key not 16 bytes".into()))?;
            let master_key = aes_ecb_decrypt_block(&enc_mk_arr, &password_key)?;

            // Determine session ID via tsid or RSA/csid path
            let session_id = if let Some(tsid) = response.temporary_session_id {
                Self::verify_tsid(&tsid, &master_key)?;
                tsid
            } else if let Some(ref csid) = response.encrypted_session_id {
                // RSA/csid path: decrypt private key, then decrypt session ID
                let privk_b64 = response.encrypted_rsa_private_key.as_ref().ok_or_else(|| {
                    ProviderError::ParseError("MEGA login: csid present but no privk".into())
                })?;
                let privk_encrypted = mega_base64_decode(privk_b64)?;
                let (p, q, d, _u) = decrypt_rsa_privkey(&privk_encrypted, &master_key)?;
                let sid = rsa_decrypt_csid(csid, &p, &q, &d)?;

                // Store RSA components for later use (share keys, etc.)
                self.rsa_components = Some((p, q, d, _u));
                sid
            } else {
                return Err(ProviderError::ParseError("MEGA login: no tsid or csid".into()));
            };

            self.api_client.set_session_id(Some(session_id.clone()));
            let user_info = self.get_user_info().await?;
            self.ensure_expected_email(user_info.email.as_deref())?;

            self.connected = true;
            self.current_path = "/".to_string();
            self.account_version = Some(prelogin.version);
            self.prelogin_salt = Some(salt_b64);
            self.session_id = Some(session_id);
            self.master_key = Some(master_key);
            self.user_handle = Some(response.user_handle.unwrap_or(user_info.user_handle));
            self.sequence_number = user_info.sequence_number;

            self.persist_session()?;
            Ok(())
        }.await;

        password_key.zeroize();
        if login_result.is_err() { self.clear_runtime_session(); }
        login_result
    }

    async fn login_v1(&mut self) -> Result<(), ProviderError> {
        let password = self.config.password.expose_secret();
        let mut password_key = kdf_v1(password.as_bytes())?;
        let email_hash = username_hash_v1(&self.config.email, &password_key)?;
        let uh_b64 = mega_base64_encode(&email_hash);

        let login_result: Result<(), ProviderError> = async {
            let response: MegaLoginResponseWire = self
                .command_with_retry(json!({
                    "a": "us",
                    "user": self.config.email.trim().to_lowercase(),
                    "uh": uh_b64,
                }))
                .await?;

            let enc_mk = mega_base64_decode(&response.encrypted_master_key)?;
            let enc_mk_arr = <[u8; 16]>::try_from(enc_mk.as_slice())
                .map_err(|_| ProviderError::ParseError("master key not 16 bytes".into()))?;
            let master_key = aes_ecb_decrypt_block(&enc_mk_arr, &password_key)?;

            let session_id = if let Some(tsid) = response.temporary_session_id {
                Self::verify_tsid(&tsid, &master_key)?;
                tsid
            } else if let Some(ref csid) = response.encrypted_session_id {
                let privk_b64 = response.encrypted_rsa_private_key.as_ref().ok_or_else(|| {
                    ProviderError::ParseError("MEGA v1 login: csid present but no privk".into())
                })?;
                let privk_encrypted = mega_base64_decode(privk_b64)?;
                let (p, q, d, _u) = decrypt_rsa_privkey(&privk_encrypted, &master_key)?;
                let sid = rsa_decrypt_csid(csid, &p, &q, &d)?;
                self.rsa_components = Some((p, q, d, _u));
                sid
            } else {
                return Err(ProviderError::ParseError("MEGA v1 login: no tsid or csid".into()));
            };

            self.api_client.set_session_id(Some(session_id.clone()));
            let user_info = self.get_user_info().await?;
            self.ensure_expected_email(user_info.email.as_deref())?;

            self.connected = true;
            self.current_path = "/".to_string();
            self.account_version = Some(1);
            self.session_id = Some(session_id);
            self.master_key = Some(master_key);
            self.user_handle = Some(response.user_handle.unwrap_or(user_info.user_handle));
            self.sequence_number = user_info.sequence_number;

            self.persist_session()?;
            Ok(())
        }.await;

        password_key.zeroize();
        if login_result.is_err() { self.clear_runtime_session(); }
        login_result
    }

    // ─── Node tree ────────────────────────────────────────────────────

    /// Fetch and decrypt the entire MEGA node tree.
    async fn fetch_nodes(&mut self) -> Result<(), ProviderError> {
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let response: FetchNodesResponseWire = self
            .command_with_retry(json!({ "a": "f", "c": 1, "r": 1 }))
            .await?;

        self.nodes.clear();
        self.children.clear();
        self.root_handle = None;
        self.trash_handle = None;

        let my_handle = self.user_handle.clone().unwrap_or_default();

        for raw in &response.f {
            // Special node types
            match raw.t {
                2 => { self.root_handle = Some(raw.h.clone()); }
                4 => { self.trash_handle = Some(raw.h.clone()); }
                _ => {}
            }

            // Decrypt node key
            let key = if let Some(ref k_field) = raw.k {
                self.decrypt_node_key_field(k_field, &master_key, &my_handle)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // Decrypt name from attributes
            let name = if !key.is_empty() {
                if let Some(ref attrs_b64) = raw.attrs {
                    match mega_base64_decode(attrs_b64) {
                        Ok(encrypted_attrs) => {
                            decrypt_node_attrs(&encrypted_attrs, &key)
                                .ok()
                                .and_then(|json_str| {
                                    serde_json::from_str::<Value>(&json_str).ok()
                                        .and_then(|v| v.get("n").and_then(|n| n.as_str()).map(String::from))
                                })
                                .unwrap_or_default()
                        }
                        Err(_) => String::new(),
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // Name special nodes
            let display_name = match raw.t {
                2 => "Cloud Drive".to_string(),
                3 => "Inbox".to_string(),
                4 => "Rubbish Bin".to_string(),
                _ => name,
            };

            let node = MegaNode {
                handle: raw.h.clone(),
                parent: raw.p.clone(),
                node_type: raw.t,
                name: display_name,
                size: raw.s,
                timestamp: raw.ts,
                key,
            };

            // Build children index
            self.children.entry(raw.p.clone()).or_default().push(raw.h.clone());
            self.nodes.insert(raw.h.clone(), node);
        }

        self.nodes_loaded = true;
        tracing::info!("[MEGA Native] Node tree loaded: {} nodes", self.nodes.len());
        Ok(())
    }

    /// Decrypt a node's key field ("owner:b64key" or "owner:b64key/owner2:b64key2").
    fn decrypt_node_key_field(&self, k_field: &str, master_key: &[u8; 16], my_handle: &str) -> Result<Vec<u8>, ProviderError> {
        // Try to find our own key entry first
        for entry in k_field.split('/') {
            let parts: Vec<&str> = entry.splitn(2, ':').collect();
            if parts.len() != 2 { continue; }
            let (owner, key_b64) = (parts[0], parts[1]);
            if owner == my_handle {
                let encrypted_key = mega_base64_decode(key_b64)?;
                return decrypt_node_key_xor(&encrypted_key, master_key);
            }
        }
        // Fallback: try the first entry (might work for shared nodes)
        if let Some(entry) = k_field.split('/').next() {
            let parts: Vec<&str> = entry.splitn(2, ':').collect();
            if parts.len() == 2 {
                let encrypted_key = mega_base64_decode(parts[1])?;
                return decrypt_node_key_xor(&encrypted_key, master_key);
            }
        }
        Err(ProviderError::ParseError("Could not decrypt node key".into()))
    }

    async fn ensure_nodes_loaded(&mut self) -> Result<(), ProviderError> {
        if !self.nodes_loaded {
            self.fetch_nodes().await?;
        }
        Ok(())
    }

    // ─── Path resolution ──────────────────────────────────────────────

    /// Resolve an absolute path to a node handle.
    fn resolve_path(&self, path: &str) -> Result<String, ProviderError> {
        let root = self.root_handle.as_ref()
            .ok_or(ProviderError::NotConnected)?;

        let clean_path = path.trim_matches('/');
        if clean_path.is_empty() {
            return Ok(root.clone());
        }

        let mut current_handle = root.clone();
        for component in clean_path.split('/') {
            if component.is_empty() { continue; }
            let child_handles = self.children.get(&current_handle).cloned().unwrap_or_default();
            let found = child_handles.iter().find(|h| {
                self.nodes.get(*h).map(|n| n.name == component).unwrap_or(false)
            });
            match found {
                Some(h) => current_handle = h.clone(),
                None => return Err(ProviderError::NotFound(format!("Path not found: {path}"))),
            }
        }
        Ok(current_handle)
    }

    /// Resolve parent path and extract the final name component.
    fn resolve_parent_and_name(&self, path: &str) -> Result<(String, String), ProviderError> {
        let clean = path.trim_matches('/');
        if clean.is_empty() {
            return Err(ProviderError::InvalidPath("Cannot operate on root".into()));
        }
        let last_slash = clean.rfind('/');
        let (parent_path, name) = match last_slash {
            Some(pos) => (&clean[..pos], &clean[pos + 1..]),
            None => ("", clean),
        };
        let parent_handle = self.resolve_path(&format!("/{parent_path}"))?;
        Ok((parent_handle, name.to_string()))
    }

    fn node_to_remote_entry(&self, node: &MegaNode) -> RemoteEntry {
        let modified = chrono::DateTime::from_timestamp(node.timestamp as i64, 0)
            .map(|dt| dt.to_rfc3339());

        RemoteEntry {
            name: node.name.clone(),
            path: self.build_node_path(&node.handle),
            is_dir: node.is_folder() || node.node_type >= 2,
            size: node.size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Build the full path for a node by walking up parents.
    fn build_node_path(&self, handle: &str) -> String {
        let mut parts = Vec::new();
        let mut current = handle.to_string();
        let root = self.root_handle.clone().unwrap_or_default();

        loop {
            if current == root || current.is_empty() { break; }
            if let Some(node) = self.nodes.get(&current) {
                if node.node_type >= 2 { break; } // root/inbox/trash
                parts.push(node.name.clone());
                current = node.parent.clone();
            } else {
                break;
            }
        }

        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn invalidate_nodes(&mut self) {
        self.nodes_loaded = false;
    }
}

// ─── StorageProvider implementation ───────────────────────────────────────

#[async_trait]
impl StorageProvider for MegaNativeProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn provider_type(&self) -> ProviderType { ProviderType::Mega }
    fn display_name(&self) -> String { self.config.email.clone() }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if self.try_resume_session().await? {
            return Ok(());
        }

        let prelogin: MegaPreloginWire = self
            .command_with_retry(json!({
                "a": "us0",
                "user": self.config.email.trim().to_lowercase(),
            }))
            .await?;

        let prelogin = MegaPreloginResponse { version: prelogin.version, salt: prelogin.salt };

        match prelogin.version {
            2 => self.login_v2(prelogin).await,
            1 => self.login_v1().await,
            v => Err(ProviderError::ParseError(format!("Unsupported MEGA account version {v}"))),
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        let should_clear = self.config.logout_on_disconnect.unwrap_or(false) || !self.config.save_session;
        if should_clear { self.clear_persisted_session()?; }
        self.clear_runtime_session();
        Ok(())
    }

    fn is_connected(&self) -> bool { self.connected }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let target_path = if path.is_empty() || path == "." { &self.current_path } else { path };
        let handle = self.resolve_path(target_path)?;

        let child_handles = self.children.get(&handle).cloned().unwrap_or_default();
        let mut entries = Vec::with_capacity(child_handles.len());

        for ch in &child_handles {
            if let Some(node) = self.nodes.get(ch) {
                // Skip special nodes (inbox=3, trash=4) when listing root
                if node.node_type >= 3 { continue; }
                // Skip nodes with empty names (decryption failed)
                if node.name.is_empty() { continue; }
                entries.push(self.node_to_remote_entry(node));
            }
        }

        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let target = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };
        // Normalize
        let normalized = normalize_path(&target);
        // Verify it exists and is a directory
        let handle = self.resolve_path(&normalized)?;
        if let Some(node) = self.nodes.get(&handle) {
            if node.is_file() {
                return Err(ProviderError::InvalidPath(format!("{normalized} is not a directory")));
            }
        }
        self.current_path = normalized;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        if self.current_path == "/" { return Ok(()); }
        let parent = match self.current_path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(pos) => self.current_path[..pos].to_string(),
        };
        self.current_path = parent;
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let bytes = self.download_to_bytes_with_progress(remote_path, on_progress).await?;
        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to write file: {e}")))?;
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.download_to_bytes_with_progress(remote_path, None).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to read local file: {e}")))?;

        self.ensure_nodes_loaded().await?;
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let (parent_handle, file_name) = self.resolve_parent_and_name(remote_path)?;
        let file_size = data.len() as u64;

        // Generate file key and nonce
        let file_key: [u8; 16] = rand::random();
        let nonce: [u8; 8] = rand::random();

        // Encrypt data
        let encrypted = aes_ctr_encrypt(&data, &file_key, &nonce, 0)?;

        // Compute chunk MACs for integrity
        let chunks = compute_chunk_boundaries(file_size);
        let mut chunk_macs = Vec::with_capacity(chunks.len());
        for &(offset, size) in &chunks {
            let chunk_data = &data[offset as usize..offset as usize + size];
            chunk_macs.push(chunk_mac(chunk_data, &file_key, &nonce)?);
        }
        let file_meta_mac = meta_mac(&chunk_macs, &file_key)?;

        // Request upload URL
        let upload_resp: RequestUploadUrlResponseWire = self
            .command_with_retry(json!({ "a": "u", "s": file_size, "ssl": 2 }))
            .await?;

        // Upload encrypted data in chunks
        let mut upload_handle = String::new();
        let mut uploaded = 0u64;

        for &(offset, size) in &chunks {
            let chunk = &encrypted[offset as usize..offset as usize + size];
            let url = format!("{}/{}", upload_resp.p, offset);

            let resp = self.api_client.client
                .post(&url)
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(map_reqwest_error)?;

            let body = resp.text().await
                .map_err(|e| ProviderError::TransferFailed(format!("Upload chunk failed: {e}")))?;

            uploaded += size as u64;
            if let Some(ref cb) = on_progress { cb(uploaded, file_size); }

            // Last chunk response is the upload handle
            if !body.is_empty() && !body.starts_with('-') {
                upload_handle = body;
            } else if body.starts_with('-') {
                return Err(ProviderError::TransferFailed(format!("MEGA upload error: {body}")));
            }
        }

        if upload_handle.is_empty() {
            return Err(ProviderError::TransferFailed("No upload handle received".into()));
        }

        // Pack node key and encrypt
        let packed_key = pack_node_key(&file_key, &nonce, &file_meta_mac)?;
        let encrypted_key = aes_ecb_encrypt_multi(&packed_key, &master_key)?;
        let key_b64 = mega_base64_encode(&encrypted_key);

        // Encrypt attributes
        let encrypted_attrs = encrypt_node_attrs(&file_name, &packed_key)?;
        let attrs_b64 = mega_base64_encode(&encrypted_attrs);

        // Create file node
        let _resp: PutNodesResponseWire = self.command_with_retry(json!({
            "a": "p",
            "t": parent_handle,
            "n": [{
                "h": upload_handle,
                "t": 0,
                "a": attrs_b64,
                "k": key_b64,
            }],
        })).await?;

        self.invalidate_nodes();
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let master_key = self.master_key.ok_or(ProviderError::NotConnected)?;

        let (parent_handle, folder_name) = self.resolve_parent_and_name(path)?;

        // Generate random folder key
        let folder_key: [u8; 16] = rand::random();

        // Encrypt attributes
        let encrypted_attrs = encrypt_node_attrs(&folder_name, &folder_key[..])?;
        let attrs_b64 = mega_base64_encode(&encrypted_attrs);

        // Encrypt folder key with master key
        let encrypted_key = aes_ecb_encrypt_multi(&folder_key, &master_key)?;
        let key_b64 = mega_base64_encode(&encrypted_key);

        let _resp: PutNodesResponseWire = self.command_with_retry(json!({
            "a": "p",
            "t": parent_handle,
            "n": [{
                "h": "xxxxxxxx",
                "t": 1,
                "a": attrs_b64,
                "k": key_b64,
            }],
        })).await?;

        self.invalidate_nodes();
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        // Soft delete: move to trash (same behavior as MEGAcmd and MEGA web client)
        self.move_to_trash(path).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;

        // Check if empty
        let children = self.children.get(&handle).cloned().unwrap_or_default();
        if !children.is_empty() {
            return Err(ProviderError::DirectoryNotEmpty(format!("{path} is not empty")));
        }

        // Soft delete: move to trash
        self.move_to_trash(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Soft delete: move to trash (MEGA moves the whole subtree)
        self.move_to_trash(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let from_handle = self.resolve_path(from)?;

        let from_node = self.nodes.get(&from_handle).cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("Source not found: {from}")))?;

        let (to_parent_handle, to_name) = self.resolve_parent_and_name(to)?;

        // If parent changed, move first
        if from_node.parent != to_parent_handle {
            let _: Value = self.command_with_retry(json!({
                "a": "m",
                "n": from_handle,
                "t": to_parent_handle,
            })).await?;
        }

        // If name changed, update attributes
        if from_node.name != to_name {
            let key = &from_node.key;
            if !key.is_empty() {
                let encrypted_attrs = encrypt_node_attrs(&to_name, key)?;
                let attrs_b64 = mega_base64_encode(&encrypted_attrs);

                let _: Value = self.command_with_retry(json!({
                    "a": "a",
                    "n": from_handle,
                    "attr": attrs_b64,
                })).await?;
            }
        }

        self.invalidate_nodes();
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let node = self.nodes.get(&handle)
            .ok_or_else(|| ProviderError::NotFound(format!("Not found: {path}")))?;
        Ok(self.node_to_remote_entry(node))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.ensure_nodes_loaded().await?;
        Ok(self.resolve_path(path).is_ok())
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected { return Err(ProviderError::NotConnected); }
        let info = self.get_user_info().await?;
        self.sequence_number = info.sequence_number;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected { return Err(ProviderError::NotConnected); }
        Ok(format!("MEGA Native API (auth v{})", self.account_version.unwrap_or(0)))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let quota: GetQuotaResponseWire = self
            .command_with_retry(json!({ "a": "uq", "xfer": 1, "strg": 1 }))
            .await?;

        Ok(StorageInfo {
            used: quota.cstrg,
            total: quota.mstrg,
            free: quota.mstrg.saturating_sub(quota.cstrg),
        })
    }

    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let node = self.nodes.get(&handle)
            .ok_or_else(|| ProviderError::NotFound(format!("Not found: {path}")))?;

        if node.key.is_empty() {
            return Err(ProviderError::ParseError("Cannot share: node has no key".into()));
        }

        // MEGA share link = export the node via `l` command, then build URL with file key
        // Step 1: Set share (export) on the node
        let _: Value = self.command_with_retry(json!({
            "a": "l",
            "n": handle,
            "i": self.sequence_number.clone().unwrap_or_default(),
        })).await?;

        // Step 2: Build the link with the file/folder key
        // For files: key is 32 bytes (file_key + nonce + metamac), export key = file_key
        // For folders: key is 16 bytes, export key = folder_key
        let export_key = if node.key.len() == 32 {
            // File: export all 32 bytes (MEGA web client uses the full compound key)
            mega_base64_encode(&node.key)
        } else {
            mega_base64_encode(&node.key)
        };

        let link_type = if node.is_file() { "file" } else { "folder" };
        let _ = &options; // acknowledge usage
        Ok(ShareLinkResult { url: format!("https://mega.nz/{}/{}#{}", link_type, handle, export_key), password: None, expires_at: None })
    }
}

/// MEGA-specific methods (trash management, share links).
impl MegaNativeProvider {
    /// Move a file or directory to the MEGA rubbish bin (soft delete via `m` command).
    pub async fn move_to_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let handle = self.resolve_path(path)?;
        let trash = self.trash_handle.clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let _: Value = self.command_with_retry(json!({
            "a": "m",
            "n": handle,
            "t": trash,
        })).await?;

        self.invalidate_nodes();
        Ok(())
    }

    /// List items in the MEGA rubbish bin.
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self.trash_handle.clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let mut entries = Vec::new();
        for ch in &child_handles {
            if let Some(node) = self.nodes.get(ch) {
                if !node.name.is_empty() {
                    entries.push(self.node_to_remote_entry(node));
                }
            }
        }
        Ok(entries)
    }

    /// Restore an item from rubbish bin to a destination path.
    pub async fn restore_from_trash(&mut self, filename: &str, dest: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self.trash_handle.clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        // Find the node in trash by name
        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let node_handle = child_handles.iter().find(|h| {
            self.nodes.get(*h).map(|n| n.name == filename).unwrap_or(false)
        }).cloned().ok_or_else(|| ProviderError::NotFound(format!("{filename} not found in trash")))?;

        let dest_handle = self.resolve_path(dest)?;

        let _: Value = self.command_with_retry(json!({
            "a": "m",
            "n": node_handle,
            "t": dest_handle,
        })).await?;

        self.invalidate_nodes();
        Ok(())
    }

    /// Permanently delete an item from the rubbish bin.
    pub async fn permanent_delete_from_trash(&mut self, filename: &str) -> Result<(), ProviderError> {
        self.ensure_nodes_loaded().await?;
        let trash = self.trash_handle.clone()
            .ok_or_else(|| ProviderError::NotFound("Trash handle not found".into()))?;

        let child_handles = self.children.get(&trash).cloned().unwrap_or_default();
        let node_handle = child_handles.iter().find(|h| {
            self.nodes.get(*h).map(|n| n.name == filename).unwrap_or(false)
        }).cloned().ok_or_else(|| ProviderError::NotFound(format!("{filename} not found in trash")))?;

        let _: Value = self.command_with_retry(json!({
            "a": "d",
            "n": node_handle,
        })).await?;

        self.invalidate_nodes();
        Ok(())
    }
}

impl MegaNativeProvider {
    /// Download with optional progress callback.
    async fn download_to_bytes_with_progress(
        &mut self,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<Vec<u8>, ProviderError> {
        self.ensure_nodes_loaded().await?;

        let handle = self.resolve_path(remote_path)?;
        let node = self.nodes.get(&handle).cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("Not found: {remote_path}")))?;

        if !node.is_file() {
            return Err(ProviderError::InvalidPath(format!("{remote_path} is not a file")));
        }

        if node.key.len() != 32 {
            return Err(ProviderError::ParseError(format!(
                "File node key invalid length: {} (need 32)", node.key.len()
            )));
        }

        let packed_key: [u8; 32] = node.key[..32].try_into()
            .map_err(|_| ProviderError::ParseError("key conversion failed".into()))?;
        let (file_key, nonce) = unpack_node_key(&packed_key)?;

        // Get download URL
        let dl_resp: GetDownloadUrlResponseWire = self
            .command_with_retry(json!({ "a": "g", "g": 1, "n": handle }))
            .await?;

        let total_size = dl_resp.s;

        // Download encrypted data with progress
        let response = self.api_client.client
            .get(&dl_resp.g)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download HTTP {}", response.status()
            )));
        }

        let mut encrypted = Vec::with_capacity(total_size as usize);
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {e}"))
            })?;
            encrypted.extend_from_slice(&chunk);
            if let Some(ref cb) = on_progress {
                cb(encrypted.len() as u64, total_size);
            }
        }

        // Decrypt
        let decrypted = aes_ctr_decrypt(&encrypted, &file_key, &nonce, 0)?;

        // Truncate to actual file size (MEGA pads to AES block boundary)
        let actual_size = node.size as usize;
        if decrypted.len() >= actual_size {
            Ok(decrypted[..actual_size].to_vec())
        } else {
            Ok(decrypted)
        }
    }
}

// ─── Path utilities ───────────────────────────────────────────────────────

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => { parts.pop(); }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{decode_command_response, map_mega_error_code, normalize_path};
    use crate::providers::ProviderError;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct SampleResponse {
        ok: i32,
    }

    #[test]
    fn decode_command_response_maps_negative_codes() {
        let err = decode_command_response::<SampleResponse>(json!(-9)).unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
    }

    #[test]
    fn decode_command_response_deserializes_objects() {
        let response = decode_command_response::<SampleResponse>(json!({ "ok": 1 })).unwrap();
        assert_eq!(response.ok, 1);
    }

    #[test]
    fn mega_error_code_mapping_keeps_auth_specific_errors() {
        let err = map_mega_error_code(-14);
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
    }

    #[test]
    fn verify_tsid_rejects_invalid_lengths() {
        let err = super::MegaNativeProvider::verify_tsid("AA", &[0u8; 16]).unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
    }

    #[test]
    fn normalize_path_basic() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/a/b/c"), "/a/b/c");
        assert_eq!(normalize_path("/a/../b"), "/b");
        assert_eq!(normalize_path("/a/./b"), "/a/b");
        assert_eq!(normalize_path("/a/b/.."), "/a");
        assert_eq!(normalize_path("//a///b//"), "/a/b");
    }
}
