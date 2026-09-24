//! Shared profile option normalization and S3 preset defaults.
//!
//! Historically duplicated between the Tauri binary, the CLI binary, and the
//! MCP pool. Keeping the logic in a single module prevents drift (see the
//! 2026-04-17 study: MCP failed to extract S3 bucket because its copy of this
//! logic was incomplete).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;

pub const S3_PROVIDER_ID_META_KEY: &str = "_aeroftp_s3_provider_id";
pub const S3_ENDPOINT_SOURCE_META_KEY: &str = "_aeroftp_s3_endpoint_source";
pub const S3_REGION_SOURCE_META_KEY: &str = "_aeroftp_s3_region_source";
pub const S3_PATH_STYLE_SOURCE_META_KEY: &str = "_aeroftp_s3_path_style_source";

/// Normalize camelCase profile option keys from the GUI to snake_case keys
/// expected by the provider factory.
pub fn normalize_profile_option_key(key: &str) -> &str {
    match key {
        "tlsMode" => "tls_mode",
        // WebDAV HTTP/HTTPS toggle introduced for local bridges (Filen Desktop,
        // MEGAcmd). The GUI stores the user's choice as `options.webdavScheme`
        // and maps it to `tls_mode` only inside `App.tsx` before calling
        // `provider_connect`. Without this normalization, every other surface
        // that reads a saved profile (CLI, MCP pool, future schedulers) would
        // silently fall back to the auto-scheme heuristic in WebDavConfig and
        // ignore the user's explicit HTTP/HTTPS pick.
        "webdavScheme" => "tls_mode",
        // FTPS profiles imported from WinSCP / FileZilla before their
        // importers wrote `tlsMode` carry the mode as `ftpsMode`, which no
        // connection read: an explicit site on port 21 was opened as implicit.
        "ftpsMode" => "tls_mode",
        "verifyCert" => "verify_cert",
        // Swift: the GUI stores `options.allowCleartextStorage`; the provider
        // reads `allow_cleartext_storage_endpoint`. Without this line a saved
        // profile loaded outside the GUI keeps the camelCase key, the provider
        // sees nothing and fails closed, so a working profile stops connecting.
        "allowCleartextStorage" => "allow_cleartext_storage_endpoint",
        // S3: same shape as the Swift line above, and here for the same reason.
        // The GUI stores `options.allowCleartextEndpoint`; `S3Config` reads
        // `allow_cleartext_endpoint`. Without this line a profile loaded outside
        // the GUI keeps the camelCase key, the consent is not seen, and a
        // deployment the user already accepted stops connecting from the CLI.
        "allowCleartextEndpoint" => "allow_cleartext_endpoint",
        "pathStyle" => "path_style",
        "accountName" => "account_name",
        "accessKey" => "access_key",
        "sasToken" => "sas_token",
        "pcloudRegion" => "region",
        // S3 STS temporary credentials / AssumeRole (issue #301). The GUI
        // persists these in `options` as camelCase; CLI / MCP / schedulers load
        // the same profile through here, so they must map to the snake_case
        // keys that `S3Config::from_provider_config` reads from `extra`.
        "sessionToken" => "session_token",
        "roleArn" => "role_arn",
        "roleExternalId" => "role_external_id",
        "roleSessionName" => "role_session_name",
        "roleDurationSeconds" => "role_duration_seconds",
        // MFA serial is a persisted identifier; the one-time MFA token code is
        // never persisted (the GUI strips it before save), so it has no key here.
        "roleMfaSerial" => "role_mfa_serial",
        other => other,
    }
}

/// Mechanical camelCase to snake_case. Pure ASCII: non-alpha chars are
/// preserved as-is. Already-snake keys pass through unchanged.
fn camel_to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Canonical option key for `ProviderConfig.extra`.
///
/// The alias table runs first, because some GUI keys are not a mechanical
/// transform of the provider key (`webdavScheme` is `tls_mode`,
/// `allowCleartextStorage` is `allow_cleartext_storage_endpoint`,
/// `pcloudRegion` is `region`). Keys the table does not know are then
/// converted from camelCase to snake_case, so a GUI-shaped
/// `privateKeyPath` still lands as `private_key_path`. Already-snake keys
/// pass through unchanged.
///
/// Agent session, AI tools, CLI and MCP all go through here. Two of those
/// used to have their own tables and knew none of the aliases, so a
/// saved WebDAV `webdavScheme` or Swift `allowCleartextStorage` arrived
/// under the wrong key and the provider ignored it.
pub fn canonicalize_profile_option_key(key: &str) -> String {
    let aliased = normalize_profile_option_key(key);
    if aliased != key {
        aliased.to_string()
    } else {
        camel_to_snake_case(key)
    }
}

