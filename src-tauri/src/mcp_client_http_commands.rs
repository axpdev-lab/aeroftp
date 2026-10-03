//! Active-user settings for outbound MCP HTTPS servers and their backend-owned
//! OAuth authorization. No model route: tools stay unavailable until routing.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Webview};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::mcp_client_commands;
use crate::mcp_client_http_config::{McpHttpAuth, McpHttpServerConfig};
use crate::mcp_client_http_transport::HttpError;
use crate::mcp_client_oauth::lifecycle::{self, TokenState};
use crate::mcp_client_oauth::{loopback, OAuthError};
use crate::user_partitions;

const SETTING_SCOPE: &str = "aeroagent_mcp_http_servers";
const MAX_SERVERS: usize = 32;
const MAX_FLOWS: usize = 32;
const MAX_SECRET: usize = 4096;

type Outcome = Option<Result<(), &'static str>>;

struct Flow {
    server: String,
    cancel: CancellationToken,
    outcome: watch::Receiver<Outcome>,
    started: std::time::Instant,
}

/// A finished attempt stays readable this long after it started, so the wait
/// that follows its begin still finds the outcome when another begin prunes
/// first. Twice the listener lifetime, so every outcome is kept for minutes.
const FINISHED_KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(600);

static FLOWS: LazyLock<Mutex<HashMap<String, Flow>>> = LazyLock::new(Default::default);

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpHttpAuthInput {
    // Empty struct variants: serde ignores extra fields on internally tagged
    // unit variants, and a caller-chosen vault account must be rejected.
    None {},
    /// The vault account is derived by the backend, never chosen by the caller.
    Bearer {},
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        client_id_metadata_url: Option<String>,
    },
}

/// The caller states the revision it edited; the backend assigns the next one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHttpServerInput {
    id: String,
    endpoint: String,
    auth: McpHttpAuthInput,
    enabled: bool,
    expected_revision: u64,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum AuthView {
    None,
    Bearer,
    #[serde(rename = "oauth")]
    OAuth {
        client_id: Option<String>,
        client_id_metadata_url: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CredentialState {
    NotRequired,
    Disabled,
    Missing,
    Saved,
    Authorized,
    Expired,
    Invalid,
}

/// Settings view. No secret, token, issuer, client registration or vault account.
#[derive(Serialize)]
pub struct McpHttpServerView {
    id: String,
    endpoint: String,
    auth: AuthView,
    enabled: bool,
    revision: u64,
    credential: CredentialState,
    expires_at: Option<i64>,
    refreshable: bool,
}

#[derive(Serialize)]
pub struct McpOAuthAttempt {
    attempt: String,
    authorization_url: String,
    browser_opened: bool,
}

fn validate_catalog(configs: &[McpHttpServerConfig]) -> Result<(), &'static str> {
    if configs.len() > MAX_SERVERS {
        return Err("MCP_CONFIG_TOO_MANY_SERVERS");
    }
    let mut ids = HashSet::new();
    for config in configs {
        config.validate()?;
        if !ids.insert(config.id.as_str()) {
            return Err("MCP_CONFIG_DUPLICATE_ID");
        }
    }
    Ok(())
}

pub(crate) fn load(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<Vec<McpHttpServerConfig>, &'static str> {
    let value = user_partitions::get_user_setting_for(conn, root_key, user_id, SETTING_SCOPE)
        .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    let configs = match value {
        Some(value) => serde_json::from_value(value).map_err(|_| "MCP_CONFIG_INVALID_STORED")?,
        None => Vec::new(),
    };
    validate_catalog(&configs)?;
    Ok(configs)
}

fn save(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    configs: &[McpHttpServerConfig],
) -> Result<(), &'static str> {
    validate_catalog(configs)?;
    let value = serde_json::to_value(configs).map_err(|_| "MCP_CONFIG_SERIALIZE_FAILED")?;
    user_partitions::set_user_setting_for(conn, root_key, user_id, SETTING_SCOPE, &value)
        .map_err(|_| "MCP_STORE_UNAVAILABLE")
}

