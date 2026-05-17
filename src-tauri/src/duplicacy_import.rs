// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Bridge: import/export Duplicacy backup-repo storage as AeroFTP profiles.
//!
//! Conceptually a Duplicacy `<repo>/.duplicacy/preferences` file is NOT a
//! transfer endpoint, it is an encrypted backup repository. The bridge value
//! is backend reuse (Fase 3 policy v1): extract the repo's storage backend
//! (B2 / S3 / SFTP / WebDAV) and materialise a normal `ServerProfileExport`
//! so AeroVault / AeroSync can point at the same storage the user already
//! backs up to. Repo-awareness is recorded only as `options.repo_kind` /
//! `options.repo_id`, no extra logic.
//!
//! Duplicacy is per-repository: the preferences file lives at
//! `<working-dir>/.duplicacy/preferences`. There is NO global config path,
//! so `default_duplicacy_config_path()` returns `None` unless the
//! `DUPLICACY_REPOSITORY` env var points at a working dir (in which case we
//! join `.duplicacy/preferences`). The caller MUST be prepared to pass the
//! repository's preferences path explicitly.
//!
//! Secret policy (per APPENDIX-BRIDGE contract):
//! | source                                   | credential | skipped/log reason                 |
//! |------------------------------------------|------------|------------------------------------|
//! | plaintext in `keys` map                  | populated  | (none)                             |
//! | env var `DUPLICACY_<NAME>_<KEY>`         | populated  | (none, resolved at import)         |
//! | absent in keys and env                   | None       | "secret in env at runtime"         |
//! | OAuth backend (gcd/one/dropbox)          | None       | "OAuth: provider-issued token, ... |
//!
//! `keys` values in `.duplicacy/preferences` are stored in plaintext (NOT
//! real encryption). On import they are re-encrypted into the AES-256-GCM
//! vault, upgrading at-rest protection. The reverse path (`export_duplicacy`)
//! re-emits them in Duplicacy's native plaintext `keys` form by design, and
//! the file is written `0600` via `atomic_write_600`.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Source model ============

/// One entry of the `.duplicacy/preferences` JSON array.
///
/// Only the fields the bridge needs are deserialized; `#[serde(default)]` on
/// `keys` and serde's tolerance of unknown fields means extra keys
/// (`repository`, `id`, `encrypted`, `nobackup_file`, filters, ...) are
/// silently ignored.
#[derive(serde::Deserialize)]
struct DupStorage {
    #[serde(default = "default_storage_name")]
    name: String,
    storage: String,
    // Real Duplicacy writes `"keys": null` (not just an absent field)
    // whenever no credential is saved in the preferences, e.g. local-backend
    // repos or `-no-save-password`. `#[serde(default)]` alone only covers an
    // ABSENT field, so an explicit `null` made serde fail with
    // "invalid type: null, expected a map" and the whole import aborted.
    // Treat both absent and null as an empty map.
    #[serde(default, deserialize_with = "de_null_or_map")]
    keys: HashMap<String, String>,
}

fn default_storage_name() -> String {
    "default".to_string()
}