/// Insert a single profile option into the `extra` map after normalizing the
/// key and serializing primitive JSON values to strings.
pub fn insert_profile_option(
    extra: &mut HashMap<String, String>,
    key: &str,
    value: &serde_json::Value,
) {
    let normalized_key = canonicalize_profile_option_key(key);
    // Credential identity is the saved record's `id`, never an options
    // field a caller can choose (CWE-639). Skip here so every surface
    // that goes through this helper (CLI, MCP, agent, AI tools) agrees.
    if normalized_key == "profile_id" {
        return;
    }
    // `ftpsMode` is only the fallback for profiles imported before the
    // importers wrote `tlsMode`: it never replaces a mode already set, so the
    // user's `tlsMode` wins whatever order the options are iterated in (the
    // callers walk a serde_json map, sorted today, insertion-ordered the day
    // a dependency enables `preserve_order`).
    if key == "ftpsMode" && extra.contains_key(&normalized_key) {
        return;
    }

    if let Some(string_value) = value.as_str() {
        extra.insert(normalized_key, string_value.to_string());
    } else if let Some(bool_value) = value.as_bool() {
        extra.insert(normalized_key, bool_value.to_string());
    } else if let Some(number_value) = value.as_i64() {
        extra.insert(normalized_key, number_value.to_string());
    } else if let Some(number_value) = value.as_u64() {
        extra.insert(normalized_key, number_value.to_string());
    } else if let Some(number_value) = value.as_f64() {
        extra.insert(normalized_key, number_value.to_string());
    }
}

/// Copy the entire `options` object from a saved profile into `extra`.
pub fn apply_profile_options(extra: &mut HashMap<String, String>, profile: &serde_json::Value) {
    if let Some(provider_id) = profile
        .get("providerId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.insert(
            crate::providers::mega_df::PROVIDER_ID_META_KEY.to_string(),
            provider_id.to_string(),
        );
        // The preset id travels under two spellings: the meta key above, and the
        // plain `provider_id` that `to_provider_config` writes for the GUI path
        // and that providers read directly (swift.rs uses it to know a preset
        // whose object store has no TLS). Write both here, once, so a surface
        // that materialises a profile without going through a form (the MCP
        // pool, a scheduler, a benchmark) is not silently missing the plain one
        // and does not have to remember. The surfaces that break are always the
        // ones that carry a profile but no form.
        extra.insert("provider_id".to_string(), provider_id.to_string());
    }

    if let Some(opts) = profile.get("options").and_then(|v| v.as_object()) {
        for (k, v) in opts {
            let normalized = canonicalize_profile_option_key(k);
            // Credential identity is the saved record's `id`, never an
            // options field a caller can choose (CWE-639).
            if normalized == "profile_id" {
                continue;
            }
            insert_profile_option(extra, k, v);
        }
    }

    // Issue #214: bind Jottacloud (and OAuth) per-profile vault keys.
    // ProviderFactory reads extra["profile_id"] to call with_profile_id;
    // without it the connect path looks at the legacy singleton and an
    // imported `jottacloud_refresh_<id>` blob is invisible. Stamped LAST
    // so options cannot override it.
    if let Some(id) = profile
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extra.insert("profile_id".to_string(), id.to_string());
    }
}

/// Filen Desktop local bridges (filen-desktop-webdav / filen-desktop-s3)
/// authenticate to a loopback server whose credentials default to admin/admin
/// unless the user changed them in Filen Desktop > Network Drive. The GUI applies
/// this fallback at connect time (App.tsx normalizeProviderConnectionParams), but
/// the saved profile keeps the fields blank, so every non-GUI surface (CLI,
/// benchmark, MCP, schedulers) must apply the same fallback when materializing the
/// ProviderConfig. Without it the bridge rejects empty credentials with "Invalid
/// credentials" (WebDAV) / "signature does not match" (S3). Explicit values win.
///
/// The backend maps username/password to the right sink for both transports:
/// `WebDavConfig` reads them directly, `S3Config` maps them to
/// access_key_id/secret_access_key, so this one helper fixes both bridges.
pub fn apply_local_bridge_credential_defaults(
    provider_id: Option<&str>,
    username: &mut String,
    password: &mut String,
) {
    if matches!(
        provider_id,
        Some("filen-desktop-webdav") | Some("filen-desktop-s3")
    ) {
        if username.trim().is_empty() {
            *username = "admin".to_string();
        }
        if password.trim().is_empty() {
            *password = "admin".to_string();
        }
    }
}

fn s3_profile_default_region(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "backblaze" => Some("auto"),
        "cloudflare-r2" => Some("auto"),
        "google-cloud-storage" => Some("auto"),
        "idrive-e2" => Some("auto"),
        "filebase" => Some("auto"),
        "storj" => Some("global"),
        "filelu-s3" => Some("global"),
        "yandex-storage" => Some("ru-central1"),
        "oracle-cloud" => Some("us-east-1"),
        "minio" => Some("us-east-1"),
        "quotaless-s3" => Some("us-east-1"),
        "ibm-cos" => Some("eu-de"),
        _ => None,
    }
}

/// Addressing style a preset is known to need. `custom-s3` is deliberately
/// absent: it is not a preset, and `S3Config` already defaults a custom
/// endpoint to path-style, which is what self-hosted MinIO/Garage/Ceph need.
fn s3_profile_default_path_style(provider_id: &str) -> Option<bool> {
    match provider_id {
        "backblaze" => Some(true),
        "mega-s4" => Some(false),
        "cloudflare-r2" => Some(true),
        "google-cloud-storage" => Some(true),
        "idrive-e2" => Some(true),
        "filebase" => Some(true),
        "wasabi" => Some(false),
        "storj" => Some(true),
        "alibaba-oss" => Some(false),
        "tencent-cos" => Some(false),
        "filelu-s3" => Some(true),
        "yandex-storage" => Some(false),
        "digitalocean-spaces" => Some(false),
        "oracle-cloud" => Some(true),
        "minio" => Some(true),
        "quotaless-s3" => Some(true),
        "ibm-cos" => Some(false),
        _ => None,
    }
}

