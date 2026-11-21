// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A secret a launch URL found in the environment or in the AWS tools' chain
//! stays in Rust. The request the webview reads carries an opaque handle in
//! its place, and the handle is bound to the connection the link named: its
//! scheme and the address the secret goes to ([`secret_destination`], read
//! from the connection's own configuration: an S3 endpoint and bucket, an
//! Azure blob endpoint, a Swift auth URL, the host of the others). A connect
//! resolves the secret again only through a handle this process issued and
//! only for a configuration that reaches that same address over a verified
//! TLS connection, so a script in the webview can neither read the secret
//! nor send it, or requests it signs, to a host of its choosing. The registry lives in this process alone: it is never
//! serialized, logged or saved, and a profile saved from the link keeps no
//! handle ([`strip_from_profiles`], on every profile write).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Where the secret of a bound connection comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum Provenance {
    /// One environment variable of the scheme's list, or `AEROFTP_PASSWORD`.
    Env(&'static str),
    /// The AWS chain (`AWS_*`, `~/.aws`) for this access key id; whether the
    /// key id came from the URL decides whether a bare
    /// `AWS_SECRET_ACCESS_KEY` pairs with it.
    AwsChain {
        access_key_id: String,
        key_from_url: bool,
    },
}

#[derive(Clone)]
struct Binding {
    token: &'static str,
    destination: String,
    provenance: Provenance,
}

/// What a bound connection resolved: the secret, and the session token the
/// AWS chain may add.
pub struct Resolved {
    pub secret: String,
    pub session_token: Option<String>,
}

fn registry() -> &'static Mutex<HashMap<String, Binding>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Binding>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 128 random bits from the operating system, in hex: nothing in it derives
/// from the variable's name or the host.
fn new_handle() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Bind a handle to the connection a launch URL named. `token` is the
/// scheme's connection token and `destination` the [`secret_destination`] of
/// the configuration the link describes, as `parse_remote_url` builds it.
pub fn register(token: &'static str, destination: &str, provenance: Provenance) -> String {
    let handle = new_handle();
    let binding = Binding {
        token,
        destination: destination.to_string(),
        provenance,
    };
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(handle.clone(), binding);
    handle
}

/// The secret of the connection `handle` was issued for, read again now.
/// `protocol`, `destination` (the [`secret_destination`] of the configuration
/// the connect will use) and `username` are the connect's own; any of them
/// that differs from the binding refuses, and so does an unknown handle:
/// neither ever falls back to reading the environment.
pub fn resolve(
    handle: &str,
    protocol: &str,
    destination: &str,
    username: &str,
) -> Result<Resolved, String> {
    resolve_with(
        handle,
        protocol,
        destination,
        username,
        |name| std::env::var(name).ok(),
        super::aws::AwsFiles::from_environment,
    )
}

pub(crate) fn resolve_with(
    handle: &str,
    protocol: &str,
    destination: &str,
    username: &str,
    env: impl Fn(&str) -> Option<String>,
    files: impl FnOnce() -> super::aws::AwsFiles,
) -> Result<Resolved, String> {
    let binding = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(handle)
        .cloned()
        .ok_or_else(|| {
            "This connection's secret came from a launch link of an earlier session: open the link again, or save the secret in the profile"
                .to_string()
        })?;
    let token = super::launch_scheme(protocol).map(|scheme| scheme.token);
    if token != Some(binding.token) || destination != binding.destination {
        return Err(format!(
            "The launch link's secret belongs to its own {}:// connection, not to this one",
            binding.token
        ));
    }
    match binding.provenance {
        Provenance::Env(var) => {
            // The variable is checked against the scheme's list again here,
            // whatever registered it.
            if !super::secret_env_allowed(binding.token, var) {
                return Err(format!(
                    "{var} is not a secret variable of {}://",
                    binding.token
                ));
            }
            env(var)
                .filter(|value| !value.is_empty())
                .map(|secret| Resolved {
                    secret,
                    session_token: None,
                })
                .ok_or_else(|| {
                    format!("{var} no longer holds the secret this link was opened with")
                })
        }
        Provenance::AwsChain {
            access_key_id,
            key_from_url,
        } => {
            if username != access_key_id {
                return Err(
                    "The launch link's AWS credentials belong to another access key id".to_string(),
                );
            }
            super::aws::resolve_chain_secret(&access_key_id, key_from_url, env, files).map(
                |(secret, session_token)| Resolved {
                    secret,
                    session_token,
                },
            )
        }
    }
}

