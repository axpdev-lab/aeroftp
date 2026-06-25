// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from Cyberduck `.duck` bookmarks (metadata only).
//!
//! Phase 1b of the profile bridge. Cyberduck stores each bookmark as an
//! Apple XML plist (`.duck`) holding host/port/user/protocol/path/region but
//! NEVER the password: the secret lives in the OS keychain (macOS Keychain,
//! Windows Credential Manager). The bridge therefore maps only the metadata
//! and the user re-enters the password after import. This is a lower
//! onboarding ROI than Phase 1a (the profile is not immediately usable), but
//! Cyberduck is the closest cloud userbase to AeroFTP and importing host,
//! protocol, region, bucket and path for dozens of bookmarks saves real work.
//!
//! Secret policy (mirror across every bridge module):
//!
//! | Source of secret              | Bridge policy                                  |
//! |-------------------------------|------------------------------------------------|
//! | plain/obfuscated in file      | populate `credential`                          |
//! | OS keychain (Cyberduck)       | `credential=None`, `has_stored_credential=Some(false)`, log "secret in OS keychain, re-enter" |
//! | OAuth token (gdrive/onedrive/dropbox) | profile NOT created; `skipped` reason "OAuth: provider-issued token, manual re-auth" |
//! | SSH key referenced            | `options.private_key_path`, no key bytes       |
//!
//! No `plist` crate: the bookmark plist is a regular subset, so the shared
//! `crate::bridge_shared::plist_field` scanner is used instead (repo policy:
//! no extra crate where a focused scanner suffices).

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Informational note attached to every Cyberduck import: the bookmark file
/// never carries the password, so the user must re-enter it. Surfaced in the
/// CLI/UI alongside the imported profile count.
pub const CYBERDUCK_SECRET_NOTE: &str =
    "Cyberduck secrets live in the OS keychain: re-enter passwords after import";

// ============ Public API shapes (mirror rclone_import.rs) ============

/// A bookmark that was skipped (unsupported protocol or OAuth stub).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyberduckSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing Cyberduck bookmarks.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyberduckImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<CyberduckSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// A server profile to export as a `.duck` bookmark (metadata only).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CyberduckExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

// ============ Mapping ============

struct MappedProfile {
    protocol: String,
    provider_id: Option<String>,
    host: String,
    port: u32,
    username: String,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
}

/// Outcome of mapping one `.duck` bookmark.
enum DuckOutcome {
    /// Metadata mapped: profile created (credential always None).
    Profile(MappedProfile),
    /// Bookmark recognised but not bridgeable: collect a skipped entry.
    Skip { kind: String, reason: String },
    /// Not a parseable bookmark (no Protocol/Hostname): ignore silently.
    NotABookmark,
}

