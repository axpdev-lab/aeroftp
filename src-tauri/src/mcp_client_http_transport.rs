//! Private modern Streamable HTTP transport with bounded JSON and SSE replies.
//! All requests use HTTPS, fixed DNS answers, no proxies, and no redirects.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde_json::{Map, Value};
use tokio::net::lookup_host;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

use crate::mcp_client_http_config::{parse_public_https, McpHttpServerConfig};
use crate::mcp_client_protocol::{self as protocol, Era};

pub(crate) const MAX_REPLY_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_HEADERS: usize = 24;
const MAX_HEADER_VALUE_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AuthChallenge {
    pub metadata_url: Option<String>,
    pub scopes: Vec<String>,
}

impl fmt::Debug for AuthChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthChallenge")
            .field("metadata_url_present", &self.metadata_url.is_some())
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    InvalidRequest,
    InvalidEndpoint,
    UnsafeAddress,
    Dns,
    Connect,
    Redirect,
    Unauthorized(AuthChallenge),
    Forbidden(AuthChallenge),
    UnsupportedVersion,
    ResponseTooLarge,
    InvalidResponse,
    Timeout,
    Cancelled,
    StaleBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeaderAnnotation {
    name: HeaderName,
    path: Vec<String>,
    primitive: &'static str,
}

fn header_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn validate_unreachable(value: &Value) -> Result<(), HttpError> {
    match value {
        Value::Object(object) => {
            if object.contains_key("x-mcp-header") {
                return Err(HttpError::InvalidRequest);
            }
            for child in object.values() {
                validate_unreachable(child)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                validate_unreachable(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_annotations(
    schema: &Value,
    path: &mut Vec<String>,
    reachable: bool,
    names: &mut HashSet<String>,
    output: &mut Vec<HeaderAnnotation>,
) -> Result<(), HttpError> {
    let object = schema.as_object().ok_or(HttpError::InvalidRequest)?;
    if let Some(raw) = object.get("x-mcp-header") {
        let suffix = raw.as_str().ok_or(HttpError::InvalidRequest)?;
        let primitive = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(HttpError::InvalidRequest)?;
        if !reachable
            || path.is_empty()
            || !header_token(suffix)
            || !matches!(primitive, "string" | "integer" | "boolean")
            || output.len() >= MAX_SCHEMA_HEADERS
            || !names.insert(suffix.to_ascii_lowercase())
        {
            return Err(HttpError::InvalidRequest);
        }
        let name = HeaderName::from_bytes(format!("Mcp-Param-{suffix}").as_bytes())
            .map_err(|_| HttpError::InvalidRequest)?;
        output.push(HeaderAnnotation {
            name,
            path: path.clone(),
            primitive: match primitive {
                "string" => "string",
                "integer" => "integer",
                _ => "boolean",
            },
        });
    }
    for (key, value) in object {
        if key == "x-mcp-header" {
            continue;
        }
        if key == "properties"
            && reachable
            && object.get("type").and_then(Value::as_str) == Some("object")
        {
            let properties = value.as_object().ok_or(HttpError::InvalidRequest)?;
            for (property, child) in properties {
                path.push(property.clone());
                collect_annotations(child, path, true, names, output)?;
                path.pop();
            }
        } else {
            validate_unreachable(value)?;
        }
    }
    Ok(())
}

fn annotations(schema: &Value) -> Result<Vec<HeaderAnnotation>, HttpError> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(HttpError::InvalidRequest);
    }
    let mut output = Vec::new();
    collect_annotations(
        schema,
        &mut Vec::new(),
        true,
        &mut HashSet::new(),
        &mut output,
    )?;
    Ok(output)
}

fn mirrored_value(value: &str) -> Result<HeaderValue, HttpError> {
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(HttpError::InvalidRequest);
    }
    let plain = value
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte) || byte == b'\t')
        && value.trim() == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="));
    let encoded = if plain {
        value.to_owned()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    };
    HeaderValue::from_str(&encoded).map_err(|_| HttpError::InvalidRequest)
}

fn argument_at<'a>(arguments: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(arguments, |value, key| value.get(key))
}

fn parameter_headers(schema: &Value, arguments: &Value) -> Result<HeaderMap, HttpError> {
    let mut headers = HeaderMap::new();
    for annotation in annotations(schema)? {
        let Some(value) = argument_at(arguments, &annotation.path) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let rendered = match annotation.primitive {
            "string" => value.as_str().ok_or(HttpError::InvalidRequest)?.to_owned(),
            "boolean" => value
                .as_bool()
                .ok_or(HttpError::InvalidRequest)?
                .to_string(),
            "integer" => {
                let number = value.as_i64().ok_or(HttpError::InvalidRequest)?;
                if !(-SAFE_INTEGER..=SAFE_INTEGER).contains(&number) {
                    return Err(HttpError::InvalidRequest);
                }
                number.to_string()
            }
            _ => return Err(HttpError::InvalidRequest),
        };
        headers.insert(annotation.name, mirrored_value(&rendered)?);
    }
    Ok(headers)
}

fn era_headers(
    era: Era,
    method: &str,
    params: &Map<String, Value>,
    schema: Option<&Value>,
) -> Result<HeaderMap, HttpError> {
    if method.is_empty()
        || method.len() > 128
        || !method.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(HttpError::InvalidRequest);
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_static(match era {
            Era::Modern => protocol::MODERN_VERSION,
            Era::Legacy(version) => version,
        }),
    );
    if matches!(era, Era::Legacy(_)) {
        return Ok(headers);
    }
    headers.insert(
        "mcp-method",
        HeaderValue::from_str(method).map_err(|_| HttpError::InvalidRequest)?,
    );
    if matches!(method, "tools/call" | "resources/read" | "prompts/get") {
        let field = if method == "resources/read" {
            "uri"
        } else {
            "name"
        };
        let name = params
            .get(field)
            .and_then(Value::as_str)
            .ok_or(HttpError::InvalidRequest)?;
        headers.insert("mcp-name", mirrored_value(name)?);
    }
    if method == "tools/call" {
        let schema = schema.ok_or(HttpError::InvalidRequest)?;
        let arguments = params.get("arguments").ok_or(HttpError::InvalidRequest)?;
        if !arguments.is_object() {
            return Err(HttpError::InvalidRequest);
        }
        headers.extend(parameter_headers(schema, arguments)?);
    }
    Ok(headers)
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || (b == 168) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        // IPv6 remains fail-closed until a complete special-use range policy
        // and platform-specific pinning tests are available.
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            let octets = ip.octets();
            (octets[0] & 0xe0) == 0x20
                && !(octets[0] == 0x20
                    && octets[1] == 0x01
                    && ((octets[2] == 0x0d && octets[3] == 0xb8)
                        || (octets[2] == 0x00 && octets[3] == 0x00)))
                && !(octets[0] == 0x20 && octets[1] == 0x02)
        }
    }
}