fn auth_from_input(id: &str, input: McpHttpAuthInput) -> McpHttpAuth {
    match input {
        McpHttpAuthInput::None {} => McpHttpAuth::None,
        McpHttpAuthInput::Bearer {} => McpHttpAuth::Bearer {
            vault_account: format!("mcp_http_bearer_{}_{}", id.len(), id),
        },
        McpHttpAuthInput::OAuth {
            client_id,
            client_id_metadata_url,
        } => McpHttpAuth::OAuth {
            client_id: client_id.filter(|value| !value.is_empty()),
            client_id_metadata_url: client_id_metadata_url.filter(|value| !value.is_empty()),
        },
    }
}

/// Applies one edit inside the caller's Immediate transaction. Every change of
/// an existing server alters its effective revision, so stored OAuth tokens
/// are removed in the same transaction; the bearer secret survives only while
/// the endpoint and bearer mode are unchanged.
fn apply_upsert(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    stdio_ids: &[String],
    input: McpHttpServerInput,
) -> Result<(), &'static str> {
    let mut configs = load(conn, root_key, user_id)?;
    let old = configs.iter().position(|item| item.id == input.id);
    let revision = match old {
        Some(index) if configs[index].revision == input.expected_revision => input
            .expected_revision
            .checked_add(1)
            .ok_or("MCP_CONFIG_REVISION_OVERFLOW")?,
        None if input.expected_revision == 0 => 1,
        _ => return Err("MCP_CONFIG_STALE_REVISION"),
    };
    if old.is_none() && stdio_ids.contains(&input.id) {
        return Err("MCP_CONFIG_DUPLICATE_ID");
    }
    let config = McpHttpServerConfig {
        auth: auth_from_input(&input.id, input.auth),
        id: input.id,
        endpoint: input.endpoint,
        enabled: input.enabled,
        revision,
    };
    config.validate()?;
    if let Some(index) = old {
        let previous = &configs[index];
        lifecycle::cleanup_in_transaction(conn, user_id, &previous.id)
            .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
        let keeps_bearer = previous.endpoint == config.endpoint
            && matches!(previous.auth, McpHttpAuth::Bearer { .. })
            && matches!(config.auth, McpHttpAuth::Bearer { .. });
        if !keeps_bearer {
            user_partitions::delete_user_credential_for(
                conn,
                user_id,
                &previous.bearer_vault_account(),
            )
            .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
        }
        configs[index] = config;
    } else {
        configs.push(config);
    }
    configs.sort_by(|a, b| a.id.cmp(&b.id));
    save(conn, root_key, user_id, &configs)
}

fn apply_remove(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    server_id: &str,
) -> Result<(), &'static str> {
    let mut configs = load(conn, root_key, user_id)?;
    let index = configs
        .iter()
        .position(|item| item.id == server_id)
        .ok_or("MCP_CONFIG_NOT_FOUND")?;
    let removed = configs.remove(index);
    lifecycle::cleanup_in_transaction(conn, user_id, &removed.id)
        .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    user_partitions::delete_user_credential_for(conn, user_id, &removed.bearer_vault_account())
        .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    save(conn, root_key, user_id, &configs)
}

fn apply_bearer(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    server_id: &str,
    secret: &str,
) -> Result<(), &'static str> {
    if secret.is_empty() || secret.len() > MAX_SECRET || secret.chars().any(char::is_control) {
        return Err("MCP_HTTP_SECRET_INVALID");
    }
    let mut configs = load(conn, root_key, user_id)?;
    let config = configs
        .iter_mut()
        .find(|item| item.id == server_id)
        .ok_or("MCP_CONFIG_NOT_FOUND")?;
    let McpHttpAuth::Bearer { vault_account } = &config.auth else {
        return Err("MCP_HTTP_INVALID_BEARER_REF");
    };
    user_partitions::set_user_credential_for(
        conn,
        root_key,
        user_id,
        vault_account,
        "mcp_http_bearer",
        secret,
    )
    .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    config.revision = config
        .revision
        .checked_add(1)
        .ok_or("MCP_CONFIG_REVISION_OVERFLOW")?;
    save(conn, root_key, user_id, &configs)
}

/// Install one reviewed HTTPS preset, disabled, with no caller-supplied endpoint.
pub(crate) fn install_disabled_endpoint(
    app: &AppHandle,
    id: &str,
    endpoint: &str,
) -> Result<(), &'static str> {
    let (mut conn, root_key, user_id) = mcp_client_commands::context(app)?;
    install_disabled_endpoint_in(
        &mut conn,
        &root_key,
        user_id,
        lifecycle::shared(),
        id,
        endpoint,
    )
}

