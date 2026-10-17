//! Private OAuth discovery, PKCE, issuer/resource binding, and token storage.
//! Browser/callback UI is a later integration step; no flow starts on its own.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::BTreeSet;
use std::fmt;

use base64::Engine;
use rand::RngCore;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroizing;

use crate::mcp_client_http_config::{parse_public_https, McpHttpAuth, McpHttpServerConfig};
use crate::mcp_client_http_transport::{self, AuthChallenge, HttpError, MAX_REPLY_BYTES};
use crate::user_partitions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OAuthError {
    Locked,
    StaleBinding,
    PendingUnavailable,
    Denied,
    InvalidGrant,
    InvalidMetadata,
    AmbiguousIssuer,
    RegistrationRequired,
    InvalidClient,
    InvalidCallback,
    InvalidToken,
    UserChanged,
    StoreUnavailable,
    Http(HttpError),
}

impl From<HttpError> for OAuthError {
    fn from(value: HttpError) -> Self {
        Self::Http(value)
    }
}

#[derive(Clone)]
pub(crate) struct ProtectedResource {
    pub resource: Url,
    pub issuers: Vec<String>,
    pub scopes_supported: Vec<String>,
}

impl fmt::Debug for ProtectedResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtectedResource")
            .field("issuer_count", &self.issuers.len())
            .field("scope_count", &self.scopes_supported.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct AuthorizationServer {
    pub issuer: String,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub registration_endpoint: Option<Url>,
    pub client_id_metadata_supported: bool,
    pub iss_supported: bool,
}

impl fmt::Debug for AuthorizationServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationServer")
            .field(
                "registration_available",
                &self.registration_endpoint.is_some(),
            )
            .field(
                "client_id_metadata_supported",
                &self.client_id_metadata_supported,
            )
            .field("iss_supported", &self.iss_supported)
            .finish_non_exhaustive()
    }
}

pub(crate) struct PendingAuthorization {
    pub user_id: i64,
    pub server_id: String,
    pub resource: String,
    pub issuer: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub state: Zeroizing<String>,
    pub verifier: Zeroizing<String>,
    pub require_iss: bool,
}

impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("user_id", &self.user_id)
            .field("server_id", &self.server_id)
            .field("scope_count", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

pub(crate) struct TokenSet {
    pub access: Zeroizing<String>,
    pub refresh: Option<Zeroizing<String>>,
    pub expires_at: Option<i64>,
    pub scopes: Vec<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("refresh_present", &self.refresh.is_some())
            .field("expires_at", &self.expires_at)
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

fn scope_list(value: Option<&Value>) -> Result<Vec<String>, OAuthError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or(OAuthError::InvalidMetadata)?;
    if array.len() > 32 {
        return Err(OAuthError::InvalidMetadata);
    }
    array
        .iter()
        .map(|scope| {
            let scope = scope.as_str().ok_or(OAuthError::InvalidMetadata)?;
            if scope.is_empty() || scope.len() > 128 || scope.chars().any(char::is_whitespace) {
                return Err(OAuthError::InvalidMetadata);
            }
            Ok(scope.to_owned())
        })
        .collect()
}

fn protected_metadata_paths(endpoint: &Url) -> Vec<Url> {
    let mut scoped = endpoint.clone();
    scoped.set_path(&format!(
        "/.well-known/oauth-protected-resource{}",
        endpoint.path()
    ));
    let mut root = endpoint.clone();
    root.set_path("/.well-known/oauth-protected-resource");
    if scoped == root {
        vec![root]
    } else {
        vec![scoped, root]
    }
}

fn validate_protected_metadata(
    document: &Value,
    endpoint: &Url,
) -> Result<ProtectedResource, OAuthError> {
    let resource = document
        .get("resource")
        .and_then(Value::as_str)
        .ok_or(OAuthError::InvalidMetadata)?;
    let resource = parse_public_https(resource, 2048).map_err(|_| OAuthError::InvalidMetadata)?;
    if &resource != endpoint {
        return Err(OAuthError::InvalidMetadata);
    }
    let issuers = document
        .get("authorization_servers")
        .and_then(Value::as_array)
        .ok_or(OAuthError::InvalidMetadata)?;
    if issuers.is_empty() || issuers.len() > 4 {
        return Err(OAuthError::InvalidMetadata);
    }
    let issuers = issuers
        .iter()
        .map(|issuer| {
            let raw = issuer.as_str().ok_or(OAuthError::InvalidMetadata)?;
            parse_public_https(raw, 2048).map_err(|_| OAuthError::InvalidMetadata)?;
            Ok(raw.to_owned())
        })
        .collect::<Result<Vec<_>, OAuthError>>()?;
    Ok(ProtectedResource {
        resource,
        issuers,
        scopes_supported: scope_list(document.get("scopes_supported"))?,
    })
}

fn issuer_metadata_paths(issuer: &Url) -> Vec<Url> {
    let path = issuer.path().trim_matches('/');
    let mut oauth = issuer.clone();
    let mut oidc_inserted = issuer.clone();
    if path.is_empty() {
        oauth.set_path("/.well-known/oauth-authorization-server");
        oidc_inserted.set_path("/.well-known/openid-configuration");
        vec![oauth, oidc_inserted]
    } else {
        oauth.set_path(&format!("/.well-known/oauth-authorization-server/{path}"));
        oidc_inserted.set_path(&format!("/.well-known/openid-configuration/{path}"));
        let mut oidc_appended = issuer.clone();
        oidc_appended.set_path(&format!("/{path}/.well-known/openid-configuration"));
        vec![oauth, oidc_inserted, oidc_appended]
    }
}

fn validate_authorization_server(
    document: &Value,
    expected_issuer: &str,
) -> Result<AuthorizationServer, OAuthError> {
    if document.get("issuer").and_then(Value::as_str) != Some(expected_issuer) {
        return Err(OAuthError::InvalidMetadata);
    }
    if !document
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method.as_str() == Some("S256")))
    {
        return Err(OAuthError::InvalidMetadata);
    }
    let endpoint = |key: &str| -> Result<Url, OAuthError> {
        let raw = document
            .get(key)
            .and_then(Value::as_str)
            .ok_or(OAuthError::InvalidMetadata)?;
        parse_public_https(raw, 2048).map_err(|_| OAuthError::InvalidMetadata)
    };
    let registration_endpoint = document
        .get("registration_endpoint")
        .and_then(Value::as_str)
        .map(|raw| parse_public_https(raw, 2048).map_err(|_| OAuthError::InvalidMetadata))
        .transpose()?;
    Ok(AuthorizationServer {
        issuer: expected_issuer.to_owned(),
        authorization_endpoint: endpoint("authorization_endpoint")?,
        token_endpoint: endpoint("token_endpoint")?,
        registration_endpoint,
        client_id_metadata_supported: document
            .get("client_id_metadata_document_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        iss_supported: document
            .get("authorization_response_iss_parameter_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

async fn get_json(
    url: &Url,
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<Option<Value>, OAuthError> {
    checkpoint(cancel, fresh)?;
    let client = oauth_client(url, cancel).await?;
    checkpoint(cancel, fresh)?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled.into()),
        result = client.get(wire_url(url)).header(ACCEPT, "application/json").send() =>
            result.map_err(|_| HttpError::Connect)?,
    };
    checkpoint(cancel, fresh)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    checkpoint(cancel, fresh)?;
    if response.status().is_redirection() {
        return Err(HttpError::Redirect.into());
    }
    if !response.status().is_success() {
        return Err(OAuthError::InvalidMetadata);
    }
    let bytes = mcp_client_http_transport::bounded_body(response, cancel).await?;
    checkpoint(cancel, fresh)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| OAuthError::InvalidMetadata)
}

pub(crate) async fn discover(
    config: &McpHttpServerConfig,
    challenge: &AuthChallenge,
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<(ProtectedResource, AuthorizationServer), OAuthError> {
    config.validate().map_err(|_| OAuthError::InvalidMetadata)?;
    if !config.enabled || !matches!(config.auth, McpHttpAuth::OAuth { .. }) {
        return Err(OAuthError::StaleBinding);
    }
    checkpoint(cancel, fresh)?;
    let endpoint =
        parse_public_https(&config.endpoint, 2048).map_err(|_| OAuthError::InvalidMetadata)?;
    let candidates = if let Some(raw) = &challenge.metadata_url {
        let challenged = parse_public_https(raw, 2048).map_err(|_| OAuthError::InvalidMetadata)?;
        if challenged.origin() != endpoint.origin() {
            return Err(OAuthError::InvalidMetadata);
        }
        vec![challenged]
    } else {
        protected_metadata_paths(&endpoint)
    };
    let mut resource = None;
    for url in candidates {
        if let Some(document) = get_json(&url, cancel, fresh).await? {
            resource = Some(validate_protected_metadata(&document, &endpoint)?);
            break;
        }
    }
    let resource = resource.ok_or(OAuthError::InvalidMetadata)?;
    if resource.issuers.len() != 1 {
        return Err(OAuthError::AmbiguousIssuer);
    }
    let issuer_raw = &resource.issuers[0];
    let issuer_url =
        parse_public_https(issuer_raw, 2048).map_err(|_| OAuthError::InvalidMetadata)?;
    let mut server = None;
    for url in issuer_metadata_paths(&issuer_url) {
        if let Some(document) = get_json(&url, cancel, fresh).await? {
            server = Some(validate_authorization_server(&document, issuer_raw)?);
            break;
        }
    }
    checkpoint(cancel, fresh)?;
    Ok((resource, server.ok_or(OAuthError::InvalidMetadata)?))
}

pub(crate) fn configured_client_id(
    config: &McpHttpServerConfig,
    server: &AuthorizationServer,
) -> Result<Option<String>, OAuthError> {
    match &config.auth {
        McpHttpAuth::OAuth {
            client_id,
            client_id_metadata_url,
        } => {
            if let Some(client_id) = client_id {
                return Ok(Some(client_id.clone()));
            }
            if let Some(url) = client_id_metadata_url {
                if !server.client_id_metadata_supported {
                    return Err(OAuthError::InvalidClient);
                }
                return Ok(Some(url.clone()));
            }
            Ok(None)
        }
        _ => Err(OAuthError::InvalidClient),
    }
}

pub(crate) async fn register_dynamic(
    server: &AuthorizationServer,
    redirect_uri: &str,
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<String, OAuthError> {
    let endpoint = server
        .registration_endpoint
        .as_ref()
        .ok_or(OAuthError::RegistrationRequired)?;
    validate_redirect_uri(redirect_uri)?;
    checkpoint(cancel, fresh)?;
    let client = oauth_client(endpoint, cancel).await?;
    checkpoint(cancel, fresh)?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled.into()),
        result = client.post(wire_url(endpoint)).json(&json!({
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "client_name": "AeroFTP"
        })).send() => result.map_err(|_| HttpError::Connect)?,
    };
    checkpoint(cancel, fresh)?;
    if response.status().is_redirection() {
        return Err(HttpError::Redirect.into());
    }
    if !response.status().is_success() {
        return Err(OAuthError::InvalidClient);
    }
    let bytes = mcp_client_http_transport::bounded_body(response, cancel).await?;
    checkpoint(cancel, fresh)?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| OAuthError::InvalidClient)?;
    let client_id = document
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or(OAuthError::InvalidClient)?;
    if client_id.is_empty() || client_id.len() > 512 || client_id.chars().any(char::is_control) {
        return Err(OAuthError::InvalidClient);
    }
    Ok(client_id.to_owned())
}

fn validate_redirect_uri(value: &str) -> Result<(), OAuthError> {
    let url = Url::parse(value).map_err(|_| OAuthError::InvalidCallback)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != "/callback"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OAuthError::InvalidCallback);
    }
    Ok(())
}

