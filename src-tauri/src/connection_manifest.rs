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

/// The `{username}` placeholder some presets keep in their server URL or
/// starting folder (CloudMe, Puter, LifetimeCloud, Nextcloud and its hosted
/// relatives). These two functions are the only places that fill it in,
/// pinned by `the_username_template_is_expanded_only_by_the_two_helpers`.
pub const USERNAME_TEMPLATE: &str = "{username}";

/// What every surface reports when a template needs a user name and the
/// profile has none: a server URL or folder with an empty segment
/// (`/dav//`) or the literal placeholder would otherwise be dialled.
pub const USERNAME_TEMPLATE_MISSING: &str =
    "This server address needs a user name: set the profile's user name";

/// The characters a user name is percent-encoded for when it fills one path
/// segment of a URL: controls, space and every character that would end the
/// segment (`/`), start a query or a fragment (`?`, `#`), or start an
/// encoding of its own (`%`). `@` and `:` are valid in a segment and stay as
/// they are, so `alice@example.com` reaches the server unchanged.
const USERNAME_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
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

/// Fill the `{username}` placeholder of a server URL with the user name as
/// one percent-encoded path segment, exactly as stored (never trimmed: the
/// login sends it that way). A template without the placeholder is returned
/// unchanged; one that needs it with a blank user name, or a name of `.` or
/// `..`, is an error.
pub fn expand_username_in_url(template: &str, username: &str) -> Result<String, String> {
    if !template.contains(USERNAME_TEMPLATE) {
        return Ok(template.to_string());
    }
    if username.trim().is_empty() {
        return Err(USERNAME_TEMPLATE_MISSING.to_string());
    }
    // A user name holding the placeholder itself would leave it in place.
    if username.contains(USERNAME_TEMPLATE) {
        return Err(USERNAME_TEMPLATE_UNRESOLVED.to_string());
    }
    // `.` and `..` are no segment a URL keeps: the parser resolves them and
    // the address climbs out of the user's folder.
    if matches!(username.trim(), "." | "..") {
        return Err(
            "This user name cannot be used in a server address: it is '.' or '..'; enter the \
             address in full instead of the {username} placeholder"
                .to_string(),
        );
    }
    let segment = percent_encoding::utf8_percent_encode(username, USERNAME_SEGMENT).to_string();
    Ok(template.replace(USERNAME_TEMPLATE, &segment))
}

/// What a location derived from a profile still holding the `{username}`
/// placeholder means: the user name could not fill it (blank, refused, or
/// literally `{username}`). Every user of a derived location (the crypt
/// overlay unlock, the AeroCloud root, the window) refuses it with this
/// message instead of acting on a folder named after the placeholder.
pub const USERNAME_TEMPLATE_UNRESOLVED: &str = "This location still holds the {username} \
     placeholder because the profile's user name cannot fill it (it is blank, contains '/', \
     '\\' or NUL, is '.' or '..', or is literally {username}): set the user name, or write the \
     folder in full instead of the placeholder";

/// Refuse a derived location that still holds the `{username}` placeholder.
/// The readers keep the template when the user name cannot fill it, so that
/// nothing climbs out of the user's folder; this is the check every user of
/// such a location runs before any request or write.
pub fn refuse_unresolved_username_template(location: &str) -> Result<(), String> {
    if location.contains(USERNAME_TEMPLATE) {
        Err(USERNAME_TEMPLATE_UNRESOLVED.to_string())
    } else {
        Ok(())
    }
}