/// Map a single `.duck` plist body to a profile or a skip reason.
///
/// Cyberduck `Protocol` values seen in the wild: `sftp`, `ftp`, `ftps`,
/// `s3`, `davs`, `dav`, `b2`, `azure`, `googledrive`, `onedrive`, `dropbox`.
fn map_duck(xml: &str) -> DuckOutcome {
    let field = |k: &str| crate::bridge_shared::plist_field(xml, k);

    let protocol_raw = match field("Protocol") {
        Some(p) if !p.is_empty() => p.to_lowercase(),
        _ => return DuckOutcome::NotABookmark,
    };

    let hostname = field("Hostname").unwrap_or_default();
    let username = field("Username").unwrap_or_default();
    let region = field("Region");
    let path = field("Path").filter(|s| !s.is_empty());
    let port_field = field("Port").and_then(|p| p.parse::<u32>().ok());

    match protocol_raw.as_str() {
        "sftp" => DuckOutcome::Profile(MappedProfile {
            protocol: "sftp".into(),
            provider_id: None,
            host: hostname,
            port: port_field.unwrap_or(22),
            username,
            options: None,
            initial_path: path,
        }),

        "ftp" => DuckOutcome::Profile(MappedProfile {
            protocol: "ftp".into(),
            provider_id: None,
            host: hostname,
            port: port_field.unwrap_or(21),
            username,
            options: None,
            initial_path: path,
        }),

        "ftps" | "ftp-ssl" | "ftpis" => DuckOutcome::Profile(MappedProfile {
            protocol: "ftps".into(),
            provider_id: None,
            host: hostname,
            port: port_field.unwrap_or(990),
            username,
            options: None,
            initial_path: path,
        }),

        // S3: provider inferred from endpoint host, falling back to region.
        "s3" => {
            let provider_id = {
                let by_host = crate::bridge_shared::map_s3_provider_from_endpoint(&hostname);
                if by_host != "custom-s3" {
                    by_host
                } else if let Some(r) = region.as_deref().filter(|s| !s.is_empty()) {
                    crate::bridge_shared::map_s3_provider(r)
                } else {
                    by_host
                }
            };
            let mut opts: Vec<(&str, Option<String>)> = vec![("region", region.clone())];
            // A Cyberduck S3 Path is bucket[/prefix]; expose the bucket too.
            if let Some(p) = path.as_deref() {
                let bucket = p.trim_start_matches('/').split('/').next().unwrap_or("");
                if !bucket.is_empty() {
                    opts.push(("bucket", Some(bucket.to_string())));
                }
            }
            let options = crate::bridge_shared::json_map(&opts);
            DuckOutcome::Profile(MappedProfile {
                protocol: "s3".into(),
                provider_id: Some(provider_id.to_string()),
                host: hostname,
                port: 443,
                username,
                options: if options.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(options))
                },
                initial_path: path,
            })
        }

        // Backblaze B2 native: route to the S3-style B2 providerId so the
        // profile lands on the same backend other bridge modules use.
        "b2" => DuckOutcome::Profile(MappedProfile {
            protocol: "s3".into(),
            provider_id: Some("backblaze-b2".into()),
            host: hostname,
            port: 443,
            username,
            options: None,
            initial_path: path,
        }),

        // davs = WebDAV over HTTPS (443), dav = WebDAV (defaults to 443,
        // 80 only if the bookmark explicitly pins it).
        "davs" => DuckOutcome::Profile(MappedProfile {
            protocol: "webdav".into(),
            provider_id: Some(crate::bridge_shared::map_webdav_vendor(&hostname).to_string()),
            host: hostname,
            port: port_field.unwrap_or(443),
            username,
            options: None,
            initial_path: path,
        }),

        "dav" => DuckOutcome::Profile(MappedProfile {
            protocol: "webdav".into(),
            provider_id: Some(crate::bridge_shared::map_webdav_vendor(&hostname).to_string()),
            host: hostname,
            port: port_field.unwrap_or(443),
            username,
            options: None,
            initial_path: path,
        }),

        // Azure needs a SAS token / account key that the bookmark never
        // carries (Credential Manager only): cannot stub a usable profile.
        "azure" => DuckOutcome::Skip {
            kind: "azure".into(),
            reason: "Azure SAS not in bookmark".into(),
        },

        // OAuth providers: provider-issued token, not bridgeable.
        "googledrive" | "google drive" | "onedrive" | "dropbox" => DuckOutcome::Skip {
            kind: protocol_raw.clone(),
            reason: "OAuth: provider-issued token, manual re-auth".into(),
        },

        other => DuckOutcome::Skip {
            kind: other.to_string(),
            reason: format!("unsupported Cyberduck protocol: {other}"),
        },
    }
}

/// Derive a human-readable bookmark name: `Nickname` if present, else the
/// hostname, else the protocol.
fn bookmark_name(xml: &str) -> String {
    crate::bridge_shared::plist_field(xml, "Nickname")
        .filter(|s| !s.is_empty())
        .or_else(|| crate::bridge_shared::plist_field(xml, "Hostname").filter(|s| !s.is_empty()))
        .or_else(|| crate::bridge_shared::plist_field(xml, "Protocol"))
        .unwrap_or_else(|| "Cyberduck bookmark".to_string())
}

