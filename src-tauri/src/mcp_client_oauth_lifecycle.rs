//! Backend-only OAuth lifecycle. The loopback listener and settings commands drive it;
//! there is no automatic refresh and no frontend access to pending state or tokens.
// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::mcp_client_commands;
use crate::mcp_client_http_config::ResolvedMcpHttpAuth;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 32;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct OAuthBinding {
    endpoint: String,
    user_id: i64,
    server_id: String,
    effective_revision: String,
    issuer: String,
    resource: String,
    client_id: String,
    redirect_uri: String,
}
impl fmt::Debug for OAuthBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthBinding { [redacted] }")
    }
}
impl OAuthBinding {
    fn new(
        config: &McpHttpServerConfig,
        key: &[u8; 32],
        pending: &PendingAuthorization,
    ) -> Result<Self, OAuthError> {
        let revision = config
            .resolve_with(key, pending.user_id, |_| Err(()))
            .map_err(|_| OAuthError::StaleBinding)?
            .effective_revision;
        Ok(Self {
            endpoint: config.endpoint.clone(),
            user_id: pending.user_id,
            server_id: config.id.clone(),
            effective_revision: revision,
            issuer: pending.issuer.clone(),
            resource: pending.resource.clone(),
            client_id: pending.client_id.clone(),
            redirect_uri: pending.redirect_uri.clone(),
        })
    }
    fn check(&self, conn: &Connection, root: &[u8; 32]) -> Result<(), OAuthError> {
        let config = live_config(conn, root, self.user_id, &self.server_id)?;
        let revision = config
            .resolve_with(root, self.user_id, |_| Err(()))
            .map_err(|_| OAuthError::StaleBinding)?
            .effective_revision;
        if revision != self.effective_revision
            || self.endpoint != config.endpoint
            || self.resource
                != parse_public_https(&config.endpoint, 2048)
                    .map_err(|_| OAuthError::StaleBinding)?
                    .as_str()
        {
            return Err(OAuthError::StaleBinding);
        }
        parse_public_https(&self.issuer, 2048).map_err(|_| OAuthError::StaleBinding)?;
        validate_redirect_uri(&self.redirect_uri)?;
        if self.client_id.is_empty()
            || self.client_id.len() > 512
            || self.client_id.chars().any(char::is_control)
        {
            return Err(OAuthError::StaleBinding);
        }
        match configured_client(&config) {
            Some(client) if client != self.client_id => Err(OAuthError::StaleBinding),
            _ => Ok(()),
        }
    }
}
fn configured_client(config: &McpHttpServerConfig) -> Option<&str> {
    match &config.auth {
        McpHttpAuth::OAuth {
            client_id,
            client_id_metadata_url,
        } => client_id.as_deref().or(client_id_metadata_url.as_deref()),
        _ => None,
    }
}
pub(crate) fn live_config(
    conn: &Connection,
    root: &[u8; 32],
    user: i64,
    id: &str,
) -> Result<McpHttpServerConfig, OAuthError> {
    if user <= 0
        || user_partitions::active_user_id(conn).map_err(|_| OAuthError::StoreUnavailable)?
            != Some(user)
    {
        return Err(OAuthError::UserChanged);
    }
    let value =
        user_partitions::get_user_setting_for(conn, root, user, "aeroagent_mcp_http_servers")
            .map_err(|_| OAuthError::StoreUnavailable)?
            .ok_or(OAuthError::StaleBinding)?;
    let configs: Vec<McpHttpServerConfig> =
        serde_json::from_value(value).map_err(|_| OAuthError::StaleBinding)?;
    let mut ids = BTreeSet::new();
    if configs.len() > 32
        || configs
            .iter()
            .any(|c| c.validate().is_err() || !ids.insert(&c.id))
    {
        return Err(OAuthError::StaleBinding);
    }
    let config = configs
        .into_iter()
        .find(|c| c.id == id)
        .ok_or(OAuthError::StaleBinding)?;
    if !config.enabled || !matches!(config.auth, McpHttpAuth::OAuth { .. }) {
        return Err(OAuthError::StaleBinding);
    }
    Ok(config)
}

struct Record {
    pending: PendingAuthorization,
    binding: OAuthBinding,
    server: AuthorizationServer,
    deadline: Instant,
    cancel: CancellationToken,
    token_snapshot: Option<String>,
}
#[derive(Default)]
pub(crate) struct PendingAuthorizationManager {
    records: Mutex<HashMap<String, Record>>,
    active: Mutex<HashMap<String, Active>>,
    // Interactive and refresh operations, cancelled together with their bindings
    // before any pending record exists (discovery) and after it is consumed.
    operations: Mutex<HashMap<String, Active>>,
}
static SHARED: LazyLock<PendingAuthorizationManager> =
    LazyLock::new(PendingAuthorizationManager::default);