/// `(username, credential, extra_options)` resolved per storage backend.
type BackendSecret = (String, Option<String>, Vec<(&'static str, Option<String>)>);

/// Deserialize `keys`, mapping JSON `null` (Duplicacy's own output when no
/// secret is stored) to an empty map instead of erroring.
fn de_null_or_map<'de, D>(de: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    Ok(Option::<HashMap<String, String>>::deserialize(de)?.unwrap_or_default())
}

/// Parse a Duplicacy `storage` URL into `(protocol, provider_id, host,
/// initial_path)`. Returns `None` for OAuth-only schemes
/// (`gcd`/`one`/`dropbox`) so the caller can emit an OAuth skipped reason,
/// and for any unknown scheme.
///
/// Reconciled to the real `crate::bridge_shared` API (spec drafts referenced
/// the pre-refactor `crate::rclone_import_shared`).
fn parse_storage_url(
    u: &str,
) -> Option<(&'static str, Option<String>, String, Option<String>)> {
    let (scheme, rest) = u.split_once("://")?;
    match scheme {
        // b2://bucket -> backblaze-b2, bucket lands in initial_path
        "b2" => Some((
            "s3",
            Some("backblaze-b2".to_string()),
            String::new(),
            (!rest.is_empty()).then(|| rest.to_string()),
        )),
        // s3|minio|wasabi://host[/path]
        "s3" | "minio" | "wasabi" => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            // Prefer the explicit scheme token; fall back to endpoint host
            // inference for a custom/unknown host.
            let mut pid = crate::bridge_shared::map_s3_provider(scheme);
            if pid == "custom-s3" {
                pid = crate::bridge_shared::map_s3_provider_from_endpoint(host);
            }
            Some((
                "s3",
                Some(pid.to_string()),
                host.to_string(),
                (!path.is_empty()).then(|| path.to_string()),
            ))
        }
        // sftp://[user@]host[/path]
        "sftp" => {
            let hostpart = rest.split('/').next().unwrap_or(rest);
            // strip a leading "user@"
            let host = hostpart.split('@').next_back().unwrap_or(hostpart);
            let path = rest
                .split_once('/')
                .map(|(_, p)| format!("/{p}"))
                .filter(|p| p != "/");
            Some(("sftp", None, host.to_string(), path))
        }
        // webdav://host[/path]
        "webdav" => Some((
            "webdav",
            Some("custom-webdav".to_string()),
            rest.split('/').next().unwrap_or(rest).to_string(),
            None,
        )),
        // gcd / one / dropbox -> OAuth, handled as a skipped remote by caller
        _ => None,
    }
}

// ============ Public surface ============