pub(crate) async fn pinned_client(
    url: &Url,
    cancel: &CancellationToken,
) -> Result<Client, HttpError> {
    let Host::Domain(host) = url.host().ok_or(HttpError::InvalidEndpoint)? else {
        return Err(HttpError::InvalidEndpoint);
    };
    let port = url
        .port_or_known_default()
        .ok_or(HttpError::InvalidEndpoint)?;
    let addresses = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(5), lookup_host((host, port))) => {
            result.map_err(|_| HttpError::Timeout)?
                .map_err(|_| HttpError::Dns)?
                .collect::<Vec<SocketAddr>>()
        }
    };
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err(HttpError::UnsafeAddress);
    }
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(REQUEST_TIMEOUT)
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| HttpError::Connect)
}

pub(crate) async fn bounded_body(
    response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, HttpError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(HttpError::Cancelled),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else { return Ok(bytes) };
        let chunk = chunk.map_err(|_| HttpError::Connect)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_REPLY_BYTES {
            return Err(HttpError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
}

async fn bounded_sse(
    response: reqwest::Response,
    expected_id: u64,
    era: Era,
    cancel: &CancellationToken,
) -> Result<Value, HttpError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(HttpError::Cancelled),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            return era_sse(&bytes, expected_id, era);
        };
        let chunk = chunk.map_err(|_| HttpError::Connect)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_REPLY_BYTES {
            return Err(HttpError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
        let lf_end = bytes
            .windows(2)
            .enumerate()
            .filter(|(_, window)| *window == b"\n\n")
            .map(|(index, _)| index + 2)
            .next_back();
        let crlf_end = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, window)| *window == b"\r\n\r\n")
            .map(|(index, _)| index + 4)
            .next_back();
        let end = lf_end.into_iter().chain(crlf_end).max();
        if let Some(end) = end {
            if let Ok(result) = era_sse(&bytes[..end], expected_id, era) {
                return Ok(result);
            }
        }
    }
}

fn era_sse(bytes: &[u8], expected_id: u64, era: Era) -> Result<Value, HttpError> {
    let input = std::str::from_utf8(bytes).map_err(|_| HttpError::InvalidResponse)?;
    let mut data = String::new();
    let mut result = None;
    for line in input.lines().chain(std::iter::once("")) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(data.trim_end_matches('\n'))
                .map_err(|_| HttpError::InvalidResponse)?;
            data.clear();
            if message.get("method").is_some() {
                if message.get("id").is_some()
                    || message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                    || !message
                        .get("method")
                        .and_then(Value::as_str)
                        .is_some_and(|method| method.starts_with("notifications/"))
                {
                    return Err(HttpError::InvalidResponse);
                }
                continue;
            }
            if result.is_some() {
                return Err(HttpError::InvalidResponse);
            }
            result = Some(
                protocol::accept_result(&message, expected_id, era)
                    .map_err(|_| HttpError::InvalidResponse)?
                    .clone(),
            );
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
        } else if !line.starts_with(':')
            && !line.starts_with("event:")
            && !line.starts_with("id:")
            && !line.starts_with("retry:")
        {
            return Err(HttpError::InvalidResponse);
        }
    }
    result.ok_or(HttpError::InvalidResponse)
}

fn parse_challenge(value: Option<&HeaderValue>) -> AuthChallenge {
    let mut parsed = AuthChallenge {
        metadata_url: None,
        scopes: Vec::new(),
    };
    let Some(raw) = value.and_then(|value| value.to_str().ok()) else {
        return parsed;
    };
    if raw.len() > 4096
        || raw.len() < 7
        || !raw
            .get(..6)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("Bearer"))
        || raw.as_bytes()[6] != b' '
    {
        return parsed;
    }
    for part in raw[7..].split(',') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if value.chars().any(char::is_control) || value.len() > 2048 {
            continue;
        }
        match key.trim() {
            "resource_metadata" => {
                if parse_public_https(value, 2048).is_ok() {
                    parsed.metadata_url = Some(value.to_owned());
                }
            }
            "scope" => {
                parsed.scopes = value
                    .split_whitespace()
                    .filter(|scope| scope.len() <= 128)
                    .take(32)
                    .map(str::to_owned)
                    .collect();
            }
            _ => {}
        }
    }
    parsed
}

fn era_reply(
    content_type: &str,
    bytes: &[u8],
    expected_id: u64,
    era: Era,
) -> Result<Value, HttpError> {
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    match media_type {
        "application/json" => {
            let message: Value =
                serde_json::from_slice(bytes).map_err(|_| HttpError::InvalidResponse)?;
            protocol::accept_result(&message, expected_id, era)
                .map_err(|_| HttpError::InvalidResponse)
                .cloned()
        }
        "text/event-stream" => era_sse(bytes, expected_id, era),
        _ => Err(HttpError::InvalidResponse),
    }
}

fn authorization_header(token: &str) -> Result<HeaderValue, HttpError> {
    if token.is_empty() || token.len() > MAX_HEADER_VALUE_BYTES {
        return Err(HttpError::InvalidRequest);
    }
    let mut value =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| HttpError::InvalidRequest)?;
    value.set_sensitive(true);
    Ok(value)
}

