// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Connection identity and defaults shared with the frontend registry.

use crate::providers::ProviderType;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSpec {
    pub token: String,
    pub backend: String,
    pub aliases: Vec<String>,
    pub credential_kind: String,
    pub default_port: u16,
    pub creation: String,
    pub default_options: Map<String, Value>,
    pub required_options: Vec<String>,
    pub native_provider_id: Option<String>,
    pub default_host: Option<String>,
    pub default_username: Option<String>,
    #[serde(default)]
    pub dev_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPreset {
    pub id: String,
    pub protocol: String,
    pub generic: bool,
    pub defaults: Map<String, Value>,
    pub first_region: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionAlias {
    pub protocol: String,
    pub provider_id: Option<String>,
    #[serde(default)]
    pub options: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionManifest {
    pub protocols: Vec<ConnectionSpec>,
    pub presets: Vec<ConnectionPreset>,
    pub aliases: HashMap<String, ConnectionAlias>,
    pub preset_aliases: HashMap<String, String>,
    pub identities: HashMap<String, String>,
}

pub fn manifest() -> &'static ConnectionManifest {
    static MANIFEST: LazyLock<ConnectionManifest> = LazyLock::new(|| {
        serde_json::from_str(include_str!("../../src/shared/connection-manifest.json"))
            .expect("validated connection manifest")
    });
    &MANIFEST
}

fn normalized(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn connection_alias(raw: &str) -> Option<&'static ConnectionAlias> {
    let key = normalized(raw);
    manifest()
        .aliases
        .iter()
        .find_map(|(name, alias)| (normalized(name) == key).then_some(alias))
}

pub fn connection_spec(raw: &str) -> Option<&'static ConnectionSpec> {
    let key = normalized(connection_alias(raw).map_or(raw, |a| &a.protocol));
    manifest()
        .protocols
        .iter()
        .find(|p| normalized(&p.token) == key || p.aliases.iter().any(|a| normalized(a) == key))
}

pub fn provider_type(raw: &str) -> Option<ProviderType> {
    let spec = connection_spec(raw)?;
    serde_json::from_value(Value::String(spec.backend.clone())).ok()
}

pub fn provider_spec(kind: ProviderType) -> Option<&'static ConnectionSpec> {
    let backend = serde_json::to_value(kind).ok()?;
    manifest()
        .protocols
        .iter()
        .find(|p| Some(p.backend.as_str()) == backend.as_str())
}

pub fn preset(raw: &str) -> Option<&'static ConnectionPreset> {
    let id = manifest()
        .preset_aliases
        .get(raw)
        .map_or(raw, String::as_str);
    manifest().presets.iter().find(|p| p.id == id)
}

pub fn canonical_protocol_and_provider<'a>(
    raw: &'a str,
    provider_id: Option<&'a str>,
) -> (&'a str, Option<&'a str>) {
    let Some(spec) = connection_spec(raw) else {
        return (raw, provider_id);
    };
    let provider_id =
        provider_id.or_else(|| connection_alias(raw).and_then(|a| a.provider_id.as_deref()));
    let provider_id =
        provider_id.map(|id| manifest().preset_aliases.get(id).map_or(id, String::as_str));
    (&spec.token, provider_id)
}

pub fn default_options(
    spec: &ConnectionSpec,
    preset: Option<&ConnectionPreset>,
) -> Map<String, Value> {
    let mut options = spec.default_options.clone();
    if let Some(preset) = preset {
        for (key, value) in &preset.defaults {
            if !matches!(
                key.as_str(),
                "server" | "port" | "basePath" | "endpointTemplate"
            ) {
                options.insert(saved_option_key(key, &spec.token), value.clone());
            }
        }
        if let Some(region) = &preset.first_region {
            options
                .entry("region")
                .or_insert_with(|| Value::String(region.clone()));
        }
    }
    options
}

fn saved_option_key(key: &str, protocol: &str) -> String {
    match crate::profile_loader::canonicalize_profile_option_key(key).as_str() {
        "tls_mode" if key != "webdavScheme" => "tlsMode".into(),
        "verify_cert" => "verifyCert".into(),
        "path_style" => "pathStyle".into(),
        "account_id" => "accountId".into(),
        "private_key_path" => "privateKeyPath".into(),
        "allow_cleartext_endpoint" => "allowCleartextEndpoint".into(),
        "allow_cleartext_storage_endpoint" => "allowCleartextStorage".into(),
        "container" if protocol == "azure" => "bucket".into(),
        "cloud_name" if protocol == "cloudinary" => "bucket".into(),
        _ => key.into(),
    }
}

