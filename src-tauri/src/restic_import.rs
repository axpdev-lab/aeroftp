// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import / export a Restic backup-repo "profile".
//!
//! ## The honest note: Restic has NO config file (by design)
//!
//! Unlike rclone, Kopia, or Duplicacy, Restic deliberately does not persist a
//! configuration file anywhere on disk. The repository is a single URL string,
//! supplied at invocation time through one of three runtime sources:
//!
//! 1. the `RESTIC_REPOSITORY` environment variable,
//! 2. the `RESTIC_REPOSITORY_FILE` environment variable (a file whose first
//!    line is that URL), or
//! 3. the `-r` / `--repo <url>` command-line argument.
//!
//! Likewise, repository backend credentials live exclusively in environment
//! variables at runtime (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`,
//! `B2_ACCOUNT_ID` / `B2_ACCOUNT_KEY`, ...). There is nothing to "scan".
//!
//! Therefore the bridge for Restic is intentionally different from every other
//! source: **`default_restic_config_path()` always returns `None`** and there
//! is no `import_restic(path)` that parses a tool config. Instead the bridge is
//! "URL + env -> profile":
//!
//! - `parse_restic_repo(url)` decomposes the repo URL into a transport,
//! - `import_restic_repo_url(url, env)` enriches it with env-injected creds
//!   (the `env` closure is injected so tests do not touch the real process
//!   environment),
//! - `import_restic(path)` interprets `path` as a `RESTIC_REPOSITORY_FILE`
//!   (reads the first line as the URL) and resolves creds via the real
//!   `std::env::var`, satisfying the common contract surface,
//! - `import_restic_from_env(env)` resolves the URL from
//!   `RESTIC_REPOSITORY` / `RESTIC_REPOSITORY_FILE`.
//!
//! Export is symmetric: Restic's *native* "config" form is a sourceable shell
//! env script. `export_restic` emits exactly that (`# source this file`,
//! `export RESTIC_REPOSITORY='...'`, `export AWS_ACCESS_KEY_ID='...'`, ...),
//! written `0600` because it carries secrets. Restic connects to ONE repo, so
//! the export targets the first profile (and errors if the list is empty).
//!
//! ## Secret policy (enforced)
//!
//! | source of the secret                      | what we do                                            |
//! |-------------------------------------------|-------------------------------------------------------|
//! | backend creds present in injected env     | populate `credential`                                 |
//! | backend creds absent (runtime-only env)   | `credential = None`, `has_stored_credential=Some(false)`, skipped reason "restic creds are runtime env, re-enter" |
//! | sftp (key / agent auth)                    | `credential = None` (no key bytes are ever copied)    |
//! | `rclone:` backend                          | not bridged here: skipped, "import via `aeroftp import rclone`" |
//! | local path / `swift:` / `azure:` / `gs:`   | skipped, "<scheme> backend not bridgeable / local path" |
//!
//! Imported credentials, when present, are stored in our AES-256-GCM vault,
//! upgrading from Restic's plaintext runtime env to authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Repo URL parser ============