#[allow(clippy::too_many_arguments)] // Explicit era/session and prepared wire request.
async fn exchange(
    era: Era,
    session: Option<&HeaderValue>,
    notification: bool,
    discovery_probe: bool,
    client: &Client,
    url: Url,
    mut headers: HeaderMap,
    body: Vec<u8>,
    id: u64,
    cancel: &CancellationToken,
) -> Result<(Value, Option<HeaderValue>), HttpError> {
    if let Some(session) = session {
        headers.insert("mcp-session-id", session.clone());
    }
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(HttpError::Cancelled),
        result = client.post(url).headers(headers).body(body).send() => result.map_err(|error| if error.is_timeout() { HttpError::Timeout } else { HttpError::Connect })?,
    };
    if response.status().is_redirection() {
        return Err(HttpError::Redirect);
    }
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(HttpError::Unauthorized(parse_challenge(
            response.headers().get("www-authenticate"),
        )));
    }
    if response.status() == StatusCode::FORBIDDEN {
        return Err(HttpError::Forbidden(parse_challenge(
            response.headers().get("www-authenticate"),
        )));
    }
    if response.status() == StatusCode::BAD_REQUEST {
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v.split(';').next().unwrap_or("").trim() != "application/json")
        {
            return Err(HttpError::InvalidResponse);
        }
        let body = bounded_body(response, cancel).await?;
        let error: Value = serde_json::from_slice(&body).map_err(|_| HttpError::InvalidResponse)?;
        if era == Era::Modern
            && (discovery_probe && sdk_legacy_probe(&error, id)
                || authoritative_legacy(&error, id)?)
        {
            return Err(HttpError::UnsupportedVersion);
        }
        return Err(HttpError::InvalidResponse);
    }
    if !response.status().is_success() {
        return Err(HttpError::InvalidResponse);
    }
    let received_session = session_header(response.headers())?;
    if matches!(era, Era::Modern) && received_session.is_some()
        || session.is_some()
            && received_session
                .as_ref()
                .is_some_and(|v| Some(v) != session)
    {
        return Err(HttpError::InvalidResponse);
    }
    if notification {
        if response.status() != StatusCode::ACCEPTED
            || !bounded_body(response, cancel).await?.is_empty()
        {
            return Err(HttpError::InvalidResponse);
        }
        return Ok((Value::Null, received_session));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(HttpError::InvalidResponse)?
        .to_owned();
    if content_type
        .split(';')
        .next()
        .is_some_and(|media| media.trim() == "text/event-stream")
    {
        Ok((
            bounded_sse(response, id, era, cancel).await?,
            received_session,
        ))
    } else {
        let bytes = bounded_body(response, cancel).await?;
        Ok((era_reply(&content_type, &bytes, id, era)?, received_session))
    }
}

#[cfg(test)]
fn request_headers(
    method: &str,
    params: &Map<String, Value>,
    schema: Option<&Value>,
) -> Result<HeaderMap, HttpError> {
    era_headers(Era::Modern, method, params, schema)
}
#[cfg(test)]
fn parse_reply(content_type: &str, bytes: &[u8], id: u64) -> Result<Value, HttpError> {
    era_reply(content_type, bytes, id, Era::Modern)
}
#[cfg(test)]
async fn send_prepared(
    client: &Client,
    url: Url,
    headers: HeaderMap,
    body: Vec<u8>,
    id: u64,
    cancel: &CancellationToken,
) -> Result<Value, HttpError> {
    Ok(exchange(
        Era::Modern,
        None,
        false,
        false,
        client,
        url,
        headers,
        body,
        id,
        cancel,
    )
    .await?
    .0)
}

/// SDK pre-handshake errors may have no correlated numeric ID. This narrow
/// exception is called only for HTTP 400 JSON on the first server/discover,
/// never for an established session or an approved operation. No message text
/// is interpreted as a version; a fresh initialize must validate the revision.
fn sdk_legacy_probe(message: &Value, id: u64) -> bool {
    let Some(reply_id) = message.get("id") else {
        return false;
    };
    let accepted_id = reply_id == &serde_json::json!(id)
        || reply_id.is_null()
        || reply_id.as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 128
                && !s.chars().any(char::is_control)
                && s.parse::<f64>().is_err()
        });
    if !accepted_id {
        return false;
    }
    // Normalize only this disposable probe ID and reuse strict envelope checks.
    let mut correlated = message.clone();
    correlated["id"] = serde_json::json!(id);
    matches!(
        protocol::reply(&correlated, id),
        Ok(protocol::Reply::Error {
            code: -32600 | -32601,
            ..
        })
    )
}

/// A downgrade needs a correlated, well-formed error explicitly excluding the
/// requested modern revision and advertising a pinned legacy revision.
fn authoritative_legacy(message: &Value, id: u64) -> Result<bool, HttpError> {
    let protocol::Reply::Error {
        code: -32022,
        data: Some(data),
    } = protocol::reply(message, id).map_err(|_| HttpError::InvalidResponse)?
    else {
        return Ok(false);
    };
    let versions = data
        .get("supported")
        .and_then(Value::as_array)
        .ok_or(HttpError::InvalidResponse)?;
    if data.get("requested").and_then(Value::as_str) != Some(protocol::MODERN_VERSION)
        || versions.is_empty()
        || versions.len() > 32
        || !versions
            .iter()
            .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64))
    {
        return Err(HttpError::InvalidResponse);
    }
    Ok(!versions.iter().any(|v| v == protocol::MODERN_VERSION)
        && versions
            .iter()
            .any(|v| v == protocol::LEGACY_PREFERRED || v == protocol::LEGACY_AEROFTP))
}