/// A Duplicacy storage entry that could not be turned into a profile.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicacySkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing a `.duplicacy/preferences` file.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicacyImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<DuplicacySkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Default config path. Duplicacy has NO global config: preferences live at
/// `<working-dir>/.duplicacy/preferences`, one per repository. We return
/// `None` so the caller knows it must pass the repo's preferences path
/// explicitly, EXCEPT when `DUPLICACY_REPOSITORY` points at a working dir,
/// in which case we join `.duplicacy/preferences` (returned only if it
/// exists).
pub fn default_duplicacy_config_path() -> Option<PathBuf> {
    if let Ok(repo) = std::env::var("DUPLICACY_REPOSITORY") {
        if !repo.is_empty() {
            let p = PathBuf::from(repo).join(".duplicacy").join("preferences");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Resolve a secret for `key` of storage `name`: prefer the in-file `keys`
/// map (plaintext), fall back to the injected env lookup under
/// `DUPLICACY_<NAME>_<KEY>` (both uppercased).
fn resolve_secret(
    name: &str,
    keys: &HashMap<String, String>,
    key: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(v) = keys.get(key) {
        if !v.is_empty() {
            return Some(v.clone());
        }
    }
    let env_key = format!(
        "DUPLICACY_{}_{}",
        name.to_uppercase(),
        key.to_uppercase()
    );
    env(&env_key).filter(|v| !v.is_empty())
}

/// Import every bridgeable storage from a `.duplicacy/preferences` file.
///
/// Uses `std::env::var` for the `DUPLICACY_<NAME>_<KEY>` fallback. For
/// deterministic tests use [`import_duplicacy_with_env`].
pub fn import_duplicacy(path: &Path) -> Result<DuplicacyImportResult, String> {
    import_duplicacy_with_env(path, &|k| std::env::var(k).ok())
}

/// Import with an injectable env lookup (testability).
pub fn import_duplicacy_with_env(
    path: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<DuplicacyImportResult, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read preferences: {e}"))?;
    let list: Vec<DupStorage> = serde_json::from_str(&raw)
        .map_err(|e| format!("parse preferences: {e}"))?;

    let total_remotes = list.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for st in list {
        let parsed = parse_storage_url(&st.storage);
        let Some((proto, pid, host, path_opt)) = parsed else {
            // Unknown scheme: OAuth providers (gcd/one/dropbox) get the
            // OAuth reason; anything else is reported as unsupported.
            let scheme = st
                .storage
                .split_once("://")
                .map(|(s, _)| s.to_string())
                .unwrap_or_else(|| st.storage.clone());
            let reason = match scheme.as_str() {
                "gcd" | "one" | "dropbox" => {
                    "OAuth: provider-issued token, manual re-auth".to_string()
                }
                other => format!("unsupported duplicacy storage scheme: {other}"),
            };
            skipped.push(DuplicacySkippedRemote {
                name: st.name.clone(),
                kind: scheme,
                reason,
            });
            continue;
        };

        // Resolve username + credential per backend, secret policy aware.
        let (username, credential, mut extra_opts): BackendSecret = match (proto, pid.as_deref()) {
            ("s3", Some("backblaze-b2")) => (
                resolve_secret(&st.name, &st.keys, "b2_id", env)
                    .unwrap_or_default(),
                resolve_secret(&st.name, &st.keys, "b2_key", env),
                Vec::new(),
            ),
            ("s3", _) => (
                resolve_secret(&st.name, &st.keys, "s3_id", env)
                    .unwrap_or_default(),
                resolve_secret(&st.name, &st.keys, "s3_secret", env),
                Vec::new(),
            ),
            ("sftp", _) => {
                // ssh_key_file referenced -> store path only, no key bytes,
                // no credential. Otherwise ssh_password.
                if let Some(keyfile) =
                    resolve_secret(&st.name, &st.keys, "ssh_key_file", env)
                {
                    (
                        String::new(),
                        None,
                        vec![("private_key_path", Some(keyfile))],
                    )
                } else {
                    (
                        String::new(),
                        resolve_secret(&st.name, &st.keys, "ssh_password", env),
                        Vec::new(),
                    )
                }
            }
            // webdav and any other resolved protocol use a generic password.
            _ => (
                String::new(),
                resolve_secret(&st.name, &st.keys, "password", env),
                Vec::new(),
            ),
        };

        // Secret-policy bookkeeping: a missing credential means either an
        // SSH key file is referenced (path only, no bytes) or the secret
        // lives in the runtime env. Both leave the credential unset, so the
        // profile is flagged as not carrying a stored credential.
        let key_file_referenced =
            extra_opts.iter().any(|(k, _)| *k == "private_key_path");
        let has_stored_credential =
            if credential.is_none() { Some(false) } else { None };
        if credential.is_none() && !key_file_referenced {
            tracing::warn!(
                "[duplicacy import] '{}' ({}): no in-file/env secret, \
                 credential left empty (secret in env at runtime)",
                st.name,
                st.storage
            );
        }

        // options: repo awareness (policy v1: data only, no logic).
        let mut opt_pairs: Vec<(&str, Option<String>)> = vec![
            ("repo_kind", Some("duplicacy".to_string())),
            ("repo_id", Some(st.name.clone())),
        ];
        opt_pairs.append(&mut extra_opts);
        let options =
            serde_json::Value::Object(crate::bridge_shared::json_map(&opt_pairs));

        let id = format!(
            "duplicacy-{}-{}",
            st.name.to_lowercase().replace(' ', "-"),
            &crate::bridge_shared::uuid_v4()[..8]
        );

        servers.push(ServerProfileExport {
            id,
            name: format!("Duplicacy {}", st.name),
            host,
            port: crate::bridge_shared::default_port_for(proto),
            username,
            protocol: Some(proto.to_string()),
            initial_path: path_opt,
            local_initial_path: None,
            color: None,
            last_connected: None,
            options: Some(options),
            provider_id: pid,
            credential,
            has_stored_credential,
            public_url_base: None,
        });
    }

    Ok(DuplicacyImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export ============

/// A server profile to export as a Duplicacy storage entry.
///
/// Mirrors `rclone_import::RcloneExportServer`. The secret is fetched from
/// the vault separately and passed via `passwords` keyed by `name`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicacyExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export profiles to a `.duplicacy/preferences` JSON array.
///
/// Duplicacy's preferences IS an array, so every exportable profile becomes
/// one entry. Unsupported protocols (OAuth backends, anything without a
/// Duplicacy storage URL form) are skipped without aborting the batch. The
/// file is written `0600` via `atomic_write_600` because the `keys` block
/// carries plaintext secrets. Returns the number of entries written.
pub fn export_duplicacy(
    servers: &[DuplicacyExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut entries: Vec<serde_json::Value> = Vec::new();

    for server in servers {
        let secret = passwords.get(&server.name).map(|s| s.as_str());
        let path = server.initial_path.clone().unwrap_or_default();

        let (url, keys) =
            match (server.protocol.as_deref(), server.provider_id.as_deref()) {
                (Some("s3"), Some("backblaze-b2")) => (
                    format!("b2://{path}"),
                    serde_json::json!({
                        "b2_id": server.username,
                        "b2_key": secret.unwrap_or(""),
                    }),
                ),
                (Some("s3"), _) => (
                    format!("s3://{}/{}", server.host, path),
                    serde_json::json!({
                        "s3_id": server.username,
                        "s3_secret": secret.unwrap_or(""),
                    }),
                ),
                (Some("sftp"), _) => {
                    // Preserve a key-file reference if the profile carried
                    // one; otherwise emit ssh_password.
                    let key_path = server
                        .options
                        .as_ref()
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get("private_key_path"))
                        .and_then(|v| v.as_str());
                    let keys = if let Some(kp) = key_path {
                        serde_json::json!({ "ssh_key_file": kp })
                    } else {
                        serde_json::json!({ "ssh_password": secret.unwrap_or("") })
                    };
                    (
                        format!("sftp://{}@{}{}", server.username, server.host, path),
                        keys,
                    )
                }
                (Some("webdav"), _) => (
                    format!("webdav://{}{}", server.host, path),
                    serde_json::json!({ "password": secret.unwrap_or("") }),
                ),
                // OAuth backends and anything else have no Duplicacy storage
                // URL form: skip without aborting the batch.
                _ => continue,
            };

        entries.push(serde_json::json!({
            "name": "default",
            "id": server.name,
            "repository": "",
            "storage": url,
            "encrypted": true,
            "keys": keys,
        }));
    }

    let body = serde_json::to_vec_pretty(&serde_json::Value::Array(entries.clone()))
        .map_err(|e| format!("serialize preferences: {e}"))?;
    crate::bridge_shared::atomic_write_600(out, &body)?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      { "name":"b2store", "id":"snap-b2", "repository":"", "encrypted":true,
        "storage":"b2://my-bucket",
        "keys":{ "b2_id":"002abc", "b2_key":"K0secret", "password":"repopw" } },
      { "name":"s3store", "id":"snap-s3", "repository":"", "encrypted":true,
        "storage":"s3://s3.wasabisys.com/backups/host1",
        "keys":{ "s3_id":"AKIA1", "s3_secret":"S3SEC", "password":"repopw2" } },
      { "name":"sshstore", "id":"snap-ssh", "repository":"", "encrypted":true,
        "storage":"sftp://deploy@nas.example.com/srv/backup",
        "keys":{ "ssh_password":"sshpw" } },
      { "name":"gdrive", "id":"snap-g", "repository":"", "encrypted":true,
        "storage":"gcd://my-gdrive",
        "keys":{ } }
    ]"#;

    fn write_tmp(content: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("dup-test-{}", crate::bridge_shared::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("preferences");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn parses_b2_s3_sftp_and_skips_oauth() {
        let p = write_tmp(FIXTURE);
        let r = import_duplicacy_with_env(&p, &|_| None).unwrap();
        assert_eq!(r.total_remotes, 4);
        assert_eq!(r.servers.len(), 3);
        assert_eq!(r.skipped.len(), 1);

        let b2 = r.servers.iter().find(|s| s.name == "Duplicacy b2store").unwrap();
        assert_eq!(b2.protocol.as_deref(), Some("s3"));
        assert_eq!(b2.provider_id.as_deref(), Some("backblaze-b2"));
        assert_eq!(b2.host, "");
        assert_eq!(b2.initial_path.as_deref(), Some("my-bucket"));
        assert_eq!(b2.username, "002abc");
        assert_eq!(b2.credential.as_deref(), Some("K0secret"));
        assert_eq!(b2.port, 443);

        let s3 = r.servers.iter().find(|s| s.name == "Duplicacy s3store").unwrap();
        assert_eq!(s3.provider_id.as_deref(), Some("wasabi"));
        assert_eq!(s3.host, "s3.wasabisys.com");
        assert_eq!(s3.initial_path.as_deref(), Some("backups/host1"));
        assert_eq!(s3.username, "AKIA1");
        assert_eq!(s3.credential.as_deref(), Some("S3SEC"));

        let ssh = r.servers.iter().find(|s| s.name == "Duplicacy sshstore").unwrap();
        assert_eq!(ssh.protocol.as_deref(), Some("sftp"));
        assert_eq!(ssh.host, "nas.example.com");
        assert_eq!(ssh.initial_path.as_deref(), Some("/srv/backup"));
        assert_eq!(ssh.credential.as_deref(), Some("sshpw"));
        assert_eq!(ssh.port, 22);

        let sk = &r.skipped[0];
        assert_eq!(sk.name, "gdrive");
        assert_eq!(sk.kind, "gcd");
        assert_eq!(sk.reason, "OAuth: provider-issued token, manual re-auth");

        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn tolerates_explicit_null_keys_from_real_duplicacy() {
        // Verbatim shape real Duplicacy writes for a local-backend repo and
        // for any storage with no saved password: `"keys": null`. Before the
        // de_null_or_map fix this aborted the entire parse with
        // "invalid type: null, expected a map".
        let real = r#"[
          { "name":"default", "id":"snapid", "repository":"",
            "storage":"/tmp/dup-local-store", "encrypted":false,
            "no_backup":false, "keys":null, "filters":"" },
          { "name":"s3null", "id":"snap2", "repository":"",
            "storage":"s3://s3.wasabisys.com/bk", "encrypted":true,
            "keys":null }
        ]"#;
        let p = write_tmp(real);
        let r = import_duplicacy_with_env(&p, &|k| match k {
            "DUPLICACY_S3NULL_S3_ID" => Some("AKIAENV".into()),
            "DUPLICACY_S3NULL_S3_SECRET" => Some("ENVSEC".into()),
            _ => None,
        })
        .expect("null keys must not abort the parse");
        assert_eq!(r.total_remotes, 2);
        // local path -> skipped (not a server), s3 -> imported via env creds
        let s3 = r
            .servers
            .iter()
            .find(|s| s.name == "Duplicacy s3null")
            .expect("s3 entry imported despite keys:null");
        assert_eq!(s3.username, "AKIAENV");
        assert_eq!(s3.credential.as_deref(), Some("ENVSEC"));
        assert!(r.skipped.iter().any(|s| s.name == "default"));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn env_fallback_supplies_missing_secret() {
        // keys empty -> the b2_key must come from the injected env closure.
        let fixture = r#"[
          { "name":"default", "id":"s", "repository":"", "encrypted":true,
            "storage":"b2://bkt", "keys":{ } }
        ]"#;
        let p = write_tmp(fixture);
        let env = |k: &str| match k {
            "DUPLICACY_DEFAULT_B2_ID" => Some("envid".to_string()),
            "DUPLICACY_DEFAULT_B2_KEY" => Some("envkey".to_string()),
            _ => None,
        };
        let r = import_duplicacy_with_env(&p, &env).unwrap();
        assert_eq!(r.servers.len(), 1);
        let s = &r.servers[0];
        assert_eq!(s.username, "envid");
        assert_eq!(s.credential.as_deref(), Some("envkey"));
        assert!(s.has_stored_credential.is_none());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn secret_policy_env_only_leaves_credential_none() {
        // No keys, no env -> credential None + has_stored_credential Some(false).
        let fixture = r#"[
          { "name":"default", "id":"s", "repository":"", "encrypted":true,
            "storage":"b2://bkt", "keys":{ } }
        ]"#;
        let p = write_tmp(fixture);
        let r = import_duplicacy_with_env(&p, &|_| None).unwrap();
        let s = &r.servers[0];
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn round_trip_import_export_import_idempotent() {
        let p = write_tmp(FIXTURE);
        let r1 = import_duplicacy_with_env(&p, &|_| None).unwrap();

        // import -> export
        let mut passwords = HashMap::new();
        let export_servers: Vec<DuplicacyExportServer> = r1
            .servers
            .iter()
            .map(|s| {
                if let Some(c) = &s.credential {
                    passwords.insert(s.name.clone(), c.clone());
                }
                DuplicacyExportServer {
                    name: s.name.clone(),
                    host: s.host.clone(),
                    port: s.port,
                    username: s.username.clone(),
                    protocol: s.protocol.clone(),
                    options: s.options.clone(),
                    provider_id: s.provider_id.clone(),
                    initial_path: s.initial_path.clone(),
                }
            })
            .collect();

        let out_dir = std::env::temp_dir()
            .join(format!("dup-rt-{}", crate::bridge_shared::uuid_v4()));
        std::fs::create_dir_all(&out_dir).unwrap();
        let out = out_dir.join("preferences");
        let n = export_duplicacy(&export_servers, &passwords, &out).unwrap();
        assert_eq!(n, 3); // b2 + s3 + sftp, oauth was already skipped

        // export -> import again, assert metadata idempotence (b2 + s3)
        let r2 = import_duplicacy_with_env(&out, &|_| None).unwrap();
        let pick = |res: &DuplicacyImportResult, pid: &str| {
            res.servers
                .iter()
                .find(|s| s.provider_id.as_deref() == Some(pid))
                .map(|s| {
                    (
                        s.protocol.clone(),
                        s.host.clone(),
                        s.username.clone(),
                        s.initial_path.clone(),
                        s.credential.clone(),
                    )
                })
                .unwrap()
        };
        assert_eq!(pick(&r1, "backblaze-b2"), pick(&r2, "backblaze-b2"));
        assert_eq!(pick(&r1, "wasabi"), pick(&r2, "wasabi"));

        let _ = std::fs::remove_dir_all(p.parent().unwrap());
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn default_path_none_without_env() {
        std::env::remove_var("DUPLICACY_REPOSITORY");
        assert!(default_duplicacy_config_path().is_none());
    }

    #[test]
    fn default_path_env_override() {
        // Build a working dir with .duplicacy/preferences and point the env
        // var at it; default path must resolve to that file, then None when
        // the file is absent.
        let work = std::env::temp_dir()
            .join(format!("dup-env-{}", crate::bridge_shared::uuid_v4()));
        let dup = work.join(".duplicacy");
        std::fs::create_dir_all(&dup).unwrap();
        let pref = dup.join("preferences");
        std::fs::write(&pref, "[]").unwrap();

        std::env::set_var("DUPLICACY_REPOSITORY", &work);
        let resolved = default_duplicacy_config_path();
        std::env::remove_var("DUPLICACY_REPOSITORY");
        assert_eq!(resolved.as_deref(), Some(pref.as_path()));

        let _ = std::fs::remove_dir_all(&work);
    }
}