/// Decompose a Restic repository URL into a transport tuple.
///
/// Returns `(protocol, provider_id, host, port, initial_path)`:
/// - `s3:s3.amazonaws.com/bucket[/path]` or `s3:https://endpoint/bucket`
///   -> `("s3", map_s3_provider_from_endpoint(host), host, 443, path)`
/// - `b2:bucket:path` -> `("s3", "backblaze-b2", "", 443, Some(bucket))`
/// - `sftp:user@host:/path` -> `("sftp", None, host, 22, path)` (user stripped)
/// - `rest:https://host/` -> `("webdav", "custom-webdav", host, 443, None)`
/// - `rclone:remote:path` -> `None` (caller chains into the rclone importer)
/// - local path / `swift:` / `azure:` / `gs:` -> `None` (not bridgeable)
/// Decomposed restic repo: `(protocol, provider_id, host, port, initial_path)`.
pub type ResticRepoTarget = (&'static str, Option<String>, String, u32, Option<String>);

pub fn parse_restic_repo(url: &str) -> Option<ResticRepoTarget> {
    let url = url.trim();

    // ---- S3 (and S3-compatible endpoints) ----
    if let Some(r) = url.strip_prefix("s3:") {
        // `s3:https://endpoint/bucket` and `s3:http://...` both legal.
        let r = r
            .strip_prefix("https://")
            .or_else(|| r.strip_prefix("http://"))
            .unwrap_or(r);
        let (host, path) = r.split_once('/').unwrap_or((r, ""));
        if host.is_empty() {
            return None;
        }
        let provider_id = crate::bridge_shared::map_s3_provider_from_endpoint(host).to_string();
        let initial_path = (!path.is_empty()).then(|| path.to_string());
        return Some(("s3", Some(provider_id), host.to_string(), 443, initial_path));
    }

    // ---- Backblaze B2 native (`b2:bucket:path`) ----
    if let Some(r) = url.strip_prefix("b2:") {
        let bucket = r.split(':').next().unwrap_or(r);
        if bucket.is_empty() {
            return None;
        }
        return Some((
            "s3",
            Some("backblaze-b2".to_string()),
            String::new(),
            443,
            Some(bucket.to_string()),
        ));
    }

    // ---- SFTP (`sftp:user@host:/path`) ----
    if let Some(r) = url.strip_prefix("sftp:") {
        // Host segment is everything up to the first ':' that introduces the
        // remote path. `user@` (if present) is stripped: AeroFTP carries the
        // SSH user separately.
        let host_part = r.split(':').next().unwrap_or(r);
        let host = host_part.rsplit('@').next().unwrap_or(host_part);
        if host.is_empty() {
            return None;
        }
        let path = r
            .split_once(':')
            .and_then(|(_, p)| (!p.is_empty()).then(|| p.to_string()));
        return Some(("sftp", None, host.to_string(), 22, path));
    }

    // ---- Restic REST server (`rest:https://host/`) -> WebDAV transport ----
    if let Some(r) = url.strip_prefix("rest:") {
        let h = r
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        if h.is_empty() {
            return None;
        }
        return Some((
            "webdav",
            Some("custom-webdav".to_string()),
            h.to_string(),
            443,
            None,
        ));
    }

    // ---- rclone backend: defer to the rclone importer ----
    if url.strip_prefix("rclone:").is_some() {
        // Caller note: the backend is defined in rclone.conf, not here. v1
        // emits a skipped entry telling the user to run the rclone importer.
        return None;
    }

    // local path / `swift:` / `azure:` / `gs:` -> not bridgeable here.
    None
}

/// Classify a non-bridgeable URL for a precise skipped-reason message.
fn skip_reason_for(url: &str) -> String {
    let url = url.trim();
    if url.strip_prefix("rclone:").is_some() {
        return "rclone: backend defined in rclone.conf, import via `aeroftp import rclone`"
            .to_string();
    }
    for scheme in ["swift", "azure", "gs"] {
        if url.strip_prefix(&format!("{scheme}:")).is_some() {
            return format!("{scheme} backend not bridgeable / local path");
        }
    }
    "local path / unsupported backend not bridgeable / local path".to_string()
}

// ============ Public result types ============

/// A Restic repo that could not be bridged (non-bridgeable backend, or creds
/// only available as runtime env).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResticSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of bridging a Restic repository URL.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResticImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<ResticSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

// ============ Default config path: NONE by design ============

/// Restic has no config file. This ALWAYS returns `None`.
///
/// Kept for the common bridge contract surface; the CLI must resolve the repo
/// URL from `--repo`, `RESTIC_REPOSITORY`, or `RESTIC_REPOSITORY_FILE` instead
/// of a per-OS default path. Documented in the module header.
pub fn default_restic_config_path() -> Option<PathBuf> {
    None
}

// ============ Core: URL + injected env -> profile ============

/// Bridge a Restic repository URL into a profile, drawing backend credentials
/// from the injected `env` closure.
///
/// The closure is injected (instead of reading `std::env`) purely for
/// testability: the CLI passes `|k| std::env::var(k).ok()`.
///
/// Credential resolution by transport:
/// - s3  -> username = `AWS_ACCESS_KEY_ID`, credential = `AWS_SECRET_ACCESS_KEY`
/// - b2  -> username = `B2_ACCOUNT_ID`,     credential = `B2_ACCOUNT_KEY`
/// - sftp -> credential `None` (key / ssh-agent auth, never copied)
///
/// When the relevant creds are absent from `env`, the profile is still emitted
/// (the backend is reusable) but `credential = None`,
/// `has_stored_credential = Some(false)`, and a skipped note records that the
/// secret must be re-entered.
pub fn import_restic_repo_url(
    url: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResticImportResult, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("empty Restic repository URL".to_string());
    }

    let source_path = url.to_string();

    let Some((protocol, provider_id, host, port, initial_path)) = parse_restic_repo(url) else {
        // Not bridgeable: report it as skipped, not as a hard error, so the
        // CLI can surface a precise message and still exit cleanly.
        return Ok(ResticImportResult {
            servers: Vec::new(),
            skipped: vec![ResticSkippedRemote {
                name: "restic-repo".to_string(),
                kind: "restic".to_string(),
                reason: skip_reason_for(url),
            }],
            source_path,
            total_remotes: 1,
        });
    };

    // Resolve credentials from the injected env per transport.
    let (username, credential): (String, Option<String>) = match protocol {
        "s3" if provider_id.as_deref() == Some("backblaze-b2") => (
            env("B2_ACCOUNT_ID").unwrap_or_default(),
            env("B2_ACCOUNT_KEY").filter(|v| !v.is_empty()),
        ),
        "s3" => (
            env("AWS_ACCESS_KEY_ID").unwrap_or_default(),
            env("AWS_SECRET_ACCESS_KEY").filter(|v| !v.is_empty()),
        ),
        // SFTP: key / ssh-agent auth. We never copy key bytes; credential
        // stays None regardless of the environment.
        "sftp" => (String::new(), None),
        _ => (String::new(), None),
    };

    // Honest secret note: when the backend would need a secret but the env
    // did not provide one, record that it must be re-entered.
    let mut skipped = Vec::new();
    let needs_secret = matches!(protocol, "s3");
    let has_stored_credential = if credential.is_some() {
        None
    } else if needs_secret {
        skipped.push(ResticSkippedRemote {
            name: "restic-repo".to_string(),
            kind: "restic".to_string(),
            reason: "restic creds are runtime env, re-enter".to_string(),
        });
        tracing::info!(
            "[restic import] '{}' backend creds absent from env: restic creds are runtime env, re-enter",
            url
        );
        Some(false)
    } else {
        // sftp / non-secret transports: legitimately no stored credential.
        Some(false)
    };

    // options.repo_kind = "restic" (Policy v1: only the marker, no logic).
    let mut opts = crate::bridge_shared::json_map(&[
        ("repo_kind", Some("restic".to_string())),
        ("repo_url", Some(url.to_string())),
    ]);
    if protocol == "s3" {
        if let Some(bucket) = initial_path.clone() {
            // Mirror the rclone/kopia S3 shape: the bucket lives in options.
            opts.insert(
                "bucket".to_string(),
                serde_json::Value::String(bucket),
            );
        }
    }

    let id = format!("restic-{}", &crate::bridge_shared::uuid_v4()[..8]);
    let server = ServerProfileExport {
        id,
        name: format!("Restic ({protocol})"),
        host,
        port,
        username,
        protocol: Some(protocol.to_string()),
        initial_path,
        local_initial_path: None,
        color: None,
        last_connected: None,
        options: Some(serde_json::Value::Object(opts)),
        provider_id,
        credential,
        has_stored_credential,
        public_url_base: None,
    };

    Ok(ResticImportResult {
        servers: vec![server],
        skipped,
        source_path,
        total_remotes: 1,
    })
}