fn session_header(headers: &HeaderMap) -> Result<Option<HeaderValue>, HttpError> {
    let mut values = headers.get_all("mcp-session-id").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some()
        || value.is_empty()
        || value.as_bytes().len() > 256
        || !value.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b))
    {
        return Err(HttpError::InvalidResponse);
    }
    let mut value = value.clone();
    value.set_sensitive(true);
    Ok(Some(value))
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpBinding {
    pub endpoint: String,
    pub user_id: i64,
    pub revision: String,
}
/// Never persisted or reused across users, endpoints or effective revisions.
pub(crate) struct HttpSession {
    binding: HttpBinding,
    era: Era,
    session: Option<HeaderValue>,
    id: u64,
    negotiated: bool,
    failed: bool,
}
impl HttpSession {
    pub(crate) fn new(binding: HttpBinding) -> Self {
        Self {
            binding,
            era: Era::Modern,
            session: None,
            id: 0,
            negotiated: false,
            failed: false,
        }
    }
    fn next_id(&mut self) -> Result<u64, HttpError> {
        self.id = self.id.checked_add(1).ok_or(HttpError::InvalidRequest)?;
        Ok(self.id)
    }
    #[allow(clippy::too_many_arguments)] // Explicit private security and wire boundaries.
    async fn wire<F, E>(
        &mut self,
        client: &Client,
        url: &Url,
        method: &str,
        params: Map<String, Value>,
        schema: Option<&Value>,
        token: Option<&str>,
        cancel: &CancellationToken,
        fresh: &mut F,
    ) -> Result<Value, E>
    where
        F: FnMut() -> Result<(), E>,
        E: From<HttpError>,
    {
        fresh()?;
        let id = self.next_id()?;
        let mut headers = era_headers(self.era, method, &params, schema)?;
        if let Some(token) = token {
            headers.insert(AUTHORIZATION, authorization_header(token)?);
        }
        let notification = method == "notifications/initialized";
        let body = if notification {
            protocol::initialized_notification()
        } else if method == "initialize" {
            protocol::initialize_request(id, "AeroFTP", env!("CARGO_PKG_VERSION"))
                .map_err(|_| HttpError::InvalidRequest)?
        } else {
            protocol::request(
                self.era,
                id,
                method,
                params,
                "AeroFTP",
                env!("CARGO_PKG_VERSION"),
            )
            .map_err(|_| HttpError::InvalidRequest)?
        };
        let bytes = serde_json::to_vec(&body).map_err(|_| HttpError::InvalidRequest)?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err(HttpError::InvalidRequest.into());
        }
        let response = exchange(
            self.era,
            self.session.as_ref(),
            notification,
            false,
            client,
            url.clone(),
            headers,
            bytes,
            id,
            cancel,
        )
        .await;
        fresh()?;
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled.into());
        }
        let (result, session) = response?;
        if method == "initialize" {
            // Reconstruct only the already correlated result for the existing validator.
            self.era = protocol::accept_legacy_initialize(
                &serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
                id,
            )
            .map_err(|_| HttpError::InvalidResponse)?;
            self.session = session;
        } else if session.is_some() && self.session.is_none() {
            return Err(HttpError::InvalidResponse.into());
        }
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    async fn call_wire<F, E>(
        &mut self,
        binding: &HttpBinding,
        client: &Client,
        url: &Url,
        method: &str,
        params: Map<String, Value>,
        schema: Option<&Value>,
        token: Option<&str>,
        cancel: &CancellationToken,
        fresh: &mut F,
    ) -> Result<Value, E>
    where
        F: FnMut() -> Result<(), E>,
        E: From<HttpError>,
    {
        if self.failed
            || &self.binding != binding
            || Url::parse(&binding.endpoint).as_ref() != Ok(url)
        {
            self.failed = true;
            self.session = None;
            return Err(HttpError::StaleBinding.into());
        }
        // Negotiation can retry discovery, never an approved mutating call.
        if !self.negotiated && method != "tools/list" {
            return Err(HttpError::InvalidRequest.into());
        }
        let result = async {
            if !self.negotiated {
                // Keep the typed transport outcome separate from caller freshness errors.
                let mut wire_fresh = || fresh();
                let id = self.next_id()?;
                wire_fresh()?;
                let mut headers = era_headers(Era::Modern, "server/discover", &Map::new(), None)?;
                if let Some(token) = token {
                    headers.insert(AUTHORIZATION, authorization_header(token)?);
                }
                let body = protocol::discover_request(id, "AeroFTP", env!("CARGO_PKG_VERSION"))
                    .map_err(|_| HttpError::InvalidRequest)?;
                let bytes = serde_json::to_vec(&body).map_err(|_| HttpError::InvalidRequest)?;
                if bytes.len() > MAX_REPLY_BYTES {
                    return Err(HttpError::InvalidRequest.into());
                }
                let outcome = exchange(
                    Era::Modern,
                    None,
                    false,
                    true,
                    client,
                    url.clone(),
                    headers,
                    bytes,
                    id,
                    cancel,
                )
                .await;
                wire_fresh()?;
                if cancel.is_cancelled() {
                    return Err(HttpError::Cancelled.into());
                }
                match outcome {
                    Ok((value, _)) => {
                        protocol::classify_probe(
                            protocol::ProbeReply::Message(
                                &serde_json::json!({"jsonrpc":"2.0","id":id,"result":value}),
                            ),
                            id,
                        )
                        .map_err(|_| HttpError::InvalidResponse)?;
                        self.negotiated = true;
                    }
                    Err(HttpError::UnsupportedVersion) => {
                        self.era = Era::Legacy(protocol::LEGACY_PREFERRED);
                        self.wire(
                            client,
                            url,
                            "initialize",
                            Map::new(),
                            None,
                            token,
                            cancel,
                            fresh,
                        )
                        .await?;
                        self.wire(
                            client,
                            url,
                            "notifications/initialized",
                            Map::new(),
                            None,
                            token,
                            cancel,
                            fresh,
                        )
                        .await?;
                        self.negotiated = true;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            self.wire(client, url, method, params, schema, token, cancel, fresh)
                .await
        }
        .await;
        if result.is_err() {
            self.failed = true;
            self.session = None;
        }
        result
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_checked<F, E>(
        &mut self,
        config: &McpHttpServerConfig,
        binding: &HttpBinding,
        method: &str,
        params: Map<String, Value>,
        schema: Option<&Value>,
        token: Option<&str>,
        cancel: &CancellationToken,
        fresh: &mut F,
    ) -> Result<Value, E>
    where
        F: FnMut() -> Result<(), E>,
        E: From<HttpError>,
    {
        let result = async {
            fresh()?;
            config.validate().map_err(|_| HttpError::InvalidEndpoint)?;
            if !config.enabled || config.endpoint != binding.endpoint || self.binding != *binding {
                return Err(HttpError::StaleBinding.into());
            }
            let url = parse_public_https(&config.endpoint, 2048)
                .map_err(|_| HttpError::InvalidEndpoint)?;
            let client = pinned_client(&url, cancel).await?;
            self.call_wire(
                binding, &client, &url, method, params, schema, token, cancel, fresh,
            )
            .await
        }
        .await;
        if result.is_err() {
            self.failed = true;
            self.session = None;
        }
        result
    }
}

#[cfg(test)]
impl HttpSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fixture_call_checked<F, E>(
        &mut self,
        binding: &HttpBinding,
        url: &Url,
        method: &str,
        params: Map<String, Value>,
        schema: Option<&Value>,
        cancel: &CancellationToken,
        fresh: &mut F,
    ) -> Result<Value, E>
    where
        F: FnMut() -> Result<(), E>,
        E: From<HttpError>,
    {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| HttpError::Connect)?;
        self.call_wire(
            binding, &client, url, method, params, schema, None, cancel, fresh,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn sequence_fixture(
        responses: Vec<Vec<u8>>,
    ) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/mcp", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(split) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..split]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= split + 4 + length {
                            break;
                        }
                    }
                    assert!(bytes.len() <= MAX_REPLY_BYTES + 4096);
                }
                requests.push(String::from_utf8(bytes).unwrap());
                socket.write_all(&response).await.unwrap();
            }
            requests
        });
        (url, task)
    }
    fn binding(url: &Url) -> HttpBinding {
        HttpBinding {
            endpoint: url.to_string(),
            user_id: 1,
            revision: "revision-1".into(),
        }
    }
    fn downgrade(id: u64) -> Value {
        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32022,"message":"unsupported","data":{"requested":protocol::MODERN_VERSION,"supported":[protocol::LEGACY_PREFERRED, protocol::LEGACY_AEROFTP]}}})
    }
    fn legacy_responses(version: &str, sse: bool) -> Vec<Vec<u8>> {
        let init = json!({"jsonrpc":"2.0","id":2,"result":{"protocolVersion":version,"capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}});
        let list = json!({"jsonrpc":"2.0","id":4,"result":{"tools":[]}}).to_string();
        vec![
            wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                &downgrade(1).to_string(),
            ),
            wire_reply(
                "200 OK",
                "application/json",
                "Mcp-Session-Id: private-fixture-session\r\n",
                &init.to_string(),
            ),
            wire_reply("202 Accepted", "application/json", "", ""),
            wire_reply(
                "200 OK",
                if sse {
                    "text/event-stream"
                } else {
                    "application/json"
                },
                "",
                &if sse {
                    format!("data: {list}\n\n")
                } else {
                    list
                },
            ),
        ]
    }
    #[tokio::test]
    async fn guarded_legacy_negotiation_initializes_and_binds_json_and_sse_session() {
        for (version, sse) in [
            (protocol::LEGACY_PREFERRED, false),
            (protocol::LEGACY_AEROFTP, true),
        ] {
            let (url, peer) = sequence_fixture(legacy_responses(version, sse)).await;
            let binding = binding(&url);
            let mut state = HttpSession::new(binding.clone());
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &binding,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert_eq!(result.unwrap()["tools"], json!([]));
            assert_eq!(state.era, Era::Legacy(version));
            assert!(state.session.as_ref().unwrap().is_sensitive());
            let requests = peer.await.unwrap();
            assert_eq!(requests.len(), 4);
            assert!(requests[0].contains("io.modelcontextprotocol/protocolVersion"));
            assert!(!requests[1].contains("mcp-session-id:"));
            for (i, method) in [
                (1, "initialize"),
                (2, "notifications/initialized"),
                (3, "tools/list"),
            ] {
                let (header, body) = requests[i].split_once("\r\n\r\n").unwrap();
                assert!(!header.contains("mcp-method:"));
                assert!(!body.contains("_meta"));
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap()["method"],
                    method
                );
                if i >= 2 {
                    assert!(header.contains("mcp-session-id: private-fixture-session"));
                    assert!(header.contains(&format!("mcp-protocol-version: {version}")));
                }
            }
        }
    }
    #[tokio::test]
    async fn deepwiki_sdk_probe_error_initializes_fresh_legacy_sse_session() {
        let captured = include_str!("../tests/fixtures/mcp-client/deepwiki-modern-probe.body.json");
        for id in [json!("server-error"), Value::Null, json!(1)] {
            let mut error: Value = serde_json::from_str(captured).unwrap();
            error["id"] = id;
            for code in [-32600, -32601] {
                error["error"]["code"] = json!(code);
                let mut responses = legacy_responses(protocol::LEGACY_PREFERRED, true);
                responses[0] = wire_reply(
                    "400 Bad Request",
                    "application/json",
                    "Mcp-Session-Id: failed-probe-session\r\n",
                    &error.to_string(),
                );
                let (url, peer) = sequence_fixture(responses).await;
                let binding = binding(&url);
                let mut state = HttpSession::new(binding.clone());
                let result: Result<Value, HttpError> = state
                    .call_wire(
                        &binding,
                        &test_client(),
                        &url,
                        "tools/list",
                        Map::new(),
                        None,
                        None,
                        &CancellationToken::new(),
                        &mut || Ok(()),
                    )
                    .await;
                assert_eq!(result.unwrap()["tools"], json!([]));
                assert_eq!(state.era, Era::Legacy(protocol::LEGACY_PREFERRED));
                let requests = peer.await.unwrap();
                let probe: Value =
                    serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_eq!(probe["method"], "server/discover");
                assert!(requests.iter().all(|r| !r.contains("failed-probe-session")));
                assert!(!requests[1].contains("mcp-session-id:"));
            }
        }
    }

    #[tokio::test]
    async fn sdk_probe_errors_never_downgrade_a_negotiated_session() {
        for era in [Era::Modern, Era::Legacy(protocol::LEGACY_PREFERRED)] {
            let captured =
                include_str!("../tests/fixtures/mcp-client/deepwiki-modern-probe.body.json");
            let (url, peer) = sequence_fixture(vec![wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                captured,
            )])
            .await;
            let binding = binding(&url);
            let mut state = HttpSession::new(binding.clone());
            state.negotiated = true;
            state.era = era;
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &binding,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert_eq!(result, Err(HttpError::InvalidResponse));
            assert!(state.failed);
            assert_eq!(peer.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn sdk_fallback_rejects_unrelated_ids_envelopes_statuses_and_media_types() {
        let captured: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/mcp-client/deepwiki-modern-probe.body.json"
        ))
        .unwrap();
        let mut cases = Vec::new();
        for (pointer, value) in [
            ("/id", json!(2)),
            ("/id", json!("2")),
            ("/id", json!(true)),
            ("/error/code", json!(-32700)),
            ("/jsonrpc", json!("1.0")),
            ("/error/message", json!("")),
        ] {
            let mut error = captured.clone();
            *error.pointer_mut(pointer).unwrap() = value;
            cases.push(wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                &error.to_string(),
            ));
        }
        for extra in ["method", "result"] {
            let mut error = captured.clone();
            error[extra] = json!({});
            cases.push(wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                &error.to_string(),
            ));
        }
        for (status, media) in [
            ("400 Bad Request", "text/html"),
            ("500 Internal Server Error", "application/json"),
            ("200 OK", "application/json"),
        ] {
            cases.push(wire_reply(status, media, "", &captured.to_string()));
        }
        for response in cases {
            let (url, peer) = sequence_fixture(vec![response]).await;
            let binding = binding(&url);
            let mut state = HttpSession::new(binding.clone());
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &binding,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert_eq!(result, Err(HttpError::InvalidResponse));
            assert!(state.failed);
            assert_eq!(peer.await.unwrap().len(), 1);
        }
    }

    #[test]
    fn downgrade_requires_authoritative_version_and_correlated_envelope() {
        assert_eq!(authoritative_legacy(&downgrade(7), 7), Ok(true));
        let mut bad = Vec::new();
        for (pointer, value) in [
            ("/id", json!(8)),
            ("/jsonrpc", json!("1.0")),
            ("/error/data/requested", json!(protocol::LEGACY_PREFERRED)),
            ("/error/data/supported", json!([])),
            ("/error/data/supported", json!([42])),
        ] {
            let mut message = downgrade(7);
            *message.pointer_mut(pointer).unwrap() = value;
            bad.push(message);
        }
        let mut both = downgrade(7);
        both["result"] = json!({});
        bad.push(both);
        let mut server_request = downgrade(7);
        server_request["method"] = json!("sampling/createMessage");
        bad.push(server_request);
        for message in bad {
            assert!(authoritative_legacy(&message, 7).is_err());
        }
        for versions in [
            json!([protocol::MODERN_VERSION, protocol::LEGACY_PREFERRED]),
            json!(["2027-01-01"]),
        ] {
            let mut message = downgrade(7);
            message["error"]["data"]["supported"] = versions;
            assert_eq!(authoritative_legacy(&message, 7), Ok(false));
        }
        for code in [-32020, -32601, -1] {
            let mut message = downgrade(7);
            message["error"]["code"] = json!(code);
            assert_eq!(authoritative_legacy(&message, 7), Ok(false));
        }
    }
    #[tokio::test]
    async fn guarded_negotiation_never_retries_auth_arbitrary_error_or_malformed_reply() {
        for response in [
            wire_reply("401 Unauthorized", "application/json", "", ""),
            wire_reply("403 Forbidden", "application/json", "", ""),
            wire_reply("400 Bad Request", "application/json", "", "{}"),
            wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                &downgrade(9).to_string(),
            ),
            wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32022,"message":"unsupported"}}"#,
            ),
            wire_reply("200 OK", "application/json", "", "bad json"),
        ] {
            let (url, peer) = sequence_fixture(vec![response]).await;
            let binding = binding(&url);
            let mut state = HttpSession::new(binding.clone());
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &binding,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert!(result.is_err());
            assert!(state.failed);
            assert!(state.session.is_none());
            assert_eq!(peer.await.unwrap().len(), 1);
        }
    }
    #[tokio::test]
    async fn modern_success_stays_stateless_and_later_version_error_never_replays_call() {
        let first = json!({"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":[protocol::MODERN_VERSION],"capabilities":{},"ttlMs":0,"cacheScope":"private"}});
        let (url, peer) = sequence_fixture(vec![
            wire_reply("200 OK", "application/json", "", &first.to_string()),
            wire_reply(
                "200 OK",
                "application/json",
                "",
                &json!({"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","tools":[]}})
                    .to_string(),
            ),
            wire_reply(
                "400 Bad Request",
                "application/json",
                "",
                &downgrade(3).to_string(),
            ),
        ])
        .await;
        let binding = binding(&url);
        let mut state = HttpSession::new(binding.clone());
        let client = test_client();
        let cancel = CancellationToken::new();
        let result: Result<Value, HttpError> = state
            .call_wire(
                &binding,
                &client,
                &url,
                "tools/list",
                Map::new(),
                None,
                None,
                &cancel,
                &mut || Ok(()),
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(state.era, Era::Modern);
        assert!(state.session.is_none());
        let schema = json!({"type":"object","properties":{}});
        let params = Map::from_iter([
            ("name".into(), json!("echo")),
            ("arguments".into(), json!({})),
        ]);
        let result: Result<Value, HttpError> = state
            .call_wire(
                &binding,
                &client,
                &url,
                "tools/call",
                params,
                Some(&schema),
                None,
                &cancel,
                &mut || Ok(()),
            )
            .await;
        assert_eq!(result, Err(HttpError::UnsupportedVersion));
        let requests = peer.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(!requests.iter().any(|r| r.contains("mcp-session-id:")));
    }
    #[tokio::test]
    async fn legacy_handshake_rejects_unknown_version_id_and_session_replacement() {
        for variant in 0..4 {
            let mut responses = legacy_responses(protocol::LEGACY_PREFERRED, false);
            match variant {
                0 => responses[1] = wire_reply("200 OK", "application/json", "", &json!({"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}}).to_string()),
                1 => responses[1] = wire_reply("200 OK", "application/json", "", &json!({"jsonrpc":"2.0","id":99,"result":{"protocolVersion":protocol::LEGACY_PREFERRED}}).to_string()),
                2 => responses[2] = wire_reply("202 Accepted", "application/json", "Mcp-Session-Id: swapped\r\n", ""),
                _ => responses[3] = wire_reply("200 OK", "application/json", "Mcp-Session-Id: swapped\r\n", r#"{"jsonrpc":"2.0","id":4,"result":{"tools":[]}}"#),
            }
            let limit = if variant < 2 { 2 } else { variant + 1 };
            responses.truncate(limit);
            let (url, peer) = sequence_fixture(responses).await;
            let binding = binding(&url);
            let mut state = HttpSession::new(binding.clone());
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &binding,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert_eq!(result, Err(HttpError::InvalidResponse));
            assert!(state.session.is_none());
            assert_eq!(peer.await.unwrap().len(), limit);
        }
    }
    #[tokio::test]
    async fn timeout_and_network_failure_never_negotiate_legacy() {
        let (url, peer) = wire_fixture(
            wire_reply("200 OK", "application/json", "", "{}"),
            Duration::from_secs(1),
        )
        .await;
        let binding = binding(&url);
        let mut state = HttpSession::new(binding.clone());
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let result: Result<Value, HttpError> = state
            .call_wire(
                &binding,
                &client,
                &url,
                "tools/list",
                Map::new(),
                None,
                None,
                &CancellationToken::new(),
                &mut || Ok(()),
            )
            .await;
        assert_eq!(result, Err(HttpError::Timeout));
        assert_eq!(state.era, Era::Modern);
        assert!(state.failed);
        peer.abort();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/mcp", listener.local_addr().unwrap())).unwrap();
        drop(listener);
        let binding = super::tests::binding(&url);
        let mut state = HttpSession::new(binding.clone());
        let result: Result<Value, HttpError> = state
            .call_wire(
                &binding,
                &client,
                &url,
                "tools/list",
                Map::new(),
                None,
                None,
                &CancellationToken::new(),
                &mut || Ok(()),
            )
            .await;
        // A refused localhost connection can exhaust the request deadline on
        // Windows. Both are terminal network errors, never legacy evidence.
        assert!(matches!(
            result,
            Err(HttpError::Connect | HttpError::Timeout)
        ));
        assert_eq!(state.era, Era::Modern);
        assert!(state.failed);
    }
    #[test]
    fn session_ids_are_bounded_unique_visible_ascii_and_sensitive() {
        for value in ["", "has space", "tab\tvalue", &"x".repeat(257)] {
            let mut headers = HeaderMap::new();
            headers.insert("mcp-session-id", HeaderValue::from_str(value).unwrap());
            assert!(session_header(&headers).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.append("mcp-session-id", HeaderValue::from_static("one"));
        headers.append("mcp-session-id", HeaderValue::from_static("two"));
        assert!(session_header(&headers).is_err());
    }
    #[tokio::test]
    async fn stale_user_config_or_endpoint_cannot_reuse_session() {
        let url = Url::parse("http://127.0.0.1:1/mcp").unwrap();
        let original = binding(&url);
        for variant in 0..3 {
            let mut state = HttpSession::new(original.clone());
            state.era = Era::Legacy(protocol::LEGACY_PREFERRED);
            state.negotiated = true;
            state.session = Some(HeaderValue::from_static("private-session"));
            let mut changed = original.clone();
            match variant {
                0 => changed.user_id += 1,
                1 => changed.revision.push('2'),
                _ => changed.endpoint.push('2'),
            };
            let result: Result<Value, HttpError> = state
                .call_wire(
                    &changed,
                    &test_client(),
                    &url,
                    "tools/list",
                    Map::new(),
                    None,
                    None,
                    &CancellationToken::new(),
                    &mut || Ok(()),
                )
                .await;
            assert_eq!(result, Err(HttpError::StaleBinding));
            assert!(state.failed);
            assert!(state.session.is_none());
        }
    }
    #[tokio::test]
    async fn freshness_and_cancellation_guard_every_negotiation_call_and_result() {
        for stop_at in 1..=8 {
            for cancelled in [false, true] {
                let (url, peer) =
                    sequence_fixture(legacy_responses(protocol::LEGACY_PREFERRED, false)).await;
                let binding = binding(&url);
                let mut state = HttpSession::new(binding.clone());
                let cancel = CancellationToken::new();
                let mut checks = 0;
                let result: Result<Value, HttpError> = state
                    .call_wire(
                        &binding,
                        &test_client(),
                        &url,
                        "tools/list",
                        Map::new(),
                        None,
                        None,
                        &cancel,
                        &mut || {
                            checks += 1;
                            if checks == stop_at {
                                if cancelled {
                                    cancel.cancel();
                                } else {
                                    return Err(HttpError::StaleBinding);
                                }
                            }
                            Ok(())
                        },
                    )
                    .await;
                assert_eq!(
                    result,
                    Err(if cancelled {
                        HttpError::Cancelled
                    } else {
                        HttpError::StaleBinding
                    })
                );
                assert!(state.failed);
                assert!(state.session.is_none());
                peer.abort();
            }
        }
    }
    #[test]
    fn bearer_header_is_sensitive_and_debug_redacted() {
        let value = authorization_header("fixture-bearer-secret").unwrap();
        assert!(value.is_sensitive());
        assert_eq!(value.to_str().unwrap(), "Bearer fixture-bearer-secret");
        assert!(!format!("{value:?}").contains("fixture-bearer-secret"));
        let mut map = HeaderMap::new();
        map.insert(AUTHORIZATION, value);
        assert!(!format!("{map:?}").contains("fixture-bearer-secret"));
        assert_eq!(
            authorization_header("bad\nvalue"),
            Err(HttpError::InvalidRequest)
        );
    }

    // Local test-only wire peer for response handling. Production still
    // constructs its own HTTPS client after DNS and address validation.
    async fn wire_fixture(
        response: Vec<u8>,
        delay: Duration,
    ) -> (Url, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(split) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..split]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
                assert!(request.len() <= MAX_REPLY_BYTES + 4096);
            }
            tokio::time::sleep(delay).await;
            let _ = socket.write_all(&response).await;
            String::from_utf8(request).unwrap()
        });
        (Url::parse(&format!("http://{address}/mcp")).unwrap(), task)
    }

    fn wire_reply(status: &str, content_type: &str, extra: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
            body.len()
        ).into_bytes()
    }

    fn test_client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    async fn fixture_request(response: Vec<u8>) -> (Result<Value, HttpError>, String) {
        let (url, peer) = wire_fixture(response, Duration::ZERO).await;
        let params = Map::from_iter([
            ("name".into(), json!("probe")),
            ("arguments".into(), json!({"region":"west"})),
        ]);
        let schema = json!({"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}});
        let headers = request_headers("tools/call", &params, Some(&schema)).unwrap();
        let body = serde_json::to_vec(
            &protocol::request(Era::Modern, 7, "tools/call", params, "AeroFTP", "4.2").unwrap(),
        )
        .unwrap();
        let result = send_prepared(
            &test_client(),
            url,
            headers,
            body,
            7,
            &CancellationToken::new(),
        )
        .await;
        (result, peer.await.unwrap())
    }

    #[tokio::test]
    async fn wire_fixture_checks_json_sse_headers_and_correlated_reply() {
        let json = r#"{"jsonrpc":"2.0","id":7,"result":{"resultType":"complete","content":[]}}"#;
        let (result, request) =
            fixture_request(wire_reply("200 OK", "application/json", "", json)).await;
        assert_eq!(result.unwrap()["content"], json!([]));
        let request = request.to_ascii_lowercase();
        assert!(request.contains("mcp-method: tools/call\r\n"));
        assert!(request.contains("mcp-name: probe\r\n"));
        assert!(request.contains("mcp-param-region: west\r\n"));
        assert!(request.contains("mcp-protocol-version: 2026-07-28\r\n"));
        assert!(request.contains("io.modelcontextprotocol/protocolversion"));

        let sse = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"resultType\":\"complete\",\"content\":[]}}\n\n";
        let (result, _) = fixture_request(wire_reply("200 OK", "text/event-stream", "", sse)).await;
        assert_eq!(result.unwrap()["content"], json!([]));
        let wrong = r#"{"jsonrpc":"2.0","id":8,"result":{"resultType":"complete"}}"#;
        let (result, _) =
            fixture_request(wire_reply("200 OK", "application/json", "", wrong)).await;
        assert_eq!(result, Err(HttpError::InvalidResponse));
    }

    #[tokio::test]
    async fn wire_fixture_rejects_redirect_large_body_and_handles_401_scope() {
        let (result, _) = fixture_request(wire_reply(
            "401 Unauthorized",
            "application/json",
            "WWW-Authenticate: Bearer scope=\"files:read files:write\"\r\n",
            "",
        ))
        .await;
        assert!(
            matches!(result, Err(HttpError::Unauthorized(challenge)) if challenge.scopes == ["files:read", "files:write"])
        );
        let (result, _) = fixture_request(wire_reply(
            "302 Found",
            "application/json",
            "Location: https://elsewhere.example/mcp\r\n",
            "",
        ))
        .await;
        assert_eq!(result, Err(HttpError::Redirect));
        let (result, _) = fixture_request(wire_reply(
            "200 OK",
            "application/json",
            "",
            &"x".repeat(MAX_REPLY_BYTES + 1),
        ))
        .await;
        assert_eq!(result, Err(HttpError::ResponseTooLarge));
        let unsupported = r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32022,"message":"unsupported","data":{"requested":"2026-07-28","supported":["2025-11-25"]}}}"#;
        let (result, _) = fixture_request(wire_reply(
            "400 Bad Request",
            "application/json",
            "",
            unsupported,
        ))
        .await;
        assert_eq!(result, Err(HttpError::UnsupportedVersion));
    }

    #[tokio::test]
    async fn wire_fixture_cancellation_stops_a_pending_request() {
        let (url, peer) = wire_fixture(
            wire_reply("200 OK", "application/json", "", "{}"),
            Duration::from_secs(1),
        )
        .await;
        let cancel = CancellationToken::new();
        let cancelled = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancelled.cancel();
        });
        let result = send_prepared(
            &test_client(),
            url,
            HeaderMap::new(),
            b"{}".to_vec(),
            7,
            &cancel,
        )
        .await;
        assert_eq!(result, Err(HttpError::Cancelled));
        peer.abort();
    }

    #[test]
    fn header_annotations_are_reachable_primitive_unique_and_safe() {
        let schema = json!({"type":"object","properties":{
            "region":{"type":"string","x-mcp-header":"Region"},
            "nested":{"type":"object","properties":{"count":{"type":"integer","x-mcp-header":"Count"}}}
        }});
        let headers =
            parameter_headers(&schema, &json!({"region":"eu","nested":{"count":42}})).unwrap();
        assert_eq!(headers["mcp-param-region"], "eu");
        assert_eq!(headers["mcp-param-count"], "42");
        for bad in [
            json!({"type":"object","properties":{"a":{"type":"number","x-mcp-header":"A"}}}),
            json!({"type":"object","items":{"x-mcp-header":"A"}}),
            json!({"type":"object","properties":{"a":{"type":"string","x-mcp-header":"A"},"b":{"type":"string","x-mcp-header":"a"}}}),
            json!({"type":"object","properties":{"a":{"type":"string","x-mcp-header":"bad\rname"}}}),
        ] {
            assert_eq!(annotations(&bad), Err(HttpError::InvalidRequest));
        }
    }

    #[test]
    fn header_values_use_exact_base64_sentinel_rules() {
        assert_eq!(mirrored_value("plain").unwrap(), "plain");
        assert_eq!(mirrored_value(" café ").unwrap(), "=?base64?IGNhZsOpIA==?=");
        assert_eq!(
            mirrored_value("=?base64?literal?=").unwrap(),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
        let schema =
            json!({"type":"object","properties":{"n":{"type":"integer","x-mcp-header":"N"}}});
        assert_eq!(
            parameter_headers(&schema, &json!({"n": SAFE_INTEGER + 1})),
            Err(HttpError::InvalidRequest)
        );
        assert!(parameter_headers(&schema, &json!({"n": null}))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn modern_headers_match_body_source_fields() {
        let params = Map::from_iter([
            ("name".into(), json!("écho")),
            ("arguments".into(), json!({"region":"west"})),
        ]);
        let schema = json!({"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}});
        let headers = request_headers("tools/call", &params, Some(&schema)).unwrap();
        assert_eq!(headers["mcp-protocol-version"], protocol::MODERN_VERSION);
        assert_eq!(headers["mcp-method"], "tools/call");
        assert_eq!(headers["mcp-name"], "=?base64?w6ljaG8=?=");
        assert_eq!(headers["mcp-param-region"], "west");
    }

    #[test]
    fn json_and_request_scoped_sse_are_bounded_and_correlated() {
        let json_reply =
            br#"{"jsonrpc":"2.0","id":7,"result":{"resultType":"complete","tools":[]}}"#;
        assert_eq!(
            parse_reply("application/json; charset=utf-8", json_reply, 7).unwrap()["tools"],
            json!([])
        );
        let sse = b": keepalive\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"resultType\":\"complete\",\"tools\":[]}}\n\n";
        assert_eq!(
            parse_reply("text/event-stream", sse, 7).unwrap()["tools"],
            json!([])
        );
        let crlf = b"data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"resultType\":\"complete\"}}\r\n\r\n";
        assert!(parse_reply("text/event-stream", crlf, 7).is_ok());
        assert_eq!(
            parse_reply("text/event-stream", sse, 8),
            Err(HttpError::InvalidResponse)
        );
        assert_eq!(
            parse_reply("text/event-stream", b"data: bad\n\n", 7),
            Err(HttpError::InvalidResponse)
        );
    }

    #[test]
    fn public_ip_policy_rejects_non_public_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.2.2",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("1.1.1.1".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn bearer_challenge_is_bounded_and_redacted() {
        let challenge = parse_challenge(Some(&HeaderValue::from_static(
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", scope=\"files:read files:write\"",
        )));
        assert_eq!(challenge.scopes, ["files:read", "files:write"]);
        assert!(challenge.metadata_url.is_some());
        assert!(!format!("{challenge:?}").contains("files:read"));
    }
}