fn random_urlsafe() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(bytes.as_mut());
    Zeroizing::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_ref()))
}

#[allow(clippy::too_many_arguments)] // Explicit issuer/resource/user/PKCE scope binding.
pub(crate) fn begin_authorization(
    config: &McpHttpServerConfig,
    resource: &ProtectedResource,
    server: &AuthorizationServer,
    client_id: &str,
    redirect_uri: &str,
    user_id: i64,
    challenge: &AuthChallenge,
    previous_scopes: &[String],
) -> Result<(Url, PendingAuthorization), OAuthError> {
    config.validate().map_err(|_| OAuthError::InvalidClient)?;
    if !config.enabled
        || !matches!(config.auth, McpHttpAuth::OAuth { .. })
        || user_id <= 0
        || client_id.is_empty()
        || client_id.len() > 512
        || client_id.chars().any(char::is_control)
        || resource.resource
            != parse_public_https(&config.endpoint, 2048)
                .map_err(|_| OAuthError::InvalidMetadata)?
    {
        return Err(OAuthError::InvalidClient);
    }
    validate_redirect_uri(redirect_uri)?;
    if !resource.issuers.contains(&server.issuer) {
        return Err(OAuthError::InvalidMetadata);
    }
    let mut scopes = BTreeSet::new();
    let requested = if challenge.scopes.is_empty() {
        &resource.scopes_supported
    } else {
        &challenge.scopes
    };
    for scope in previous_scopes.iter().chain(requested) {
        if scope.is_empty() || scope.len() > 128 || scope.chars().any(char::is_whitespace) {
            return Err(OAuthError::InvalidMetadata);
        }
        scopes.insert(scope.clone());
    }
    if scopes.len() > 32 {
        return Err(OAuthError::InvalidMetadata);
    }
    let state = random_urlsafe();
    let verifier = random_urlsafe();
    let digest = Sha256::digest(verifier.as_bytes());
    let pkce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let mut url = server.authorization_endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("state", &state);
        query.append_pair("code_challenge", &pkce);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("resource", resource.resource.as_str());
        if !scopes.is_empty() {
            query.append_pair(
                "scope",
                &scopes.iter().cloned().collect::<Vec<_>>().join(" "),
            );
        }
    }
    let pending = PendingAuthorization {
        user_id,
        server_id: config.id.clone(),
        resource: resource.resource.as_str().to_owned(),
        issuer: server.issuer.clone(),
        client_id: client_id.to_owned(),
        redirect_uri: redirect_uri.to_owned(),
        scopes: scopes.into_iter().collect(),
        state,
        verifier,
        require_iss: server.iss_supported,
    };
    Ok((url, pending))
}