/// The one process-wide manager. Vault lock, partition lock and user switch
/// invalidate it from code paths that hold no AppHandle.
pub(crate) fn shared() -> &'static PendingAuthorizationManager {
    &SHARED
}
pub(crate) struct OperationLease<'a> {
    manager: &'a PendingAuthorizationManager,
    id: String,
}
impl Drop for OperationLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut operations) = self.manager.operations.lock() {
            operations.remove(&self.id);
        }
    }
}
struct Active {
    user: i64,
    server: String,
    cancel: CancellationToken,
}
struct Finish<'a> {
    manager: &'a PendingAuthorizationManager,
    handle: &'a str,
}
impl Drop for Finish<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.manager.active.lock() {
            active.remove(self.handle);
        }
    }
}
impl PendingAuthorizationManager {
    fn insert(&self, record: Record) -> Result<String, OAuthError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        records.retain(|_, r| r.deadline > Instant::now() && !r.cancel.is_cancelled());
        // A consumed exchange or refresh keeps its lease until completion. Do
        // not supersede it: its eventual error must not cancel a newer attempt.
        let active = self
            .active
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        if active.values().any(|attempt| {
            attempt.user == record.binding.user_id && attempt.server == record.binding.server_id
        }) {
            return Err(OAuthError::PendingUnavailable);
        }
        drop(active);
        // Only a still-pending browser attempt may be superseded.
        records.retain(|_, r| {
            let keep = r.binding.user_id != record.binding.user_id
                || r.binding.server_id != record.binding.server_id;
            if !keep {
                r.cancel.cancel();
            }
            keep
        });
        if records.len() >= MAX_PENDING {
            return Err(OAuthError::PendingUnavailable);
        }
        let handle = random_urlsafe().to_string();
        records.insert(handle.clone(), record);
        Ok(handle)
    }
    fn take(&self, handle: &str) -> Result<Record, OAuthError> {
        // Remove under one mutex before any callback validation or await.
        let mut records = self
            .records
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        let record = records
            .remove(handle)
            .ok_or(OAuthError::PendingUnavailable)?;
        if record.deadline <= Instant::now() || record.cancel.is_cancelled() {
            return Err(OAuthError::PendingUnavailable);
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        if active.len() >= MAX_PENDING {
            return Err(OAuthError::PendingUnavailable);
        }
        active.insert(
            handle.to_owned(),
            Active {
                user: record.binding.user_id,
                server: record.binding.server_id.clone(),
                cancel: record.cancel.clone(),
            },
        );
        Ok(record)
    }
    fn lease_refresh(
        &self,
        user: i64,
        server: &str,
        cancel: CancellationToken,
    ) -> Result<String, OAuthError> {
        let records = self
            .records
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        if records.values().any(|r| {
            r.binding.user_id == user
                && r.binding.server_id == server
                && r.deadline > Instant::now()
                && !r.cancel.is_cancelled()
        }) {
            return Err(OAuthError::PendingUnavailable);
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        if active.len() >= MAX_PENDING
            || active
                .values()
                .any(|a| a.user == user && a.server == server)
        {
            return Err(OAuthError::PendingUnavailable);
        }
        let handle = random_urlsafe().to_string();
        active.insert(
            handle.clone(),
            Active {
                user,
                server: server.to_owned(),
                cancel,
            },
        );
        Ok(handle)
    }
    fn invalidate_error(&self, user: i64, server: &str, error: &OAuthError) {
        match error {
            OAuthError::UserChanged | OAuthError::Locked => self.invalidate_all(),
            _ => self.invalidate(user, Some(server)),
        }
    }
    /// A new interactive authorization supersedes (cancels) an older operation
    /// for the same binding; a refresh never supersedes and is refused instead.
    pub(crate) fn register_operation(
        &self,
        user: i64,
        server: &str,
        cancel: CancellationToken,
        supersede: bool,
    ) -> Result<OperationLease<'_>, OAuthError> {
        let mut operations = self
            .operations
            .lock()
            .map_err(|_| OAuthError::PendingUnavailable)?;
        operations.retain(|_, op| !op.cancel.is_cancelled());
        let same = |op: &Active| op.user == user && op.server == server;
        if !supersede && operations.values().any(same) {
            return Err(OAuthError::PendingUnavailable);
        }
        for op in operations.values().filter(|op| same(op)) {
            op.cancel.cancel();
        }
        operations.retain(|_, op| !same(op));
        if operations.len() >= MAX_PENDING {
            return Err(OAuthError::PendingUnavailable);
        }
        let id = random_urlsafe().to_string();
        operations.insert(
            id.clone(),
            Active {
                user,
                server: server.to_owned(),
                cancel,
            },
        );
        Ok(OperationLease { manager: self, id })
    }
    /// Cancellation of one still-pending record, for the listener that owns it.
    pub(crate) fn pending_cancel(&self, handle: &str) -> Option<CancellationToken> {
        let records = self.records.lock().ok()?;
        records.get(handle).map(|record| record.cancel.clone())
    }
    /// Lets the backend listener ignore stray local requests without consuming
    /// the record. Only an exact, constant-time state match reaches `complete`.
    pub(crate) fn pending_state_matches(&self, handle: &str, state: &str) -> bool {
        use subtle::ConstantTimeEq;
        let Ok(records) = self.records.lock() else {
            return false;
        };
        records.get(handle).is_some_and(|record| {
            record.deadline > Instant::now()
                && !record.cancel.is_cancelled()
                && bool::from(record.pending.state.as_bytes().ct_eq(state.as_bytes()))
        })
    }
    /// Drops one unconsumed record after its listener has ended.
    pub(crate) fn discard(&self, handle: &str) {
        if let Ok(mut records) = self.records.lock() {
            if let Some(record) = records.remove(handle) {
                record.cancel.cancel();
            }
        }
    }
    fn cancel_operations(&self, user: Option<i64>, server: Option<&str>) {
        if let Ok(operations) = self.operations.lock() {
            for op in operations.values() {
                if user.is_none_or(|user| user == op.user)
                    && server.is_none_or(|server| server == op.server)
                {
                    op.cancel.cancel();
                }
            }
        }
    }
    pub(crate) fn invalidate(&self, user: i64, server: Option<&str>) {
        self.cancel_operations(Some(user), server);
        if let Ok(mut records) = self.records.lock() {
            records.retain(|_, r| {
                let remove =
                    r.binding.user_id == user && server.is_none_or(|s| s == r.binding.server_id);
                if remove {
                    r.cancel.cancel();
                }
                !remove
            });
            if let Ok(active) = self.active.lock() {
                for attempt in active.values() {
                    if attempt.user == user && server.is_none_or(|target| target == attempt.server)
                    {
                        attempt.cancel.cancel();
                    }
                }
            }
        }
    }
    pub(crate) fn invalidate_all(&self) {
        self.cancel_operations(None, None);
        if let Ok(mut records) = self.records.lock() {
            for record in records.values() {
                record.cancel.cancel();
            }
            records.clear();
            if let Ok(active) = self.active.lock() {
                for attempt in active.values() {
                    attempt.cancel.cancel();
                }
            }
        }
    }
}
fn callback(pending: &PendingAuthorization, raw: &str) -> Result<Zeroizing<String>, OAuthError> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) {
        return Err(OAuthError::InvalidCallback);
    }
    let url = Url::parse(raw).map_err(|_| OAuthError::InvalidCallback)?;
    let redirect = Url::parse(&pending.redirect_uri).map_err(|_| OAuthError::InvalidCallback)?;
    if url.origin() != redirect.origin()
        || url.path() != redirect.path()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OAuthError::InvalidCallback);
    }
    let mut fields = HashMap::new();
    for (key, value) in url.query_pairs() {
        if fields.len() >= 16
            || fields
                .insert(key.into_owned(), Zeroizing::new(value.into_owned()))
                .is_some()
        {
            return Err(OAuthError::InvalidCallback);
        }
    }
    let state = fields.get("state").ok_or(OAuthError::InvalidCallback)?;
    let issuer = fields.get("iss").map(|s| s.as_str());
    // Denials must also carry valid state and issuer; no error text is retained.
    if let Some(error) = fields.get("error") {
        if fields.contains_key("code")
            || error.is_empty()
            || error.len() > 128
            || error.chars().any(char::is_control)
        {
            return Err(OAuthError::InvalidCallback);
        }
        validate_callback(pending, state, issuer, "denial")?;
        return Err(OAuthError::Denied);
    }
    let code = fields.get("code").ok_or(OAuthError::InvalidCallback)?;
    validate_callback(pending, state, issuer, code)?;
    Ok(Zeroizing::new(code.to_string()))
}
fn keys(server: &str) -> [String; 3] {
    let id = blake3::hash(server.as_bytes()).to_hex();
    [
        format!("mcp_oauth_access_{id}"),
        format!("mcp_oauth_refresh_{id}"),
        format!("mcp_oauth_meta_{id}"),
    ]
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    binding: OAuthBinding,
    generation: String,
    expires_at: Option<i64>,
    scopes: Vec<String>,
}