/// Where the secret of `config` goes: the address its provider authenticates
/// to, computed from the configuration with the provider's own rules, so the
/// launch and the connect compare what the connection really reaches.
///
/// - S3: the origin of the endpoint (`s3_endpoint_and_path_style`, the one
///   the connection reads, where an explicit endpoint wins over the host),
///   or `aws` for an AWS endpoint, whose region must then be a plain region
///   name since it becomes part of the host; plus the bucket, which a
///   virtual-hosted request puts in the host.
/// - Azure: the origin of the blob endpoint (the host, else the account).
/// - Swift: the origin of the auth URL.
/// - Every other provider: the host of the configuration (a fixed API host
///   for the services that have one).
///
/// A configuration that turns certificate checks off or allows a cleartext
/// endpoint is refused: the secret, or the requests it signs, would travel
/// where a network attacker reads them.
pub fn secret_destination(config: &crate::providers::ProviderConfig) -> Result<String, String> {
    use crate::providers::ProviderType;
    let option = |key: &str| config.extra.get(key).map(|value| value.trim());
    if option("verify_cert") == Some("false")
        || [
            "allow_cleartext_endpoint",
            "allow_cleartext_storage_endpoint",
        ]
        .iter()
        .any(|key| option(key).is_some_and(|value| value.eq_ignore_ascii_case("true")))
    {
        return Err("A launch link's secret travels only over a verified TLS connection, and this connection turns that check off".to_string());
    }
    let destination = match config.provider_type {
        ProviderType::S3 => {
            let (endpoint, _) = crate::providers::types::s3_endpoint_and_path_style(config);
            // AWS is told by the host the client parses, as `is_aws_endpoint`
            // tells it: `evil.example#.amazonaws.com` is evil.example.
            let service = match endpoint.as_deref() {
                Some(endpoint) => {
                    let url = parse_address(endpoint)?;
                    if is_aws_host(url.host_str().unwrap_or_default()) {
                        "aws".to_string()
                    } else {
                        url.origin().ascii_serialization()
                    }
                }
                None => {
                    // `https://s3.<region>.amazonaws.com`: the region is part
                    // of the host, so it is a plain region name or nothing.
                    let region = option("region")
                        .filter(|r| !r.is_empty())
                        .unwrap_or("us-east-1");
                    if !region
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                    {
                        return Err(format!("{region:?} is not an S3 region name"));
                    }
                    "aws".to_string()
                }
            };
            format!("{service} bucket={}", option("bucket").unwrap_or_default())
        }
        ProviderType::Azure => {
            // The endpoint alone: the key and the container are not read
            // for it, so placeholders stand in where the connect has none.
            let mut probe = config.clone();
            probe.password = Some(String::new());
            probe
                .extra
                .entry("container".to_string())
                .or_insert_with(|| "-".to_string());
            let azure = crate::providers::types::AzureConfig::from_provider_config(&probe)
                .map_err(|err| err.to_string())?;
            origin(&azure.blob_endpoint())?
        }
        ProviderType::Swift => {
            let swift = crate::providers::swift::SwiftConfig::from_provider_config(config)
                .map_err(|err| err.to_string())?;
            origin(&swift.auth_url)?
        }
        _ => config.host.trim().to_ascii_lowercase(),
    };
    Ok(format!("{:?} {destination}", config.provider_type))
}

/// An address as the HTTP client parses it, a bare host read as `https`.
/// Comparing its origin normalizes what does not change the peer: the case
/// of the host, a default port written or omitted, and any userinfo.
fn parse_address(address: &str) -> Result<url::Url, String> {
    let address = address.trim();
    let url = if address.contains("://") {
        url::Url::parse(address)
    } else {
        url::Url::parse(&format!("https://{address}"))
    }
    .map_err(|err| format!("Invalid address {address:?}: {err}"))?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("Invalid address {address:?}: no host"));
    }
    Ok(url)
}