/// Read the repository URL from `RESTIC_REPOSITORY` or, failing that, from the
/// file named by `RESTIC_REPOSITORY_FILE` (first line), then bridge it.
pub fn import_restic_from_env(
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResticImportResult, String> {
    if let Some(url) = env("RESTIC_REPOSITORY").filter(|v| !v.trim().is_empty()) {
        return import_restic_repo_url(url.trim(), env);
    }
    if let Some(file) = env("RESTIC_REPOSITORY_FILE").filter(|v| !v.trim().is_empty()) {
        return import_restic(Path::new(file.trim()));
    }
    Err("no Restic repository: set RESTIC_REPOSITORY, RESTIC_REPOSITORY_FILE, or pass --repo"
        .to_string())
}

/// Common contract entrypoint: interpret `path` as a `RESTIC_REPOSITORY_FILE`
/// (read the first non-empty line as the repo URL) and resolve credentials
/// from the real process environment.
pub fn import_restic(path: &Path) -> Result<ResticImportResult, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read RESTIC_REPOSITORY_FILE: {e}"))?;
    let url = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| "RESTIC_REPOSITORY_FILE is empty".to_string())?;

    let real_env = |k: &str| std::env::var(k).ok();
    let mut result = import_restic_repo_url(url, &real_env)?;
    // Keep the source pointing at the file, not the URL, for this entrypoint.
    result.source_path = path.display().to_string();
    Ok(result)
}