fn install_disabled_endpoint_in(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    manager: &lifecycle::PendingAuthorizationManager,
    id: &str,
    endpoint: &str,
) -> Result<(), &'static str> {
    let server_id = id.to_string();
    let id = id.to_string();
    let endpoint = endpoint.to_string();
    write_catalog_in(
        conn,
        root_key,
        user_id,
        manager,
        &server_id,
        false,
        move |conn, root_key, user_id, stdio_ids| {
            if load(conn, root_key, user_id)?
                .iter()
                .any(|config| config.id == id)
            {
                return Err("MCP_INSTALL_EXISTS");
            }
            apply_upsert(
                conn,
                root_key,
                user_id,
                stdio_ids,
                McpHttpServerInput {
                    id,
                    endpoint,
                    auth: McpHttpAuthInput::None {},
                    enabled: false,
                    expected_revision: 0,
                },
            )
        },
    )
}

/// Runs one catalog write: Immediate transaction before the read, pending and
/// in-flight attempts for the binding cancelled before and after the commit.
fn write_catalog(
    app: &AppHandle,
    server_id: &str,
    apply: impl FnOnce(&Connection, &[u8; 32], i64, &[String]) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let (mut conn, root_key, user_id) = mcp_client_commands::context(app)?;
    write_catalog_in(
        &mut conn,
        &root_key,
        user_id,
        lifecycle::shared(),
        server_id,
        true,
        apply,
    )
}

/// `invalidate_first` is false only where the id must not exist yet (a preset
/// install): nothing can be pending for a new id, and invalidating first would
/// cancel an existing server's OAuth attempts before the transaction refuses
/// the duplicate.
fn write_catalog_in(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    manager: &lifecycle::PendingAuthorizationManager,
    server_id: &str,
    invalidate_first: bool,
    apply: impl FnOnce(&Connection, &[u8; 32], i64, &[String]) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if invalidate_first {
        manager.invalidate(user_id, Some(server_id));
    }
    let transaction = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    let stdio_ids = mcp_client_commands::load(&transaction, root_key, user_id)?
        .into_iter()
        .map(|config| config.id)
        .collect::<Vec<_>>();
    apply(&transaction, root_key, user_id, &stdio_ids)?;
    transaction.commit().map_err(|_| "MCP_STORE_UNAVAILABLE")?;
    manager.invalidate(user_id, Some(server_id));
    Ok(())
}

fn view(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    config: McpHttpServerConfig,
) -> Result<McpHttpServerView, &'static str> {
    let mut expires_at = None;
    let mut refreshable = false;
    let credential = match &config.auth {
        McpHttpAuth::None => CredentialState::NotRequired,
        McpHttpAuth::Bearer { vault_account } => {
            match user_partitions::get_user_credential_for(conn, root_key, user_id, vault_account)
                .map_err(|_| "MCP_STORE_UNAVAILABLE")?
            {
                Some(_) => CredentialState::Saved,
                None => CredentialState::Missing,
            }
        }
        McpHttpAuth::OAuth { .. } if !config.enabled => CredentialState::Disabled,
        McpHttpAuth::OAuth { .. } => {
            let summary = lifecycle::token_summary(conn, root_key, user_id, &config.id)
                .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
            expires_at = summary.expires_at;
            refreshable = summary.refreshable;
            match summary.state {
                TokenState::Missing => CredentialState::Missing,
                TokenState::Authorized => CredentialState::Authorized,
                TokenState::Expired => CredentialState::Expired,
                TokenState::Invalid => CredentialState::Invalid,
            }
        }
    };
    let auth = match config.auth {
        McpHttpAuth::None => AuthView::None,
        McpHttpAuth::Bearer { .. } => AuthView::Bearer,
        McpHttpAuth::OAuth {
            client_id,
            client_id_metadata_url,
        } => AuthView::OAuth {
            client_id,
            client_id_metadata_url,
        },
    };
    Ok(McpHttpServerView {
        id: config.id,
        endpoint: config.endpoint,
        auth,
        enabled: config.enabled,
        revision: config.revision,
        credential,
        expires_at,
        refreshable,
    })
}

