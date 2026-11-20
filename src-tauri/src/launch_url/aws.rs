// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The AWS credential chain an `s3://` launch URL falls back to.
//!
//! Precedence, highest first:
//!
//! | Source       | Access key id          | Secret                      | Session token       | Region                              |
//! |--------------|------------------------|-----------------------------|---------------------|-------------------------------------|
//! | URL          | user part              | password part               | (never in a URL)    | `?region=`                          |
//! | AeroFTP env  |                        | `AEROFTP_PASSWORD`          |                     |                                     |
//! | AWS env      | `AWS_ACCESS_KEY_ID`    | `AWS_SECRET_ACCESS_KEY`     | `AWS_SESSION_TOKEN` | `AWS_REGION`, `AWS_DEFAULT_REGION`  |
//! | AWS files    | `aws_access_key_id`    | `aws_secret_access_key`     | `aws_session_token` | `region`                            |
//!
//! The files are `~/.aws/credentials` and `~/.aws/config` (or the paths in
//! `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE`), section
//! `AWS_PROFILE` or `default`; `~/.aws/config` spells it `[profile name]`.
//!
//! A secret and its session token are taken only together with their own
//! access key id: an AWS variable or file pair whose key id differs from the
//! one already chosen belongs to another identity and is not mixed in. The
//! one exception is a bare `AWS_SECRET_ACCESS_KEY` beside a key id from the
//! URL, which is how the CLI has always read it.

use std::collections::HashMap;

type Sections = HashMap<String, HashMap<String, String>>;

/// Where the secret came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Url,
    AeroftpEnv,
    AwsEnv,
    AwsFiles,
}

/// What the chain resolved. `secret_access_key` is `None` when no source has
/// one for the chosen key id; the caller then asks for it or fails.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
    pub region: Option<String>,
    pub secret_source: Option<CredentialSource>,
}

/// The two AWS files, read as INI sections.
#[derive(Debug, Default)]
pub struct AwsFiles {
    pub credentials: Sections,
    pub config: Sections,
}

impl AwsFiles {
    /// The files the AWS tools read, from this process's environment. A
    /// missing, unreadable or oversized file contributes nothing.
    pub fn from_environment() -> Self {
        let Some(credentials_path) =
            crate::aws_credentials_import::default_aws_credentials_config_path()
        else {
            return Self::default();
        };
        let config_path = crate::aws_credentials_import::aws_config_path_for(&credentials_path);
        Self {
            credentials: read_sections(&credentials_path),
            config: config_path
                .map(|path| read_sections(&path))
                .unwrap_or_default(),
        }
    }
}

fn read_sections(path: &std::path::Path) -> Sections {
    if !crate::aws_credentials_import::is_readable_regular_file(path) {
        return Sections::new();
    }
    std::fs::read_to_string(path)
        .map(|content| crate::bridge_shared::parse_ini_sections(&content))
        .unwrap_or_default()
}