fn s3_profile_static_endpoint(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "filelu-s3" => Some("s5lu.com"),
        "filebase" => Some("https://s3.filebase.io"),
        "yandex-storage" => Some("https://storage.yandexcloud.net"),
        "quotaless-s3" => Some("https://io.quotaless.cloud:8000"),
        _ => None,
    }
}

/// Region an explicit endpoint names, read back through the preset's
/// `{region}` template (`s3.us-south.cloud-object-storage.appdomain.cloud`
/// gives `us-south` for `ibm-cos`). `None` for a preset without such a
/// template, a template with other placeholders, or a host the template does
/// not produce (a virtual-hosted `bucket.` host, another provider). Mirrors
/// `regionFromS3Endpoint` in `src/providers/registry.ts`.
pub fn s3_region_from_endpoint(provider_id: &str, endpoint: &str) -> Option<String> {
    fn without_scheme(value: &str) -> &str {
        value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .unwrap_or(value)
    }
    let template = without_scheme(s3_profile_endpoint_template(provider_id)?);
    let (prefix, suffix) = template.split_once("{region}")?;
    if prefix.contains('{') || suffix.contains('{') {
        return None;
    }
    let host = without_scheme(endpoint.trim())
        .split(['/', '?', '#'])
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    let region = host.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then(|| region.to_string())
}

/// Endpoint host segment for a Cloudflare R2 jurisdiction code.
///
/// `eu` and `us` are the two jurisdictions R2 publishes; everything else,
/// including the empty string, is the default placement and contributes no
/// segment. Kept deliberately total (no `Option`): the caller substitutes into
/// a host template, where "unknown" and "not set" must both mean the default
/// endpoint rather than a failure to resolve. Mirrors `jurisdictionSegment` in
/// `src/providers/registry.ts`.
pub fn r2_jurisdiction_segment(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "eu" => ".eu",
        "us" => ".us",
        _ => "",
    }
}

fn s3_profile_endpoint_template(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "mega-s4" => Some("s3.{region}.s4.mega.io"),
        "cloudflare-r2" => Some("{accountId}{jurisdiction}.r2.cloudflarestorage.com"),
        "google-cloud-storage" => Some("https://storage.googleapis.com"),
        "wasabi" => Some("https://s3.{region}.wasabisys.com"),
        "alibaba-oss" => Some("https://oss-{region}.aliyuncs.com"),
        "tencent-cos" => Some("https://cos.{region}.myqcloud.com"),
        "digitalocean-spaces" => Some("https://{region}.digitaloceanspaces.com"),
        "ibm-cos" => Some("https://s3.{region}.cloud-object-storage.appdomain.cloud"),
        _ => None,
    }
}