/// Redacted, stable codes for the settings UI. Transport detail never leaves.
fn oauth_code(error: &OAuthError) -> &'static str {
    match error {
        OAuthError::Locked => "MCP_OAUTH_LOCKED",
        OAuthError::UserChanged => "MCP_OAUTH_USER_CHANGED",
        OAuthError::StaleBinding => "MCP_OAUTH_STALE",
        OAuthError::PendingUnavailable => "MCP_OAUTH_BUSY",
        OAuthError::Denied => "MCP_OAUTH_DENIED",
        OAuthError::InvalidGrant | OAuthError::InvalidToken => "MCP_OAUTH_REAUTHORIZE",
        OAuthError::InvalidMetadata | OAuthError::AmbiguousIssuer => "MCP_OAUTH_METADATA",
        OAuthError::RegistrationRequired | OAuthError::InvalidClient => "MCP_OAUTH_CLIENT",
        OAuthError::InvalidCallback => "MCP_OAUTH_CALLBACK",
        OAuthError::StoreUnavailable => "MCP_STORE_UNAVAILABLE",
        OAuthError::Http(HttpError::Cancelled) => "MCP_OAUTH_CANCELLED",
        OAuthError::Http(HttpError::Timeout) => "MCP_OAUTH_TIMEOUT",
        OAuthError::Http(
            HttpError::Dns
            | HttpError::Connect
            | HttpError::UnsafeAddress
            | HttpError::InvalidEndpoint,
        ) => "MCP_HTTP_UNREACHABLE",
        OAuthError::Http(_) => "MCP_HTTP_FAILED",
    }
}

fn main_window(webview: &Webview, what: &str) -> Result<(), &'static str> {
    crate::only_main_window(webview.label(), what).map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> Result<T, &'static str> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| "MCP_STORE_UNAVAILABLE")?
}

#[tauri::command]
pub async fn mcp_client_http_list_servers(
    webview: Webview,
    app: AppHandle,
) -> Result<Vec<McpHttpServerView>, &'static str> {
    main_window(&webview, "mcp_client_http_list_servers")?;
    blocking(move || {
        let (conn, root_key, user_id) = mcp_client_commands::context(&app)?;
        load(&conn, &root_key, user_id)?
            .into_iter()
            .map(|config| view(&conn, &root_key, user_id, config))
            .collect()
    })
    .await
}

#[tauri::command]
pub async fn mcp_client_http_upsert_server(
    webview: Webview,
    app: AppHandle,
    server: McpHttpServerInput,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_upsert_server")?;
    blocking(move || {
        let id = server.id.clone();
        write_catalog(&app, &id, |conn, root_key, user_id, stdio_ids| {
            apply_upsert(conn, root_key, user_id, stdio_ids, server)
        })
    })
    .await
}

#[tauri::command]
pub async fn mcp_client_http_remove_server(
    webview: Webview,
    app: AppHandle,
    server_id: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_remove_server")?;
    blocking(move || {
        write_catalog(&app, &server_id, |conn, root_key, user_id, _| {
            apply_remove(conn, root_key, user_id, &server_id)
        })
    })
    .await
}

#[tauri::command]
pub async fn mcp_client_http_set_bearer(
    webview: Webview,
    app: AppHandle,
    server_id: String,
    secret: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_set_bearer")?;
    let secret = Zeroizing::new(secret);
    blocking(move || {
        write_catalog(&app, &server_id, |conn, root_key, user_id, _| {
            apply_bearer(conn, root_key, user_id, &server_id, &secret)
        })
    })
    .await
}

/// Removes stored OAuth tokens for one server; the configuration is kept.
#[tauri::command]
pub async fn mcp_client_http_oauth_sign_out(
    webview: Webview,
    app: AppHandle,
    server_id: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_oauth_sign_out")?;
    blocking(move || {
        let (mut conn, root_key, user_id) = mcp_client_commands::context(&app)?;
        let manager = lifecycle::shared();
        manager.invalidate(user_id, Some(&server_id));
        let transaction = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
        if !load(&transaction, &root_key, user_id)?
            .iter()
            .any(|config| config.id == server_id)
        {
            return Err("MCP_CONFIG_NOT_FOUND");
        }
        lifecycle::cleanup_in_transaction(&transaction, user_id, &server_id)
            .map_err(|_| "MCP_STORE_UNAVAILABLE")?;
        transaction.commit().map_err(|_| "MCP_STORE_UNAVAILABLE")?;
        manager.invalidate(user_id, Some(&server_id));
        Ok(())
    })
    .await
}