pub(crate) fn validate_callback(
    pending: &PendingAuthorization,
    returned_state: &str,
    returned_iss: Option<&str>,
    code: &str,
) -> Result<(), OAuthError> {
    if returned_state != pending.state.as_str()
        || code.is_empty()
        || code.len() > 4096
        || code.chars().any(char::is_control)
        || returned_iss.is_some_and(|issuer| issuer != pending.issuer)
        || (pending.require_iss && returned_iss != Some(&pending.issuer))
    {
        return Err(OAuthError::InvalidCallback);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Private one-shot lifecycle supplies the freshness guard.
async fn exchange_code(
    pending: &PendingAuthorization,
    server: &AuthorizationServer,
    code: &str,
    returned_state: &str,
    returned_iss: Option<&str>,
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<TokenSet, OAuthError> {
    validate_callback(pending, returned_state, returned_iss, code)?;
    if pending.issuer != server.issuer {
        return Err(OAuthError::InvalidMetadata);
    }
    checkpoint(cancel, fresh)?;
    let client = oauth_client(&server.token_endpoint, cancel).await?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", pending.client_id.as_str()),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("code_verifier", pending.verifier.as_str()),
            ("resource", pending.resource.as_str()),
        ])
        .finish();
    checkpoint(cancel, fresh)?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled.into()),
        result = client.post(wire_url(&server.token_endpoint))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form).send() => result.map_err(|_| HttpError::Connect)?,
    };
    checkpoint(cancel, fresh)?;
    if response.status().is_redirection() {
        return Err(HttpError::Redirect.into());
    }
    if !response.status().is_success() {
        return Err(OAuthError::InvalidToken);
    }
    let bytes = Zeroizing::new(mcp_client_http_transport::bounded_body(response, cancel).await?);
    checkpoint(cancel, fresh)?;
    parse_token_response(&bytes, &pending.scopes)
}