/// Used inside the caller's Immediate catalog transaction on disable/remove/rebind.
/// Deletion intentionally needs no unlocked key; it is scoped to the supplied user.
pub(crate) fn cleanup_in_transaction(
    conn: &Connection,
    user: i64,
    server: &str,
) -> Result<(), OAuthError> {
    if conn.is_autocommit() {
        return Err(OAuthError::StoreUnavailable);
    }
    let [access, refresh, meta] = keys(server);
    user_partitions::delete_user_credential_for(conn, user, &access)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    user_partitions::delete_user_credential_for(conn, user, &refresh)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    user_partitions::delete_user_setting_for(conn, user, &meta)
        .map_err(|_| OAuthError::StoreUnavailable)
}
#[cfg(test)]
pub(crate) fn cleanup(conn: &mut Connection, user: i64, server: &str) -> Result<(), OAuthError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    cleanup_in_transaction(&tx, user, server)?;
    tx.commit().map_err(|_| OAuthError::StoreUnavailable)
}
fn read(
    conn: &Connection,
    root: &[u8; 32],
    user: i64,
    server: &str,
) -> Result<Option<(Metadata, TokenSet)>, OAuthError> {
    let [access_key, refresh_key, meta_key] = keys(server);
    let Some(meta) = user_partitions::get_user_setting_for(conn, root, user, &meta_key)
        .map_err(|_| OAuthError::StoreUnavailable)?
    else {
        return Ok(None);
    };
    let meta: Metadata = serde_json::from_value(meta).map_err(|_| OAuthError::InvalidToken)?;
    if meta.binding.user_id != user
        || meta.binding.server_id != server
        || meta.generation.len() != 43
    {
        return Err(OAuthError::StaleBinding);
    }
    meta.binding.check(conn, root)?;
    let access = user_partitions::get_user_credential_for(conn, root, user, &access_key)
        .map_err(|_| OAuthError::StoreUnavailable)?
        .ok_or(OAuthError::InvalidToken)?;
    let refresh = user_partitions::get_user_credential_for(conn, root, user, &refresh_key)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    if access.is_empty()
        || access.len() > 4096
        || access.chars().any(char::is_control)
        || refresh
            .as_ref()
            .is_some_and(|r| r.is_empty() || r.len() > 4096 || r.chars().any(char::is_control))
    {
        return Err(OAuthError::InvalidToken);
    }
    scope_list(Some(&json!(meta.scopes)))?;
    let tokens = TokenSet {
        access,
        refresh,
        expires_at: meta.expires_at,
        scopes: meta.scopes.clone(),
    };
    Ok(Some((meta, tokens)))
}
// Observe all three encrypted rows, including secret edits that preserve generation.
fn snapshot(
    conn: &Connection,
    root: &[u8; 32],
    user: i64,
    server: &str,
) -> Result<Option<String>, OAuthError> {
    let [access, refresh, meta] = keys(server);
    let metadata = user_partitions::get_user_setting_for(conn, root, user, &meta)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    let access = user_partitions::get_user_credential_for(conn, root, user, &access)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    let refresh = user_partitions::get_user_credential_for(conn, root, user, &refresh)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    if metadata.is_none() && access.is_none() && refresh.is_none() {
        return Ok(None);
    }
    if metadata.is_none() || access.is_none() {
        return Err(OAuthError::InvalidToken);
    }
    let mut hash = blake3::Hasher::new_keyed(root);
    hash.update(b"mcp_oauth_snapshot_v1");
    let metadata = serde_json::to_vec(&metadata).map_err(|_| OAuthError::InvalidToken)?;
    for field in [
        metadata.as_slice(),
        access.as_ref().map_or(&[], |v| v.as_bytes()),
        refresh.as_ref().map_or(&[], |v| v.as_bytes()),
    ] {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    Ok(Some(hash.finalize().to_hex().to_string()))
}
fn save(
    conn: &mut Connection,
    root: &[u8; 32],
    binding: &OAuthBinding,
    tokens: &TokenSet,
    expected_generation: Option<&str>,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<(), OAuthError> {
    fresh()?;
    // The session before the reservation (user_partitions::SessionHold); the
    // freshness checks below read under the session this thread holds.
    let _session = user_partitions::hold_user_session(conn, binding.user_id);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    binding.check(&tx, root)?;
    if snapshot(&tx, root, binding.user_id, &binding.server_id)?.as_deref() != expected_generation {
        return Err(OAuthError::StaleBinding);
    }
    let [access, refresh, meta] = keys(&binding.server_id);
    user_partitions::set_user_credential_for(
        &tx,
        root,
        binding.user_id,
        &access,
        "mcp_http_oauth",
        &tokens.access,
    )
    .map_err(|_| OAuthError::StoreUnavailable)?;
    if let Some(token) = &tokens.refresh {
        user_partitions::set_user_credential_for(
            &tx,
            root,
            binding.user_id,
            &refresh,
            "mcp_http_oauth",
            token,
        )
        .map_err(|_| OAuthError::StoreUnavailable)?;
    } else {
        user_partitions::delete_user_credential_for(&tx, binding.user_id, &refresh)
            .map_err(|_| OAuthError::StoreUnavailable)?;
    }
    let metadata = Metadata {
        binding: binding.clone(),
        generation: random_urlsafe().to_string(),
        expires_at: tokens.expires_at,
        scopes: tokens.scopes.clone(),
    };
    user_partitions::set_user_setting_for(
        &tx,
        root,
        binding.user_id,
        &meta,
        &serde_json::to_value(metadata).map_err(|_| OAuthError::InvalidToken)?,
    )
    .map_err(|_| OAuthError::StoreUnavailable)?;
    binding.check(&tx, root)?;
    fresh()?;
    tx.commit().map_err(|_| OAuthError::StoreUnavailable)
}

// One path owns refresh completion: successful rotation and invalid_grant cleanup
// both compare the complete pre-request snapshot inside an Immediate transaction.
fn finish_refresh(
    conn: &mut Connection,
    root: &[u8; 32],
    binding: &OAuthBinding,
    expected_snapshot: Option<&str>,
    outcome: Result<TokenSet, OAuthError>,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<(), OAuthError> {
    fresh()?;
    match outcome {
        Ok(tokens) => save(conn, root, binding, &tokens, expected_snapshot, fresh),
        Err(OAuthError::InvalidGrant) => {
            let _session = user_partitions::hold_user_session(conn, binding.user_id);
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|_| OAuthError::StoreUnavailable)?;
            let (meta, _) = read(&tx, root, binding.user_id, &binding.server_id)?
                .ok_or(OAuthError::StaleBinding)?;
            if meta.binding != *binding
                || snapshot(&tx, root, binding.user_id, &binding.server_id)?.as_deref()
                    != expected_snapshot
            {
                return Err(OAuthError::StaleBinding);
            }
            cleanup_in_transaction(&tx, binding.user_id, &binding.server_id)?;
            fresh()?;
            tx.commit().map_err(|_| OAuthError::StoreUnavailable)?;
            Err(OAuthError::InvalidGrant)
        }
        Err(error) => Err(error),
    }
}
/// Separate OAuth resolver: config-only none/bearer resolution cannot mint OAuth auth.
pub(crate) fn resolve_token(
    conn: &mut Connection,
    root: &[u8; 32],
    key: &[u8; 32],
    user: i64,
    config: &McpHttpServerConfig,
) -> Result<ResolvedMcpHttpAuth, OAuthError> {
    let _session = user_partitions::hold_user_session(conn, user);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    let (meta, tokens) = read(&tx, root, user, &config.id)?.ok_or(OAuthError::InvalidToken)?;
    let expected = config
        .resolve_with(root, user, |_| Err(()))
        .map_err(|_| OAuthError::StaleBinding)?;
    if expected.effective_revision != meta.binding.effective_revision {
        return Err(OAuthError::StaleBinding);
    }
    if tokens
        .expires_at
        .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp())
    {
        return Err(OAuthError::InvalidToken);
    }
    let revision = token_revision(key, &meta, &tokens)?;
    tx.commit().map_err(|_| OAuthError::StoreUnavailable)?;
    Ok(ResolvedMcpHttpAuth {
        effective_revision: revision,
        bearer: Some(tokens.access),
    })
}
fn token_revision(
    key: &[u8; 32],
    meta: &Metadata,
    tokens: &TokenSet,
) -> Result<String, OAuthError> {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(b"mcp_oauth_token_v1");
    for field in [
        serde_json::to_vec(meta).map_err(|_| OAuthError::InvalidToken)?,
        tokens.access.as_bytes().to_vec(),
        tokens
            .refresh
            .as_ref()
            .map_or(Vec::new(), |r| r.as_bytes().to_vec()),
    ] {
        let field = Zeroizing::new(field);
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(&field);
    }
    Ok(hash.finalize().to_hex().to_string())
}
// Freshness must never run migrations or request a second writer while save owns
// an Immediate transaction. Open the existing database read-only, fail on lock.
fn fresh_context(app: &AppHandle) -> Result<(Connection, Zeroizing<[u8; 32]>, i64), OAuthError> {
    let store = crate::credential_store::CredentialStore::from_cache().ok_or(OAuthError::Locked)?;
    let root = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let path = user_partitions::db_path(app).map_err(|_| OAuthError::StoreUnavailable)?;
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    let user = user_partitions::active_user_id(&conn)
        .map_err(|_| OAuthError::StoreUnavailable)?
        .ok_or(OAuthError::UserChanged)?;
    Ok((conn, root, user))
}
fn app_check(app: &AppHandle, binding: &OAuthBinding) -> Result<(), OAuthError> {
    let (conn, root, user) = fresh_context(app)?;
    if user != binding.user_id {
        return Err(OAuthError::UserChanged);
    }
    binding.check(&conn, &root)
}

/// The caller retains only an opaque handle and browser URL. All PKCE state stays here.
#[allow(clippy::too_many_arguments)] // Private backend inputs, never frontend-supplied binding.
pub(crate) async fn start(
    app: &AppHandle,
    manager: &PendingAuthorizationManager,
    server_id: &str,
    challenge: &AuthChallenge,
    redirect: &str,
    previous_scopes: &[String],
    cancel: &CancellationToken,
) -> Result<(String, Url), OAuthError> {
    let (conn, root, user) =
        mcp_client_commands::context(app).map_err(|_| OAuthError::StoreUnavailable)?;
    let config = live_config(&conn, &root, user, server_id)?;
    let token_snapshot = snapshot(&conn, &root, user, server_id)?;
    drop(conn);
    let revision = config
        .resolve_with(&root, user, |_| Err(()))
        .map_err(|_| OAuthError::StaleBinding)?
        .effective_revision;
    let mut fresh = || {
        let result = (|| {
            let (conn, root, current_user) = fresh_context(app)?;
            let current = live_config(&conn, &root, user, server_id)?;
            if snapshot(&conn, &root, user, server_id)? != token_snapshot {
                return Err(OAuthError::StaleBinding);
            }
            if current_user != user
                || current
                    .resolve_with(&root, user, |_| Err(()))
                    .map_err(|_| OAuthError::StaleBinding)?
                    .effective_revision
                    != revision
            {
                return Err(OAuthError::StaleBinding);
            }
            Ok(())
        })();
        if let Err(error) = &result {
            manager.invalidate_error(user, server_id, error);
        }
        result
    };
    let result = async {
        let (resource, server) = discover(&config, challenge, cancel, &mut fresh).await?;
        let client = match configured_client_id(&config, &server)? {
            Some(client) => client,
            None => register_dynamic(&server, redirect, cancel, &mut fresh).await?,
        };
        let (url, pending) = begin_authorization(
            &config,
            &resource,
            &server,
            &client,
            redirect,
            user,
            challenge,
            previous_scopes,
        )?;
        checkpoint(cancel, &mut fresh)?;
        let binding = OAuthBinding::new(&config, &root, &pending)?;
        let handle = manager.insert(Record {
            pending,
            binding,
            server,
            deadline: Instant::now() + TTL,
            cancel: cancel.child_token(),
            token_snapshot: token_snapshot.clone(),
        })?;
        Ok((handle, url))
    }
    .await;
    if let Err(error) = &result {
        manager.invalidate_error(user, server_id, error);
    }
    result
}
pub(crate) async fn complete(
    app: &AppHandle,
    manager: &PendingAuthorizationManager,
    handle: &str,
    callback_url: &str,
    cancel: &CancellationToken,
) -> Result<(), OAuthError> {
    let record = manager.take(handle)?;
    let _finish = Finish { manager, handle };
    let mut fresh = || {
        if record.deadline <= Instant::now() {
            return Err(OAuthError::PendingUnavailable);
        }
        checkpoint(&record.cancel, &mut || {
            app_check(app, &record.binding)?;
            let (conn, root, user) = fresh_context(app)?;
            if snapshot(&conn, &root, user, &record.binding.server_id)? != record.token_snapshot {
                return Err(OAuthError::StaleBinding);
            }
            Ok(())
        })
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(OAuthError::Http(HttpError::Cancelled)),
        result = async {
        checkpoint(cancel, &mut fresh)?;
        let code = callback(&record.pending, callback_url)?;
        let tokens = exchange_code(
            &record.pending,
            &record.server,
            &code,
            &record.pending.state,
            Some(&record.binding.issuer),
            &record.cancel,
            &mut fresh,
        )
        .await?;
        checkpoint(cancel, &mut fresh)?;
        let (mut conn, root, _) =
            mcp_client_commands::context(app).map_err(|_| OAuthError::StoreUnavailable)?;
        save(
            &mut conn,
            &root,
            &record.binding,
            &tokens,
            record.token_snapshot.as_deref(),
            &mut || checkpoint(cancel, &mut fresh),
        )
        } => result,
    };
    if let Err(error) = &result {
        manager.invalidate_error(record.binding.user_id, &record.binding.server_id, error);
    }
    result
}

/// What a refresh starts from: the live configuration, the stored tokens and
/// their snapshot, read in one transaction.
fn refresh_start(
    conn: &mut Connection,
    root: &[u8; 32],
    user: i64,
    server_id: &str,
) -> Result<(McpHttpServerConfig, Metadata, TokenSet, Option<String>), OAuthError> {
    let _session = user_partitions::hold_user_session(conn, user);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| OAuthError::StoreUnavailable)?;
    let config = live_config(&tx, root, user, server_id)?;
    let (meta, previous) = read(&tx, root, user, server_id)?.ok_or(OAuthError::InvalidToken)?;
    let token_snapshot = snapshot(&tx, root, user, server_id)?;
    tx.commit().map_err(|_| OAuthError::StoreUnavailable)?;
    Ok((config, meta, previous, token_snapshot))
}