/// Map one `.duck` body, pushing into `servers` or `skipped`.
fn ingest_duck(
    xml: &str,
    servers: &mut Vec<ServerProfileExport>,
    skipped: &mut Vec<CyberduckSkippedRemote>,
) {
    let name = bookmark_name(xml);
    match map_duck(xml) {
        DuckOutcome::Profile(m) => {
            let id = format!(
                "cyberduck-{}-{}",
                name.to_lowercase().replace(' ', "-"),
                &crate::bridge_shared::uuid_v4()[..8]
            );
            servers.push(ServerProfileExport {
                id,
                name,
                host: m.host,
                port: m.port,
                username: m.username,
                protocol: Some(m.protocol),
                initial_path: m.initial_path,
                local_initial_path: None,
                color: None,
                last_connected: None,
                options: m.options,
                provider_id: m.provider_id,
                // Secret is in the OS keychain, never in the file.
                credential: None,
                has_stored_credential: Some(false),
                public_url_base: None,
                ..Default::default()
            });
        }
        DuckOutcome::Skip { kind, reason } => {
            skipped.push(CyberduckSkippedRemote { name, kind, reason });
        }
        DuckOutcome::NotABookmark => {}
    }
}

// ============ Default config path ============

/// Returns the default Cyberduck `Bookmarks` directory for the platform.
///
/// Cyberduck stores one `.duck` file per bookmark under a `Bookmarks`
/// folder. There is no Linux build of Cyberduck, so Linux returns `None`.
/// An explicit `CYBERDUCK_BOOKMARKS` env var overrides the per-OS default.
pub fn default_cyberduck_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CYBERDUCK_BOOKMARKS") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(pb);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home).join("Library/Application Support/Cyberduck/Bookmarks");
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = PathBuf::from(appdata).join("Cyberduck").join("Bookmarks");
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Linux: no Cyberduck build exists.
    None
}

// ============ Import ============

/// Import Cyberduck bookmarks (metadata only).
///
/// `path` may be a single `.duck` file or a `Bookmarks` directory: in the
/// directory case every `*.duck` entry (non-recursive) is parsed. Passwords
/// are never present in the file (OS keychain): every imported profile has
/// `credential=None` and `has_stored_credential=Some(false)`. OAuth bookmarks
/// (googledrive/onedrive/dropbox) and Azure go to `skipped`.
pub fn import_cyberduck(path: &Path) -> Result<CyberduckImportResult, String> {
    let mut servers: Vec<ServerProfileExport> = Vec::new();
    let mut skipped: Vec<CyberduckSkippedRemote> = Vec::new();
    let mut total_remotes = 0usize;

    if path.is_dir() {
        let entries =
            std::fs::read_dir(path).map_err(|e| format!("read Cyberduck Bookmarks dir: {e}"))?;
        let mut duck_files: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file()
                && p.extension()
                    .and_then(|s| s.to_str())
                    .map(|e| e.eq_ignore_ascii_case("duck"))
                    .unwrap_or(false)
            {
                duck_files.push(p);
            }
        }
        // Deterministic order so import IDs/skip order are stable.
        duck_files.sort();
        for p in duck_files {
            match std::fs::read_to_string(&p) {
                Ok(xml) => {
                    total_remotes += 1;
                    ingest_duck(&xml, &mut servers, &mut skipped);
                }
                Err(e) => {
                    total_remotes += 1;
                    skipped.push(CyberduckSkippedRemote {
                        name: p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.display().to_string()),
                        kind: "unreadable".into(),
                        reason: format!("read .duck: {e}"),
                    });
                }
            }
        }
    } else {
        let xml = std::fs::read_to_string(path).map_err(|e| format!("read .duck: {e}"))?;
        total_remotes = 1;
        ingest_duck(&xml, &mut servers, &mut skipped);
    }

    Ok(CyberduckImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export ============

/// Map an AeroFTP protocol/providerId back to a Cyberduck `Protocol` token.
fn cyberduck_protocol(proto: &str, provider_id: Option<&str>) -> Option<&'static str> {
    match proto {
        "sftp" => Some("sftp"),
        "ftp" => Some("ftp"),
        "ftps" => Some("ftps"),
        "webdav" => Some("davs"),
        "s3" => {
            if provider_id == Some("backblaze-b2") {
                Some("b2")
            } else {
                Some("s3")
            }
        }
        // OAuth / Azure profiles cannot round-trip into a usable bookmark.
        _ => None,
    }
}

