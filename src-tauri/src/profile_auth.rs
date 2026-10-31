// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Profile-bound headless authentication shared by CLI and MCP. Only public
//! authorization URLs and readiness metadata leave this module. Pending PKCE,
//! request-token secrets, PEMs and resulting tokens stay in the encrypted vault.
use crate::{credential_store::CredentialStore, providers::*, user_partitions as users};
use secrecy::ExposeSecret;
use serde_json::{json, Value};

/// Durable profile auth entries travel with a profile. Pending attempts and
/// connection deadlines are local lifecycle state and are deleted only.
pub const DURABLE_KEY_PREFIXES: [&str; 3] = ["profile_auth_config", "github_oauth", "github_pem"];
pub const TRANSIENT_KEY_PREFIXES: [&str; 2] = ["profile_auth_pending", "profile_auth_deadline"];

pub struct AuthScope<'a> {
    pub store: &'a CredentialStore,
    pub user_id: Option<i64>,
}
impl AuthScope<'_> {
    fn read(&self, key: &str) -> Result<Option<String>, String> {
        match self.user_id {
            Some(id) => users::resolve_credential_for_user(self.store, id, key)
                .map(|v| v.map(|s| s.to_string())),
            None => Ok(self.store.get(key).ok()),
        }
    }
    /// Profile-bound reads cannot adopt another account's global mirror when
    /// the captured account has no row. Keep legacy fallback only where the
    /// profile/key ownership proves it belongs to this account.
    pub(crate) fn read_profile(
        &self,
        profile_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        let Some(user_id) = self.user_id else {
            return self.read(key);
        };
        users::init_or_migrate_cli(self.store)?;
        let conn = users::open_or_init_cli()?;
        let root_key = zeroize::Zeroizing::new(self.store.derive_user_partition_wrapping_key());
        if let Some(secret) = users::get_user_credential_for(&conn, &root_key, user_id, key)? {
            return Ok(Some(secret.to_string()));
        }
        let owners = users::profile_credential_owner_ids(self.store, profile_id, key)?;
        if owners.as_slice() == [user_id]
            || (owners.is_empty() && users::legacy_credential_owner_id()? == Some(user_id))
        {
            return Ok(self.store.get(key).ok());
        }
        Ok(None)
    }
    fn write(&self, key: &str, value: &str) -> Result<(), String> {
        match self.user_id {
            Some(id) => {
                users::store_credential_for_user_typed_dual(self.store, id, key, "oauth", value)
            }
            None => self.store.store(key, value).map_err(|e| e.to_string()),
        }
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        match self.user_id {
            Some(id) => users::delete_credential_for_user_dual(self.store, id, key),
            None => self.store.delete(key).map_err(|e| e.to_string()),
        }
    }
    fn config(&self, id: &str) -> Result<Value, String> {
        match self.read(&format!("profile_auth_config_{id}"))? {
            Some(raw) => {
                let value: Value =
                    serde_json::from_str(&raw).map_err(|_| "Invalid saved auth configuration")?;
                if !value.is_object() {
                    return Err("Invalid saved auth configuration: expected an object".into());
                }
                Ok(value)
            }
            None => Ok(json!({})),
        }
    }
}
fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn identity(profile: &Value) -> Result<(&str, ProviderType), String> {
    let id = field(profile, "id");
    if id.is_empty() {
        return Err("Authentication requires a saved profile ID".into());
    }
    let kind = ProviderType::from_lowercase(field(profile, "protocol"))
        .ok_or("Unknown profile protocol")?;
    if kind == ProviderType::GooglePhotos && !cfg!(debug_assertions) {
        return Err("Google Photos is available only in DEV builds".into());
    }
    Ok((id, kind))
}
fn oauth_kind(kind: ProviderType) -> Option<OAuthProvider> {
    Some(match kind {
        ProviderType::GoogleDrive => OAuthProvider::Google,
        ProviderType::GooglePhotos => OAuthProvider::GooglePhotos,
        ProviderType::Dropbox => OAuthProvider::Dropbox,
        ProviderType::OneDrive => OAuthProvider::OneDrive,
        ProviderType::Box => OAuthProvider::Box,
        ProviderType::PCloud => OAuthProvider::PCloud,
        ProviderType::ZohoWorkdrive => OAuthProvider::ZohoWorkdrive,
        ProviderType::YandexDisk => OAuthProvider::YandexDisk,
        _ => return None,
    })
}
fn oauth_key(provider: OAuthProvider, id: &str) -> String {
    format!("oauth_{provider:?}_{id}").to_lowercase()
}
fn client_settings(scope: &AuthScope<'_>, profile: &Value, config: &Value) -> (String, String) {
    let mut legacy = crate::bridge_commands::resolve_oauth_client_config(
        scope.store,
        field(profile, "protocol"),
    );
    if legacy.0.is_empty() && field(profile, "protocol") == "fourshared" {
        if let Ok(raw) = scope.store.get("fourshared_oauth_settings") {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                legacy = (
                    field(&value, "consumer_key").into(),
                    field(&value, "consumer_secret").into(),
                );
            }
        }
    }
    if !field(config, "client_id").is_empty() {
        (
            field(config, "client_id").into(),
            field(config, "client_secret").into(),
        )
    } else {
        legacy
    }
}
fn oauth_config(
    kind: ProviderType,
    id: &str,
    client: &(String, String),
    region: &str,
    port: u16,
) -> Result<OAuthConfig, String> {
    if client.0.is_empty() {
        return Err("Missing OAuth client ID. Use profile-auth --request-json-file with action=configure first".into());
    }
    let config = match kind {
        ProviderType::GoogleDrive => OAuthConfig::google_with_port(&client.0, &client.1, port),
        ProviderType::GooglePhotos => {
            OAuthConfig::google_photos_with_port(&client.0, &client.1, port)
        }
        ProviderType::Dropbox => OAuthConfig::dropbox_with_port(&client.0, &client.1, port),
        ProviderType::OneDrive => OAuthConfig::onedrive_with_port(&client.0, &client.1, port),
        ProviderType::Box => OAuthConfig::box_cloud_with_port(&client.0, &client.1, port),
        ProviderType::PCloud => OAuthConfig::pcloud_with_port(&client.0, &client.1, port, region),
        ProviderType::ZohoWorkdrive => {
            OAuthConfig::zoho_with_port(&client.0, &client.1, port, region)
        }
        ProviderType::YandexDisk => OAuthConfig::yandex_disk_with_port(&client.0, &client.1, port),
        _ => return Err("This profile does not use OAuth2".into()),
    };
    Ok(config.with_profile_id(id))
}
fn github_mode<'a>(profile: &'a Value, config: &'a Value) -> &'a str {
    profile
        .pointer("/options/githubAuthMode")
        .or_else(|| profile.pointer("/options/github_auth_mode"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if field(config, "mode").is_empty() {
                "pat"
            } else {
                field(config, "mode")
            }
        })
}
fn app_ids(profile: &Value, config: &Value) -> Result<(String, String), String> {
    let app = profile
        .pointer("/options/githubAppId")
        .or_else(|| profile.pointer("/options/github_app_id"))
        .or_else(|| config.get("app_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let install = profile
        .pointer("/options/githubInstallationId")
        .or_else(|| profile.pointer("/options/github_installation_id"))
        .or_else(|| config.get("installation_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if app.is_empty()
        || install.is_empty()
        || !app.bytes().all(|b| b.is_ascii_digit())
        || !install.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(
            "GitHub App requires numeric app_id and installation_id; configure profile-auth first"
                .into(),
        );
    }
    Ok((app.into(), install.into()))
}
fn client_region(scope: &AuthScope<'_>, profile: &Value, config: &Value) -> String {
    if !field(config, "region").is_empty() {
        return field(config, "region").into();
    }
    profile
        .pointer("/options/pcloudRegion")
        .or_else(|| profile.pointer("/options/region"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            scope
                .store
                .get(&format!("oauth_{}_region", field(profile, "protocol")))
                .ok()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "us".into())
}
fn github_device_token(
    scope: &AuthScope<'_>,
    id: &str,
    config: &Value,
) -> Result<Option<String>, String> {
    let own = scope
        .read(&format!("github_oauth_{id}"))?
        .filter(|token| !token.is_empty());
    if own.is_some() || !field(config, "mode").is_empty() {
        return Ok(own);
    }
    // Compatibility for GUI profiles that predate profile-bound headless auth.
    scope
        .read("github_oauth_token")
        .map(|value| value.filter(|token| !token.is_empty()))
}
fn github_pem(
    scope: &AuthScope<'_>,
    id: &str,
    app: &str,
    install: &str,
) -> Result<Option<String>, String> {
    if let Some(raw) = scope.read(&format!("github_pem_{id}"))? {
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| "Invalid profile-bound GitHub PEM; configure again")?;
        if field(&value, "app_id") != app || field(&value, "installation_id") != install {
            return Err(
                "GitHub PEM belongs to a different App or installation; configure again".into(),
            );
        }
        return Ok(Some(field(&value, "pem").into()));
    }
    scope.read(&format!("github_pem_{app}_{install}"))
}
fn materialize_github_metadata(
    scope: &AuthScope<'_>,
    id: &str,
    config: &Value,
) -> Result<(), String> {
    let apply = |profiles: &mut Vec<Value>| {
        let Some(profile) = profiles
            .iter_mut()
            .find(|profile| field(profile, "id") == id)
        else {
            return false;
        };
        if !profile["options"].is_object() {
            profile["options"] = json!({});
        }
        profile["options"]["githubAuthMode"] = config["mode"].clone();
        for (source, target) in [
            ("app_id", "githubAppId"),
            ("installation_id", "githubInstallationId"),
        ] {
            if config.get(source).is_some() {
                profile["options"][target] = config[source].clone();
            }
        }
        true
    };
    if let Some(user) = scope.user_id {
        users::cli_update_server_profiles_for_user(scope.store, user, apply)?;
    } else {
        // Read and written under the lock of the list it stands in for.
        let _lock = users::ProfileListLock::acquire_for_path(&users::cli_db_path()?)?;
        if let Ok(raw) = scope.store.get("config_server_profiles") {
            let mut profiles: Vec<Value> =
                serde_json::from_str(&raw).map_err(|_| "Invalid saved profiles")?;
            if apply(&mut profiles) {
                scope
                    .store
                    .store(
                        "config_server_profiles",
                        &serde_json::to_string(&profiles).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
/// Only GitHub readiness belongs to this route; other auth-source rules remain shared.
pub fn saved_github_readiness(
    store: &CredentialStore,
    id: &str,
    profile: Option<&Value>,
    user_id: Option<i64>,
) -> &'static str {
    let user_id = user_id.or_else(|| {
        users::cli_get_active_user(store)
            .ok()
            .flatten()
            .map(|user| user.id)
    });
    let found = if profile.is_none() {
        users::mcp_list_active_server_profiles(store)
            .ok()
            .and_then(|profiles| profiles.into_iter().find(|p| field(p, "id") == id))
    } else {
        None
    };
    let fallback = json!({"id":id,"protocol":"github","host":""});
    readiness(
        &AuthScope { store, user_id },
        profile.or(found.as_ref()).unwrap_or(&fallback),
    )
    .unwrap_or("no_credentials")
}
pub(crate) fn connection_deadline(
    scope: &AuthScope<'_>,
    profile: &Value,
) -> Result<Option<i64>, String> {
    if field(profile, "protocol").eq_ignore_ascii_case("github")
        && github_mode(profile, &scope.config(field(profile, "id"))?) == "app"
    {
        return scope
            .read(&format!("profile_auth_deadline_{}", field(profile, "id")))?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| "Invalid GitHub token expiry".into())
            })
            .transpose();
    }
    Ok(None)
}

fn binding(profile: &Value) -> Value {
    json!({"id":profile["id"],"protocol":profile["protocol"],"host":profile["host"]})
}
fn callback(request: &Value) -> Result<(String, String), String> {
    if let Some(raw) = request.get("callback_url").and_then(Value::as_str) {
        let url = url::Url::parse(raw).map_err(|_| "Invalid callback URL")?;
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        if query.contains_key("error") {
            return Err("Authorization was denied; begin again".into());
        }
        Ok((
            query.get("code").cloned().unwrap_or_default(),
            query.get("state").cloned().unwrap_or_default(),
        ))
    } else {
        Ok((
            field(request, "code").into(),
            field(request, "state").into(),
        ))
    }
}

// Fixed production endpoints; tests substitute a loopback fixture internally.
// No endpoint override is exposed through CLI, MCP, profiles or environment.
struct AuthEndpoints {
    device: String,
    device_token: String,
    installation: String,
    oauth1_begin: String,
    oauth1_finish: String,
    #[cfg(test)]
    oauth2_token: Option<String>,
}
impl Default for AuthEndpoints {
    fn default() -> Self {
        Self {
            device: "https://github.com/login/device/code".into(),
            device_token: "https://github.com/login/oauth/access_token".into(),
            installation: "https://api.github.com/app/installations".into(),
            oauth1_begin: "https://api.4shared.com/v1_2/oauth/initiate".into(),
            oauth1_finish: "https://api.4shared.com/v1_2/oauth/token".into(),
            #[cfg(test)]
            oauth2_token: None,
        }
    }
}

/// Execute one bounded authorization step. The saved pending record binds every
/// callback to its profile, host, user and expiry across independent processes.
pub async fn execute(
    scope: &AuthScope<'_>,
    profile: &Value,
    request: &Value,
) -> Result<Value, String> {
    execute_at(scope, profile, request, &AuthEndpoints::default()).await
}
async fn execute_at(
    scope: &AuthScope<'_>,
    profile: &Value,
    request: &Value,
    endpoints: &AuthEndpoints,
) -> Result<Value, String> {
    let (id, kind) = identity(profile)?;
    let pending_key = format!("profile_auth_pending_{id}");
    let config_key = format!("profile_auth_config_{id}");
    let config = scope.config(id)?;
    let now = chrono::Utc::now().timestamp();
    match field(request, "action") {
        "status" => Ok(
            json!({"status":readiness(scope,profile)?,"pending":scope.read(&pending_key)?.is_some()}),
        ),
        "configure" => {
            let mut next = config.clone();
            if kind == ProviderType::GitHub {
                let mode = request
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("authorize");
                if !matches!(mode, "authorize" | "app" | "pat") {
                    return Err("mode must be authorize, app or pat".into());
                }
                next["mode"] = json!(mode);
                for key in ["app_id", "installation_id"] {
                    if request.get(key).is_some() {
                        next[key] = request[key].clone();
                    }
                }
                if mode == "app" {
                    let (app, install) = app_ids(&json!({}), &next)?;
                    if !field(request, "pem").is_empty() {
                        github::auth::validate_pem(field(request, "pem"), &app)?;
                        scope.write(&format!("github_pem_{id}"), &json!({"app_id":app,"installation_id":install,"pem":field(request,"pem")}).to_string())?;
                    }
                }
            } else if oauth_kind(kind).is_some() || kind == ProviderType::FourShared {
                for key in ["client_id", "client_secret", "region"] {
                    if request.get(key).is_some() {
                        if !request[key].is_string() {
                            return Err(format!("{key} must be a string"));
                        }
                        next[key] = request[key].clone();
                    }
                }
                if field(&next, "client_id").is_empty() {
                    return Err("client_id is required".into());
                }
            } else {
                return Err("This profile has no configurable OAuth application".into());
            }
            scope.write(&config_key, &next.to_string())?;
            if kind == ProviderType::GitHub {
                materialize_github_metadata(scope, id, &next)?;
                scope.delete(&format!("profile_auth_deadline_{id}"))?;
            }
            scope.delete(&pending_key)?;
            Ok(json!({"status":"configured"}))
        }
        "cancel" => {
            if kind == ProviderType::Twake {
                if let Some(raw) = scope.read(&pending_key)? {
                    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                        if let Ok(pending) =
                            twake::TwakePendingSignIn::from_vault_value(&value["flow"])
                        {
                            pending.abandon().await;
                        }
                    }
                }
            }
            scope.delete(&pending_key)?;
            Ok(json!({"status":"cancelled"}))
        }
        "begin" => {
            if kind == ProviderType::Twake {
                if let Some(raw) = scope.read(&pending_key)? {
                    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                        if let Ok(pending) =
                            twake::TwakePendingSignIn::from_vault_value(&value["flow"])
                        {
                            pending.abandon().await;
                        }
                    }
                    scope.delete(&pending_key)?;
                }
            }
            let port = request
                .get("redirect_port")
                .and_then(Value::as_u64)
                .unwrap_or(match kind {
                    ProviderType::Dropbox => 17548,
                    ProviderType::OneDrive => 27154,
                    ProviderType::Box => 9484,
                    ProviderType::PCloud => 17384,
                    ProviderType::ZohoWorkdrive => 18765,
                    ProviderType::YandexDisk => 19847,
                    _ => 19899,
                });
            let port = u16::try_from(port)
                .ok()
                .filter(|p| *p > 0)
                .ok_or("redirect_port must be 1..65535")?;
            let (mut public, flow) = if kind == ProviderType::GitHub {
                if github_mode(profile, &config) != "authorize" {
                    return Err("Configure mode=authorize for Device Flow; App mode refreshes from its PEM on connect".into());
                }
                let device = github::auth::request_device_code_at(&endpoints.device).await?;
                (
                    json!({"status":"authorization_required","verification_uri":device.verification_uri,"user_code":device.user_code,"expires_in":device.expires_in,"interval":device.interval}),
                    json!({"device_code":device.device_code,"interval":device.interval}),
                )
            } else if kind == ProviderType::Twake {
                let pending =
                    twake::begin_sign_in(field(profile, "host"), port, env!("CARGO_PKG_VERSION"))
                        .await
                        .map_err(|e| e.to_string())?;
                (
                    json!({"status":"authorization_required","authorization_url":pending.auth_url,"expires_in":600}),
                    pending.to_vault_value(),
                )
            } else if kind == ProviderType::FourShared {
                let client = client_settings(scope, profile, &config);
                if client.0.is_empty() || client.1.is_empty() {
                    return Err("4shared requires client_id and client_secret; configure profile-auth first".into());
                }
                let (token, secret) =
                    oauth1::request_token(&client.0, &client.1, &endpoints.oauth1_begin, "oob")
                        .await?;
                (
                    json!({"status":"authorization_required","authorization_url":oauth1::authorize_url("https://api.4shared.com/v1_2/oauth/authorize",&token),"expires_in":600}),
                    json!({"token":token,"secret":secret,"client_id":client.0,"client_secret":client.1}),
                )
            } else {
                let client = client_settings(scope, profile, &config);
                let config = oauth_config(
                    kind,
                    id,
                    &client,
                    &client_region(scope, profile, &config),
                    port,
                )?;
                #[cfg(test)]
                let config = {
                    let mut config = config;
                    if let Some(url) = &endpoints.oauth2_token {
                        config.token_url = url.clone();
                    }
                    config
                };
                let (url, flow) = OAuth2Manager::new()
                    .begin_headless(config)
                    .await
                    .map_err(|e| e.to_string())?;
                (
                    json!({"status":"authorization_required","authorization_url":url,"expires_in":600}),
                    serde_json::to_value(flow).map_err(|e| e.to_string())?,
                )
            };
            let ttl = public
                .get("expires_in")
                .and_then(Value::as_i64)
                .unwrap_or(600)
                .min(600);
            public["expires_in"] = json!(ttl);
            scope.write(&pending_key,&json!({"binding":binding(profile),"user_id":scope.user_id,"expires_at":now+ttl,"flow":flow}).to_string())?;
            Ok(public)
        }
        "finish" => {
            let raw = scope
                .read(&pending_key)?
                .ok_or("No pending authorization; use action=begin first")?;
            let pending: Value = serde_json::from_str(&raw)
                .map_err(|_| "Invalid pending authorization; begin again")?;
            if pending["binding"] != binding(profile)
                || pending["user_id"] != json!(scope.user_id)
                || pending["expires_at"].as_i64().unwrap_or(0) <= now
            {
                return Err(
                    "Authorization expired or belongs to a different profile/user; begin again"
                        .into(),
                );
            }
            let flow = &pending["flow"];
            if kind == ProviderType::GitHub {
                let interval = flow["interval"].as_u64().unwrap_or(5);
                let seconds = pending["expires_at"].as_i64().unwrap_or(now) - now;
                let token = tokio::time::timeout(
                    std::time::Duration::from_secs(seconds.max(1) as u64),
                    github::auth::poll_for_token_at(
                        &endpoints.device_token,
                        field(flow, "device_code"),
                        interval,
                    ),
                )
                .await
                .map_err(|_| "Device authorization expired; begin again")??;
                scope.write(&format!("github_oauth_{id}"), &token)?;
            } else if kind == ProviderType::FourShared {
                if field(request, "oauth_token") != field(flow, "token") {
                    return Err("OAuth request token mismatch".into());
                }
                let (token, secret) = oauth1::access_token(
                    field(flow, "client_id"),
                    field(flow, "client_secret"),
                    &endpoints.oauth1_finish,
                    field(flow, "token"),
                    field(flow, "secret"),
                    field(request, "verifier"),
                )
                .await?;
                scope.write(
                    &format!("oauth_fourshared_{id}"),
                    &format!("{token}:{secret}"),
                )?;
            } else {
                let (code, state) = callback(request)?;
                if code.is_empty() {
                    return Err("Authorization code is required".into());
                }
                if kind == ProviderType::Twake {
                    let pending = twake::TwakePendingSignIn::from_vault_value(flow)?;
                    if state != pending.state {
                        return Err("Authorization state mismatch".into());
                    }
                    let credentials = twake::finish_sign_in(&pending, &code)
                        .await
                        .map_err(|e| e.to_string())?;
                    scope.write(&format!("server_{id}"), &credentials.to_stored())?;
                } else {
                    let pending: oauth2::HeadlessOAuthFlow =
                        serde_json::from_value(flow.clone())
                            .map_err(|_| "Invalid OAuth pending state")?;
                    if pending.config.profile_id() != id {
                        return Err("OAuth profile binding mismatch".into());
                    }
                    let tokens = OAuth2Manager::new()
                        .finish_headless(&pending, &code, &state)
                        .await
                        .map_err(|e| e.to_string())?;
                    scope.write(
                        &oauth_key(pending.config.provider, id),
                        &serde_json::to_string(&tokens).map_err(|e| e.to_string())?,
                    )?;
                }
            }
            scope.delete(&pending_key)?;
            Ok(json!({"status":"authorized"}))
        }
        _ => Err("Unknown auth action; use configure, begin, finish, status or cancel".into()),
    }
}

pub fn readiness(scope: &AuthScope<'_>, profile: &Value) -> Result<&'static str, String> {
    let (id, kind) = identity(profile)?;
    let config = scope.config(id)?;
    let present = if kind == ProviderType::GitHub {
        match github_mode(profile, &config) {
            "authorize" => github_device_token(scope, id, &config)?.is_some(),
            "app" => {
                let (app, install) = app_ids(profile, &config)?;
                github_pem(scope, id, &app, &install)?
                    .is_some_and(|pem| github::auth::validate_pem(&pem, &app).is_ok())
            }
            _ => scope.read(&format!("server_{id}"))?.is_some_and(|s| {
                !crate::profile_loader::parse_server_credential(&s, "")
                    .1
                    .is_empty()
            }),
        }
    } else if let Some(provider) = oauth_kind(kind) {
        if let Some(raw) = scope.read(&oauth_key(provider, id))? {
            let tokens: oauth2::StoredTokens =
                serde_json::from_str(&raw).map_err(|_| "Invalid stored OAuth token")?;
            if tokens.is_expired() {
                return Ok(if tokens.refresh_token.is_some() {
                    "needs_refresh"
                } else {
                    "expired"
                });
            }
            !tokens.access_token.is_empty()
        } else {
            false
        }
    } else if kind == ProviderType::FourShared {
        scope
            .read(&format!("oauth_fourshared_{id}"))?
            .is_some_and(|s| {
                s.split_once(':')
                    .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
            })
    } else {
        scope
            .read(&format!("server_{id}"))?
            .is_some_and(|s| !s.is_empty())
    };
    Ok(if present { "valid" } else { "no_credentials" })
}

/// Construct the same authenticated backend for CLI and MCP. No browser is opened.
pub async fn provider_for_profile(
    scope: &AuthScope<'_>,
    profile: &Value,
) -> Result<Option<Box<dyn StorageProvider>>, String> {
    provider_at(scope, profile, &AuthEndpoints::default()).await
}
async fn provider_at(
    scope: &AuthScope<'_>,
    profile: &Value,
    endpoints: &AuthEndpoints,
) -> Result<Option<Box<dyn StorageProvider>>, String> {
    let (id, kind) = identity(profile)?;
    let config = scope.config(id)?;
    #[cfg(feature = "proton-native")]
    if kind == ProviderType::Proton {
        let (extra, host) =
            crate::profile_loader::profile_connection_extra(profile, kind, field(profile, "host"));
        let config = proton::ProtonConfig::from_provider_config(&ProviderConfig {
            name: field(profile, "name").into(),
            provider_type: kind,
            host,
            port: None,
            username: Some(field(profile, "username").into()),
            password: None,
            initial_path: None,
            extra,
        })
        .map_err(|error| error.to_string())?;
        if config.connection_mode == proton::ProtonConnectionMode::Native {
            return Ok(Some(Box::new(
                proton_native::provider::ProtonNativeProvider::new(config)
                    .with_credential_user(scope.user_id),
            )));
        }
    }
    if kind == ProviderType::GitHub && matches!(github_mode(profile, &config), "authorize" | "app")
    {
        let token = if github_mode(profile, &config) == "authorize" {
            github_device_token(scope,id,&config)?.ok_or("GitHub authorization missing; use profile-auth configure mode=authorize, begin, then finish")?
        } else {
            let (app, install) = app_ids(profile, &config)?;
            let pem = github_pem(scope, id, &app, &install)?.ok_or(
                "GitHub App PEM missing or belongs to another App; configure profile-auth again",
            )?;
            let token = github::auth::get_installation_token_at(
                &format!("{}/{install}/access_tokens", endpoints.installation),
                &pem,
                &app,
                &install,
            )
            .await?;
            let expires = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
                .map_err(|_| "GitHub returned an invalid installation expiry")?
                .timestamp()
                - 60;
            scope.write(&format!("profile_auth_deadline_{id}"), &expires.to_string())?;
            token.token
        };
        let (extra, host) =
            crate::profile_loader::profile_connection_extra(profile, kind, field(profile, "host"));
        let provider = ProviderFactory::create(&ProviderConfig {
            name: field(profile, "name").into(),
            provider_type: kind,
            host,
            port: Some(443),
            username: None,
            password: Some(token),
            initial_path: None,
            extra,
        })
        .map_err(|e| e.to_string())?;
        return Ok(Some(provider));
    }
    if kind == ProviderType::YandexDisk
        && field(&config, "client_id").is_empty()
        && scope
            .read(&oauth_key(OAuthProvider::YandexDisk, id))?
            .is_none()
        && scope.read("oauth_yandexdisk")?.is_none()
    {
        return Ok(None);
    }
    if oauth_kind(kind).is_some() {
        let client = client_settings(scope, profile, &config);
        let provider: Box<dyn StorageProvider> = match kind {
            ProviderType::GoogleDrive => Box::new(
                GoogleDriveProvider::new(google_drive::GoogleDriveConfig::new(
                    &client.0, &client.1,
                ))
                .with_profile_id(id)
                .with_credential_user(scope.user_id),
            ),
            ProviderType::GooglePhotos => Box::new(
                GooglePhotosProvider::new(google_photos::GooglePhotosConfig::new(
                    &client.0, &client.1,
                ))
                .with_profile_id(id)
                .with_credential_user(scope.user_id),
            ),
            ProviderType::Dropbox => Box::new(
                DropboxProvider::new(dropbox::DropboxConfig::new(&client.0, &client.1))
                    .with_profile_id(id)
                    .with_credential_user(scope.user_id),
            ),
            ProviderType::OneDrive => Box::new(
                OneDriveProvider::new(onedrive::OneDriveConfig::new(&client.0, &client.1))
                    .with_profile_id(id)
                    .with_credential_user(scope.user_id),
            ),
            ProviderType::Box => Box::new(
                BoxProvider::new(types::BoxConfig {
                    client_id: client.0,
                    client_secret: client.1,
                })
                .with_profile_id(id)
                .with_credential_user(scope.user_id),
            ),
            ProviderType::PCloud => Box::new(
                PCloudProvider::new(types::PCloudConfig {
                    client_id: client.0,
                    client_secret: client.1,
                    region: client_region(scope, profile, &config),
                })
                .with_profile_id(id)
                .with_credential_user(scope.user_id),
            ),
            ProviderType::ZohoWorkdrive => Box::new(
                ZohoWorkdriveProvider::new(zoho_workdrive::ZohoWorkdriveConfig::new(
                    &client.0,
                    &client.1,
                    &client_region(scope, profile, &config),
                ))
                .with_profile_id(id)
                .with_credential_user(scope.user_id),
            ),
            ProviderType::YandexDisk => {
                let settings = oauth_config(
                    kind,
                    id,
                    &client,
                    &client_region(scope, profile, &config),
                    19847,
                )?;
                let token = OAuth2Manager::new()
                    .with_credential_user(scope.user_id)
                    .get_valid_token(&settings)
                    .await
                    .map_err(|e| format!("{e}; use profile-auth begin/finish"))?;
                Box::new(YandexDiskProvider::new(
                    token.expose_secret().to_owned(),
                    None,
                ))
            }
            _ => unreachable!(),
        };
        return Ok(Some(provider));
    }
    if kind == ProviderType::FourShared {
        let client = client_settings(scope, profile, &config);
        let raw = scope
            .read(&format!("oauth_fourshared_{id}"))?
            .or_else(|| {
                crate::bridge_commands::resolve_profile_oauth_token(scope.store, "fourshared", id)
            })
            .ok_or("4shared authorization missing; use profile-auth configure, begin and finish")?;
        let (token, secret) = raw
            .split_once(':')
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
            .ok_or("Invalid 4shared credentials; authorize again")?;
        if client.0.is_empty() || client.1.is_empty() {
            return Err("4shared client configuration missing; use profile-auth configure".into());
        }
        return Ok(Some(Box::new(FourSharedProvider::new(
            types::FourSharedConfig {
                consumer_key: client.0,
                consumer_secret: secrecy::SecretString::from(client.1),
                access_token: secrecy::SecretString::from(token.to_owned()),
                access_token_secret: secrecy::SecretString::from(secret.to_owned()),
            },
        ))));
    }
    Ok(None)
}

/// A successful server-secret seed must have a sink in this profile's route.
pub fn validate_server_seed(
    scope: Option<&AuthScope<'_>>,
    profile: &Value,
    raw: &str,
) -> Result<(), String> {
    let kind = ProviderType::from_lowercase(field(profile, "protocol"))
        .ok_or("Unknown profile protocol")?;
    if kind.requires_oauth2() || kind == ProviderType::FourShared {
        return Err("This provider stores profile-bound OAuth tokens; use profile-auth configure, begin and finish instead of a server credential".into());
    }
    if kind == ProviderType::Twake {
        let (_, secret) = crate::profile_loader::parse_server_credential(raw, "");
        twake::TwakeCredentials::from_stored(&secret).map_err(|_| {
            "Twake requires its signed-in credential bundle; use profile-auth begin and finish"
        })?;
    }
    if kind == ProviderType::GitHub {
        let config = match scope {
            Some(scope) => scope.config(field(profile, "id"))?,
            None => json!({}),
        };
        if matches!(github_mode(profile, &config), "app" | "authorize") {
            return Err("GitHub App and Device Flow use profile-auth; a server credential is only used in PAT mode".into());
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "profile_auth_tests.rs"]
mod tests;