/// Refresh is explicit: the caller must reload the active user's binding and
/// revalidate config revision before storing the replacement token set.
async fn refresh_access_token(
    resource: &ProtectedResource,
    server: &AuthorizationServer,
    client_id: &str,
    previous: &TokenSet,
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<TokenSet, OAuthError> {
    if !resource.issuers.contains(&server.issuer)
        || client_id.is_empty()
        || client_id.len() > 512
        || client_id.chars().any(char::is_control)
    {
        return Err(OAuthError::InvalidClient);
    }
    let refresh = previous.refresh.as_ref().ok_or(OAuthError::InvalidToken)?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id),
            ("resource", resource.resource.as_str()),
        ])
        .finish();
    checkpoint(cancel, fresh)?;
    let client = oauth_client(&server.token_endpoint, cancel).await?;
    checkpoint(cancel, fresh)?;
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled.into()),
        result = client.post(wire_url(&server.token_endpoint))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form).send() => result.map_err(|_| HttpError::Connect)?,
    };
    checkpoint(cancel, fresh)?;
    if response.status().is_redirection() {
        return Err(HttpError::Redirect.into());
    }
    let status = response.status();
    let bytes = Zeroizing::new(mcp_client_http_transport::bounded_body(response, cancel).await?);
    checkpoint(cancel, fresh)?;
    if !status.is_success() {
        #[derive(serde::Deserialize)]
        struct TokenErrorReply {
            error: String,
            #[serde(default)]
            error_description: Option<String>,
            #[serde(default)]
            error_uri: Option<String>,
        }
        let reply: TokenErrorReply =
            serde_json::from_slice(&bytes).map_err(|_| OAuthError::InvalidToken)?;
        if reply.error.len() > 128
            || reply
                .error_description
                .as_ref()
                .is_some_and(|v| v.len() > 4096)
            || reply.error_uri.as_ref().is_some_and(|v| v.len() > 2048)
        {
            return Err(OAuthError::InvalidToken);
        }
        return Err(
            if status == reqwest::StatusCode::BAD_REQUEST && reply.error == "invalid_grant" {
                OAuthError::InvalidGrant
            } else {
                OAuthError::InvalidToken
            },
        );
    }
    let mut replacement = parse_token_response(&bytes, &previous.scopes)?;
    if replacement.refresh.is_none() {
        replacement.refresh = Some(Zeroizing::new(refresh.to_string()));
    }
    Ok(replacement)
}

fn parse_token_response(bytes: &[u8], requested_scopes: &[String]) -> Result<TokenSet, OAuthError> {
    fn take_string(value: &mut Value) -> Option<String> {
        match std::mem::take(value) {
            Value::String(value) => Some(value),
            _ => None,
        }
    }
    if bytes.len() > MAX_REPLY_BYTES {
        return Err(OAuthError::InvalidToken);
    }
    let mut document: Value =
        serde_json::from_slice(bytes).map_err(|_| OAuthError::InvalidToken)?;
    if document
        .get("token_type")
        .and_then(Value::as_str)
        .is_none_or(|kind| !kind.eq_ignore_ascii_case("Bearer"))
    {
        return Err(OAuthError::InvalidToken);
    }
    let access = document
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or(OAuthError::InvalidToken)?;
    if access.is_empty() || access.len() > 4096 || access.chars().any(char::is_control) {
        return Err(OAuthError::InvalidToken);
    }
    let refresh = document.get("refresh_token").and_then(Value::as_str);
    if refresh.is_some_and(|token| {
        token.is_empty() || token.len() > 4096 || token.chars().any(char::is_control)
    }) {
        return Err(OAuthError::InvalidToken);
    }
    let lifetime = document.get("expires_in").and_then(Value::as_u64);
    if lifetime.is_some_and(|seconds| seconds > 31_536_000) {
        return Err(OAuthError::InvalidToken);
    }
    let scopes = if let Some(raw) = document.get("scope").and_then(Value::as_str) {
        let scopes = raw
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if scopes.len() > 32 || scopes.iter().any(|scope| scope.len() > 128) {
            return Err(OAuthError::InvalidToken);
        }
        scopes
    } else {
        requested_scopes.to_vec()
    };
    let now = chrono::Utc::now().timestamp();
    let access = document
        .get_mut("access_token")
        .and_then(take_string)
        .ok_or(OAuthError::InvalidToken)?;
    let refresh = document.get_mut("refresh_token").and_then(take_string);
    Ok(TokenSet {
        access: Zeroizing::new(access),
        refresh: refresh.map(Zeroizing::new),
        expires_at: lifetime.and_then(|seconds| now.checked_add(seconds as i64)),
        scopes,
    })
}