/// Fill the `{username}` placeholder of a folder path with the user name as
/// it is (a path is a decoded string; the WebDAV client encodes it on the
/// wire). A blank user name is an error, and so is one that could move the
/// folder: a `/`, `\\` or NUL inside it, or a name of `.` or `..`.
pub fn expand_username_in_path(template: &str, username: &str) -> Result<String, String> {
    if !template.contains(USERNAME_TEMPLATE) {
        return Ok(template.to_string());
    }
    if username.trim().is_empty() {
        return Err(USERNAME_TEMPLATE_MISSING.to_string());
    }
    if username.contains(USERNAME_TEMPLATE) {
        return Err(USERNAME_TEMPLATE_UNRESOLVED.to_string());
    }
    if username.contains(['/', '\\', '\0']) || matches!(username.trim(), "." | "..") {
        return Err(
            "This user name cannot be used in a folder path: it contains '/', '\\', a NUL \
             character, or is '.' or '..'; enter the folder path in full instead of the \
             {username} placeholder"
                .to_string(),
        );
    }
    Ok(template.replace(USERNAME_TEMPLATE, username))
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

/// The canonical preset id for a stored provider id: an alias of the manifest
/// (`fastly-object-storage`, `backblaze-b2`, `azure-blob`) becomes its target,
/// any other id comes back unchanged. One rule for every surface that keys or
/// maps on the id, so an old or hand-edited profile that carries the alias
/// behaves like one that carries the canonical id.
pub fn canonical_preset_id(raw: &str) -> &str {
    manifest()
        .preset_aliases
        .get(raw)
        .map_or(raw, String::as_str)
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
        // The app reads only `allowScpFallback`: saved as `allow_scp_fallback`,
        // an SFTP-only profile would fall back to SCP from the app.
        "allow_scp_fallback" => "allowScpFallback".into(),
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
            || normalized.contains("passphrase")
            || normalized.contains("secret")
            || normalized.contains("token")
            || normalized.contains("apikey")
            || normalized == "accesskey"
            || normalized == "privatekey"
        {
            return Err(format!("'{key}' is a credential or ephemeral field; store secrets through the credential commands"));
        }
        if !(value.is_string() || value.is_boolean() || value.is_number()) {
            return Err(format!(
                "Connection option '{key}' must be a string, boolean or number"
            ));
        }
        let saved_key = saved_option_key(key, &spec.token);
        let canonical_key = crate::profile_loader::canonicalize_profile_option_key(&saved_key);
        // An alias supplied by the caller overrides the same option's default,
        // using its saved spelling rather than persisting two competing keys.
        let saved_key = merged
            .keys()
            .find(|existing| {
                crate::profile_loader::canonicalize_profile_option_key(existing) == canonical_key
            })
            .cloned()
            .unwrap_or(saved_key);
        merged.insert(saved_key, value.clone());
    }
    if let Some(mode) = merged.get("mega_mode") {
        if !matches!(mode.as_str(), Some("native" | "megacmd")) {
            return Err("mega_mode must be native or megacmd".into());
        }
    }
    for (key, allowed) in [
        (
            "tlsMode",
            &["none", "explicit", "implicit", "explicit_if_available"][..],
        ),
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
    // The SCP fallback switch, under whichever spelling it was saved: the
    // connect side reads only a literal `false`, so a `"False"` or a `0` would
    // leave the fallback on without a word.
    if let Some(key) = merged.iter().find_map(|(key, value)| {
        (crate::profile_loader::canonicalize_profile_option_key(key) == "allow_scp_fallback"
            && !value.is_boolean())
        .then_some(key)
    }) {
        return Err(format!("Connection option '{key}' must be a boolean"));
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
        .filter(|host| !host.trim().is_empty())
        .or_else(|| defaults?.get("server")?.as_str())
        .or(spec.default_host.as_deref())
        .unwrap_or("");
    let host = expand_username_in_url(host, username)?;
    let path = initial_path
        .or_else(|| defaults?.get("basePath")?.as_str())
        .unwrap_or("/");
    let path = expand_username_in_path(path, username)?;
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

#[cfg(test)]
mod tests {
    use super::{
        expand_username_in_path, expand_username_in_url, resolve_profile, USERNAME_TEMPLATE_MISSING,
    };

    #[test]
    fn a_user_name_fills_a_url_template_as_one_encoded_segment() {
        let t = "https://dav.example.test/dav/{username}/";
        for (user, segment) in [
            ("alice", "alice"),
            ("alice@example.com", "alice@example.com"),
            ("a b", "a%20b"),
            ("50%", "50%25"),
            ("a/b", "a%2Fb"),
            ("a?b", "a%3Fb"),
            ("a#b", "a%23b"),
        ] {
            assert_eq!(
                expand_username_in_url(t, user).unwrap(),
                format!("https://dav.example.test/dav/{segment}/"),
                "{user}"
            );
        }
        // No placeholder: unchanged, whatever the user name.
        assert_eq!(
            expand_username_in_url("https://h/dav/", "").unwrap(),
            "https://h/dav/"
        );
    }

    /// The shared case table the frontend runs too: the user name goes in as
    /// stored (never trimmed), `$&` and `$1` are no pattern, `{username}` as a
    /// user name is filled once, and `.`/`..` never reach a URL.
    #[test]
    fn the_shared_username_template_cases_hold_untrimmed_and_literal() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/shared/username-template-cases.json"
        ))
        .unwrap();
        let path_t = table["pathTemplate"].as_str().unwrap();
        let url_t = table["urlTemplate"].as_str().unwrap();
        for case in table["cases"].as_array().unwrap() {
            let user = case["user"].as_str().unwrap();
            assert_eq!(
                expand_username_in_path(path_t, user).ok(),
                case["path"].as_str().map(str::to_string),
                "path {user:?}"
            );
            assert_eq!(
                expand_username_in_url(url_t, user).ok(),
                case["url"].as_str().map(str::to_string),
                "url {user:?}"
            );
        }
    }

    /// The window shows the same refusal text as the backend: it reads it from
    /// the shared table, and this pins the backend constant to it.
    #[test]
    fn the_unresolved_template_message_matches_the_shared_table() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/shared/username-template-cases.json"
        ))
        .unwrap();
        assert_eq!(
            table["unresolvedMessage"].as_str().unwrap(),
            super::USERNAME_TEMPLATE_UNRESOLVED
        );
        assert_eq!(
            super::refuse_unresolved_username_template("/users/{username}/vault"),
            Err(super::USERNAME_TEMPLATE_UNRESOLVED.to_string())
        );
        assert_eq!(
            super::refuse_unresolved_username_template("/users/alice/vault"),
            Ok(())
        );
    }

    #[test]
    fn a_template_that_needs_a_user_name_refuses_a_blank_one() {
        for user in ["", "   "] {
            assert_eq!(
                expand_username_in_url("https://h/dav/{username}/", user).unwrap_err(),
                USERNAME_TEMPLATE_MISSING
            );
            assert_eq!(
                expand_username_in_path("/remote.php/dav/files/{username}/", user).unwrap_err(),
                USERNAME_TEMPLATE_MISSING
            );
        }
    }

    #[test]
    fn a_user_name_that_could_move_the_folder_is_refused_in_a_path() {
        let t = "/remote.php/dav/files/{username}/";
        for user in ["a/b", "/abs", "..", ".", "a\\b", "a\0b"] {
            let err = expand_username_in_path(t, user).unwrap_err();
            assert!(
                err.contains("cannot be used in a folder path"),
                "{user:?}: {err}"
            );
        }
        // Valid names go in as written: a path is a decoded string.
        assert_eq!(
            expand_username_in_path(t, "alice@example.com").unwrap(),
            "/remote.php/dav/files/alice@example.com/"
        );
        assert_eq!(
            expand_username_in_path(t, "a b").unwrap(),
            "/remote.php/dav/files/a b/"
        );
    }

    #[test]
    fn a_profile_created_with_a_blank_user_name_on_a_template_preset_is_refused() {
        for preset in ["cloudme", "lifetimecloud", "tabdigital", "felicloud"] {
            let err = resolve_profile(
                "webdav",
                Some(preset),
                None,
                None,
                Some(""),
                None,
                &serde_json::json!({}),
            )
            .unwrap_err();
            assert_eq!(err, USERNAME_TEMPLATE_MISSING, "{preset}");
        }
    }

    /// The two helpers above are the only places `{username}` is filled in:
    /// every other site calls them, so a twelfth hand-written expansion with
    /// its own encoding and its own blank-user behaviour cannot reappear.
    #[test]
    fn the_username_template_is_expanded_only_by_the_two_helpers() {
        let placeholder = ["{", "username", "}"].concat();
        let forbidden = [
            format!("replace(\"{placeholder}\""),
            format!("replacen(\"{placeholder}\""),
            format!("split(\"{placeholder}\")"),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.ends_with("connection_manifest.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if forbidden.iter().any(|f| line.contains(f.as_str())) {
                            offenders.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "expand {placeholder} through connection_manifest::expand_username_in_url / \
             expand_username_in_path, not by hand: {offenders:?}"
        );
    }

    /// No caller hands the helpers a trimmed user name: the login sends the
    /// name as stored, and a trimmed one put the encrypted folder of the CLI
    /// and AeroCloud somewhere else than the window's (plaintext uploads).
    /// Blankness is judged inside the helpers only. The check reads every call
    /// site's user-name argument and the `let` that binds it in the 40 lines
    /// above the call.
    #[test]
    fn no_caller_trims_the_user_name_it_passes_to_the_helpers() {
        let call = regex::Regex::new(
            r"(?s)expand_username_in_(?:url|path)\(\s*[^,]+?,\s*(&?[A-Za-z_][A-Za-z0-9_.]*)\s*\)",
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        let mut offenders = Vec::new();
        let mut calls = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs")
                    || path.ends_with("connection_manifest.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for m in call.captures_iter(&text) {
                    calls += 1;
                    let arg = m[1].trim_start_matches('&');
                    let at = m.get(0).unwrap().start();
                    let line = text[..at].lines().count();
                    let above: Vec<&str> = text[..at].lines().rev().take(40).collect();
                    let ident = arg.rsplit('.').next().unwrap();
                    let binding = format!("let {ident} =");
                    let traced = above.iter().position(|l| l.contains(&binding)).map(|i| {
                        above[..=i]
                            .iter()
                            .rev()
                            .copied()
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                    let trimmed = arg.contains("trim")
                        || traced.as_deref().is_some_and(|b| {
                            let stmt = &b[b.find(&binding).unwrap()..];
                            let stmt = &stmt[..stmt.find(';').unwrap_or(stmt.len())];
                            stmt.contains("trim")
                        });
                    if trimmed {
                        offenders.push(format!("{}:{line}", path.display()));
                    }
                }
            }
        }
        assert!(calls >= 8, "the guard found only {calls} call sites");
        assert!(
            offenders.is_empty(),
            "a trimmed user name reaches the helpers: {offenders:?}"
        );
    }

    use crate::providers::{FtpConfig, FtpTlsMode, ProviderConfig, ProviderType};
    use serde_json::json;

    fn ftp_mode(profile: &serde_json::Value, kind: ProviderType) -> FtpTlsMode {
        let (extra, _) =
            crate::profile_loader::profile_connection_extra(profile, kind, "synthetic.test");
        FtpConfig::from_provider_config(&ProviderConfig {
            name: "synthetic".into(),
            provider_type: kind,
            host: "synthetic.test".into(),
            port: Some(21),
            username: None,
            password: None,
            initial_path: None,
            extra,
        })
        .unwrap()
        .tls_mode
    }

    #[test]
    fn saved_ftp_modes_survive_creation_defaults() {
        for (protocol, kind, options, expected) in [
            // A saved FTP profile without a mode requires TLS, as in the GUI
            // (owner decision 2026-11-06, #1506 Requested 198); it read as
            // plain FTP before.
            ("ftp", ProviderType::Ftp, json!({}), FtpTlsMode::Explicit),
            (
                "ftp",
                ProviderType::Ftp,
                json!({"tlsMode":"none"}),
                FtpTlsMode::None,
            ),
            ("ftps", ProviderType::Ftps, json!({}), FtpTlsMode::Implicit),
            (
                "ftps",
                ProviderType::Ftps,
                json!({"ftpsMode":"explicit"}),
                FtpTlsMode::Explicit,
            ),
            (
                "ftps",
                ProviderType::Ftps,
                json!({"ftpsMode":"explicit","tlsMode":"implicit"}),
                FtpTlsMode::Implicit,
            ),
        ] {
            let saved = json!({"protocol":protocol,"port":21,"options":options});
            assert_eq!(ftp_mode(&saved, kind), expected, "{saved}");
        }
        let created = super::resolve_profile(
            "ftp",
            None,
            Some("synthetic.test"),
            None,
            None,
            None,
            &json!({}),
        )
        .unwrap();
        assert_eq!(ftp_mode(&created, ProviderType::Ftp), FtpTlsMode::Explicit);
    }

    #[test]
    fn accepted_ftp_tls_modes_reach_the_backend_unchanged() {
        for (mode, expected) in [
            ("none", FtpTlsMode::None),
            ("explicit", FtpTlsMode::Explicit),
            ("implicit", FtpTlsMode::Implicit),
            ("explicit_if_available", FtpTlsMode::ExplicitIfAvailable),
        ] {
            let saved = super::resolve_profile(
                "ftp",
                None,
                Some("synthetic.test"),
                None,
                None,
                None,
                &json!({"tlsMode":mode}),
            )
            .unwrap();
            assert_eq!(ftp_mode(&saved, ProviderType::Ftp), expected);
        }
        assert!(super::resolve_profile(
            "ftp",
            None,
            Some("synthetic.test"),
            None,
            None,
            None,
            &json!({"tlsMode":"auto"}),
        )
        .is_err());
    }

    #[test]
    fn the_scp_fallback_switch_must_be_a_boolean_under_either_spelling() {
        let add = |options: serde_json::Value| {
            super::resolve_profile(
                "sftp",
                None,
                Some("synthetic.test"),
                None,
                Some("root"),
                None,
                &options,
            )
        };
        for key in ["allowScpFallback", "allow_scp_fallback"] {
            for bad in [json!("False"), json!("0"), json!(0)] {
                let err = add(json!({ key: bad })).unwrap_err();
                assert!(err.contains("must be a boolean"), "{key}={bad}: {err}");
            }
            let saved = add(json!({ key: false })).expect(key);
            assert_eq!(
                saved["options"]["allowScpFallback"],
                json!(false),
                "{key}: {saved}"
            );
            assert!(
                saved["options"].get("allow_scp_fallback").is_none(),
                "{key}"
            );
        }
    }

    #[test]
    fn caller_option_aliases_replace_the_default_saved_key() {
        for (protocol, options, key, expected) in [
            (
                "mega",
                json!({"megaMode":"megacmd"}),
                "mega_mode",
                json!("megacmd"),
            ),
            (
                "mega",
                json!({"saveSession":false}),
                "save_session",
                json!(false),
            ),
            (
                "proton",
                json!({"protonMode":"cli"}),
                "proton_mode",
                json!("cli"),
            ),
        ] {
            let saved =
                super::resolve_profile(protocol, None, None, None, None, None, &options).unwrap();
            assert_eq!(saved["options"][key], expected, "{saved}");
            let kind = super::provider_type(protocol).unwrap();
            let (extra, _) = crate::profile_loader::profile_connection_extra(&saved, kind, "");
            let expected = expected
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| expected.to_string());
            assert_eq!(extra.get(key), Some(&expected));
            for alias in options.as_object().unwrap().keys() {
                assert!(
                    saved["options"].get(alias).is_none(),
                    "duplicate alias: {saved}"
                );
            }
        }
    }

    #[test]
    fn secret_option_aliases_are_rejected_before_persistence() {
        let accepted: Vec<_> = ["accessKey", "access_key", "keyPassphrase", "key_passphrase"]
            .into_iter()
            .filter(|key| {
                let options =
                    serde_json::json!({(*key):"synthetic secret","container":"synthetic-container"});
                super::resolve_profile("azure", None, None, None, None, None, &options).is_ok()
            })
            .collect();
        assert!(accepted.is_empty(), "Secret fields persisted: {accepted:?}");
    }

    #[test]
    fn legacy_proton_mode_is_preserved_while_new_profiles_materialize_native() {
        let profile = serde_json::json!({"protocol":"proton","host":"","options":{}});
        let (extra, _) = crate::profile_loader::profile_connection_extra(
            &profile,
            crate::providers::ProviderType::Proton,
            "",
        );
        let config = crate::providers::ProviderConfig {
            name: "synthetic".into(),
            provider_type: crate::providers::ProviderType::Proton,
            host: "".into(),
            port: None,
            username: None,
            password: None,
            initial_path: None,
            extra,
        };
        assert_eq!(
            crate::providers::proton::ProtonConfig::from_provider_config(&config)
                .unwrap()
                .connection_mode,
            crate::providers::proton::ProtonConnectionMode::Cli
        );
        let created = super::resolve_profile(
            "proton",
            None,
            None,
            None,
            None,
            None,
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(created["options"]["proton_mode"], "native");
    }

    #[test]
    fn lifetimecloud_preset_resolves_username_template_with_omitted_or_blank_host() {
        for host in [None, Some(""), Some("   ")] {
            let created = super::resolve_profile(
                "webdav",
                Some("lifetimecloud"),
                host,
                None,
                Some("alice"),
                None,
                &json!({}),
            )
            .expect("lifetimecloud preset should resolve");
            assert_eq!(created["protocol"], "webdav");
            assert_eq!(created["providerId"], "lifetimecloud");
            assert_eq!(created["host"], "https://lifetimecloud.me/dav/alice/");
            assert_eq!(created["port"], 443);
            assert_eq!(created["username"], "alice");
        }
    }
}