/// Materialize a new saved connection. User input wins over preset and protocol defaults.
pub fn resolve_profile(
    protocol: &str,
    provider_id: Option<&str>,
    host: Option<&str>,
    port: Option<u16>,
    username: Option<&str>,
    initial_path: Option<&str>,
    options: &Value,
) -> Result<Value, String> {
    let spec = connection_spec(protocol).ok_or_else(|| format!("Unknown protocol '{protocol}'"))?;
    if spec.creation != "profile" {
        return Err(format!("{} uses {} onboarding; use the device discovery or peer commands instead of profile-add", spec.token, spec.creation));
    }
    if spec.dev_only && !cfg!(debug_assertions) {
        return Err(format!("{} is available only in DEV builds", spec.token));
    }
    let (_, provider_id) = canonical_protocol_and_provider(protocol, provider_id);
    let provider_id = provider_id.or(spec.native_provider_id.as_deref());
    let candidate = provider_id.and_then(preset);
    let preset = candidate.filter(|preset| preset.protocol == spec.token);
    if let Some(id) = provider_id.filter(|_| preset.is_none()) {
        if Some(id) != spec.native_provider_id.as_deref()
            && manifest().identities.get(id).map(String::as_str) != Some(spec.token.as_str())
        {
            if let Some(other) = candidate {
                return Err(format!(
                    "Preset '{}' uses {}, not {}",
                    other.id, other.protocol, spec.token
                ));
            }
            return Err(format!(
                "Unknown preset '{id}' for {}; omit --provider-id for a custom connection",
                spec.token
            ));
        }
    }
    let defaults = preset.map(|p| &p.defaults);
    let mut merged = default_options(spec, preset);
    if let Some(alias) = connection_alias(protocol) {
        merged.extend(alias.options.clone());
    }
    let input = options
        .as_object()
        .ok_or("Connection options must be a JSON object")?;
    for (key, value) in input {
        let normalized = normalized(key);
        if normalized == "profileid"
            || normalized == "twofactorcode"
            || normalized == "rolemfatokencode"
            || normalized.contains("password")
            || normalized.contains("secret")
            || normalized.contains("token")
            || normalized.contains("apikey")
            || normalized == "privatekey"
        {
            return Err(format!("'{key}' is a credential or ephemeral field; store secrets through the credential commands"));
        }
        if !(value.is_string() || value.is_boolean() || value.is_number()) {
            return Err(format!(
                "Connection option '{key}' must be a string, boolean or number"
            ));
        }
        merged.insert(saved_option_key(key, &spec.token), value.clone());
    }
    if let Some(mode) = merged.get("mega_mode") {
        if !matches!(mode.as_str(), Some("native" | "megacmd")) {
            return Err("mega_mode must be native or megacmd".into());
        }
    }
    for (key, allowed) in [
        ("tlsMode", &["none", "explicit", "implicit", "auto"][..]),
        ("webdavScheme", &["http", "https", "auto"][..]),
    ] {
        if let Some(value) = merged.get(key) {
            if !value.as_str().is_some_and(|value| allowed.contains(&value)) {
                return Err(format!("Invalid {key}; expected {}", allowed.join(", ")));
            }
        }
    }
    for key in [
        "verifyCert",
        "pathStyle",
        "anonymous",
        "save_session",
        "allowCleartextEndpoint",
        "allowCleartextStorage",
    ] {
        if merged.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("Connection option '{key}' must be a boolean"));
        }
    }
    let port = port
        .or_else(|| {
            defaults?
                .get("port")?
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
        })
        .unwrap_or(spec.default_port);
    if port == 0 {
        return Err("Network profiles require a nonzero port".into());
    }
    let explicit_host = host.is_some_and(|host| !host.trim().is_empty());
    let username = username.or(spec.default_username.as_deref()).unwrap_or("");
    let host = host
        .or_else(|| defaults?.get("server")?.as_str())
        .or(spec.default_host.as_deref())
        .unwrap_or("")
        .replace("{username}", username);
    let path = initial_path
        .or_else(|| defaults?.get("basePath")?.as_str())
        .unwrap_or("/")
        .replace("{username}", username);
    let mut profile = serde_json::json!({"protocol":spec.token,"host":host,"port":port,"username":username,"initialPath":if path.is_empty(){"/"}else{&path},"options":merged});
    if let Some(id) = provider_id.filter(|_| !preset.is_some_and(|p| p.generic)) {
        profile["providerId"] = Value::String(id.into());
    }
    if spec.token == "s3" {
        if explicit_host && !input.contains_key("endpoint") {
            profile["options"]
                .as_object_mut()
                .unwrap()
                .remove("endpoint");
        }
        if (explicit_host || input.contains_key("endpoint")) && !input.contains_key("region") {
            profile["options"].as_object_mut().unwrap().remove("region");
        }
        let (extra, _) =
            crate::profile_loader::profile_connection_extra(&profile, ProviderType::S3, &host);
        for key in ["endpoint", "region"] {
            if let Some(value) = extra.get(key) {
                profile["options"][key] = Value::String(value.clone());
            }
        }
    }
    let kind = provider_type(&spec.token).ok_or("Connection has no backend")?;
    let (extra, _) = crate::profile_loader::profile_connection_extra(&profile, kind, &host);
    for key in &spec.required_options {
        if !extra.get(key).is_some_and(|value| !value.trim().is_empty()) {
            return Err(format!("{} requires connection option '{key}'; pass --options-json or the corresponding connection flag", spec.token));
        }
    }
    Ok(profile)
}