/// Explicit refresh only. Never triggered by a 401 and never replays a tool call.
#[tauri::command]
pub async fn mcp_client_http_oauth_refresh(
    webview: Webview,
    app: AppHandle,
    server_id: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_oauth_refresh")?;
    let runtime = tokio::runtime::Handle::current();
    blocking(move || {
        let (conn, _, user_id) = mcp_client_commands::context(&app)?;
        drop(conn);
        let manager = lifecycle::shared();
        let operation = CancellationToken::new();
        let _lease = manager
            .register_operation(user_id, &server_id, operation.clone(), false)
            .map_err(|error| oauth_code(&error))?;
        runtime
            .block_on(lifecycle::refresh(&app, manager, &server_id, &operation))
            .map_err(|error| oauth_code(&error))
    })
    .await
}

fn prune_flows(flows: &mut HashMap<String, Flow>) {
    flows.retain(|_, flow| {
        flow.outcome.borrow().is_none() || flow.started.elapsed() < FINISHED_KEPT_FOR
    });
}

/// Starts a backend-owned authorization. SQLite and the loopback exchange run
/// on a blocking worker with the captured runtime, never on the GUI thread.
#[tauri::command]
pub async fn mcp_client_http_oauth_begin(
    webview: Webview,
    app: AppHandle,
    server_id: String,
) -> Result<McpOAuthAttempt, &'static str> {
    main_window(&webview, "mcp_client_http_oauth_begin")?;
    let user_id = {
        let app = app.clone();
        blocking(move || mcp_client_commands::context(&app).map(|(_, _, user)| user)).await?
    };
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let operation = CancellationToken::new();
    let (done, outcome) = watch::channel(None);
    {
        let mut flows = FLOWS.lock().map_err(|_| "MCP_OAUTH_BUSY")?;
        prune_flows(&mut flows);
        for flow in flows.values().filter(|flow| flow.server == server_id) {
            flow.cancel.cancel();
        }
        if flows.len() >= MAX_FLOWS {
            return Err("MCP_OAUTH_BUSY");
        }
        flows.insert(
            attempt.clone(),
            Flow {
                server: server_id.clone(),
                cancel: operation.clone(),
                outcome: outcome.clone(),
                started: std::time::Instant::now(),
            },
        );
    }
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let runtime = tokio::runtime::Handle::current();
    std::mem::drop(tokio::task::spawn_blocking(move || {
        let manager = lifecycle::shared();
        let result = match manager.register_operation(user_id, &server_id, operation.clone(), true)
        {
            Ok(_lease) => runtime.block_on(loopback::authorize(
                &app,
                manager,
                &server_id,
                &operation,
                loopback::open_in_browser,
                |url, opened| {
                    let _ = started_tx.send((url.to_string(), opened));
                },
            )),
            Err(error) => Err(error),
        };
        let _ = done.send(Some(result.map_err(|error| oauth_code(&error))));
    }));
    match started.await {
        Ok((authorization_url, browser_opened)) => Ok(McpOAuthAttempt {
            attempt,
            authorization_url,
            browser_opened,
        }),
        // The flow ended before the backend owned a pending record.
        Err(_) => match wait_outcome(&attempt).await {
            Ok(()) => Err("MCP_OAUTH_CALLBACK"),
            Err(code) => Err(code),
        },
    }
}

async fn wait_outcome(attempt: &str) -> Result<(), &'static str> {
    let mut outcome = {
        let flows = FLOWS.lock().map_err(|_| "MCP_OAUTH_BUSY")?;
        flows
            .get(attempt)
            .ok_or("MCP_OAUTH_UNKNOWN_ATTEMPT")?
            .outcome
            .clone()
    };
    let result = outcome
        .wait_for(Option::is_some)
        .await
        .map(|finished| *finished)
        .unwrap_or(None)
        .unwrap_or(Err("MCP_OAUTH_CANCELLED"));
    if let Ok(mut flows) = FLOWS.lock() {
        flows.remove(attempt);
    }
    result
}

/// Resolves when the attempt ends with a redacted outcome code.
#[tauri::command]
pub async fn mcp_client_http_oauth_wait(
    webview: Webview,
    attempt: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_oauth_wait")?;
    wait_outcome(&attempt).await
}