/// `scheme://host[:port]` of an address.
fn origin(address: &str) -> Result<String, String> {
    Ok(parse_address(address)?.origin().ascii_serialization())
}

/// An AWS host, as the S3 provider tells one (`is_aws_endpoint`).
fn is_aws_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "amazonaws.com"
        || host.ends_with(".amazonaws.com")
        || host.ends_with(".amazonaws.com.cn")
}

/// Drop a launch handle from profiles about to be saved: a profile saved
/// from a launch link keeps neither its secret nor its handle, and its next
/// connect uses the vault or asks.
pub fn strip_from_profiles(profiles: &mut [serde_json::Value]) {
    for profile in profiles {
        if let Some(options) = profile
            .get_mut("options")
            .and_then(serde_json::Value::as_object_mut)
        {
            options.remove("launchSecret");
            options.remove("launch_secret");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3_destination(
        endpoint: Option<&str>,
        host: &str,
        port: Option<u16>,
    ) -> Result<String, String> {
        let mut extra = HashMap::new();
        extra.insert("bucket".to_string(), "b1".to_string());
        if let Some(endpoint) = endpoint {
            extra.insert("endpoint".to_string(), endpoint.to_string());
        }
        secret_destination(&crate::providers::ProviderConfig {
            name: String::new(),
            provider_type: crate::providers::ProviderType::S3,
            host: host.to_string(),
            port,
            username: None,
            password: None,
            initial_path: None,
            extra,
        })
    }

    #[test]
    fn the_destination_is_the_peer_the_client_parses() {
        let link = s3_destination(None, "minio.lan", Some(443)).unwrap();
        // The same peer written another way: host case, the default port
        // written, userinfo, a trailing slash, an endpoint over the host.
        for (endpoint, host, port) in [
            (None, "MINIO.lan", Some(443)),
            (None, "minio.lan", None),
            (Some("https://minio.lan:443/"), "", None),
            (Some("https://user@minio.lan"), "other.example", None),
        ] {
            assert_eq!(
                s3_destination(endpoint, host, port).as_ref(),
                Ok(&link),
                "{endpoint:?} {host}"
            );
        }
        // Another peer: a delimiter after a lookalike AWS name, userinfo in
        // front of another host, another port, another scheme.
        for endpoint in [
            "https://evil.example#.amazonaws.com",
            "https://evil.example?.amazonaws.com",
            // Written without a scheme, the endpoint reaches the client as typed.
            "evil.example#.amazonaws.com",
            "evil.example?.amazonaws.com",
            "https://minio.lan@evil.example",
            "https://minio.lan:9443",
            "http://minio.lan",
        ] {
            assert_ne!(
                s3_destination(Some(endpoint), "minio.lan", None).as_ref(),
                Ok(&link),
                "{endpoint}"
            );
        }
        // Every AWS endpoint is the one service; a region is a plain name.
        let aws = s3_destination(None, "s3.amazonaws.com", None).unwrap();
        assert_eq!(
            s3_destination(Some("https://s3.eu-west-1.amazonaws.com"), "", None).unwrap(),
            aws
        );
        for lookalike in [
            "https://evil.example#.amazonaws.com",
            "evil.example#.amazonaws.com",
            "evil.example?.amazonaws.com",
        ] {
            assert_ne!(
                s3_destination(Some(lookalike), "", None).as_ref(),
                Ok(&aws),
                "{lookalike}"
            );
        }
    }

    #[test]
    fn a_saved_profile_keeps_no_launch_handle() {
        let mut profiles = vec![
            serde_json::json!({"id": "a", "options": {"launchSecret": "0123", "launch_secret": "x", "bucket": "b1"}}),
            serde_json::json!({"id": "b"}),
        ];
        strip_from_profiles(&mut profiles);
        assert_eq!(profiles[0]["options"], serde_json::json!({"bucket": "b1"}));
        assert_eq!(profiles[1], serde_json::json!({"id": "b"}));
    }

    const SENTINEL: &str = "sentinel-env-secret-0d5c";

    fn env_with(var: &'static str) -> impl Fn(&str) -> Option<String> {
        move |name| (name == var).then(|| SENTINEL.to_string())
    }

    #[test]
    fn a_handle_is_random_and_names_neither_the_variable_nor_the_host() {
        let first = register("gitlab", "git.example.com", Provenance::Env("GITLAB_TOKEN"));
        let second = register("gitlab", "git.example.com", Provenance::Env("GITLAB_TOKEN"));
        assert_ne!(first, second);
        for handle in [&first, &second] {
            assert_eq!(handle.len(), 32);
            assert!(handle.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(!handle.contains("gitlab") && !handle.contains("GITLAB"));
        }
    }

    #[test]
    fn the_connection_the_link_named_reads_its_secret() {
        let handle = register("gitlab", "git.example.com", Provenance::Env("GITLAB_TOKEN"));
        let resolved = resolve_with(
            &handle,
            "gitlab",
            "git.example.com",
            "",
            env_with("GITLAB_TOKEN"),
            Default::default,
        )
        .unwrap();
        assert_eq!(resolved.secret, SENTINEL);
        // An alias of the scheme is the same connection.
        let handle = register(
            "backblaze",
            "api.backblazeb2.com",
            Provenance::Env("B2_APPLICATION_KEY"),
        );
        assert!(resolve_with(
            &handle,
            "b2",
            "api.backblazeb2.com",
            "",
            env_with("B2_APPLICATION_KEY"),
            Default::default,
        )
        .is_ok());
    }

    #[test]
    fn a_call_without_the_links_handle_or_for_another_host_reads_no_environment() {
        let read = std::cell::Cell::new(0);
        let counting = |name: &str| {
            read.set(read.get() + 1);
            env_with("GITLAB_TOKEN")(name)
        };
        // A script that names a variable or invents a handle.
        for handle in ["GITLAB_TOKEN", "00000000000000000000000000000000", ""] {
            let err = resolve_with(
                handle,
                "gitlab",
                "evil.example",
                "",
                counting,
                Default::default,
            )
            .err()
            .unwrap();
            assert!(!err.contains(SENTINEL), "{err}");
        }
        // The link's own handle, sent to another host or another scheme.
        let handle = register("gitlab", "git.example.com", Provenance::Env("GITLAB_TOKEN"));
        for (protocol, host) in [
            ("gitlab", "evil.example"),
            ("gitlab", "git.example.com:8443"),
            ("github", "git.example.com"),
        ] {
            let err = resolve_with(&handle, protocol, host, "", counting, Default::default)
                .err()
                .unwrap();
            assert!(!err.contains(SENTINEL), "{err}");
        }
        assert_eq!(
            read.get(),
            0,
            "a refused connect never reads the environment"
        );
    }

    #[test]
    fn a_variable_outside_the_schemes_list_is_refused_at_resolve_time() {
        let handle = register("gitlab", "git.example.com", Provenance::Env("HOME"));
        let err = resolve_with(
            &handle,
            "gitlab",
            "git.example.com",
            "",
            env_with("HOME"),
            Default::default,
        )
        .err()
        .unwrap();
        assert!(err.contains("not a secret variable"), "{err}");
        let handle = register(
            "gitlab",
            "git.example.com",
            Provenance::Env("AEROFTP_PASSWORD"),
        );
        assert!(resolve_with(
            &handle,
            "gitlab",
            "git.example.com",
            "",
            env_with("AEROFTP_PASSWORD"),
            Default::default,
        )
        .is_ok());
    }

    #[test]
    fn an_aws_chain_handle_reads_the_chain_only_for_its_key_id() {
        let handle = register(
            "s3",
            "minio.example",
            Provenance::AwsChain {
                access_key_id: "AKID".into(),
                key_from_url: true,
            },
        );
        let env = |name: &str| match name {
            "AWS_SECRET_ACCESS_KEY" => Some(SENTINEL.to_string()),
            _ => None,
        };
        let resolved = resolve_with(
            &handle,
            "s3",
            "minio.example",
            "AKID",
            env,
            Default::default,
        )
        .unwrap();
        assert_eq!(resolved.secret, SENTINEL);
        assert!(resolve_with(
            &handle,
            "s3",
            "minio.example",
            "OTHER",
            env,
            Default::default
        )
        .is_err());
        assert!(
            resolve_with(&handle, "s3", "evil.example", "AKID", env, Default::default).is_err()
        );
    }
}