/// Resolve S3 preset defaults (region, path_style, endpoint) from the provider
/// id. Values already present in `extra` take precedence. Returns the resolved
/// endpoint string so callers can use it as fallback host.
///
/// `host` is the profile's own host. Several importers (Cyberduck, restic)
/// and older profiles carry the endpoint there and nowhere else; when it names
/// a non-AWS endpoint it is the profile's explicit endpoint and wins over the
/// preset template, exactly like `extra["endpoint"]`. Without this the template
/// wrote `extra["endpoint"]`, which `S3Config` reads before the host, so a
/// Wasabi or IBM COS profile in one region was silently pointed at another.
pub fn apply_s3_profile_defaults(
    extra: &mut HashMap<String, String>,
    provider_id: Option<&str>,
    host: &str,
) -> Option<String> {
    let provider_id = provider_id?;
    extra.insert(S3_PROVIDER_ID_META_KEY.to_string(), provider_id.to_string());

    // The explicit endpoint, if any: the stored option first, then a non-AWS
    // host (see below). A region it names is the one to sign with: SigV4
    // puts the region in the credential scope, and a regional endpoint
    // (IBM COS, Wasabi, ...) expects its own, not the preset default.
    let explicit_endpoint = extra
        .get("endpoint")
        .map(|endpoint| endpoint.trim().to_string())
        .filter(|endpoint| !endpoint.is_empty())
        .or_else(|| {
            let host = host.trim();
            (crate::bridge_shared::map_s3_provider_from_endpoint(host) != "amazon-s3")
                .then(|| host.to_string())
        });

    let region_source = if extra.contains_key("region") {
        "profile"
    } else if let Some(region) = explicit_endpoint
        .as_deref()
        .and_then(|endpoint| s3_region_from_endpoint(provider_id, endpoint))
    {
        extra.insert("region".to_string(), region);
        "endpoint"
    } else {
        if let Some(default_region) = s3_profile_default_region(provider_id) {
            extra.insert("region".to_string(), default_region.to_string());
        }
        "preset"
    };
    extra.insert(
        S3_REGION_SOURCE_META_KEY.to_string(),
        region_source.to_string(),
    );

    let path_style_from_profile = extra.contains_key("path_style");
    if !path_style_from_profile {
        if let Some(path_style) = s3_profile_default_path_style(provider_id) {
            extra.insert("path_style".to_string(), path_style.to_string());
        }
    }
    extra.insert(
        S3_PATH_STYLE_SOURCE_META_KEY.to_string(),
        if path_style_from_profile {
            "profile"
        } else {
            "preset"
        }
        .to_string(),
    );

    let endpoint_from_profile = extra
        .get("endpoint")
        .map(|endpoint| endpoint.trim())
        .filter(|endpoint| !endpoint.is_empty())
        .is_some();

    if let Some(existing_endpoint) = extra
        .get("endpoint")
        .map(|endpoint| endpoint.trim())
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_string)
    {
        extra.insert(
            S3_ENDPOINT_SOURCE_META_KEY.to_string(),
            "profile".to_string(),
        );
        return Some(existing_endpoint);
    }

    let host = host.trim();
    if crate::bridge_shared::map_s3_provider_from_endpoint(host) != "amazon-s3" {
        extra.insert(
            S3_ENDPOINT_SOURCE_META_KEY.to_string(),
            "profile".to_string(),
        );
        return Some(host.to_string());
    }

    let resolved_endpoint = if let Some(endpoint) = s3_profile_static_endpoint(provider_id) {
        Some(endpoint.to_string())
    } else {
        let template = s3_profile_endpoint_template(provider_id)?;
        let mut endpoint = template.to_string();

        if endpoint.contains("{region}") {
            let region = extra.get("region").map(String::as_str)?;
            endpoint = endpoint.replace("{region}", region);
        }

        // Jurisdiction: a bucket created in one answers ONLY on its own host
        // (`<account>.eu.r2.cloudflarestorage.com`). An absent or unknown value
        // is the default (no segment), never an error: every profile saved
        // before this option existed has no such key and must keep resolving to
        // the host it resolved to before.
        if endpoint.contains("{jurisdiction}") {
            let segment = r2_jurisdiction_segment(
                extra
                    .get("jurisdiction")
                    .map(String::as_str)
                    .unwrap_or_default(),
            );
            endpoint = endpoint.replace("{jurisdiction}", segment);
        }

        if endpoint.contains("{accountId}") {
            let account_id = extra
                .get("accountId")
                .or_else(|| extra.get("account_id"))
                .map(String::as_str)?;
            endpoint = endpoint.replace("{accountId}", account_id);
        }

        if endpoint.contains('{') {
            None
        } else {
            Some(endpoint)
        }
    }?;

    extra.insert("endpoint".to_string(), resolved_endpoint.clone());
    extra.insert(
        S3_ENDPOINT_SOURCE_META_KEY.to_string(),
        if endpoint_from_profile {
            "profile"
        } else {
            "preset"
        }
        .to_string(),
    );
    Some(resolved_endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_known_camel_case_keys() {
        assert_eq!(normalize_profile_option_key("tlsMode"), "tls_mode");
        assert_eq!(
            normalize_profile_option_key("allowCleartextStorage"),
            "allow_cleartext_storage_endpoint"
        );
        assert_eq!(normalize_profile_option_key("webdavScheme"), "tls_mode");
        assert_eq!(normalize_profile_option_key("verifyCert"), "verify_cert");
        assert_eq!(normalize_profile_option_key("pathStyle"), "path_style");
        assert_eq!(normalize_profile_option_key("accountName"), "account_name");
        assert_eq!(normalize_profile_option_key("accessKey"), "access_key");
        assert_eq!(normalize_profile_option_key("sasToken"), "sas_token");
        assert_eq!(normalize_profile_option_key("pcloudRegion"), "region");
    }

    #[test]
    fn normalize_passes_through_unknown_keys() {
        assert_eq!(normalize_profile_option_key("region"), "region");
        assert_eq!(normalize_profile_option_key("endpoint"), "endpoint");
        assert_eq!(normalize_profile_option_key("bucket"), "bucket");
        assert_eq!(
            normalize_profile_option_key("private_key_path"),
            "private_key_path"
        );
        // Mechanical conversion is canonicalize's job, not the alias table's.
        assert_eq!(
            normalize_profile_option_key("privateKeyPath"),
            "privateKeyPath"
        );
    }

    #[test]
    fn canonicalize_aliases_then_mechanical_camel_case() {
        assert_eq!(canonicalize_profile_option_key("webdavScheme"), "tls_mode");
        assert_eq!(
            canonicalize_profile_option_key("allowCleartextStorage"),
            "allow_cleartext_storage_endpoint"
        );
        assert_eq!(canonicalize_profile_option_key("pcloudRegion"), "region");
        assert_eq!(
            canonicalize_profile_option_key("sessionToken"),
            "session_token"
        );
        assert_eq!(canonicalize_profile_option_key("roleArn"), "role_arn");
        assert_eq!(
            canonicalize_profile_option_key("privateKeyPath"),
            "private_key_path"
        );
        assert_eq!(
            canonicalize_profile_option_key("trustUnknownHosts"),
            "trust_unknown_hosts"
        );
        assert_eq!(
            canonicalize_profile_option_key("sseKmsKeyId"),
            "sse_kms_key_id"
        );
        assert_eq!(canonicalize_profile_option_key("bucket"), "bucket");
        assert_eq!(
            canonicalize_profile_option_key("private_key_path"),
            "private_key_path"
        );
        assert_eq!(canonicalize_profile_option_key("drive_id"), "drive_id");
        // A mechanical rewrite of the alias would be the wrong provider key.
        assert_ne!(
            canonicalize_profile_option_key("webdavScheme"),
            "webdav_scheme"
        );
        assert_ne!(
            canonicalize_profile_option_key("allowCleartextStorage"),
            "allow_cleartext_storage"
        );
        assert_ne!(
            canonicalize_profile_option_key("pcloudRegion"),
            "pcloud_region"
        );
    }

    #[test]
    fn insert_profile_option_maps_non_alias_camel_case() {
        let mut extra = HashMap::new();
        insert_profile_option(&mut extra, "privateKeyPath", &json!("/tmp/id"));
        insert_profile_option(&mut extra, "webdavScheme", &json!("https"));
        insert_profile_option(&mut extra, "allowCleartextStorage", &json!(true));
        assert_eq!(
            extra.get("private_key_path").map(String::as_str),
            Some("/tmp/id")
        );
        assert_eq!(extra.get("tls_mode").map(String::as_str), Some("https"));
        assert_eq!(
            extra
                .get("allow_cleartext_storage_endpoint")
                .map(String::as_str),
            Some("true")
        );
        assert!(!extra.contains_key("webdavScheme"));
        assert!(!extra.contains_key("webdav_scheme"));
        assert!(!extra.contains_key("allowCleartextStorage"));
        assert!(!extra.contains_key("allow_cleartext_storage"));
    }

    #[test]
    fn ibm_cos_preset_resolves_region_path_style_and_endpoint() {
        // IBM Cloud Object Storage (free tier): regional public endpoint
        // `s3.{region}.cloud-object-storage.appdomain.cloud`, virtual-hosted
        // addressing, EU default. Explicit profile values must win over the
        // preset (same precedence contract as the wasabi preset).
        let mut extra = HashMap::new();
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("ibm-cos"), "");
        assert_eq!(
            endpoint.as_deref(),
            Some("https://s3.eu-de.cloud-object-storage.appdomain.cloud")
        );
        assert_eq!(extra.get("region").map(String::as_str), Some("eu-de"));
        assert_eq!(extra.get("path_style").map(String::as_str), Some("false"));

        let mut explicit = HashMap::new();
        explicit.insert("region".to_string(), "us-south".to_string());
        explicit.insert("path_style".to_string(), "true".to_string());
        let endpoint = apply_s3_profile_defaults(&mut explicit, Some("ibm-cos"), "");
        assert_eq!(
            endpoint.as_deref(),
            Some("https://s3.us-south.cloud-object-storage.appdomain.cloud")
        );
        assert_eq!(explicit.get("path_style").map(String::as_str), Some("true"));
    }

    #[test]
    fn s3_endpoint_carried_in_host_wins_over_the_preset_template() {
        // Cyberduck and restic imports (and older profiles) carry the endpoint
        // only in the profile host. The template used to write
        // `extra["endpoint"]`, which S3Config reads before the host, so an IBM
        // COS bucket in us-south without a region was sent to eu-de, and a
        // Wasabi bucket in eu-central-2 to the region default.
        for (preset, host) in [
            (
                "ibm-cos",
                "s3.us-south.cloud-object-storage.appdomain.cloud",
            ),
            ("wasabi", "s3.eu-central-2.wasabisys.com"),
            (
                "ibm-cos",
                "https://s3.jp-tok.cloud-object-storage.appdomain.cloud",
            ),
        ] {
            let mut extra = HashMap::new();
            let endpoint = apply_s3_profile_defaults(&mut extra, Some(preset), host);
            assert_eq!(endpoint.as_deref(), Some(host), "{preset}");
            assert!(
                !extra.contains_key("endpoint"),
                "{preset}: host must stay the endpoint"
            );
            assert_eq!(
                extra.get(S3_ENDPOINT_SOURCE_META_KEY).map(String::as_str),
                Some("profile")
            );
        }
        // An explicit endpoint option still wins over the host.
        let mut extra = HashMap::new();
        extra.insert(
            "endpoint".to_string(),
            "https://s3.eu-gb.cloud-object-storage.appdomain.cloud".to_string(),
        );
        let endpoint = apply_s3_profile_defaults(
            &mut extra,
            Some("ibm-cos"),
            "s3.us-south.cloud-object-storage.appdomain.cloud",
        );
        assert_eq!(
            endpoint.as_deref(),
            Some("https://s3.eu-gb.cloud-object-storage.appdomain.cloud")
        );
        // An AWS host is not an explicit endpoint: the preset still resolves.
        let mut extra = HashMap::new();
        extra.insert("region".to_string(), "eu-central-1".to_string());
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("wasabi"), "s3.amazonaws.com");
        assert_eq!(
            endpoint.as_deref(),
            Some("https://s3.eu-central-1.wasabisys.com")
        );
    }

    #[test]
    fn s3_region_is_read_back_from_a_regional_endpoint() {
        // Same case table as src/providers/s3ProfileLocation.test.ts.
        for (preset, endpoint, region) in [
            (
                "ibm-cos",
                "s3.us-south.cloud-object-storage.appdomain.cloud",
                Some("us-south"),
            ),
            (
                "ibm-cos",
                "https://s3.eu-gb.cloud-object-storage.appdomain.cloud/bucket",
                Some("eu-gb"),
            ),
            (
                "ibm-cos",
                "https://S3.JP-OSA.cloud-object-storage.appdomain.cloud:443",
                Some("jp-osa"),
            ),
            (
                "wasabi",
                "https://s3.eu-central-2.wasabisys.com",
                Some("eu-central-2"),
            ),
            ("wasabi", "s3.wasabisys.com", None),
            (
                "mega-s4",
                "s3.eu-central-2.s4.mega.io",
                Some("eu-central-2"),
            ),
            (
                "alibaba-oss",
                "https://oss-eu-central-1.aliyuncs.com",
                Some("eu-central-1"),
            ),
            (
                "tencent-cos",
                "https://cos.eu-frankfurt.myqcloud.com",
                Some("eu-frankfurt"),
            ),
            (
                "digitalocean-spaces",
                "https://fra1.digitaloceanspaces.com",
                Some("fra1"),
            ),
            (
                "digitalocean-spaces",
                "https://bucket.fra1.digitaloceanspaces.com",
                None,
            ),
            (
                "cloudflare-r2",
                "https://abc.r2.cloudflarestorage.com",
                None,
            ),
            ("backblaze", "s3.eu-central-003.backblazeb2.com", None),
        ] {
            assert_eq!(
                s3_region_from_endpoint(preset, endpoint).as_deref(),
                region,
                "{preset} {endpoint}"
            );
        }
    }

    #[test]
    fn s3_signing_region_follows_an_explicit_endpoint_without_a_stored_region() {
        // An imported IBM profile in us-south must not sign with the eu-de
        // preset default (SigV4 credential scope, CodeRabbit on PR #923).
        let mut extra = HashMap::new();
        apply_s3_profile_defaults(
            &mut extra,
            Some("ibm-cos"),
            "s3.us-south.cloud-object-storage.appdomain.cloud",
        );
        assert_eq!(extra.get("region").map(String::as_str), Some("us-south"));
        assert_eq!(
            extra.get(S3_REGION_SOURCE_META_KEY).map(String::as_str),
            Some("endpoint")
        );
        // Stored endpoint option, same rule.
        let mut extra = HashMap::new();
        extra.insert(
            "endpoint".to_string(),
            "https://s3.eu-central-2.wasabisys.com".to_string(),
        );
        apply_s3_profile_defaults(&mut extra, Some("wasabi"), "");
        assert_eq!(
            extra.get("region").map(String::as_str),
            Some("eu-central-2")
        );
        // A stored region always wins; an unreadable host keeps the default.
        let mut extra = HashMap::new();
        extra.insert("region".to_string(), "eu-gb".to_string());
        apply_s3_profile_defaults(
            &mut extra,
            Some("ibm-cos"),
            "s3.us-south.cloud-object-storage.appdomain.cloud",
        );
        assert_eq!(extra.get("region").map(String::as_str), Some("eu-gb"));
        let mut extra = HashMap::new();
        apply_s3_profile_defaults(&mut extra, Some("ibm-cos"), "cos.example.internal");
        assert_eq!(extra.get("region").map(String::as_str), Some("eu-de"));
    }

    #[test]
    fn webdav_local_bridge_https_profile_maps_to_tls_mode() {
        // Reproduces the Filen Desktop (local WebDAV) profile shape saved by
        // the GUI when the user picks HTTPS in the bridge toggle. Before this
        // commit the CLI and MCP read `webdavScheme` as a literal key and the
        // WebDavConfig auto-scheme picked HTTP for `local.webdav.filen.io`,
        // breaking the connect.
        let profile = json!({
            "options": {
                "webdavScheme": "https",
                "verifyCert": false,
                "anonymous": false,
            }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(extra.get("tls_mode").map(String::as_str), Some("https"));
        assert_eq!(extra.get("verify_cert").map(String::as_str), Some("false"));
        assert_eq!(extra.get("anonymous").map(String::as_str), Some("false"));
        assert!(
            !extra.contains_key("webdavScheme"),
            "raw camelCase key must not leak into provider config"
        );
    }

    #[test]
    fn ftp_tls_mode_still_normalizes() {
        // Guard rail: the new webdavScheme branch must not regress the
        // existing FTP/FTPS path that uses tlsMode (explicit/implicit/none).
        let profile = json!({
            "options": {
                "tlsMode": "explicit",
                "verifyCert": true,
            }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(extra.get("tls_mode").map(String::as_str), Some("explicit"));
        assert_eq!(extra.get("verify_cert").map(String::as_str), Some("true"));
    }

    #[test]
    fn filen_desktop_s3_profile_keeps_endpoint_and_disables_cert_check() {
        // Filen Desktop (local S3) preset shape. apply_profile_options must
        // preserve the endpoint as-is (pass-through) and normalize verifyCert
        // so S3Config.verify_cert is read correctly. apply_s3_profile_defaults
        // must then keep the user-supplied endpoint untouched.
        let profile = json!({
            "providerId": "filen-desktop-s3",
            "options": {
                "endpoint": "https://local.s3.filen.io:1800",
                "region": "filen",
                "bucket": "filen",
                "pathStyle": true,
                "verifyCert": false,
            }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("filen-desktop-s3"), "");
        assert_eq!(endpoint.as_deref(), Some("https://local.s3.filen.io:1800"));
        assert_eq!(extra.get("region").map(String::as_str), Some("filen"));
        assert_eq!(extra.get("bucket").map(String::as_str), Some("filen"));
        assert_eq!(extra.get("path_style").map(String::as_str), Some("true"));
        assert_eq!(extra.get("verify_cert").map(String::as_str), Some("false"));
        assert_eq!(
            extra.get(S3_ENDPOINT_SOURCE_META_KEY).map(String::as_str),
            Some("profile")
        );
    }

    #[test]
    fn s3_cleartext_consent_survives_the_trip_from_the_gui_to_the_provider() {
        // The GUI writes `allowCleartextEndpoint`; `S3Config` reads
        // `allow_cleartext_endpoint`. Without the normalization line the consent
        // is invisible outside the GUI and a deployment the user already
        // accepted stops connecting from the CLI, the MCP pool and schedulers.
        let profile = json!({
            "providerId": "minio",
            "options": { "endpoint": "http://minio.example.com:9000", "allowCleartextEndpoint": true }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(
            extra.get("allow_cleartext_endpoint").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn a_profile_that_never_consented_carries_no_consent_key() {
        let profile = json!({
            "providerId": "minio",
            "options": { "endpoint": "http://minio.example.com:9000" }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert!(!extra.contains_key("allow_cleartext_endpoint"));
    }

    #[test]
    fn r2_profile_without_jurisdiction_resolves_the_default_endpoint() {
        // The regression this guards: every R2 profile saved before the
        // jurisdiction option existed carries no such key, and the template now
        // has a `{jurisdiction}` placeholder. An unresolved placeholder makes
        // `apply_s3_profile_defaults` return None, which would leave those
        // profiles with no endpoint at all.
        let profile = json!({
            "providerId": "cloudflare-r2",
            "options": { "accountId": "a1b2c3d4e5f6", "bucket": "my-r2-bucket" }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("cloudflare-r2"), "");
        assert_eq!(
            endpoint.as_deref(),
            Some("a1b2c3d4e5f6.r2.cloudflarestorage.com")
        );
    }

    #[test]
    fn r2_jurisdiction_selects_its_own_endpoint_host() {
        for (code, expected) in [
            ("eu", "a1b2c3d4e5f6.eu.r2.cloudflarestorage.com"),
            ("us", "a1b2c3d4e5f6.us.r2.cloudflarestorage.com"),
            // Unknown and empty both mean "no jurisdiction": a value this build
            // does not know must not strand the profile without an endpoint.
            ("", "a1b2c3d4e5f6.r2.cloudflarestorage.com"),
            ("mars", "a1b2c3d4e5f6.r2.cloudflarestorage.com"),
            // The user picked it from a list, but a profile can be hand-edited
            // or come from an import, so case is not assumed.
            ("EU", "a1b2c3d4e5f6.eu.r2.cloudflarestorage.com"),
        ] {
            let profile = json!({
                "providerId": "cloudflare-r2",
                "options": {
                    "accountId": "a1b2c3d4e5f6",
                    "bucket": "my-r2-bucket",
                    "jurisdiction": code,
                }
            });
            let mut extra = HashMap::new();
            apply_profile_options(&mut extra, &profile);
            let endpoint = apply_s3_profile_defaults(&mut extra, Some("cloudflare-r2"), "");
            assert_eq!(
                endpoint.as_deref(),
                Some(expected),
                "jurisdiction {:?} resolved to the wrong host",
                code
            );
        }
    }

    #[test]
    fn r2_endpoint_saved_on_the_profile_wins_over_the_jurisdiction() {
        // An endpoint already on the profile is pass-through everywhere else in
        // this function; the jurisdiction must not start overriding it, or a
        // user who unlocked the endpoint field to type a host by hand would
        // have it silently rewritten.
        let profile = json!({
            "providerId": "cloudflare-r2",
            "options": {
                "accountId": "a1b2c3d4e5f6",
                "jurisdiction": "eu",
                "endpoint": "https://r2.example.test",
            }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("cloudflare-r2"), "");
        assert_eq!(endpoint.as_deref(), Some("https://r2.example.test"));
    }

    #[test]
    fn local_bridge_blank_credentials_default_to_admin() {
        // The headless parity fix (#368): a saved Filen Desktop bridge profile
        // keeps the credential fields blank (the GUI fills admin/admin only at
        // connect time), so the CLI/MCP must inject the same fallback.
        for id in ["filen-desktop-webdav", "filen-desktop-s3"] {
            let mut user = String::new();
            let mut pass = String::new();
            apply_local_bridge_credential_defaults(Some(id), &mut user, &mut pass);
            assert_eq!(
                user, "admin",
                "{id}: blank username should default to admin"
            );
            assert_eq!(
                pass, "admin",
                "{id}: blank password should default to admin"
            );
        }
    }

    #[test]
    fn local_bridge_explicit_credentials_win() {
        // A user who changed the bridge creds in Filen Desktop keeps them.
        let mut user = "alice".to_string();
        let mut pass = "s3cret".to_string();
        apply_local_bridge_credential_defaults(Some("filen-desktop-webdav"), &mut user, &mut pass);
        assert_eq!(user, "alice");
        assert_eq!(pass, "s3cret");
    }

    #[test]
    fn local_bridge_only_one_field_blank_gets_filled() {
        // Only-username-set: blank password fills, username untouched.
        let mut user = "alice".to_string();
        let mut pass = String::new();
        apply_local_bridge_credential_defaults(Some("filen-desktop-s3"), &mut user, &mut pass);
        assert_eq!(user, "alice");
        assert_eq!(pass, "admin");

        // Only-password-set: blank username fills, password untouched.
        let mut user = "   ".to_string(); // whitespace counts as blank
        let mut pass = "keep".to_string();
        apply_local_bridge_credential_defaults(Some("filen-desktop-webdav"), &mut user, &mut pass);
        assert_eq!(user, "admin");
        assert_eq!(pass, "keep");
    }

    #[test]
    fn local_bridge_other_providers_untouched() {
        // Any non-bridge provider id (or none) must never get the admin fallback.
        for id in [Some("custom-s3"), Some("webdav"), Some("filen"), None] {
            let mut user = String::new();
            let mut pass = String::new();
            apply_local_bridge_credential_defaults(id, &mut user, &mut pass);
            assert_eq!(user, "", "{id:?}: username must stay blank");
            assert_eq!(pass, "", "{id:?}: password must stay blank");
        }
    }

    #[test]
    fn s3_sts_assume_role_options_normalize_to_snake_case() {
        // A GUI-saved AWS AssumeRole profile carries camelCase keys; CLI / MCP
        // must see them as the snake_case keys S3Config reads (issue #301).
        let profile = json!({
            "providerId": "amazon-s3",
            "options": {
                "bucket": "data",
                "region": "us-east-1",
                "sessionToken": "FwoGZXIvYXdzEXAMPLE",
                "roleArn": "arn:aws:iam::123456789012:role/Demo",
                "roleExternalId": "ext-42",
                "roleSessionName": "team-sync",
                "roleDurationSeconds": 7200,
            }
        });
        let mut extra = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(
            extra.get("session_token").map(String::as_str),
            Some("FwoGZXIvYXdzEXAMPLE")
        );
        assert_eq!(
            extra.get("role_arn").map(String::as_str),
            Some("arn:aws:iam::123456789012:role/Demo")
        );
        assert_eq!(
            extra.get("role_external_id").map(String::as_str),
            Some("ext-42")
        );
        assert_eq!(
            extra.get("role_session_name").map(String::as_str),
            Some("team-sync")
        );
        // Numeric JSON value is serialized to a string for `extra`.
        assert_eq!(
            extra.get("role_duration_seconds").map(String::as_str),
            Some("7200")
        );
    }
}

#[cfg(test)]
mod provider_id_reach_tests {
    use super::{apply_profile_options, insert_profile_option};
    use serde_json::json;
    use std::collections::HashMap;

    /// The MCP pool, a scheduler and a benchmark all build `extra` from a saved
    /// profile with no connection form in the picture, so anything the GUI adds
    /// on the side is missing there. The preset id is one of those things, and
    /// Swift now depends on it to know that a provider's object store has no
    /// TLS. Pin that `apply_profile_options` alone is enough.
    #[test]
    fn a_profile_with_no_form_still_carries_the_preset_id_both_ways() {
        let profile = serde_json::json!({
            "providerId": "blomp",
            "options": {}
        });
        let mut extra: HashMap<String, String> = HashMap::new();
        apply_profile_options(&mut extra, &profile);

        assert_eq!(extra.get("provider_id").map(String::as_str), Some("blomp"));
        assert_eq!(
            extra
                .get(crate::providers::mega_df::PROVIDER_ID_META_KEY)
                .map(String::as_str),
            Some("blomp")
        );

        // And that is exactly what Swift needs to keep a saved Blomp profile
        // working over a surface that never sees the GUI.
        let config = crate::providers::ProviderConfig {
            name: "Blomp".to_string(),
            provider_type: crate::providers::ProviderType::Swift,
            host: "https://authenticate.blomp.com".to_string(),
            port: None,
            username: Some("user".to_string()),
            password: Some("pw".to_string()),
            initial_path: None,
            extra,
        };
        let swift = crate::providers::swift::SwiftConfig::from_provider_config(&config)
            .expect("swift config");
        assert!(swift.allow_cleartext_storage_endpoint);
    }

    #[test]
    fn a_saved_profile_carries_its_id_into_extra() {
        let profile = serde_json::json!({
            "id": "srv_1771799399856_swqija1mi",
            "protocol": "jottacloud",
            "options": {}
        });
        let mut extra: HashMap<String, String> = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(
            extra.get("profile_id").map(String::as_str),
            Some("srv_1771799399856_swqija1mi")
        );
    }

    #[test]
    fn options_cannot_override_the_saved_profile_id() {
        let profile = serde_json::json!({
            "id": "srv_real",
            "protocol": "jottacloud",
            "options": { "profile_id": "srv_attacker", "profileId": "srv_attacker_camel" }
        });
        let mut extra: HashMap<String, String> = HashMap::new();
        apply_profile_options(&mut extra, &profile);
        assert_eq!(
            extra.get("profile_id").map(String::as_str),
            Some("srv_real")
        );
    }

    #[test]
    fn insert_profile_option_drops_options_borne_profile_id() {
        let mut extra = HashMap::new();
        insert_profile_option(&mut extra, "profile_id", &json!("srv_attacker"));
        insert_profile_option(&mut extra, "profileId", &json!("srv_attacker_camel"));
        insert_profile_option(&mut extra, "bucket", &json!("ok"));
        assert!(!extra.contains_key("profile_id"));
        assert_eq!(extra.get("bucket").map(String::as_str), Some("ok"));
    }
}