/// Explicit refresh re-discovers issuer endpoints, checks token generation around each
/// request and compare-and-swaps the replacement. No transport retry is performed.
pub(crate) async fn refresh(
    app: &AppHandle,
    manager: &PendingAuthorizationManager,
    server_id: &str,
    cancel: &CancellationToken,
) -> Result<(), OAuthError> {
    let (mut conn, root, user) =
        mcp_client_commands::context(app).map_err(|_| OAuthError::StoreUnavailable)?;
    let (config, meta, previous, token_snapshot) =
        refresh_start(&mut conn, &root, user, server_id)?;
    drop(conn);
    let operation = cancel.child_token();
    let handle = manager.lease_refresh(user, server_id, operation.clone())?;
    let _finish = Finish {
        manager,
        handle: &handle,
    };
    let revision = token_revision(&root, &meta, &previous)?;
    let mut fresh = || {
        if operation.is_cancelled() {
            return Err(OAuthError::Http(HttpError::Cancelled));
        }
        let result = (|| {
            let (mut conn, root, current_user) = fresh_context(app)?;
            if current_user != user {
                return Err(OAuthError::UserChanged);
            }
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
                .map_err(|_| OAuthError::StoreUnavailable)?;
            let (current, tokens) =
                read(&tx, &root, user, server_id)?.ok_or(OAuthError::StaleBinding)?;
            if token_revision(&root, &current, &tokens)? != revision {
                return Err(OAuthError::StaleBinding);
            }
            Ok(())
        })();
        if let Err(error) = &result {
            manager.invalidate_error(user, server_id, error);
        }
        result
    };
    let result = async {
        let (resource, server) = discover(
            &config,
            &AuthChallenge {
                metadata_url: None,
                scopes: vec![],
            },
            &operation,
            &mut fresh,
        )
        .await?;
        if server.issuer != meta.binding.issuer
            || resource.resource.as_str() != meta.binding.resource
        {
            return Err(OAuthError::StaleBinding);
        }
        let tokens = refresh_access_token(
            &resource,
            &server,
            &meta.binding.client_id,
            &previous,
            &operation,
            &mut fresh,
        )
        .await;
        checkpoint(cancel, &mut fresh)?;
        let (mut conn, root, _) =
            mcp_client_commands::context(app).map_err(|_| OAuthError::StoreUnavailable)?;
        finish_refresh(
            &mut conn,
            &root,
            &meta.binding,
            token_snapshot.as_deref(),
            tokens,
            &mut || checkpoint(cancel, &mut fresh),
        )
    }
    .await;
    if let Err(error) = &result {
        manager.invalidate_error(user, server_id, error);
    }
    result
}