/// XML-escape a value for a plist `<string>` body.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn sanitize_filename(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "bookmark".to_string()
    } else {
        trimmed
    }
}

/// Export server profiles as Cyberduck `.duck` bookmarks (metadata only).
///
/// Per the bridge contract every source is bidirectional. Cyberduck keeps
/// secrets in the OS keychain, so the written bookmarks deliberately carry
/// NO password: the `passwords` map is accepted for signature symmetry with
/// the other exporters but is not written into the file (the user re-enters
/// the secret in Cyberduck's keychain prompt). One `.duck` plist is written
/// per exportable profile into `out` (created if absent); OAuth/Azure
/// profiles are skipped. Returns the number of bookmarks written.
pub fn export_cyberduck(
    servers: &[CyberduckExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    // Signature symmetry with the other exporters; Cyberduck never persists
    // the secret in the file, so the map is intentionally unused.
    let _ = passwords;

    std::fs::create_dir_all(out).map_err(|e| format!("create Cyberduck Bookmarks dir: {e}"))?;

    let mut exported = 0usize;
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for server in servers {
        let proto = server.protocol.as_deref().unwrap_or("ftp");
        let Some(duck_proto) = cyberduck_protocol(proto, server.provider_id.as_deref()) else {
            continue;
        };

        // Cyberduck S3 Path is bucket[/prefix]; prefer the options.bucket
        // when present, else the stored initial_path.
        let path_value = {
            let bucket = server
                .options
                .as_ref()
                .and_then(|v| v.get("bucket"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            bucket
                .or_else(|| server.initial_path.clone())
                .filter(|s| !s.is_empty())
        };

        let region = server
            .options
            .as_ref()
            .and_then(|v| v.get("region"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let mut body = String::new();
        body.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        body.push_str(
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        );
        body.push_str("<plist version=\"1.0\">\n<dict>\n");
        body.push_str(&format!(
            "\t<key>Protocol</key>\n\t<string>{}</string>\n",
            duck_proto
        ));
        body.push_str(&format!(
            "\t<key>Nickname</key>\n\t<string>{}</string>\n",
            xml_escape(&server.name)
        ));
        body.push_str(&format!(
            "\t<key>Hostname</key>\n\t<string>{}</string>\n",
            xml_escape(&server.host)
        ));
        body.push_str(&format!(
            "\t<key>Port</key>\n\t<string>{}</string>\n",
            server.port
        ));
        body.push_str(&format!(
            "\t<key>Username</key>\n\t<string>{}</string>\n",
            xml_escape(&server.username)
        ));
        if let Some(p) = &path_value {
            body.push_str(&format!(
                "\t<key>Path</key>\n\t<string>{}</string>\n",
                xml_escape(p)
            ));
        }
        if let Some(r) = &region {
            body.push_str(&format!(
                "\t<key>Region</key>\n\t<string>{}</string>\n",
                xml_escape(r)
            ));
        }
        body.push_str("</dict>\n</plist>\n");

        // Stable, collision-free file name per bookmark.
        let base = sanitize_filename(&server.name);
        let mut file_name = format!("{base}.duck");
        let mut n = 2;
        while used_names.contains(&file_name) {
            file_name = format!("{base}-{n}.duck");
            n += 1;
        }
        used_names.insert(file_name.clone());

        crate::bridge_shared::atomic_write_600(&out.join(&file_name), body.as_bytes())?;
        exported += 1;
    }

    Ok(exported)
}

// ============ Tests ============

#[cfg(test)]
mod tests {
    use super::*;

    const SFTP_DUCK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Protocol</key>
	<string>sftp</string>
	<key>Nickname</key>
	<string>My NAS</string>
	<key>Hostname</key>
	<string>nas.example.com</string>
	<key>Port</key>
	<string>2222</string>
	<key>Username</key>
	<string>admin</string>
	<key>Path</key>
	<string>/volume1/data</string>
</dict>
</plist>"#;

    const S3_DUCK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>Protocol</key>
	<string>s3</string>
	<key>Nickname</key>
	<string>Wasabi Backup</string>
	<key>Hostname</key>
	<string>s3.wasabisys.com</string>
	<key>Port</key>
	<string>443</string>
	<key>Username</key>
	<string>AKIAEXAMPLE</string>
	<key>Path</key>
	<string>/my-bucket/backups</string>
	<key>Region</key>
	<string>us-east-1</string>
</dict>
</plist>"#;

    const GDRIVE_DUCK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>Protocol</key>
	<string>googledrive</string>
	<key>Nickname</key>
	<string>Personal Drive</string>
	<key>Hostname</key>
	<string>www.googleapis.com</string>
	<key>Username</key>
	<string>user@example.com</string>
</dict>
</plist>"#;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aeroftp-cyberduck-test-{}",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn test_parse_sftp_bookmark() {
        let mut servers = Vec::new();
        let mut skipped = Vec::new();
        ingest_duck(SFTP_DUCK, &mut servers, &mut skipped);
        assert_eq!(servers.len(), 1);
        assert!(skipped.is_empty());
        let s = &servers[0];
        assert_eq!(s.name, "My NAS");
        assert_eq!(s.protocol.as_deref(), Some("sftp"));
        assert_eq!(s.host, "nas.example.com");
        assert_eq!(s.port, 2222);
        assert_eq!(s.username, "admin");
        assert_eq!(s.initial_path.as_deref(), Some("/volume1/data"));
        // Secret policy: keychain, never in the file.
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
    }

    #[test]
    fn test_parse_s3_bookmark_with_region() {
        let mut servers = Vec::new();
        let mut skipped = Vec::new();
        ingest_duck(S3_DUCK, &mut servers, &mut skipped);
        assert_eq!(servers.len(), 1);
        let s = &servers[0];
        assert_eq!(s.protocol.as_deref(), Some("s3"));
        // Provider inferred from the wasabisys.com endpoint host.
        assert_eq!(s.provider_id.as_deref(), Some("wasabi"));
        let opts = s.options.as_ref().expect("S3 options").as_object().unwrap();
        assert_eq!(
            opts.get("region").and_then(|v| v.as_str()),
            Some("us-east-1")
        );
        assert_eq!(
            opts.get("bucket").and_then(|v| v.as_str()),
            Some("my-bucket")
        );
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
    }

    #[test]
    fn test_googledrive_is_skipped_oauth() {
        let mut servers = Vec::new();
        let mut skipped = Vec::new();
        ingest_duck(GDRIVE_DUCK, &mut servers, &mut skipped);
        assert!(servers.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].kind, "googledrive");
        assert_eq!(
            skipped[0].reason,
            "OAuth: provider-issued token, manual re-auth"
        );
    }

    #[test]
    fn test_import_directory_of_bookmarks() {
        let dir = std::env::temp_dir().join(format!(
            "aeroftp-cyberduck-dir-{}",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nas.duck"), SFTP_DUCK).unwrap();
        std::fs::write(dir.join("wasabi.duck"), S3_DUCK).unwrap();
        std::fs::write(dir.join("drive.duck"), GDRIVE_DUCK).unwrap();
        // A non-.duck file must be ignored entirely.
        std::fs::write(dir.join("README.txt"), "ignore me").unwrap();

        let result = import_cyberduck(&dir).expect("should import dir");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(result.total_remotes, 3);
        assert_eq!(result.servers.len(), 2); // sftp + s3
        assert_eq!(result.skipped.len(), 1); // gdrive OAuth
        assert_eq!(
            result.skipped[0].reason,
            "OAuth: provider-issued token, manual re-auth"
        );
    }

    #[test]
    fn test_secret_policy_metadata_only() {
        // File-plain -> would be Some elsewhere; Cyberduck keychain -> None.
        let p = write_tmp("nas.duck", SFTP_DUCK);
        let result = import_cyberduck(&p).expect("import single file");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
        assert_eq!(result.servers.len(), 1);
        let s = &result.servers[0];
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        // Note constant is the documented expectation surface.
        assert!(CYBERDUCK_SECRET_NOTE.contains("keychain"));
    }

    #[test]
    fn test_roundtrip_metadata_only_idempotence() {
        // import -> export -> import; for a keychain source idempotence is
        // metadata-only (no credential ever travels).
        let p = write_tmp("nas.duck", SFTP_DUCK);
        let first = import_cyberduck(&p).expect("first import");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
        assert_eq!(first.servers.len(), 1);

        let export_servers: Vec<CyberduckExportServer> = first
            .servers
            .iter()
            .map(|s| CyberduckExportServer {
                name: s.name.clone(),
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                protocol: s.protocol.clone(),
                options: s.options.clone(),
                provider_id: s.provider_id.clone(),
                initial_path: s.initial_path.clone(),
            })
            .collect();

        let out_dir = std::env::temp_dir().join(format!(
            "aeroftp-cyberduck-rt-{}",
            crate::bridge_shared::uuid_v4()
        ));
        let passwords = HashMap::new();
        let written = export_cyberduck(&export_servers, &passwords, &out_dir).expect("export");
        assert_eq!(written, 1);

        let second = import_cyberduck(&out_dir).expect("re-import");
        std::fs::remove_dir_all(&out_dir).ok();

        assert_eq!(second.servers.len(), 1);
        let a = &first.servers[0];
        let b = &second.servers[0];
        assert_eq!(a.host, b.host);
        assert_eq!(a.port, b.port);
        assert_eq!(a.protocol, b.protocol);
        assert_eq!(a.username, b.username);
        assert_eq!(a.initial_path, b.initial_path);
        // Metadata-only idempotence: credential stays None on both sides.
        assert!(a.credential.is_none());
        assert!(b.credential.is_none());
        assert_eq!(b.has_stored_credential, Some(false));
    }

    #[test]
    fn test_default_path_env_override() {
        // On every OS the CYBERDUCK_BOOKMARKS override wins when it exists.
        let dir = std::env::temp_dir().join(format!(
            "aeroftp-cyberduck-envpath-{}",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let prev = std::env::var("CYBERDUCK_BOOKMARKS").ok();
        std::env::set_var("CYBERDUCK_BOOKMARKS", &dir);
        let resolved = default_cyberduck_config_path();
        assert_eq!(resolved.as_deref(), Some(dir.as_path()));

        // Non-existent override falls through (to per-OS default or None).
        std::env::set_var("CYBERDUCK_BOOKMARKS", dir.join("does-not-exist"));
        let missing = default_cyberduck_config_path();
        assert_ne!(missing.as_deref(), Some(dir.as_path()));

        match prev {
            Some(v) => std::env::set_var("CYBERDUCK_BOOKMARKS", v),
            None => std::env::remove_var("CYBERDUCK_BOOKMARKS"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_has_no_default_path() {
        // No Cyberduck build on Linux: without an env override, None.
        let prev = std::env::var("CYBERDUCK_BOOKMARKS").ok();
        std::env::remove_var("CYBERDUCK_BOOKMARKS");
        assert!(default_cyberduck_config_path().is_none());
        if let Some(v) = prev {
            std::env::set_var("CYBERDUCK_BOOKMARKS", v);
        }
    }
}