// ============ Export: Restic's native sourceable env script ============

/// A profile to export as a Restic env script.
///
/// Mirrors `rclone_import::RcloneExportServer`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResticExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Quote a value for safe POSIX `source`-ing inside single quotes.
///
/// Single-quote literals cannot contain a single quote, so the standard shell
/// idiom closes the quote, inserts an escaped quote, and reopens:
/// `'` becomes `'\''`. The result is always safe to `source`.
fn sh_squote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Export the FIRST profile as a sourceable Restic env script.
///
/// Restic connects to exactly ONE repository, so a multi-profile export makes
/// no sense: this exports `servers[0]` and errors if the slice is empty. The
/// emitted file is Restic's native "config" form (a shell script you `source`),
/// written `0600` because it carries secrets.
///
/// Returns the number of profiles written (always `0` on error, `1` on
/// success) for parity with the other `export_*` signatures.
pub fn export_restic(
    servers: &[ResticExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let p = servers
        .first()
        .ok_or_else(|| "restic export: no profile to export (need at least one)".to_string())?;

    let secret = passwords.get(&p.name).map(|s| s.as_str()).unwrap_or("");

    let opts = p.options.as_ref().and_then(|v| v.as_object());
    let bucket = opts
        .and_then(|m| m.get("bucket"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| p.initial_path.clone())
        .unwrap_or_default();

    // (RESTIC_REPOSITORY value, cred env var #1, value, env var #2, value)
    let (repo, k1, v1, k2, v2): (String, &str, String, &str, String) =
        match (p.protocol.as_deref(), p.provider_id.as_deref()) {
            (Some("s3"), Some("backblaze-b2")) => (
                format!("b2:{bucket}"),
                "B2_ACCOUNT_ID",
                p.username.clone(),
                "B2_ACCOUNT_KEY",
                secret.to_string(),
            ),
            (Some("s3"), _) => {
                // `s3:<endpoint>/<bucket-or-path>` (drop trailing slash on the
                // host so the URL never doubles up the separator).
                let host = p.host.trim_end_matches('/');
                let repo = if bucket.is_empty() {
                    format!("s3:{host}")
                } else {
                    format!("s3:{host}/{}", bucket.trim_start_matches('/'))
                };
                (
                    repo,
                    "AWS_ACCESS_KEY_ID",
                    p.username.clone(),
                    "AWS_SECRET_ACCESS_KEY",
                    secret.to_string(),
                )
            }
            (Some("sftp"), _) => {
                // sftp:user@host:/path (key / agent auth: no secret env).
                let path = p.initial_path.clone().unwrap_or_default();
                (
                    format!("sftp:{}@{}:{}", p.username, p.host, path),
                    "",
                    String::new(),
                    "",
                    String::new(),
                )
            }
            (Some("webdav"), _) => {
                // Restic's REST server transport.
                let host = p.host.trim_end_matches('/');
                let host = host
                    .trim_start_matches("https://")
                    .trim_start_matches("http://");
                (
                    format!("rest:https://{host}/"),
                    "",
                    String::new(),
                    "",
                    String::new(),
                )
            }
            (other, _) => {
                return Err(format!(
                    "restic export: protocol {other:?} is not a Restic backend"
                ))
            }
        };

    let mut s = String::from("# Restic env, source this file\n");
    s.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    s.push_str(&format!(
        "export RESTIC_REPOSITORY={}\n",
        sh_squote(&repo)
    ));
    if !k1.is_empty() {
        s.push_str(&format!("export {k1}={}\n", sh_squote(&v1)));
        s.push_str(&format!("export {k2}={}\n", sh_squote(&v2)));
    }

    crate::bridge_shared::atomic_write_600(out, s.as_bytes())?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_restic_repo: every documented scheme ----

    #[test]
    fn parse_s3_bare_endpoint_form() {
        let (proto, pid, host, port, path) =
            parse_restic_repo("s3:s3.amazonaws.com/my-bucket/restic").expect("s3 bare");
        assert_eq!(proto, "s3");
        assert_eq!(pid.as_deref(), Some("amazon-s3"));
        assert_eq!(host, "s3.amazonaws.com");
        assert_eq!(port, 443);
        assert_eq!(path.as_deref(), Some("my-bucket/restic"));
    }

    #[test]
    fn parse_s3_https_endpoint_form() {
        let (proto, pid, host, port, path) =
            parse_restic_repo("s3:https://s3.wasabisys.com/backups").expect("s3 https");
        assert_eq!(proto, "s3");
        assert_eq!(pid.as_deref(), Some("wasabi"));
        assert_eq!(host, "s3.wasabisys.com");
        assert_eq!(port, 443);
        assert_eq!(path.as_deref(), Some("backups"));
    }

    #[test]
    fn parse_b2_form() {
        let (proto, pid, host, port, path) =
            parse_restic_repo("b2:my-bucket:path/to/repo").expect("b2");
        assert_eq!(proto, "s3");
        assert_eq!(pid.as_deref(), Some("backblaze-b2"));
        assert_eq!(host, "");
        assert_eq!(port, 443);
        assert_eq!(path.as_deref(), Some("my-bucket"));
    }

    #[test]
    fn parse_sftp_form_strips_user() {
        let (proto, pid, host, port, path) =
            parse_restic_repo("sftp:backup@nas.example.com:/srv/restic").expect("sftp");
        assert_eq!(proto, "sftp");
        assert!(pid.is_none());
        assert_eq!(host, "nas.example.com");
        assert_eq!(port, 22);
        assert_eq!(path.as_deref(), Some("/srv/restic"));
    }

    #[test]
    fn parse_rest_form() {
        let (proto, pid, host, port, path) =
            parse_restic_repo("rest:https://restic.example.com/").expect("rest");
        assert_eq!(proto, "webdav");
        assert_eq!(pid.as_deref(), Some("custom-webdav"));
        assert_eq!(host, "restic.example.com");
        assert_eq!(port, 443);
        assert!(path.is_none());
    }

    #[test]
    fn parse_rclone_is_none() {
        assert!(parse_restic_repo("rclone:myremote:path/to/repo").is_none());
    }

    #[test]
    fn parse_local_path_is_none() {
        assert!(parse_restic_repo("/var/backups/restic-repo").is_none());
        assert!(parse_restic_repo("/tmp/restic").is_none());
    }

    #[test]
    fn parse_swift_is_none() {
        assert!(parse_restic_repo("swift:my-container:/restic").is_none());
        assert!(parse_restic_repo("azure:container:/restic").is_none());
        assert!(parse_restic_repo("gs:bucket:/restic").is_none());
    }

    // ---- import_restic_repo_url: credential resolution ----

    #[test]
    fn import_s3_with_injected_aws_creds_populates_credential() {
        let env = |k: &str| match k {
            "AWS_ACCESS_KEY_ID" => Some("AKIAEXAMPLE".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("supersecret".to_string()),
            _ => None,
        };
        let r =
            import_restic_repo_url("s3:s3.amazonaws.com/bucket/restic", &env).expect("import");
        assert_eq!(r.servers.len(), 1);
        assert!(r.skipped.is_empty());
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("s3"));
        assert_eq!(s.username, "AKIAEXAMPLE");
        assert_eq!(s.credential.as_deref(), Some("supersecret"));
        assert!(s.has_stored_credential.is_none());
        // repo_kind marker present (Policy v1).
        let opts = s.options.as_ref().and_then(|v| v.as_object()).unwrap();
        assert_eq!(opts.get("repo_kind").and_then(|v| v.as_str()), Some("restic"));
    }

    #[test]
    fn import_s3_without_creds_skips_with_reason() {
        let env = |_: &str| None;
        let r = import_restic_repo_url("s3:s3.amazonaws.com/bucket", &env).expect("import");
        assert_eq!(r.servers.len(), 1, "backend still reusable");
        let s = &r.servers[0];
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].reason, "restic creds are runtime env, re-enter");
    }

    #[test]
    fn import_b2_with_injected_b2_creds() {
        let env = |k: &str| match k {
            "B2_ACCOUNT_ID" => Some("002b2id".to_string()),
            "B2_ACCOUNT_KEY" => Some("K002b2key".to_string()),
            _ => None,
        };
        let r = import_restic_repo_url("b2:my-bucket:path", &env).expect("import");
        let s = &r.servers[0];
        assert_eq!(s.provider_id.as_deref(), Some("backblaze-b2"));
        assert_eq!(s.username, "002b2id");
        assert_eq!(s.credential.as_deref(), Some("K002b2key"));
        assert_eq!(s.initial_path.as_deref(), Some("my-bucket"));
    }

    #[test]
    fn import_sftp_has_no_credential_no_secret_skip() {
        let env = |_: &str| None;
        let r =
            import_restic_repo_url("sftp:u@host.example.com:/srv/restic", &env).expect("import");
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("sftp"));
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        // sftp uses key / agent auth: NOT the runtime-env-creds skip reason.
        assert!(r
            .skipped
            .iter()
            .all(|x| x.reason != "restic creds are runtime env, re-enter"));
    }

    #[test]
    fn import_rclone_url_is_skipped_with_chain_hint() {
        let env = |_: &str| None;
        let r = import_restic_repo_url("rclone:myremote:bucket/restic", &env).expect("import");
        assert!(r.servers.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("aeroftp import rclone"));
    }

    #[test]
    fn import_empty_url_is_error() {
        let env = |_: &str| None;
        assert!(import_restic_repo_url("   ", &env).is_err());
    }

    // ---- round-trip: build -> export -> re-parse RESTIC_REPOSITORY ----

    #[test]
    fn round_trip_export_then_reparse_is_idempotent() {
        let servers = vec![ResticExportServer {
            name: "prod-s3".to_string(),
            host: "s3.wasabisys.com".to_string(),
            port: 443,
            username: "AKIAEXAMPLE".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "repo_kind": "restic",
                "bucket": "my-bucket/restic"
            })),
            provider_id: Some("wasabi".to_string()),
            initial_path: Some("my-bucket/restic".to_string()),
        }];
        let mut passwords = HashMap::new();
        passwords.insert("prod-s3".to_string(), "s3secret".to_string());

        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-restic-{}.env",
            &crate::bridge_shared::uuid_v4()[..8]
        ));
        let n = export_restic(&servers, &passwords, &tmp).expect("export");
        assert_eq!(n, 1);

        let script = std::fs::read_to_string(&tmp).expect("read script");
        std::fs::remove_file(&tmp).ok();

        // Extract the RESTIC_REPOSITORY value and re-parse it.
        let repo_line = script
            .lines()
            .find(|l| l.starts_with("export RESTIC_REPOSITORY="))
            .expect("RESTIC_REPOSITORY line");
        let raw = repo_line
            .trim_start_matches("export RESTIC_REPOSITORY=")
            .trim_matches('\'');
        let (proto, pid, host, port, _path) =
            parse_restic_repo(raw).expect("re-parse exported repo URL");
        assert_eq!(proto, "s3");
        assert_eq!(pid.as_deref(), Some("wasabi"));
        assert_eq!(host, "s3.wasabisys.com");
        assert_eq!(port, 443);

        // Credential env vars were emitted.
        assert!(script.contains("export AWS_ACCESS_KEY_ID='AKIAEXAMPLE'"));
        assert!(script.contains("export AWS_SECRET_ACCESS_KEY='s3secret'"));
    }

    #[test]
    fn export_empty_slice_is_error() {
        let tmp = std::env::temp_dir().join("aeroftp-test-restic-empty.env");
        let r = export_restic(&[], &HashMap::new(), &tmp);
        assert!(r.is_err());
    }

    // ---- shell escaping: single quote in a secret must stay sourceable ----

    #[test]
    fn export_escapes_single_quote_in_secret() {
        let servers = vec![ResticExportServer {
            name: "tricky".to_string(),
            host: "s3.amazonaws.com".to_string(),
            port: 443,
            username: "AKIA'INJECT".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "bucket": "b" })),
            provider_id: Some("amazon-s3".to_string()),
            initial_path: Some("b".to_string()),
        }];
        let mut passwords = HashMap::new();
        // A secret containing a single quote and a shell metacharacter.
        passwords.insert("tricky".to_string(), "se'cret; rm -rf /".to_string());

        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-restic-quote-{}.env",
            &crate::bridge_shared::uuid_v4()[..8]
        ));
        export_restic(&servers, &passwords, &tmp).expect("export");
        let script = std::fs::read_to_string(&tmp).expect("read");
        std::fs::remove_file(&tmp).ok();

        // The `'` is escaped as the POSIX idiom `'\''`, so the literal value
        // round-trips and nothing can break out of the quoting.
        assert!(
            script.contains(r#"export AWS_SECRET_ACCESS_KEY='se'\''cret; rm -rf /'"#),
            "secret single-quote not escaped safely:\n{script}"
        );
        assert!(
            script.contains(r#"export AWS_ACCESS_KEY_ID='AKIA'\''INJECT'"#),
            "username single-quote not escaped safely:\n{script}"
        );
        // Reconstruct what a POSIX shell would assign and assert it equals the
        // original (the escape is information-preserving, not lossy).
        let decode = |v: &str| v.replace(r"'\''", "'");
        assert_eq!(decode(r#"se'\''cret; rm -rf /"#), "se'cret; rm -rf /");
    }

    #[test]
    fn sh_squote_handles_quote() {
        assert_eq!(sh_squote("abc"), "'abc'");
        assert_eq!(sh_squote("a'b"), r#"'a'\''b'"#);
        assert_eq!(sh_squote(""), "''");
    }

    // ---- default config path is None by design ----

    #[test]
    fn default_config_path_is_always_none() {
        assert!(default_restic_config_path().is_none());
    }

    // ---- import_restic reads a RESTIC_REPOSITORY_FILE ----

    #[test]
    fn import_restic_reads_repository_file_first_line() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-restic-repofile-{}",
            &crate::bridge_shared::uuid_v4()[..8]
        ));
        std::fs::write(&tmp, "\n  rest:https://restic.example.com/  \n# comment\n")
            .expect("write repo file");
        let r = import_restic(&tmp).expect("import from file");
        std::fs::remove_file(&tmp).ok();
        assert_eq!(r.servers.len(), 1);
        assert_eq!(r.servers[0].protocol.as_deref(), Some("webdav"));
        assert_eq!(r.servers[0].host, "restic.example.com");
        assert_eq!(r.source_path, tmp.display().to_string());
    }

    #[test]
    fn import_from_env_prefers_repository_over_file() {
        let env = |k: &str| match k {
            "RESTIC_REPOSITORY" => Some("sftp:u@host:/srv/restic".to_string()),
            _ => None,
        };
        let r = import_restic_from_env(&env).expect("from env");
        assert_eq!(r.servers[0].protocol.as_deref(), Some("sftp"));
    }

    #[test]
    fn import_from_env_errors_when_nothing_set() {
        let env = |_: &str| None;
        assert!(import_restic_from_env(&env).is_err());
    }
}
