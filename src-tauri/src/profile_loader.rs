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
        "verifyCert" => "verify_cert",
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
        other => other,
    }
}

/// Insert a single profile option into the `extra` map after normalizing the
/// key and serializing primitive JSON values to strings.
pub fn insert_profile_option(
    extra: &mut HashMap<String, String>,
    key: &str,
    value: &serde_json::Value,
) {
    let normalized_key = normalize_profile_option_key(key).to_string();

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
    }

    if let Some(opts) = profile.get("options").and_then(|v| v.as_object()) {
        for (k, v) in opts {
            insert_profile_option(extra, k, v);
        }
    }
}

fn s3_profile_default_region(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "backblaze" => Some("auto"),
        "cloudflare-r2" => Some("auto"),
        "google-cloud-storage" => Some("auto"),
        "idrive-e2" => Some("auto"),
        "storj" => Some("global"),
        "filelu-s3" => Some("global"),
        "yandex-storage" => Some("ru-central1"),
        "oracle-cloud" => Some("us-east-1"),
        "minio" => Some("us-east-1"),
        "quotaless-s3" => Some("us-east-1"),
        _ => None,
    }
}

fn s3_profile_default_path_style(provider_id: &str) -> Option<bool> {
    match provider_id {
        "custom-s3" => Some(false),
        "backblaze" => Some(true),
        "mega-s4" => Some(false),
        "cloudflare-r2" => Some(true),
        "google-cloud-storage" => Some(true),
        "idrive-e2" => Some(true),
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
        _ => None,
    }
}

fn s3_profile_static_endpoint(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "filelu-s3" => Some("s5lu.com"),
        "yandex-storage" => Some("https://storage.yandexcloud.net"),
        "quotaless-s3" => Some("https://io.quotaless.cloud:8000"),
        _ => None,
    }
}

fn s3_profile_endpoint_template(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "mega-s4" => Some("s3.{region}.s4.mega.io"),
        "cloudflare-r2" => Some("{accountId}.r2.cloudflarestorage.com"),
        "google-cloud-storage" => Some("https://storage.googleapis.com"),
        "wasabi" => Some("https://s3.{region}.wasabisys.com"),
        "alibaba-oss" => Some("https://oss-{region}.aliyuncs.com"),
        "tencent-cos" => Some("https://cos.{region}.myqcloud.com"),
        "digitalocean-spaces" => Some("https://{region}.digitaloceanspaces.com"),
        _ => None,
    }
}

/// Resolve S3 preset defaults (region, path_style, endpoint) from the provider
/// id. Values already present in `extra` take precedence. Returns the resolved
/// endpoint string so callers can use it as fallback host.
pub fn apply_s3_profile_defaults(
    extra: &mut HashMap<String, String>,
    provider_id: Option<&str>,
) -> Option<String> {
    let provider_id = provider_id?;
    extra.insert(S3_PROVIDER_ID_META_KEY.to_string(), provider_id.to_string());

    let region_from_profile = extra.contains_key("region");
    if !region_from_profile {
        if let Some(default_region) = s3_profile_default_region(provider_id) {
            extra.insert("region".to_string(), default_region.to_string());
        }
    }
    extra.insert(
        S3_REGION_SOURCE_META_KEY.to_string(),
        if region_from_profile {
            "profile"
        } else {
            "preset"
        }
        .to_string(),
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

    let resolved_endpoint = if let Some(endpoint) = s3_profile_static_endpoint(provider_id) {
        Some(endpoint.to_string())
    } else {
        let template = s3_profile_endpoint_template(provider_id)?;
        let mut endpoint = template.to_string();

        if endpoint.contains("{region}") {
            let region = extra.get("region").map(String::as_str)?;
            endpoint = endpoint.replace("{region}", region);
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
        let endpoint = apply_s3_profile_defaults(&mut extra, Some("filen-desktop-s3"));
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
        assert_eq!(extra.get("role_external_id").map(String::as_str), Some("ext-42"));
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