/// User cancel owns the listener and network lifetime of its attempt.
#[tauri::command]
pub async fn mcp_client_http_oauth_cancel(
    webview: Webview,
    attempt: String,
) -> Result<(), &'static str> {
    main_window(&webview, "mcp_client_http_oauth_cancel")?;
    let flows = FLOWS.lock().map_err(|_| "MCP_OAUTH_BUSY")?;
    if let Some(flow) = flows.get(&attempt) {
        flow.cancel.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: [u8; 32] = [7; 32];

    #[test]
    fn a_finished_attempt_stays_readable_until_it_ages_out() {
        let flow = |finished: bool, age: u64| {
            let (done, outcome) = watch::channel(None);
            if finished {
                done.send(Some(Ok(()))).unwrap();
            }
            Flow {
                server: "remote".into(),
                cancel: CancellationToken::new(),
                outcome,
                started: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(age))
                    .unwrap(),
            }
        };
        let mut flows = HashMap::from([
            ("running-old".to_string(), flow(false, 1_000)),
            ("finished-now".to_string(), flow(true, 1)),
            ("finished-old".to_string(), flow(true, 1_000)),
        ]);
        prune_flows(&mut flows);
        let mut kept: Vec<_> = flows.keys().cloned().collect();
        kept.sort();
        // A finished attempt whose wait has not run yet must survive another begin.
        assert_eq!(kept, ["finished-now", "running-old"]);
    }

    #[test]
    fn installing_an_existing_id_leaves_its_oauth_attempts_alone() {
        let (_dir, mut conn, user) = database();
        let manager = lifecycle::PendingAuthorizationManager::default();
        install_disabled_endpoint_in(
            &mut conn,
            &ROOT,
            user,
            &manager,
            "deepwiki",
            "https://mcp.example.com/mcp",
        )
        .unwrap();
        let attempt = CancellationToken::new();
        let _lease = manager
            .register_operation(user, "deepwiki", attempt.clone(), true)
            .unwrap();
        assert_eq!(
            install_disabled_endpoint_in(
                &mut conn,
                &ROOT,
                user,
                &manager,
                "deepwiki",
                "https://mcp.example.com/mcp"
            ),
            Err("MCP_INSTALL_EXISTS")
        );
        assert!(
            !attempt.is_cancelled(),
            "a refused duplicate must not cancel the existing server's authorization"
        );
    }

    fn database() -> (tempfile::TempDir, Connection, i64) {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(dir.path().join("mcp-http.sqlite")).unwrap();
        user_partitions::init_db_schema(&conn).unwrap();
        let user =
            user_partitions::create_user(&mut conn, &ROOT, "MCP HTTP", None, None, None).unwrap();
        (dir, conn, user.id)
    }

    fn input(id: &str, auth: McpHttpAuthInput, expected_revision: u64) -> McpHttpServerInput {
        McpHttpServerInput {
            id: id.into(),
            endpoint: "https://mcp.example.com/mcp".into(),
            auth,
            enabled: true,
            expected_revision,
        }
    }

    fn write(
        conn: &mut Connection,
        apply: impl FnOnce(&Connection) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        apply(&tx)?;
        tx.commit().unwrap();
        Ok(())
    }

    fn put_oauth_rows(conn: &Connection, user: i64, server: &str) {
        let id = blake3::hash(server.as_bytes()).to_hex();
        for kind in ["access", "refresh"] {
            user_partitions::set_user_credential_for(
                conn,
                &ROOT,
                user,
                &format!("mcp_oauth_{kind}_{id}"),
                "mcp_http_oauth",
                "token",
            )
            .unwrap();
        }
        user_partitions::set_user_setting_for(
            conn,
            &ROOT,
            user,
            &format!("mcp_oauth_meta_{id}"),
            &serde_json::json!({}),
        )
        .unwrap();
    }

    fn oauth_rows(conn: &Connection, user: i64, server: &str) -> usize {
        let id = blake3::hash(server.as_bytes()).to_hex();
        ["access", "refresh"]
            .iter()
            .filter(|kind| {
                user_partitions::get_user_credential_for(
                    conn,
                    &ROOT,
                    user,
                    &format!("mcp_oauth_{kind}_{id}"),
                )
                .unwrap()
                .is_some()
            })
            .count()
            + usize::from(
                user_partitions::get_user_setting_for(
                    conn,
                    &ROOT,
                    user,
                    &format!("mcp_oauth_meta_{id}"),
                )
                .unwrap()
                .is_some(),
            )
    }

    #[test]
    fn backend_assigns_revisions_and_derives_bearer_reference() {
        let (_dir, mut conn, user) = database();
        let none = || McpHttpAuthInput::None {};
        assert_eq!(
            write(&mut conn, |c| apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", none(), 1)
            )),
            Err("MCP_CONFIG_STALE_REVISION")
        );
        write(&mut conn, |c| {
            apply_upsert(c, &ROOT, user, &[], input("one", none(), 0))
        })
        .unwrap();
        assert_eq!(
            write(&mut conn, |c| apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", none(), 0)
            )),
            Err("MCP_CONFIG_STALE_REVISION")
        );
        assert_eq!(
            write(&mut conn, |c| apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", none(), 7)
            )),
            Err("MCP_CONFIG_STALE_REVISION")
        );
        write(&mut conn, |c| {
            apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", McpHttpAuthInput::Bearer {}, 1),
            )
        })
        .unwrap();
        let stored = load(&conn, &ROOT, user).unwrap();
        assert_eq!(stored[0].revision, 2);
        assert!(stored[0].validate().is_ok());
        assert!(matches!(
            &stored[0].auth,
            McpHttpAuth::Bearer { vault_account } if vault_account == "mcp_http_bearer_3_one"
        ));
        assert_eq!(
            write(&mut conn, |c| {
                apply_upsert(c, &ROOT, user, &["two".into()], input("two", none(), 0))
            }),
            Err("MCP_CONFIG_DUPLICATE_ID")
        );
        let mut local = input("three", none(), 0);
        local.endpoint = "https://127.0.0.1/mcp".into();
        assert_eq!(
            write(&mut conn, |c| apply_upsert(c, &ROOT, user, &[], local)),
            Err("MCP_HTTP_INVALID_ENDPOINT")
        );
        assert!(serde_json::from_str::<McpHttpServerInput>(
            r#"{"id":"x","endpoint":"https://mcp.example.com/mcp","auth":{"mode":"bearer"},"enabled":true,"expected_revision":0}"#
        )
        .is_ok());
        assert!(serde_json::from_str::<McpHttpServerInput>(
            r#"{"id":"x","endpoint":"https://mcp.example.com/mcp","auth":{"mode":"bearer","vault_account":"mcp_env_1_x_A"},"enabled":true,"expected_revision":0}"#
        )
        .is_err());
        assert!(serde_json::from_str::<McpHttpServerInput>(
            r#"{"id":"x","endpoint":"https://mcp.example.com/mcp","auth":{"mode":"oauth"},"enabled":true,"expected_revision":0,"revision":9}"#
        )
        .is_err());
    }

    #[test]
    fn disable_rebind_and_remove_clean_tokens_atomically_for_that_user_and_server() {
        let (_dir, mut conn, user) = database();
        let oauth = || McpHttpAuthInput::OAuth {
            client_id: Some("client".into()),
            client_id_metadata_url: None,
        };
        write(&mut conn, |c| {
            apply_upsert(c, &ROOT, user, &[], input("one", oauth(), 0))
        })
        .unwrap();
        write(&mut conn, |c| {
            apply_upsert(c, &ROOT, user, &[], input("two", oauth(), 0))
        })
        .unwrap();
        put_oauth_rows(&conn, user, "one");
        put_oauth_rows(&conn, user, "two");
        let mut disabled = input("one", oauth(), 1);
        disabled.enabled = false;
        write(&mut conn, |c| apply_upsert(c, &ROOT, user, &[], disabled)).unwrap();
        assert_eq!(oauth_rows(&conn, user, "one"), 0);
        assert_eq!(oauth_rows(&conn, user, "two"), 3);
        // A failing edit rolls the cleanup back with the catalog.
        let mut broken = input("two", oauth(), 1);
        broken.endpoint = "http://mcp.example.com/mcp".into();
        assert!(write(&mut conn, |c| apply_upsert(c, &ROOT, user, &[], broken)).is_err());
        assert_eq!(oauth_rows(&conn, user, "two"), 3);
        write(&mut conn, |c| apply_remove(c, &ROOT, user, "two")).unwrap();
        assert_eq!(oauth_rows(&conn, user, "two"), 0);
        assert_eq!(load(&conn, &ROOT, user).unwrap().len(), 1);
        assert_eq!(
            write(&mut conn, |c| apply_remove(c, &ROOT, user, "two")),
            Err("MCP_CONFIG_NOT_FOUND")
        );
    }

    #[test]
    fn bearer_secret_is_bounded_and_survives_only_an_unchanged_binding() {
        let (_dir, mut conn, user) = database();
        let account = "mcp_http_bearer_3_one";
        let secret = |conn: &Connection| {
            user_partitions::get_user_credential_for(conn, &ROOT, user, account)
                .unwrap()
                .is_some()
        };
        write(&mut conn, |c| {
            apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", McpHttpAuthInput::None {}, 0),
            )
        })
        .unwrap();
        assert_eq!(
            write(&mut conn, |c| apply_bearer(c, &ROOT, user, "one", "token")),
            Err("MCP_HTTP_INVALID_BEARER_REF")
        );
        write(&mut conn, |c| {
            apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", McpHttpAuthInput::Bearer {}, 1),
            )
        })
        .unwrap();
        for bad in ["", "line\nbreak", &"x".repeat(MAX_SECRET + 1)] {
            assert_eq!(
                write(&mut conn, |c| apply_bearer(c, &ROOT, user, "one", bad)),
                Err("MCP_HTTP_SECRET_INVALID")
            );
        }
        write(&mut conn, |c| apply_bearer(c, &ROOT, user, "one", "token")).unwrap();
        assert!(secret(&conn));
        assert_eq!(load(&conn, &ROOT, user).unwrap()[0].revision, 3);
        let mut toggled = input("one", McpHttpAuthInput::Bearer {}, 3);
        toggled.enabled = false;
        write(&mut conn, |c| apply_upsert(c, &ROOT, user, &[], toggled)).unwrap();
        assert!(secret(&conn));
        let mut moved = input("one", McpHttpAuthInput::Bearer {}, 4);
        moved.endpoint = "https://other.example.com/mcp".into();
        write(&mut conn, |c| apply_upsert(c, &ROOT, user, &[], moved)).unwrap();
        assert!(!secret(&conn));
        write(&mut conn, |c| apply_bearer(c, &ROOT, user, "one", "token")).unwrap();
        write(&mut conn, |c| apply_remove(c, &ROOT, user, "one")).unwrap();
        assert!(!secret(&conn));
    }

    #[test]
    fn view_reports_redacted_credential_state() {
        let (_dir, mut conn, user) = database();
        write(&mut conn, |c| {
            apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input("one", McpHttpAuthInput::Bearer {}, 0),
            )
        })
        .unwrap();
        write(&mut conn, |c| {
            apply_bearer(c, &ROOT, user, "one", "very-secret")
        })
        .unwrap();
        write(&mut conn, |c| {
            apply_upsert(
                c,
                &ROOT,
                user,
                &[],
                input(
                    "two",
                    McpHttpAuthInput::OAuth {
                        client_id: None,
                        client_id_metadata_url: None,
                    },
                    0,
                ),
            )
        })
        .unwrap();
        let views = load(&conn, &ROOT, user)
            .unwrap()
            .into_iter()
            .map(|config| view(&conn, &ROOT, user, config).unwrap())
            .collect::<Vec<_>>();
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains("very-secret") && !json.contains("vault_account"));
        assert!(json.contains(r#""credential":"saved""#));
        assert!(json.contains(r#""mode":"oauth""#) && json.contains(r#""credential":"missing""#));
        // Malformed or foreign token rows are reported, never trusted.
        put_oauth_rows(&conn, user, "two");
        let two = load(&conn, &ROOT, user).unwrap().remove(1);
        assert!(matches!(
            view(&conn, &ROOT, user, two).unwrap().credential,
            CredentialState::Invalid
        ));
    }

    #[test]
    fn oauth_codes_are_redacted_and_stable() {
        assert_eq!(
            oauth_code(&OAuthError::Http(HttpError::Unauthorized(
                crate::mcp_client_http_transport::AuthChallenge {
                    metadata_url: Some("https://secret.example.com/x".into()),
                    scopes: vec!["admin".into()],
                }
            ))),
            "MCP_HTTP_FAILED"
        );
        assert_eq!(
            oauth_code(&OAuthError::Http(HttpError::Cancelled)),
            "MCP_OAUTH_CANCELLED"
        );
        assert_eq!(
            oauth_code(&OAuthError::InvalidGrant),
            "MCP_OAUTH_REAUTHORIZE"
        );
    }
}
