//! Connection pool for MCP server
//!
//! Reuses StorageProvider connections across tool calls instead of
//! creating a new TCP/TLS/SSH connection for each request (~200ms-5s saved).
//!
//! - `Arc<Mutex<Box<dyn StorageProvider>>>` because the trait uses `&mut self`
//! - Idle timeout eviction (default 5 min)
//! - Periodic cleanup in the server main loop

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::credential_store::CredentialStore;
use crate::profile_loader::{apply_profile_options, apply_s3_profile_defaults};
use crate::providers::{ProviderConfig, ProviderFactory, ProviderType, StorageProvider};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// A pooled connection with last-used timestamp (millis since pool creation)
/// and usage counters.
///
/// `last_used` was previously a `Mutex<Instant>` — meaning every pool read
/// serialized against every pool write across ALL pooled profiles. Replaced
/// with `AtomicU64` so hot-path reads are lock-free.
struct PooledConnection {
    provider: Arc<Mutex<Box<dyn StorageProvider>>>,
    last_used_ms: AtomicU64,
    profile_name: String,
    protocol: String,
    connected_at: DateTime<Utc>,
    requests_served: AtomicU64,
}

/// Process-wide monotonic anchor used to convert `Instant` to a 64-bit
/// millisecond delta that fits in `AtomicU64`. Cheaper than a system time
/// conversion for every pool operation.
static POOL_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

#[inline]
fn now_ms() -> u64 {
    Instant::now().duration_since(*POOL_EPOCH).as_millis() as u64
}

/// Connection pool keyed by profile ID.
pub struct ConnectionPool {
    connections: Mutex<HashMap<String, PooledConnection>>,
    max_connections: usize,
    idle_timeout: Duration,
}