/// Backend challenge acquisition: one unauthenticated tools/list against the
/// pinned public endpoint. Only its 401/403 Bearer challenge (or an open server)
/// feeds discovery; nothing from the frontend shapes metadata, scope or resource.
pub(crate) async fn acquire_challenge(
    app: &AppHandle,
    server_id: &str,
    cancel: &CancellationToken,
) -> Result<AuthChallenge, OAuthError> {
    let (conn, root, user) =
        mcp_client_commands::context(app).map_err(|_| OAuthError::StoreUnavailable)?;
    let config = live_config(&conn, &root, user, server_id)?;
    drop(conn);
    let revision = config
        .resolve_with(&root, user, |_| Err(()))
        .map_err(|_| OAuthError::StaleBinding)?
        .effective_revision;
    let binding = mcp_client_http_transport::HttpBinding {
        endpoint: config.endpoint.clone(),
        user_id: user,
        revision: revision.clone(),
    };
    let mut fresh = || {
        let (conn, root, current) = fresh_context(app)?;
        if current != user {
            return Err(OAuthError::UserChanged);
        }
        let live = live_config(&conn, &root, user, server_id)?;
        if live
            .resolve_with(&root, user, |_| Err(()))
            .map_err(|_| OAuthError::StaleBinding)?
            .effective_revision
            != revision
        {
            return Err(OAuthError::StaleBinding);
        }
        Ok(())
    };
    let mut session = mcp_client_http_transport::HttpSession::new(binding.clone());
    let outcome = session
        .call_checked::<_, OAuthError>(
            &config,
            &binding,
            "tools/list",
            serde_json::Map::new(),
            None,
            None,
            cancel,
            &mut || checkpoint(cancel, &mut fresh),
        )
        .await;
    challenge_from_probe(outcome)
}
fn challenge_from_probe(outcome: Result<Value, OAuthError>) -> Result<AuthChallenge, OAuthError> {
    match outcome {
        Ok(_) => Ok(AuthChallenge {
            metadata_url: None,
            scopes: Vec::new(),
        }),
        Err(OAuthError::Http(HttpError::Unauthorized(challenge)))
        | Err(OAuthError::Http(HttpError::Forbidden(challenge))) => Ok(challenge),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenState {
    Missing,
    Authorized,
    Expired,
    Invalid,
}
/// Redacted token state for settings: no token, issuer, client or scope value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TokenSummary {
    pub state: TokenState,
    pub expires_at: Option<i64>,
    pub refreshable: bool,
}
pub(crate) fn token_summary(
    conn: &Connection,
    root: &[u8; 32],
    user: i64,
    server: &str,
) -> Result<TokenSummary, OAuthError> {
    let summary = |state, expires_at, refreshable| TokenSummary {
        state,
        expires_at,
        refreshable,
    };
    match read(conn, root, user, server) {
        Ok(None) => Ok(summary(TokenState::Missing, None, false)),
        Ok(Some((meta, tokens))) => {
            let expired = meta
                .expires_at
                .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp());
            Ok(summary(
                if expired {
                    TokenState::Expired
                } else {
                    TokenState::Authorized
                },
                meta.expires_at,
                tokens.refresh.is_some(),
            ))
        }
        Err(OAuthError::StaleBinding | OAuthError::InvalidToken) => {
            Ok(summary(TokenState::Invalid, None, false))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (McpHttpServerConfig, ProtectedResource, AuthorizationServer) {
        let config = McpHttpServerConfig {
            id: "fixture".into(),
            endpoint: "https://mcp.example.com/mcp".into(),
            auth: McpHttpAuth::OAuth {
                client_id: Some("client".into()),
                client_id_metadata_url: None,
            },
            enabled: true,
            revision: 1,
        };
        let resource = ProtectedResource {
            resource: Url::parse(&config.endpoint).unwrap(),
            issuers: vec!["https://auth.example.com".into()],
            scopes_supported: vec!["read".into()],
        };
        let server = AuthorizationServer {
            issuer: resource.issuers[0].clone(),
            authorization_endpoint: Url::parse("https://auth.example.com/authorize").unwrap(),
            token_endpoint: Url::parse("https://auth.example.com/token").unwrap(),
            registration_endpoint: None,
            client_id_metadata_supported: false,
            iss_supported: true,
        };
        (config, resource, server)
    }
    fn record(user: i64) -> Record {
        let (config, resource, server) = fixture();
        let (_, pending) = begin_authorization(
            &config,
            &resource,
            &server,
            "client",
            "http://127.0.0.1:49152/callback",
            user,
            &AuthChallenge {
                metadata_url: None,
                scopes: vec![],
            },
            &[],
        )
        .unwrap();
        let binding = OAuthBinding::new(&config, &[42; 32], &pending).unwrap();
        Record {
            pending,
            binding,
            server,
            deadline: Instant::now() + TTL,
            cancel: CancellationToken::new(),
            token_snapshot: None,
        }
    }
    fn callback_url(record: &Record) -> String {
        let mut url = Url::parse(&record.pending.redirect_uri).unwrap();
        url.query_pairs_mut()
            .append_pair("state", &record.pending.state)
            .append_pair("iss", &record.binding.issuer)
            .append_pair("code", "code");
        url.to_string()
    }
    fn database() -> (Connection, i64, Record) {
        let mut conn = Connection::open_in_memory().unwrap();
        user_partitions::migrate_legacy_payloads(&mut conn, None, None, &[42; 32]).unwrap();
        let user = user_partitions::active_user_id(&conn).unwrap().unwrap();
        let (config, _, _) = fixture();
        user_partitions::set_user_setting_for(
            &conn,
            &[42; 32],
            user,
            "aeroagent_mcp_http_servers",
            &json!([config]),
        )
        .unwrap();
        (conn, user, record(user))
    }
    fn tokens() -> TokenSet {
        parse_token_response(br#"{"token_type":"Bearer","access_token":"access-secret","refresh_token":"refresh-secret","expires_in":300}"#, &[]).unwrap()
    }
    /// Tokens saved for one account are not read for another.
    #[test]
    fn tokens_stay_in_the_partition_they_were_saved_to() {
        let (mut conn, first, record) = database();
        let second = user_partitions::create_user(&mut conn, &[42; 32], "Second", None, None, None)
            .unwrap()
            .id;
        user_partitions::set_user_setting_for(
            &conn,
            &[42; 32],
            second,
            "aeroagent_mcp_http_servers",
            &json!([fixture().0]),
        )
        .unwrap();
        save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &tokens(),
            None,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(read(&conn, &[42; 32], second, "fixture").unwrap().is_none());
        let (_, stored) = read(&conn, &[42; 32], first, "fixture").unwrap().unwrap();
        assert_eq!(stored.access.as_str(), "access-secret");
        assert_eq!(
            stored.refresh.as_deref().map(String::as_str),
            Some("refresh-secret")
        );
        assert!(!format!("{stored:?}").contains("access-secret"));
    }
    /// Every transaction that reads or writes the tokens takes the session
    /// before the database's reservation, and the freshness check inside a
    /// save, which reads the partition on a connection of its own, reads under
    /// the session its thread holds instead of waiting on it. The account has
    /// a passphrase, the kind whose partition calls take the session.
    #[test]
    fn token_transactions_take_the_session_before_the_reservation() {
        let _guard = user_partitions::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let (path, user) = user_partitions::passphrase_account_db(dir.path(), &[42; 32]);
        let conn = Connection::open(&path).unwrap();
        user_partitions::set_user_setting_for(
            &conn,
            &[42; 32],
            user,
            "aeroagent_mcp_http_servers",
            &json!([fixture().0]),
        )
        .unwrap();
        let binding = record(user).binding;

        let (saving, live) = (binding.clone(), path.clone());
        let saved = user_partitions::wait_for_the_session_without_reserving(
            &path,
            user,
            move |mut conn| {
                let mut checks = 0;
                save(&mut conn, &[42; 32], &saving, &tokens(), None, &mut || {
                    checks += 1;
                    // The check before the transaction reads nothing here, or
                    // it would wait for the session before the save got to it.
                    if checks == 1 {
                        return Ok(());
                    }
                    // What `app_check` reads: the live binding, on its own connection.
                    let live = Connection::open(&live).map_err(|_| OAuthError::StoreUnavailable)?;
                    saving.check(&live, &[42; 32])
                })
            },
        );
        assert_eq!(saved, Ok(()));

        let resolved = user_partitions::wait_for_the_session_without_reserving(
            &path,
            user,
            move |mut conn| {
                resolve_token(&mut conn, &[42; 32], &[7; 32], user, &fixture().0)
                    .map(|auth| auth.bearer.is_some())
            },
        );
        assert_eq!(resolved, Ok(true));

        let started = user_partitions::wait_for_the_session_without_reserving(
            &path,
            user,
            move |mut conn| {
                refresh_start(&mut conn, &[42; 32], user, "fixture").map(|start| start.3)
            },
        );
        let token_snapshot = started.unwrap().expect("a snapshot of the saved tokens");

        let cleaned = user_partitions::wait_for_the_session_without_reserving(
            &path,
            user,
            move |mut conn| {
                finish_refresh(
                    &mut conn,
                    &[42; 32],
                    &binding,
                    Some(&token_snapshot),
                    Err(OAuthError::InvalidGrant),
                    &mut || Ok(()),
                )
            },
        );
        assert_eq!(cleaned, Err(OAuthError::InvalidGrant));
        assert!(snapshot(&conn, &[42; 32], user, "fixture")
            .unwrap()
            .is_none());
    }
    #[test]
    fn callback_origin_path_denial_duplicates_and_mixups_consume_once() {
        for mutation in 0..10 {
            let manager = PendingAuthorizationManager::default();
            let record = record(1);
            let mut url = Url::parse(&callback_url(&record)).unwrap();
            match mutation {
                0 => {
                    url.set_host(Some("localhost")).unwrap();
                }
                1 => {
                    url.set_path("/other");
                }
                2 => {
                    url.set_port(Some(49153)).unwrap();
                }
                3 => {
                    url.query_pairs_mut().append_pair("state", "duplicate");
                }
                4 => {
                    url.query_pairs_mut().append_pair("code", "duplicate");
                }
                5 => {
                    url.query_pairs_mut().append_pair("iss", "duplicate");
                }
                6 => {
                    url.set_query(Some(
                        "state=wrong&iss=https%3A%2F%2Fauth.example.com&code=code",
                    ));
                }
                7 => {
                    url.set_query(Some(&format!(
                        "state={}&iss=https%3A%2F%2Fevil.example.com&code=code",
                        record.pending.state.as_str()
                    )));
                }
                8 => {
                    url.set_query(Some(&format!(
                        "state={}&iss=https%3A%2F%2Fauth.example.com&error=access_denied",
                        record.pending.state.as_str()
                    )));
                }
                _ => {
                    url.set_fragment(Some("fragment"));
                }
            }
            let handle = manager.insert(record).unwrap();
            let consumed = manager.take(&handle).unwrap();
            assert_eq!(
                callback(&consumed.pending, url.as_str()).unwrap_err(),
                if mutation == 8 {
                    OAuthError::Denied
                } else {
                    OAuthError::InvalidCallback
                }
            );
            assert!(matches!(
                manager.take(&handle),
                Err(OAuthError::PendingUnavailable)
            ));
        }
        let r = record(1);
        assert_eq!(
            callback(&r.pending, &callback_url(&r)).unwrap().as_str(),
            "code"
        );
    }
    #[test]
    fn operations_supersede_refuse_and_cancel_with_their_binding() {
        let manager = PendingAuthorizationManager::default();
        let first = CancellationToken::new();
        let lease = manager
            .register_operation(1, "one", first.clone(), true)
            .unwrap();
        assert!(matches!(
            manager.register_operation(1, "one", CancellationToken::new(), false),
            Err(OAuthError::PendingUnavailable)
        ));
        let other = CancellationToken::new();
        let _other = manager
            .register_operation(1, "two", other.clone(), false)
            .unwrap();
        let second = CancellationToken::new();
        let _second = manager
            .register_operation(1, "one", second.clone(), true)
            .unwrap();
        assert!(first.is_cancelled() && !second.is_cancelled());
        drop(lease);
        manager.invalidate(1, Some("one"));
        assert!(second.is_cancelled() && !other.is_cancelled());
        manager.invalidate(2, None);
        assert!(!other.is_cancelled());
        manager.invalidate_all();
        assert!(other.is_cancelled());
        let fresh = CancellationToken::new();
        drop(
            manager
                .register_operation(1, "one", fresh.clone(), false)
                .unwrap(),
        );
        assert!(manager.operations.lock().unwrap().len() <= 2);
    }
    #[test]
    fn listener_state_check_never_consumes_and_discard_cancels() {
        let manager = PendingAuthorizationManager::default();
        let record = record(1);
        let state = record.pending.state.to_string();
        let handle = manager.insert(record).unwrap();
        assert!(!manager.pending_state_matches(&handle, "wrong"));
        assert!(!manager.pending_state_matches("unknown", &state));
        assert!(manager.pending_state_matches(&handle, &state));
        let cancel = manager.pending_cancel(&handle).unwrap();
        manager.discard(&handle);
        assert!(cancel.is_cancelled());
        assert!(!manager.pending_state_matches(&handle, &state));
        assert!(matches!(
            manager.take(&handle),
            Err(OAuthError::PendingUnavailable)
        ));
    }
    #[test]
    fn partition_lock_and_user_switch_cancel_shared_attempts() {
        let _guard = user_partitions::test_lock();
        let token = CancellationToken::new();
        let _lease = shared()
            .register_operation(i64::MAX, "hook-test", token.clone(), true)
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(user_partitions::user_partitions_lock_session())
            .unwrap();
        assert!(token.is_cancelled());
    }
    #[test]
    fn only_a_bearer_challenge_or_an_open_server_starts_discovery() {
        let challenge = AuthChallenge {
            metadata_url: Some(
                "https://mcp.example.com/.well-known/oauth-protected-resource".into(),
            ),
            scopes: vec!["read".into()],
        };
        assert_eq!(
            challenge_from_probe(Err(HttpError::Unauthorized(challenge.clone()).into())),
            Ok(challenge.clone())
        );
        assert_eq!(
            challenge_from_probe(Err(HttpError::Forbidden(challenge.clone()).into())),
            Ok(challenge)
        );
        assert_eq!(
            challenge_from_probe(Ok(json!({"tools": []})))
                .unwrap()
                .metadata_url,
            None
        );
        for error in [
            HttpError::Redirect,
            HttpError::UnsafeAddress,
            HttpError::InvalidResponse,
            HttpError::Cancelled,
        ] {
            assert_eq!(
                challenge_from_probe(Err(error.clone().into())),
                Err(OAuthError::Http(error))
            );
        }
    }
    #[test]
    fn pending_concurrent_consumption_expiry_cancel_and_bounded_invalidation() {
        let manager = std::sync::Arc::new(PendingAuthorizationManager::default());
        let handle = manager.insert(record(1)).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let (manager, barrier, handle) = (manager.clone(), barrier.clone(), handle.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.take(&handle).is_ok()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
        drop(Finish {
            manager: &manager,
            handle: &handle,
        });
        let mut expired = record(1);
        expired.deadline = Instant::now();
        let handle = manager.insert(expired).unwrap();
        assert!(manager.take(&handle).is_err());
        let cancelled = record(1);
        cancelled.cancel.cancel();
        let handle = manager.insert(cancelled).unwrap();
        assert!(manager.take(&handle).is_err());
        for user in 1..=MAX_PENDING as i64 {
            manager.insert(record(user)).unwrap();
        }
        assert!(manager.insert(record(100)).is_err());
        manager.invalidate_all();
        assert!(manager.records.lock().unwrap().is_empty());
    }
    #[test]
    fn bound_storage_resolver_rejects_config_user_expiry_and_secret_changes() {
        let (mut conn, user, record) = database();
        save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &tokens(),
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let first = resolve_token(&mut conn, &[42; 32], &[7; 32], user, &fixture().0).unwrap();
        let [access, _, _] = keys("fixture");
        user_partitions::set_user_credential_for(
            &conn,
            &[42; 32],
            user,
            &access,
            "mcp_http_oauth",
            "changed-secret",
        )
        .unwrap();
        assert_ne!(
            first.effective_revision,
            resolve_token(&mut conn, &[42; 32], &[7; 32], user, &fixture().0)
                .unwrap()
                .effective_revision
        );
        let mut changed = fixture().0;
        changed.revision += 1;
        user_partitions::set_user_setting_for(
            &conn,
            &[42; 32],
            user,
            "aeroagent_mcp_http_servers",
            &json!([changed]),
        )
        .unwrap();
        assert!(matches!(
            resolve_token(&mut conn, &[42; 32], &[7; 32], user, &fixture().0),
            Err(OAuthError::StaleBinding)
        ));
        assert!(resolve_token(&mut conn, &[42; 32], &[7; 32], user + 1, &fixture().0).is_err());
        assert!(!format!("{first:?}").contains("access-secret"));
    }
    #[test]
    fn token_save_rechecks_snapshot_and_rolls_back_on_final_freshness_failure() {
        let (mut conn, user, record) = database();
        let mut calls = 0;
        let error = save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &tokens(),
            None,
            &mut || {
                calls += 1;
                if calls == 2 {
                    Err(OAuthError::UserChanged)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(error, Err(OAuthError::UserChanged));
        assert!(snapshot(&conn, &[42; 32], user, "fixture")
            .unwrap()
            .is_none());
        save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &tokens(),
            None,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(
            save(
                &mut conn,
                &[42; 32],
                &record.binding,
                &tokens(),
                None,
                &mut || Ok(())
            ),
            Err(OAuthError::StaleBinding)
        );
        let old = snapshot(&conn, &[42; 32], user, "fixture")
            .unwrap()
            .unwrap();
        let mut expired = tokens();
        expired.expires_at = Some(chrono::Utc::now().timestamp() - 1);
        save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &expired,
            Some(&old),
            &mut || Ok(()),
        )
        .unwrap();
        assert!(matches!(
            resolve_token(&mut conn, &[42; 32], &[7; 32], user, &fixture().0),
            Err(OAuthError::InvalidToken)
        ));
    }
    #[test]
    fn cleanup_is_atomic_and_keeps_other_users_and_servers() {
        let (mut conn, user, record) = database();
        save(
            &mut conn,
            &[42; 32],
            &record.binding,
            &tokens(),
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let before = snapshot(&conn, &[42; 32], user, "fixture").unwrap();
        conn.execute_batch("CREATE TRIGGER deny_oauth_cleanup BEFORE DELETE ON user_settings BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(cleanup(&mut conn, user, "fixture").is_err());
        assert_eq!(snapshot(&conn, &[42; 32], user, "fixture").unwrap(), before);
        conn.execute_batch("DROP TRIGGER deny_oauth_cleanup;")
            .unwrap();
        cleanup(&mut conn, user + 1, "fixture").unwrap();
        cleanup(&mut conn, user, "other-server").unwrap();
        assert_eq!(snapshot(&conn, &[42; 32], user, "fixture").unwrap(), before);
        cleanup(&mut conn, user, "fixture").unwrap();
        assert!(snapshot(&conn, &[42; 32], user, "fixture")
            .unwrap()
            .is_none());
        assert!(cleanup_in_transaction(&conn, user, "fixture").is_err());
    }

    // Real HTTP bytes, scoped test-only URL/client override. No production downgrade.
    async fn wire(status: u16, body: String) -> (u16, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 4096];
                let count = stream.read(&mut bytes).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..count]);
                if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let response = format!("HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (port, task)
    }
    async fn injected<F: std::future::Future>(port: u16, future: F) -> F::Output {
        WIRE_PORT
            .scope(
                port,
                WIRE_CLIENT.scope(
                    reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .unwrap(),
                    future,
                ),
            )
            .await
    }
    #[tokio::test]
    async fn exchange_wire_checks_pkce_and_each_freshness_boundary() {
        let record = record(1);
        let response = r#"{"token_type":"Bearer","access_token":"wire-secret","expires_in":300}"#;
        let (port, server) = wire(200, response.into()).await;
        let mut calls = 0;
        let result = injected(
            port,
            exchange_code(
                &record.pending,
                &record.server,
                "code",
                &record.pending.state,
                Some(&record.binding.issuer),
                &record.cancel,
                &mut || {
                    calls += 1;
                    Ok(())
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.access.as_str(), "wire-secret");
        let request = server.await.unwrap();
        assert!(request.contains("code_verifier="));
        assert!(request.contains("resource=https%3A%2F%2Fmcp.example.com%2Fmcp"));
        assert!(calls >= 4);
        for fail in 1..=calls {
            let (port, server) = wire(200, response.into()).await;
            let mut call = 0;
            let result = injected(
                port,
                exchange_code(
                    &record.pending,
                    &record.server,
                    "code",
                    &record.pending.state,
                    Some(&record.binding.issuer),
                    &record.cancel,
                    &mut || {
                        call += 1;
                        if call == fail {
                            Err(OAuthError::StaleBinding)
                        } else {
                            Ok(())
                        }
                    },
                ),
            )
            .await;
            assert!(matches!(result, Err(OAuthError::StaleBinding)));
            server.abort();
        }
    }
    #[tokio::test]
    async fn refresh_wire_rotation_preservation_invalid_grant_and_cancel() {
        let (_, resource, server) = fixture();
        let previous = tokens();
        for (status, response, expected) in [
            (
                200,
                r#"{"token_type":"Bearer","access_token":"new","expires_in":300}"#,
                "refresh-secret",
            ),
            (
                200,
                r#"{"token_type":"Bearer","access_token":"new","refresh_token":"rotated","expires_in":300}"#,
                "rotated",
            ),
            (
                400,
                r#"{"error":"invalid_grant","error_description":"do not surface"}"#,
                "invalid",
            ),
            (500, r#"{"error":"invalid_grant"}"#, "other"),
            (
                400,
                r#"{"error":"invalid_grant","error_codes":[70000],"timestamp":"fixture","trace_id":"fixture","correlation_id":"fixture"}"#,
                "invalid",
            ),
            (
                400,
                r#"{"error":"invalid_grant","error":"invalid_grant"}"#,
                "other",
            ),
        ] {
            let (port, task) = wire(status, response.into()).await;
            let result = injected(
                port,
                refresh_access_token(
                    &resource,
                    &server,
                    "client",
                    &previous,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                ),
            )
            .await;
            task.await.unwrap();
            match expected {
                "invalid" => assert!(matches!(result, Err(OAuthError::InvalidGrant))),
                "other" => assert!(matches!(result, Err(OAuthError::InvalidToken))),
                _ => assert_eq!(result.unwrap().refresh.unwrap().as_str(), expected),
            }
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            refresh_access_token(
                &resource,
                &server,
                "client",
                &previous,
                &cancel,
                &mut || Ok(())
            )
            .await,
            Err(OAuthError::Http(HttpError::Cancelled))
        ));
    }
    #[tokio::test]
    async fn discovery_gets_recheck_each_freshness_boundary_and_cancel() {
        let (config, _, _) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = axum::Router::new()
            .route("/.well-known/oauth-protected-resource/mcp", axum::routing::get(|| async { axum::Json(json!({"resource":"https://mcp.example.com/mcp","authorization_servers":["https://auth.example.com"]})) }))
            .route("/.well-known/oauth-authorization-server", axum::routing::get(|| async { axum::Json(json!({"issuer":"https://auth.example.com","authorization_endpoint":"https://auth.example.com/authorize","token_endpoint":"https://auth.example.com/token","code_challenge_methods_supported":["S256"]})) }));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let cancel = CancellationToken::new();
        let challenge = AuthChallenge {
            metadata_url: None,
            scopes: vec![],
        };
        let mut count = 0;
        injected(
            port,
            discover(&config, &challenge, &cancel, &mut || {
                count += 1;
                Ok(())
            }),
        )
        .await
        .unwrap();
        assert!(count >= 10);
        for fail in 1..=count {
            let mut calls = 0;
            assert!(matches!(
                injected(
                    port,
                    discover(&config, &challenge, &cancel, &mut || {
                        calls += 1;
                        if calls == fail {
                            Err(OAuthError::UserChanged)
                        } else {
                            Ok(())
                        }
                    })
                )
                .await,
                Err(OAuthError::UserChanged)
            ));
        }
        let mut calls = 0;
        let cancelled = CancellationToken::new();
        assert!(matches!(
            injected(
                port,
                discover(&config, &challenge, &cancelled, &mut || {
                    calls += 1;
                    if calls == 4 {
                        cancelled.cancel();
                    }
                    Ok(())
                })
            )
            .await,
            Err(OAuthError::Http(HttpError::Cancelled))
        ));
        server.abort();
    }
    #[test]
    fn binding_mutations_disable_remove_endpoint_client_and_lock_fail_closed() {
        for mutation in 0..7 {
            let (mut conn, user, record) = database();
            save(
                &mut conn,
                &[42; 32],
                &record.binding,
                &tokens(),
                None,
                &mut || Ok(()),
            )
            .unwrap();
            let mut config = fixture().0;
            match mutation {
                0 => config.enabled = false,
                1 => config.endpoint = "https://other.example.com/mcp".into(),
                2 => {
                    config.auth = McpHttpAuth::OAuth {
                        client_id: Some("other-client".into()),
                        client_id_metadata_url: None,
                    }
                }
                3 => config.auth = McpHttpAuth::None,
                4 => config.revision += 1,
                5 => {
                    user_partitions::delete_user_setting_for(
                        &conn,
                        user,
                        "aeroagent_mcp_http_servers",
                    )
                    .unwrap();
                }
                _ => (),
            }
            if mutation < 5 {
                user_partitions::set_user_setting_for(
                    &conn,
                    &[42; 32],
                    user,
                    "aeroagent_mcp_http_servers",
                    &json!([config]),
                )
                .unwrap();
            }
            let key = if mutation == 6 { [43; 32] } else { [42; 32] };
            assert!(resolve_token(&mut conn, &key, &[7; 32], user, &fixture().0).is_err());
        }
        let manager = PendingAuthorizationManager::default();
        let handle = manager.insert(record(1)).unwrap();
        let consumed = manager.take(&handle).unwrap();
        manager.invalidate(1, Some("fixture"));
        assert!(consumed.cancel.is_cancelled());
    }
    #[test]
    fn canonical_resource_accepts_root_and_case_without_normalizing_issuer() {
        let (mut conn, user, _) = database();
        let (mut config, mut resource, server) = fixture();
        config.endpoint = "HTTPS://MCP.EXAMPLE.COM".into();
        resource.resource = Url::parse(&config.endpoint).unwrap();
        user_partitions::set_user_setting_for(
            &conn,
            &[42; 32],
            user,
            "aeroagent_mcp_http_servers",
            &json!([config]),
        )
        .unwrap();
        let (_, pending) = begin_authorization(
            &config,
            &resource,
            &server,
            "client",
            "http://127.0.0.1:49152/callback",
            user,
            &AuthChallenge {
                metadata_url: None,
                scopes: vec![],
            },
            &[],
        )
        .unwrap();
        let binding = OAuthBinding::new(&config, &[42; 32], &pending).unwrap();
        save(&mut conn, &[42; 32], &binding, &tokens(), None, &mut || {
            Ok(())
        })
        .unwrap();
        assert!(resolve_token(&mut conn, &[42; 32], &[7; 32], user, &config).is_ok());
        assert_eq!(
            validate_callback(
                &pending,
                &pending.state,
                Some("HTTPS://AUTH.EXAMPLE.COM"),
                "code"
            ),
            Err(OAuthError::InvalidCallback)
        );
    }
    #[tokio::test]
    async fn pending_invalidation_interrupts_an_inflight_exchange() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = received.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 4096];
            stream.read_exact(&mut bytes[..1]).await.unwrap();
            signal.notify_one();
            std::future::pending::<()>().await;
        });
        let manager = std::sync::Arc::new(PendingAuthorizationManager::default());
        let handle = manager.insert(record(1)).unwrap();
        let consumed = manager.take(&handle).unwrap();
        let cancel_manager = manager.clone();
        let cancel_task = tokio::spawn(async move {
            received.notified().await;
            cancel_manager.invalidate(1, Some("fixture"));
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            injected(
                port,
                exchange_code(
                    &consumed.pending,
                    &consumed.server,
                    "code",
                    &consumed.pending.state,
                    Some(&consumed.binding.issuer),
                    &consumed.cancel,
                    &mut || Ok(()),
                ),
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(OAuthError::Http(HttpError::Cancelled))
        ));
        cancel_task.await.unwrap();
        server.abort();
    }
    #[test]
    fn explicit_refresh_lease_serializes_rotation_and_releases_on_drop() {
        let manager = PendingAuthorizationManager::default();
        let cancel = CancellationToken::new();
        let handle = manager.lease_refresh(1, "fixture", cancel.clone()).unwrap();
        assert!(manager
            .lease_refresh(1, "fixture", CancellationToken::new())
            .is_err());
        manager.invalidate(1, Some("fixture"));
        assert!(cancel.is_cancelled());
        drop(Finish {
            manager: &manager,
            handle: &handle,
        });
        assert!(manager
            .lease_refresh(1, "fixture", CancellationToken::new())
            .is_ok());
    }
    #[tokio::test]
    async fn invalid_grant_wire_completion_cleans_atomically_and_cancel_or_stale_preserves_tokens()
    {
        for mode in 0..4 {
            let (mut conn, user, record) = database();
            save(
                &mut conn,
                &[42; 32],
                &record.binding,
                &tokens(),
                None,
                &mut || Ok(()),
            )
            .unwrap();
            let before = snapshot(&conn, &[42; 32], user, "fixture").unwrap();
            let (port, task) = wire(400, r#"{"error":"invalid_grant"}"#.into()).await;
            let previous = tokens();
            let (_, resource, server) = fixture();
            let outcome = injected(
                port,
                refresh_access_token(
                    &resource,
                    &server,
                    "client",
                    &previous,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                ),
            )
            .await;
            task.await.unwrap();
            if mode == 2 {
                let [access, _, _] = keys("fixture");
                user_partitions::set_user_credential_for(
                    &conn,
                    &[42; 32],
                    user,
                    &access,
                    "mcp_http_oauth",
                    "concurrently-rotated",
                )
                .unwrap();
            }
            if mode == 3 {
                conn.execute_batch("CREATE TRIGGER deny_invalid_grant_cleanup BEFORE DELETE ON user_settings BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
            }
            let expected = snapshot(&conn, &[42; 32], user, "fixture").unwrap();
            let mut calls = 0;
            let result = finish_refresh(
                &mut conn,
                &[42; 32],
                &record.binding,
                before.as_deref(),
                outcome,
                &mut || {
                    calls += 1;
                    if mode == 1 && calls == 2 {
                        Err(OAuthError::Http(HttpError::Cancelled))
                    } else {
                        Ok(())
                    }
                },
            );
            match mode {
                0 => {
                    assert_eq!(result, Err(OAuthError::InvalidGrant));
                    assert!(snapshot(&conn, &[42; 32], user, "fixture")
                        .unwrap()
                        .is_none());
                }
                1 => assert_eq!(result, Err(OAuthError::Http(HttpError::Cancelled))),
                2 => assert_eq!(result, Err(OAuthError::StaleBinding)),
                _ => assert_eq!(result, Err(OAuthError::StoreUnavailable)),
            }
            if mode != 0 {
                assert_eq!(
                    snapshot(&conn, &[42; 32], user, "fixture").unwrap(),
                    expected
                );
            }
        }
    }
    #[test]
    fn local_oauth_errors_preserve_other_servers_and_users_while_lock_invalidates_all() {
        for error in [
            OAuthError::Denied,
            OAuthError::InvalidCallback,
            OAuthError::InvalidToken,
            OAuthError::StaleBinding,
            OAuthError::Http(HttpError::Cancelled),
        ] {
            let manager = PendingAuthorizationManager::default();
            let affected = CancellationToken::new();
            let other_server = CancellationToken::new();
            let other_user = CancellationToken::new();
            manager
                .lease_refresh(1, "fixture", affected.clone())
                .unwrap();
            manager
                .lease_refresh(1, "other-server", other_server.clone())
                .unwrap();
            manager
                .lease_refresh(2, "other-server", other_user.clone())
                .unwrap();
            let pending = record(2);
            let handle = manager.insert(pending).unwrap();
            let pending = record(3);
            let preserved_handle = manager.insert(pending).unwrap();
            manager.invalidate_error(1, "fixture", &error);
            assert!(affected.is_cancelled());
            assert!(!other_server.is_cancelled());
            assert!(!other_user.is_cancelled());
            assert!(manager.take(&preserved_handle).is_ok());
            assert!(manager.take(&handle).is_ok());
            manager.invalidate_error(1, "fixture", &OAuthError::Locked);
            assert!(other_server.is_cancelled());
        }
    }
    #[test]
    fn active_exchange_cannot_be_superseded_and_pending_authorization_blocks_refresh() {
        let manager = PendingAuthorizationManager::default();
        let handle = manager.insert(record(1)).unwrap();
        let active = manager.take(&handle).unwrap();
        assert!(manager.insert(record(1)).is_err());
        assert!(!active.cancel.is_cancelled());
        drop(Finish {
            manager: &manager,
            handle: &handle,
        });
        let pending_handle = manager.insert(record(1)).unwrap();
        assert!(manager
            .lease_refresh(1, "fixture", CancellationToken::new())
            .is_err());
        manager.invalidate(1, Some("fixture"));
        assert!(manager.take(&pending_handle).is_err());
        assert!(manager
            .lease_refresh(1, "fixture", CancellationToken::new())
            .is_ok());
    }
}