/// Resolve an `s3://` URL's credentials. `env` reads one variable; `files`
/// is called only when the URL and the environment leave something open, so
/// a complete URL never touches `~/.aws`.
pub fn resolve(
    url_user: &str,
    url_secret: Option<&str>,
    url_region: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    files: impl FnOnce() -> AwsFiles,
) -> S3Credentials {
    let env = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    let non_empty = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };

    let url_user = non_empty(Some(url_user));
    let url_secret = url_secret.filter(|secret| !secret.is_empty());
    let url_region = non_empty(url_region);
    let aws_key = env("AWS_ACCESS_KEY_ID");
    let aws_secret = env("AWS_SECRET_ACCESS_KEY");
    let aws_region = env("AWS_REGION").or_else(|| env("AWS_DEFAULT_REGION"));
    let aeroftp_secret = env("AEROFTP_PASSWORD");

    let needs_files = (url_user.is_none() && aws_key.is_none())
        || (url_secret.is_none() && aeroftp_secret.is_none() && aws_secret.is_none())
        || (url_region.is_none() && aws_region.is_none());
    let files = if needs_files {
        files()
    } else {
        AwsFiles::default()
    };
    let profile = env("AWS_PROFILE").unwrap_or_else(|| "default".to_string());
    let file_value = |key: &str| {
        let from = |sections: &Sections| {
            non_empty(
                sections
                    .get(&profile)
                    .and_then(|section| section.get(key))
                    .map(String::as_str),
            )
        };
        from(&files.credentials).or_else(|| from(&files.config))
    };
    let file_key = file_value("aws_access_key_id");

    let key_from_url = url_user.is_some();
    let access_key_id = url_user
        .or_else(|| aws_key.clone())
        .or_else(|| file_key.clone())
        .unwrap_or_default();

    let (secret_access_key, session_token, secret_source) = if let Some(secret) = url_secret {
        (Some(secret.to_string()), None, Some(CredentialSource::Url))
    } else if let Some(secret) = aeroftp_secret {
        (Some(secret), None, Some(CredentialSource::AeroftpEnv))
    } else if let Some(secret) = aws_secret.filter(|_| match &aws_key {
        Some(key) => *key == access_key_id,
        None => key_from_url,
    }) {
        (
            Some(secret),
            env("AWS_SESSION_TOKEN"),
            Some(CredentialSource::AwsEnv),
        )
    } else if let Some(secret) = file_value("aws_secret_access_key")
        .filter(|_| file_key.as_deref() == Some(access_key_id.as_str()))
    {
        (
            Some(secret),
            file_value("aws_session_token"),
            Some(CredentialSource::AwsFiles),
        )
    } else {
        (None, None, None)
    };

    let region = url_region
        .or(aws_region)
        .or_else(|| {
            non_empty(
                files
                    .config
                    .get(&profile)
                    .and_then(|s| s.get("region"))
                    .map(String::as_str),
            )
        })
        .or_else(|| {
            non_empty(
                files
                    .credentials
                    .get(&profile)
                    .and_then(|s| s.get("region"))
                    .map(String::as_str),
            )
        });

    S3Credentials {
        access_key_id,
        secret_access_key,
        session_token,
        region,
        secret_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    fn files_with(credentials: &str, config: &str) -> AwsFiles {
        AwsFiles {
            credentials: crate::bridge_shared::parse_ini_sections(credentials),
            config: crate::bridge_shared::parse_ini_sections(config),
        }
    }

    const CREDENTIALS: &str = "[default]\naws_access_key_id = FILEKEY\naws_secret_access_key = filesecret\naws_session_token = filetoken\n\n[work]\naws_access_key_id = WORKKEY\naws_secret_access_key = worksecret\n";
    const CONFIG: &str = "[default]\nregion = eu-west-3\n\n[profile work]\nregion = ap-south-1\n";

    const AWS_ENV: &[(&str, &str)] = &[
        ("AWS_ACCESS_KEY_ID", "ENVKEY"),
        ("AWS_SECRET_ACCESS_KEY", "envsecret"),
        ("AWS_SESSION_TOKEN", "envtoken"),
        ("AWS_REGION", "us-west-2"),
    ];

    #[test]
    fn the_url_wins_over_every_other_source() {
        let resolved = resolve(
            "URLKEY",
            Some("urlsecret"),
            Some("eu-central-1"),
            env_of(&[
                ("AEROFTP_PASSWORD", "aeroftp"),
                ("AWS_ACCESS_KEY_ID", "ENVKEY"),
                ("AWS_SECRET_ACCESS_KEY", "envsecret"),
                ("AWS_SESSION_TOKEN", "envtoken"),
                ("AWS_REGION", "us-west-2"),
            ]),
            || panic!("a complete URL must not read the AWS files"),
        );
        assert_eq!(resolved.access_key_id, "URLKEY");
        assert_eq!(resolved.secret_access_key.as_deref(), Some("urlsecret"));
        assert_eq!(resolved.session_token, None);
        assert_eq!(resolved.region.as_deref(), Some("eu-central-1"));
        assert_eq!(resolved.secret_source, Some(CredentialSource::Url));
    }

    #[test]
    fn the_aeroftp_variable_wins_over_the_aws_variables_and_files() {
        let resolved = resolve(
            "URLKEY",
            None,
            None,
            env_of(&[
                ("AEROFTP_PASSWORD", "aeroftp"),
                ("AWS_SECRET_ACCESS_KEY", "envsecret"),
            ]),
            || files_with(CREDENTIALS, CONFIG),
        );
        assert_eq!(resolved.secret_access_key.as_deref(), Some("aeroftp"));
        assert_eq!(resolved.secret_source, Some(CredentialSource::AeroftpEnv));
        assert_eq!(resolved.session_token, None);
        assert_eq!(resolved.region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn the_aws_variables_win_over_the_files() {
        let resolved = resolve("", None, None, env_of(AWS_ENV), || {
            files_with(CREDENTIALS, CONFIG)
        });
        assert_eq!(resolved.access_key_id, "ENVKEY");
        assert_eq!(resolved.secret_access_key.as_deref(), Some("envsecret"));
        assert_eq!(resolved.session_token.as_deref(), Some("envtoken"));
        assert_eq!(resolved.region.as_deref(), Some("us-west-2"));
        assert_eq!(resolved.secret_source, Some(CredentialSource::AwsEnv));
    }

    #[test]
    fn the_files_fill_in_when_nothing_else_does() {
        let resolved = resolve("", None, None, env_of(&[]), || {
            files_with(CREDENTIALS, CONFIG)
        });
        assert_eq!(resolved.access_key_id, "FILEKEY");
        assert_eq!(resolved.secret_access_key.as_deref(), Some("filesecret"));
        assert_eq!(resolved.session_token.as_deref(), Some("filetoken"));
        assert_eq!(resolved.region.as_deref(), Some("eu-west-3"));
        assert_eq!(resolved.secret_source, Some(CredentialSource::AwsFiles));
    }

    #[test]
    fn aws_profile_picks_the_section_in_both_files() {
        let resolved = resolve("", None, None, env_of(&[("AWS_PROFILE", "work")]), || {
            files_with(CREDENTIALS, CONFIG)
        });
        assert_eq!(resolved.access_key_id, "WORKKEY");
        assert_eq!(resolved.secret_access_key.as_deref(), Some("worksecret"));
        assert_eq!(resolved.session_token, None);
        assert_eq!(resolved.region.as_deref(), Some("ap-south-1"));
    }

    #[test]
    fn aws_default_region_is_read_when_aws_region_is_unset() {
        let resolved = resolve(
            "URLKEY",
            Some("urlsecret"),
            None,
            env_of(&[("AWS_DEFAULT_REGION", "sa-east-1")]),
            || panic!("the environment already answered"),
        );
        assert_eq!(resolved.region.as_deref(), Some("sa-east-1"));
    }

    #[test]
    fn a_secret_of_another_key_id_is_never_mixed_in() {
        // The URL names URLKEY; the AWS variables and the file belong to other
        // identities, so no secret is taken from them.
        let resolved = resolve("URLKEY", None, None, env_of(AWS_ENV), || {
            files_with(CREDENTIALS, CONFIG)
        });
        assert_eq!(resolved.access_key_id, "URLKEY");
        assert_eq!(resolved.secret_access_key, None);
        assert_eq!(resolved.session_token, None);
        assert_eq!(resolved.secret_source, None);

        // A key id from the AWS variables never takes the file's secret.
        let resolved = resolve(
            "",
            None,
            None,
            env_of(&[("AWS_ACCESS_KEY_ID", "ENVKEY")]),
            || files_with(CREDENTIALS, CONFIG),
        );
        assert_eq!(resolved.access_key_id, "ENVKEY");
        assert_eq!(resolved.secret_access_key, None);
    }

    #[test]
    fn a_bare_aws_secret_pairs_with_the_url_key_id_only() {
        let resolved = resolve(
            "URLKEY",
            None,
            None,
            env_of(&[("AWS_SECRET_ACCESS_KEY", "envsecret")]),
            AwsFiles::default,
        );
        assert_eq!(resolved.secret_access_key.as_deref(), Some("envsecret"));

        // With the key id from the files, the bare variable is not its pair.
        let resolved = resolve(
            "",
            None,
            None,
            env_of(&[("AWS_SECRET_ACCESS_KEY", "envsecret")]),
            || files_with(CREDENTIALS, CONFIG),
        );
        assert_eq!(resolved.access_key_id, "FILEKEY");
        assert_eq!(resolved.secret_access_key.as_deref(), Some("filesecret"));
    }

    #[test]
    fn empty_values_count_as_unset() {
        let resolved = resolve(
            "",
            Some(""),
            Some(""),
            env_of(&[
                ("AEROFTP_PASSWORD", ""),
                ("AWS_ACCESS_KEY_ID", " "),
                ("AWS_REGION", ""),
            ]),
            || files_with(CREDENTIALS, CONFIG),
        );
        assert_eq!(resolved.access_key_id, "FILEKEY");
        assert_eq!(resolved.secret_source, Some(CredentialSource::AwsFiles));
        assert_eq!(resolved.region.as_deref(), Some("eu-west-3"));
    }
}