impl ConnectionPool {
    pub fn new(max_connections: usize, idle_timeout: Duration) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            max_connections,
            idle_timeout,
        }
    }

    /// Maximum number of simultaneous pooled connections.
    pub fn max_size(&self) -> usize {
        self.max_connections
    }

    /// Idle timeout applied to each pooled connection.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Get a cloned Arc to the provider Mutex for the given server.
    /// Reuses a pooled connection if available, otherwise creates a new one.
    /// The returned Arc can be locked independently of the pool's connections lock.
    pub async fn get_provider(
        &self,
        server_query: &str,
    ) -> Result<Arc<Mutex<Box<dyn StorageProvider>>>, String> {
        let profile_id = resolve_profile_id(server_query)?;

        // Check for existing pooled connection
        {
            let conns = self.connections.lock().await;
            if let Some(entry) = conns.get(&profile_id) {
                entry.last_used_ms.store(now_ms(), Ordering::Relaxed);
                entry.requests_served.fetch_add(1, Ordering::Relaxed);
                return Ok(Arc::clone(&entry.provider));
            }
        }

        // Create new connection
        let (provider, name, protocol) = create_provider_from_vault(server_query)?;
        let mut connected = provider;
        connected.connect().await.map_err(|e| {
            // Sanitize connection errors to prevent credential leakage to AI clients
            let safe_msg = crate::providers::sanitize_api_error(&e.to_string());
            format!("Connection to '{}' failed: {}", server_query, safe_msg)
        })?;

        let arc = Arc::new(Mutex::new(connected));

        let entry = PooledConnection {
            provider: Arc::clone(&arc),
            last_used_ms: AtomicU64::new(now_ms()),
            profile_name: name,
            protocol,
            connected_at: Utc::now(),
            requests_served: AtomicU64::new(1),
        };

        // Evict oldest if at capacity. Candidates are selected inside the map
        // lock but the actual `disconnect().await` is done outside — otherwise
        // one hung SFTP provider freezes every pool read.
        let victim = {
            let mut conns = self.connections.lock().await;
            if conns.len() >= self.max_connections {
                pick_lru_victim(&conns).and_then(|id| conns.remove(&id))
            } else {
                None
            }
        };
        if let Some(entry) = victim {
            disconnect_outside_lock(entry).await;
        }

        let mut conns = self.connections.lock().await;
        conns.insert(profile_id, entry);

        Ok(arc)
    }

    /// Invalidate a pooled connection after a transport-level error without
    /// blocking on its graceful disconnect.
    ///
    /// This is the recovery path for MCP tool calls that hit "Data connection
    /// is already open", "broken pipe", `NotConnected`, etc. The pool entry
    /// is removed synchronously so the next `get_provider()` call opens a
    /// fresh connection, and the old provider's `disconnect()` is best-effort
    /// in a detached task — we do not want a hung FTP socket to stall the
    /// retry.
    ///
    /// Returns the profile name that was evicted, or `None` if nothing matched.
    pub async fn invalidate(&self, server_query: &str) -> Option<String> {
        let entry = {
            let mut conns = self.connections.lock().await;
            let matched_id = self.match_entry_id(&conns, server_query);
            match matched_id {
                Some(id) => conns.remove(&id),
                None => None,
            }
        }?;
        let name = entry.profile_name.clone();
        // Fire-and-forget disconnect. A previously broken connection can take
        // tens of seconds to error out on .disconnect(); awaiting it would
        // defeat the purpose of fast recovery.
        tokio::spawn(async move {
            disconnect_outside_lock(entry).await;
        });
        Some(name)
    }

    /// Resolve `server_query` to the matching pool entry id, matching by id
    /// (case-sensitive) then profile name (case-insensitive equal, then
    /// case-insensitive substring). Shared between `close_one` and
    /// `invalidate` so lookups stay consistent.
    fn match_entry_id(
        &self,
        conns: &HashMap<String, PooledConnection>,
        server_query: &str,
    ) -> Option<String> {
        let query_lower = server_query.to_lowercase();
        conns
            .iter()
            .find(|(id, entry)| {
                id.as_str() == server_query || entry.profile_name.to_lowercase() == query_lower
            })
            .map(|(id, _)| id.clone())
            .or_else(|| {
                conns
                    .iter()
                    .find(|(_, entry)| entry.profile_name.to_lowercase().contains(&query_lower))
                    .map(|(id, _)| id.clone())
            })
    }

    /// Explicitly close a single pooled connection. Returns the profile name
    /// that was evicted, or `None` if no connection matched.
    ///
    /// Accepts either the profile id or the profile name (case-insensitive).
    pub async fn close_one(&self, server_query: &str) -> Option<String> {
        let entry = {
            let mut conns = self.connections.lock().await;
            let id = self.match_entry_id(&conns, server_query)?;
            conns.remove(&id)?
        };
        let name = entry.profile_name.clone();
        // Disconnect outside the pool lock so a hung network does not stall
        // every other pool operation.
        disconnect_outside_lock(entry).await;
        Some(name)
    }

    /// Remove idle connections older than the timeout. Entries currently in
    /// use (strong_count > 1 on the provider Arc) are spared — otherwise a
    /// long-running upload could be disconnected mid-transfer. This is the
    /// same invariant used by `r2d2`/`bb8`.
    pub async fn evict_idle(&self) {
        let timeout = self.idle_timeout;
        let victims = {
            let mut conns = self.connections.lock().await;
            let now_ms = now_ms();
            let timeout_ms = timeout.as_millis() as u64;
            let victim_ids: Vec<String> = conns
                .iter()
                .filter_map(|(id, entry)| {
                    let last_ms = entry.last_used_ms.load(Ordering::Relaxed);
                    let idle_ms = now_ms.saturating_sub(last_ms);
                    if idle_ms > timeout_ms && Arc::strong_count(&entry.provider) == 1 {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            victim_ids
                .into_iter()
                .filter_map(|id| conns.remove(&id))
                .collect::<Vec<_>>()
        };

        // Disconnect outside the lock.
        for entry in victims {
            let name = entry.profile_name.clone();
            disconnect_outside_lock(entry).await;
            eprintln!("[mcp-pool] evicted idle connection: {}", name);
        }
    }

    /// Get pool status for the `aeroftp://connections` resource.
    ///
    /// Exposes the pooled connection set with full metadata: profile id,
    /// name, protocol, idle time, connected_at timestamp, and the running
    /// request counter. Agents can use this to plan cache-friendly call
    /// orderings and decide when to issue `aeroftp_close_connection`.
    pub async fn status(&self) -> Vec<serde_json::Value> {
        let conns = self.connections.lock().await;
        let mut result = Vec::new();
        let now_ms = now_ms();
        for (id, entry) in conns.iter() {
            let last_ms = entry.last_used_ms.load(Ordering::Relaxed);
            let idle_secs = now_ms.saturating_sub(last_ms) / 1000;
            let requests_served = entry.requests_served.load(Ordering::Relaxed);
            let in_use = Arc::strong_count(&entry.provider) > 1;
            let state = if in_use { "busy" } else { "idle" };
            result.push(serde_json::json!({
                "profile_id": id,
                "name": entry.profile_name,
                "protocol": entry.protocol,
                "state": state,
                "idle_secs": idle_secs,
                "connected_at": entry.connected_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "requests_served": requests_served,
            }));
        }
        result
    }
}

/// Pick the LRU entry that is not currently in use.
fn pick_lru_victim(conns: &HashMap<String, PooledConnection>) -> Option<String> {
    conns
        .iter()
        .filter(|(_, entry)| Arc::strong_count(&entry.provider) == 1)
        .min_by_key(|(_, entry)| entry.last_used_ms.load(Ordering::Relaxed))
        .map(|(id, _)| id.clone())
}

/// Drop a removed pool entry with an awaited disconnect. Must be called
/// OUTSIDE `self.connections.lock()` — the provider's `.disconnect().await`
/// can take seconds on stalled networks.
async fn disconnect_outside_lock(entry: PooledConnection) {
    // Arc::try_unwrap lets us get sole ownership of the provider when no
    // caller is still using it. If someone is (strong_count > 1), just drop
    // our reference and let the last holder clean up naturally.
    match Arc::try_unwrap(entry.provider) {
        Ok(mutex) => {
            let mut provider = mutex.into_inner();
            let _ = provider.disconnect().await;
        }
        Err(_arc) => {
            // Another caller still holds it; they own the disconnect lifecycle.
        }
    }
}

/// Resolve a server query (name, ID, or substring) to a profile ID.
fn resolve_profile_id(server_query: &str) -> Result<String, String> {
    let store = CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open. Cannot connect to server.".to_string())?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    let query_lower = server_query.to_lowercase();
    let matched = profiles
        .iter()
        .find(|p| {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
            name == query_lower || id == server_query
        })
        .or_else(|| {
            profiles.iter().find(|p| {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                name.contains(&query_lower)
            })
        })
        .ok_or_else(|| format!("Server '{}' not found in saved profiles", server_query))?;

    Ok(matched
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(server_query)
        .to_string())
}

/// Create a StorageProvider from vault credentials. Supports all non-OAuth2 protocols
/// plus OAuth2 providers when valid tokens exist in the vault.
///
/// Returns the provider, the profile name and the profile's protocol label
/// (upper-case) so the pool can surface it via `aeroftp://connections`.
fn create_provider_from_vault(
    server_query: &str,
) -> Result<(Box<dyn StorageProvider>, String, String), String> {
    let store = CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open. Cannot connect to server.".to_string())?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    let query_lower = server_query.to_lowercase();
    let matched = profiles
        .iter()
        .find(|p| {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
            name == query_lower || id == server_query
        })
        .or_else(|| {
            profiles.iter().find(|p| {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                name.contains(&query_lower)
            })
        })
        .ok_or_else(|| format!("Server '{}' not found in saved profiles", server_query))?;

    let profile_id = matched.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let profile_name = matched
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed");
    let protocol = matched
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let host = matched.get("host").and_then(|v| v.as_str()).unwrap_or("");
    let port = matched.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let username = matched
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let initial_path = matched
        .get("initialPath")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    // Load the credential blob. The GUI stores either a raw password string or a
    // JSON object with {username, password, access_token, ...}. The S3 bucket
    // and provider-specific options live in the profile's `options` field, not
    // in the credential blob.
    let raw_cred = store
        .get(&format!("server_{}", profile_id))
        .unwrap_or_default();

    let (resolved_username, password) =
        if let Ok(cred_val) = serde_json::from_str::<serde_json::Value>(&raw_cred) {
            if let Some(obj) = cred_val.as_object() {
                let u = obj
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let p = obj
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| obj.get("access_token").and_then(|v| v.as_str()))
                    .or_else(|| obj.get("api_key").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                (
                    if u.is_empty() {
                        username.to_string()
                    } else {
                        u
                    },
                    p,
                )
            } else {
                (username.to_string(), raw_cred.trim_matches('"').to_string())
            }
        } else {
            (username.to_string(), raw_cred)
        };

    let username: &str = &resolved_username;

    // Build extra options from the profile (bucket, region, endpoint, etc.).
    // This mirrors how the CLI resolves S3 profile defaults — the vault copy
    // alone does not carry bucket/region because they live in `profile.options`.
    let mut extra: HashMap<String, String> = HashMap::new();
    apply_profile_options(&mut extra, matched);

    let provider_type = match protocol.to_uppercase().as_str() {
        "FTP" => ProviderType::Ftp,
        "FTPS" => ProviderType::Ftps,
        "SFTP" => ProviderType::Sftp,
        "WEBDAV" | "WEBDAVS" => ProviderType::WebDav,
        "S3" => ProviderType::S3,
        "GITHUB" => ProviderType::GitHub,
        "GITLAB" => ProviderType::GitLab,
        "MEGA" => ProviderType::Mega,
        "AZURE" => ProviderType::Azure,
        "FILEN" => ProviderType::Filen,
        "INTERNXT" => ProviderType::Internxt,
        "KDRIVE" => ProviderType::KDrive,
        "JOTTACLOUD" => ProviderType::Jottacloud,
        "DRIMECLOUD" | "DRIME" => ProviderType::DrimeCloud,
        "FILELU" => ProviderType::FileLu,
        "KOOFR" => ProviderType::Koofr,
        "OPENDRIVE" => ProviderType::OpenDrive,
        "YANDEXDISK" | "YANDEX" => ProviderType::YandexDisk,
        "SWIFT" => ProviderType::Swift,
        // OAuth2 providers — only if token is present
        "GOOGLEDRIVE" | "GOOGLE_DRIVE" => ProviderType::GoogleDrive,
        "DROPBOX" => ProviderType::Dropbox,
        "ONEDRIVE" => ProviderType::OneDrive,
        "BOX" => ProviderType::Box,
        "PCLOUD" => ProviderType::PCloud,
        "ZOHOWORKDRIVE" | "ZOHO" => ProviderType::ZohoWorkdrive,
        "FOURSHARED" | "4SHARED" => ProviderType::FourShared,
        other => {
            return Err(format!(
                "Protocol '{}' on server '{}' is not yet supported via MCP. \
                 Supported: FTP, FTPS, SFTP, WebDAV, S3, GitHub, GitLab, MEGA, Azure, \
                 Filen, Internxt, kDrive, Jottacloud, DrimeCloud, FileLu, Koofr, \
                 OpenDrive, YandexDisk, Swift. OAuth2 providers (Google Drive, Dropbox, \
                 OneDrive, Box, pCloud, Zoho) require valid tokens in vault.",
                other, profile_name
            ));
        }
    };

    // For OAuth2 providers, check that we have a valid token
    if (provider_type.requires_oauth2() || matches!(provider_type, ProviderType::FourShared))
        && password.is_empty()
    {
        return Err(format!(
            "OAuth2 provider '{}' on server '{}' requires a valid access token. \
             Re-authenticate via the AeroFTP GUI first, then retry.",
            protocol, profile_name
        ));
    }

    // Azure: GUI stores container as "bucket" in options; map to "container".
    if provider_type == ProviderType::Azure && !extra.contains_key("container") {
        if let Some(bucket) = extra.remove("bucket") {
            extra.insert("container".to_string(), bucket);
        }
    }

    // S3: resolve preset defaults (region, path_style, endpoint) so that
    // providers like Storj/Cloudflare R2/Wasabi receive a valid config even
    // when the profile only stores the bucket name + provider id.
    let mut resolved_host = host.to_string();
    if provider_type == ProviderType::S3 {
        let provider_id = matched.get("providerId").and_then(|v| v.as_str());
        if let Some(resolved_endpoint) = apply_s3_profile_defaults(&mut extra, provider_id) {
            if resolved_host.trim().is_empty() {
                resolved_host = resolved_endpoint;
            }
        }
    }

    // Mega: default to native protocol.
    if provider_type == ProviderType::Mega && !extra.contains_key("mega_mode") {
        extra.insert("mega_mode".to_string(), "native".to_string());
    }

    let config = ProviderConfig {
        name: profile_name.to_string(),
        provider_type,
        host: resolved_host,
        port: if port > 0 { Some(port) } else { None },
        username: if username.is_empty() {
            None
        } else {
            Some(username.to_string())
        },
        password: if password.is_empty() {
            None
        } else {
            Some(password)
        },
        initial_path: Some(initial_path.to_string()),
        extra,
    };

    let provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider for '{}': {}", profile_name, e))?;

    Ok((provider, profile_name.to_string(), protocol.to_uppercase()))
}