#[path = "mcp_client_oauth_lifecycle.rs"]
pub(crate) mod lifecycle;
#[path = "mcp_client_oauth_loopback.rs"]
pub(crate) mod loopback;

fn checkpoint(
    cancel: &CancellationToken,
    fresh: &mut impl FnMut() -> Result<(), OAuthError>,
) -> Result<(), OAuthError> {
    if cancel.is_cancelled() {
        return Err(HttpError::Cancelled.into());
    }
    fresh()
}

// Only unit fixtures can inject a wire client. Production always pins public HTTPS.
#[cfg(test)]
tokio::task_local! { static WIRE_CLIENT: reqwest::Client; static WIRE_PORT: u16; }
fn wire_url(url: &Url) -> Url {
    #[cfg(test)]
    if let Ok(port) = WIRE_PORT.try_with(|p| *p) {
        let mut local = Url::parse(&format!("http://127.0.0.1:{port}")).expect("fixture URL");
        local.set_path(url.path());
        return local;
    }
    url.clone()
}
async fn oauth_client(url: &Url, cancel: &CancellationToken) -> Result<reqwest::Client, HttpError> {
    #[cfg(test)]
    if let Ok(client) = WIRE_CLIENT.try_with(Clone::clone) {
        return Ok(client);
    }
    mcp_client_http_transport::pinned_client(url, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Url {
        Url::parse("https://mcp.example.com/public/mcp").unwrap()
    }

    fn resource() -> ProtectedResource {
        validate_protected_metadata(
            &json!({
                "resource": endpoint().as_str(),
                "authorization_servers": ["https://auth.example.com/tenant1"],
                "scopes_supported": ["files:read"]
            }),
            &endpoint(),
        )
        .unwrap()
    }

    fn server() -> AuthorizationServer {
        validate_authorization_server(
            &json!({
                "issuer": "https://auth.example.com/tenant1",
                "authorization_endpoint": "https://auth.example.com/authorize",
                "token_endpoint": "https://auth.example.com/token",
                "registration_endpoint": "https://auth.example.com/register",
                "code_challenge_methods_supported": ["S256"],
                "authorization_response_iss_parameter_supported": true
            }),
            "https://auth.example.com/tenant1",
        )
        .unwrap()
    }

    #[test]
    fn authorization_metadata_requires_explicit_s256_pkce() {
        let base = json!({"issuer":"https://auth.example.com/tenant1",
            "authorization_endpoint":"https://auth.example.com/authorize","token_endpoint":"https://auth.example.com/token"});
        assert!(matches!(
            validate_authorization_server(&base, "https://auth.example.com/tenant1"),
            Err(OAuthError::InvalidMetadata)
        ));
        for methods in [Value::Null, json!("S256"), json!([]), json!(["plain"])] {
            let mut document = base.clone();
            document["code_challenge_methods_supported"] = methods;
            assert!(matches!(
                validate_authorization_server(&document, "https://auth.example.com/tenant1"),
                Err(OAuthError::InvalidMetadata)
            ));
        }
        let mut valid = base;
        valid["code_challenge_methods_supported"] = json!(["S256"]);
        assert!(validate_authorization_server(&valid, "https://auth.example.com/tenant1").is_ok());
    }

    fn config() -> McpHttpServerConfig {
        McpHttpServerConfig {
            id: "remote".into(),
            endpoint: endpoint().as_str().into(),
            auth: McpHttpAuth::OAuth {
                client_id: Some("public-client".into()),
                client_id_metadata_url: None,
            },
            enabled: true,
            revision: 1,
        }
    }

    #[test]
    fn protected_resource_and_issuer_paths_follow_spec_order() {
        let paths = protected_metadata_paths(&endpoint());
        assert_eq!(
            paths[0].as_str(),
            "https://mcp.example.com/.well-known/oauth-protected-resource/public/mcp"
        );
        assert_eq!(
            paths[1].as_str(),
            "https://mcp.example.com/.well-known/oauth-protected-resource"
        );
        let issuer = Url::parse("https://auth.example.com/tenant1").unwrap();
        let paths = issuer_metadata_paths(&issuer);
        assert_eq!(
            paths[0].as_str(),
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1"
        );
        assert_eq!(
            paths[1].as_str(),
            "https://auth.example.com/.well-known/openid-configuration/tenant1"
        );
        assert_eq!(
            paths[2].as_str(),
            "https://auth.example.com/tenant1/.well-known/openid-configuration"
        );
    }

    #[test]
    fn resource_and_issuer_mixups_fail_closed() {
        let wrong_resource = json!({"resource":"https://other.example.com/mcp","authorization_servers":["https://auth.example.com"]});
        assert!(validate_protected_metadata(&wrong_resource, &endpoint()).is_err());
        let wrong_issuer = json!({"issuer":"https://evil.example.com","authorization_endpoint":"https://auth.example.com/authorize","token_endpoint":"https://auth.example.com/token"});
        assert!(validate_authorization_server(&wrong_issuer, "https://auth.example.com").is_err());
        let multiple = json!({"resource":endpoint().as_str(),"authorization_servers":["https://auth.example.com","https://other.example.com"]});
        assert_eq!(
            validate_protected_metadata(&multiple, &endpoint())
                .unwrap()
                .issuers
                .len(),
            2
        );
    }

    #[test]
    fn pkce_state_issuer_and_resource_are_bound() {
        let challenge = AuthChallenge {
            metadata_url: None,
            scopes: vec!["files:write".into()],
        };
        let (url, pending) = begin_authorization(
            &config(),
            &resource(),
            &server(),
            "public-client",
            "http://127.0.0.1:49152/callback",
            7,
            &challenge,
            &["files:read".into()],
        )
        .unwrap();
        let params = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(params.get("resource").unwrap(), &endpoint().as_str());
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(pending.scopes, ["files:read", "files:write"]);
        assert!(validate_callback(
            &pending,
            &pending.state,
            Some("https://auth.example.com/tenant1"),
            "code"
        )
        .is_ok());
        assert_eq!(
            validate_callback(
                &pending,
                "wrong",
                Some("https://auth.example.com/tenant1"),
                "code"
            ),
            Err(OAuthError::InvalidCallback)
        );
        assert_eq!(
            validate_callback(
                &pending,
                &pending.state,
                Some("https://evil.example.com"),
                "code"
            ),
            Err(OAuthError::InvalidCallback)
        );
        assert!(!format!("{pending:?}").contains(pending.verifier.as_str()));
    }

    #[test]
    fn token_values_are_redacted() {
        let tokens = parse_token_response(br#"{"token_type":"Bearer","access_token":"secret-access","refresh_token":"secret-refresh","expires_in":300,"scope":"files:read"}"#, &["files:read".into()]).unwrap();
        assert_eq!(tokens.access.as_str(), "secret-access");
        assert!(!format!("{tokens:?}").contains("secret-access"));
        assert!(
            parse_token_response(br#"{"token_type":"Basic","access_token":"secret"}"#, &[])
                .is_err()
        );
    }
}
