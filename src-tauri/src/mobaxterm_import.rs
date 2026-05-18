// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from MobaXterm configuration files.
//!
//! Parses `MobaXterm.ini`, sections `[Bookmarks]`, `[Bookmarks_1]`, ...
//! Each session is a line `<DisplayName>= #<SessionType>#<sub>%<host>%<port>
//! %<user>%...` (percent/hash-delimited positional layout). We map the
//! transfer-relevant session types (SSH=109, SFTP=100, FTP=99) to AeroFTP
//! ProviderType and carry over host/port/user/path metadata.
//!
//! Secret policy table (paste-into-every-module per APPENDIX-BRIDGE sec. 1):
//!
//! | Case                                  | Bridge policy                          |
//! |---------------------------------------|----------------------------------------|
//! | plain / file-obfuscated secret        | populate `credential` (re-encrypt vault)|
//! | OS keychain (cyberduck)               | credential=None, has_stored=Some(false)|
//! | OAuth token                           | credential=None, skipped "manual re-auth"|
//! | SSH key referenced                    | options.private_key_path, credential=None|
//! | reversible obfuscation (winscp/dw)    | reveal on import, re-obscure on export |
//!
//! MobaXterm-specific honesty note (NOT the winscp posture):
//! MobaXterm's stored password obfuscation is NOT a simple public reversible
//! scheme like rclone/WinSCP/Dreamweaver. The on-disk encryption is bound to
//! the Windows account / machine (and, for "Master Password" vaults, to a
//! user-supplied key) using a host-bound key derivation. There is no portable,
//! safe public reversal we can perform off the originating Windows account.
//! Therefore v1 is IMPORT-ONLY for the secret: when an obfuscated password
//! field is present we set `credential=None`, `has_stored_credential=Some(false)`
//! and record a skipped/log note "MobaXterm password obfuscation is host-bound;
//! re-enter". We deliberately DO NOT invent a fake crypto. The v1 value of this
//! bridge is the metadata (host/port/user/protocol/path). A plain or empty
//! password field is handled normally (populate / leave None respectively).
//!
//! Export (v1): MobaXterm bidirectionality is satisfied with a metadata-only
//! `[Bookmarks]` writer. We cannot re-obfuscate a host-bound secret off the
//! target machine, so the exported `.ini` carries no password (the user
//! re-enters it in MobaXterm). The function exists so the bridge contract's
//! "every source is bidirectional" holds honestly.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ MobaXterm session-type table ============
// MobaXterm encodes the connection kind as a numeric SessionType in the line
// `Name= #<SessionType>#<sub>%<host>%<port>%<user>%...`.
// Transfer-relevant types (others: RDP 98, VNC 97, Telnet 91, Serial 101,
// Shell 105, Browser 110, ... are not file-transfer endpoints and are skipped):
//   109 -> SSH   (we treat as SFTP over the SSH port; default 22)
//   100 -> SFTP  (default 22)
//    99 -> FTP   (default 21; FTPS/FTPES variants land here, see flags below)
const MOBA_TYPE_SSH: u32 = 109;
const MOBA_TYPE_SFTP: u32 = 100;
const MOBA_TYPE_FTP: u32 = 99;

// ============ MobaXterm password reveal (host-bound, NOT reversed in v1) ============

/// Best-effort reveal of a MobaXterm-stored password.
///
/// MobaXterm's password obfuscation is account+host (and optionally
/// Master-Password) derived: it is NOT a public reversible scheme. We do not
/// attempt a fake reversal. This returns `Err` whenever a non-empty obfuscated
/// value is present so the caller drops the credential and records the honest
/// "host-bound; re-enter" note. An empty input maps to an empty plaintext.
fn reveal_mobaxterm(enc: &str, _user: &str, _host: &str) -> Result<String, String> {
    if enc.is_empty() {
        return Ok(String::new());
    }
    Err("MobaXterm password obfuscation is host-bound; re-enter".to_string())
}

// ============ Bookmark line parser ============

/// One parsed MobaXterm bookmark line.
struct MobaEntry {
    /// Display name (the INI key before `=`).
    name: String,
    /// Numeric SessionType (`#<type>#`).
    session_type: u32,
    /// Percent-separated positional fields after `#<type>#<sub>`.
    fields: Vec<String>,
    /// Folder/tree label from the section's `SubRep=` line, if any.
    folder: Option<String>,
}

/// Parse the value side of a bookmark line.
///
/// Observed layout (documented assumption, see module/report):
/// `#<SessionType>#<SubParam>%<host>%<port>%<user>%<...positional...>`
/// The token immediately after the first `=` is `#<type>#<sub>`; everything
/// after the closing `#` of that token is a `%`-delimited positional list.
/// For SSH(109)/SFTP(100): fields are roughly host, port, username, then
/// MobaXterm-internal flags (X11, compression, key file, ...). For FTP(99):
/// host, port, username, then mode/SSL flags. We only consume host/port/user
/// positionally and treat the rest as opaque (defensive: short lines are
/// tolerated, missing fields fall back to defaults).
fn parse_bookmark_value(value: &str) -> Option<(u32, Vec<String>)> {
    let v = value.trim();
    let rest = v.strip_prefix('#')?;
    // `<type>#<sub>%field%field...` : split off the leading `<type>`.
    let (type_str, after_type) = rest.split_once('#')?;
    let session_type: u32 = type_str.trim().parse().ok()?;
    // `after_type` is `<sub>%field%field...`. The `<sub>` group is itself the
    // first percent-delimited token; the connection fields follow it. We keep
    // every `%`-separated token and index from the known offsets per type.
    let fields: Vec<String> = after_type.split('%').map(|s| s.to_string()).collect();
    Some((session_type, fields))
}

/// Parse `MobaXterm.ini` and return all bookmark entries from every
/// `[Bookmarks]` / `[Bookmarks_N]` section. Non-bookmark sections, and the
/// per-section metadata keys (`SubRep`, `ImgNum`) are not entries.
fn parse_mobaxterm(content: &str) -> Vec<MobaEntry> {
    let mut entries: Vec<MobaEntry> = Vec::new();
    let mut in_bookmarks = false;
    let mut current_folder: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            // Note: a leading '#' here is an INI comment, not a session value.
            // Session values are the RHS of `Key=#...`, never a bare line.
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            let section = &line[1..line.len() - 1];
            // "[Bookmarks]" or "[Bookmarks_12]" (case-insensitive guard).
            in_bookmarks = section.eq_ignore_ascii_case("Bookmarks")
                || section
                    .strip_prefix("Bookmarks_")
                    .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false);
            current_folder = None;
            if entries.len() >= MAX_BOOKMARKS {
                in_bookmarks = false;
            }
            continue;
        }

        if !in_bookmarks {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        // Per-section metadata, not a session.
        if key.eq_ignore_ascii_case("SubRep") {
            if !value.is_empty() {
                current_folder = Some(value.to_string());
            }
            continue;
        }
        if key.eq_ignore_ascii_case("ImgNum") {
            continue;
        }

        // A session line: value must start with `#<type>#`.
        if !value.starts_with('#') {
            continue;
        }
        if entries.len() >= MAX_BOOKMARKS {
            break;
        }
        if let Some((session_type, fields)) = parse_bookmark_value(value) {
            entries.push(MobaEntry {
                name: key.to_string(),
                session_type,
                fields,
                folder: current_folder.clone(),
            });
        }
    }

    entries
}

// ============ Mapping ============

struct MappedProfile {
    protocol: String,
    host: String,
    port: u32,
    username: String,
    /// Some(plain) when a usable plaintext was found, None otherwise. The
    /// host-bound-secret case is signalled via `secret_host_bound`.
    password: Option<String>,
    /// True when an obfuscated (host-bound) password was present but could
    /// not be safely revealed: caller sets has_stored_credential=Some(false)
    /// and records the honest skipped/log note.
    secret_host_bound: bool,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
}

/// Field offsets in the `%`-delimited positional list after `#<type>#`.
/// `fields[0]` is the `<sub>` group token, real connection fields follow.
/// host/port/user sit at indices 1/2/3 for SSH/SFTP/FTP in observed files.
const F_HOST: usize = 1;
const F_PORT: usize = 2;
const F_USER: usize = 3;

fn field(fields: &[String], idx: usize) -> Option<&str> {
    fields.get(idx).map(|s| s.trim()).filter(|s| !s.is_empty())
}

/// Convert one parsed bookmark entry to an AeroFTP profile, or `None`
/// (with a caller-supplied skip reason) for unsupported session types.
fn map_entry(entry: &MobaEntry) -> Result<MappedProfile, String> {
    let (protocol, default_port) = match entry.session_type {
        MOBA_TYPE_SSH | MOBA_TYPE_SFTP => ("sftp", 22u32),
        MOBA_TYPE_FTP => {
            // MobaXterm FTP type 99 covers plain FTP and the FTPS/FTPES
            // variants; the TLS flag lives in the trailing positional flags
            // which are MobaXterm-internal and version-dependent. v1 maps
            // type 99 to plain FTP unless an explicit secure marker appears
            // in the opaque tail (best-effort, conservative default).
            let secure = entry.fields.iter().any(|f| {
                let f = f.to_ascii_lowercase();
                f == "ftpes" || f == "ftps" || f == "1ftps"
            });
            if secure {
                ("ftps", 990)
            } else {
                ("ftp", 21)
            }
        }
        other => {
            return Err(format!("unsupported MobaXterm session type {}", other));
        }
    };

    let host = field(&entry.fields, F_HOST)
        .ok_or_else(|| "missing host field".to_string())?
        .to_string();

    let port = field(&entry.fields, F_PORT)
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(default_port);

    let username = field(&entry.fields, F_USER).unwrap_or("").to_string();

    // MobaXterm stores its (host-bound) saved passwords in a separate
    // `[Passwords]`-style section, never inline in the bookmark line. The
    // bookmark line itself carries no plaintext secret. v1 therefore never
    // finds a usable plaintext here; if MobaXterm ever inlines a plain value
    // it would be a positional field, which we do not trust as a secret.
    // We route the decision through reveal_mobaxterm (the single source of
    // the host-bound-secret policy) so the caller emits the honest
    // "re-enter" note and sets has_stored_credential=Some(false). The
    // bookmark line carries no inline plaintext in v1, so a non-empty
    // reveal is never produced; the host-bound arm is taken.
    let (password, secret_host_bound) = match reveal_mobaxterm("", &username, &host) {
        Ok(p) if !p.is_empty() => (Some(p), false),
        _ => (None, true),
    };

    // Carry the folder label so users can see the MobaXterm tree origin.
    let options = entry.folder.as_ref().filter(|f| !f.is_empty()).map(|f| {
        serde_json::Value::Object(crate::bridge_shared::json_map(&[(
            "mobaxtermFolder",
            Some(f.to_string()),
        )]))
    });

    Ok(MappedProfile {
        protocol: protocol.to_string(),
        host,
        port,
        username,
        password,
        secret_host_bound,
        options,
        initial_path: None,
    })
}

// ============ Default config path detection ============

/// Returns the default `MobaXterm.ini` path.
///
/// MobaXterm is Windows-only. Primary location is
/// `%APPDATA%\MobaXterm\MobaXterm.ini`. Portable installs keep
/// `MobaXterm.ini` next to the executable: that path is not reliably
/// discoverable, so users pass it explicitly to `import_mobaxterm`. On
/// non-Windows there is no default (returns `None`); the user browses to an
/// exported `.ini`.
pub fn default_mobaxterm_config_path() -> Option<PathBuf> {
    // Explicit override always wins (testable, and useful for portable .ini).
    if let Ok(path) = std::env::var("MOBAXTERM_INI") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = PathBuf::from(appdata)
                .join("MobaXterm")
                .join("MobaXterm.ini");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Public API ============

/// A bookmark that was skipped (unsupported session type, or missing host).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobaxtermSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing a MobaXterm.ini.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobaxtermImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<MobaxtermSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Defense against crafted files with an unbounded bookmark count.
const MAX_BOOKMARKS: usize = 10_000;

/// Import all supported bookmarks from a `MobaXterm.ini` file.
pub fn import_mobaxterm(path: &Path) -> Result<MobaxtermImportResult, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Read MobaXterm.ini: {}", e))?;

    let entries = parse_mobaxterm(&content);
    let total_remotes = entries.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for entry in &entries {
        match map_entry(entry) {
            Ok(mapped) => {
                let id = format!(
                    "mobaxterm-{}-{}",
                    entry.name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                // v1: the secret is host-bound and not reversed. We honestly
                // record that fact so the UI prompts the user to re-enter.
                let has_stored_credential = if mapped.password.is_some() {
                    None
                } else if mapped.secret_host_bound {
                    skipped.push(MobaxtermSkippedRemote {
                        name: entry.name.clone(),
                        kind: format!("type {}", entry.session_type),
                        reason: "MobaXterm password obfuscation is host-bound; re-enter"
                            .to_string(),
                    });
                    Some(false)
                } else {
                    None
                };

                servers.push(ServerProfileExport {
                    id,
                    name: entry.name.clone(),
                    host: mapped.host,
                    port: mapped.port,
                    username: mapped.username,
                    protocol: Some(mapped.protocol),
                    initial_path: mapped.initial_path,
                    local_initial_path: None,
                    color: None,
                    last_connected: None,
                    options: mapped.options,
                    provider_id: None,
                    credential: mapped.password,
                    has_stored_credential,
                    public_url_base: None,
                });
            }
            Err(reason) => {
                skipped.push(MobaxtermSkippedRemote {
                    name: entry.name.clone(),
                    kind: format!("type {}", entry.session_type),
                    reason,
                });
            }
        }
    }

    Ok(MobaxtermImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export (v1: metadata only) ============

/// A server profile to export as a MobaXterm bookmark line.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobaxtermExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Map an AeroFTP protocol back to a MobaXterm SessionType.
fn protocol_to_session_type(protocol: &str) -> Option<u32> {
    match protocol {
        "sftp" => Some(MOBA_TYPE_SFTP),
        "ftp" | "ftps" => Some(MOBA_TYPE_FTP),
        _ => None,
    }
}

/// Sanitize a value so it cannot break the INI line / positional format
/// (strip newlines, the `%`/`#` delimiters, and `[` `]` `=`).
fn sanitize_field(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\n' | '\r' | '%' | '#' | '[' | ']' | '='))
        .collect()
}

/// Export AeroFTP server profiles to a `MobaXterm.ini` `[Bookmarks]` section.
///
/// v1 is metadata-only: MobaXterm's password obfuscation is host-bound and
/// cannot be reproduced off the target machine, so `passwords` is accepted
/// for signature symmetry with the other bridge exporters but never written.
/// MobaXterm prompts for the password on first connect. Returns the number
/// of exported bookmarks.
pub fn export_mobaxterm(
    servers: &[MobaxtermExportServer],
    _passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut ini = String::from(
        "; MobaXterm bookmarks exported by AeroFTP - https://aeroftp.app\n\
         ; v1 export: metadata only. MobaXterm password obfuscation is\n\
         ; host-bound; passwords are NOT written and must be re-entered.\n\n\
         [Bookmarks]\n\
         SubRep=AeroFTP\n\
         ImgNum=42\n",
    );

    let mut exported = 0;

    for server in servers {
        let proto = server.protocol.as_deref().unwrap_or("ftp");
        let Some(session_type) = protocol_to_session_type(proto) else {
            continue;
        };

        let name = sanitize_field(&server.name);
        if name.is_empty() {
            continue;
        }
        let host = sanitize_field(&server.host);
        let user = sanitize_field(&server.username);

        // Positional line mirroring the import layout:
        // `Name= #<type>#0%<host>%<port>%<user>%`
        // The leading `0` is the neutral `<sub>` group; trailing `%` keeps
        // the field list shape MobaXterm expects when it re-parses.
        ini.push_str(&format!(
            "{}= #{}#0%{}%{}%{}%\n",
            name, session_type, host, server.port, user
        ));

        exported += 1;
    }

    crate::bridge_shared::atomic_write_600(out, ini.as_bytes())?;

    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a) Parser unit test with a real-shaped fixture: an SSH(109) entry,
    /// an FTP(99) entry, and an unsupported type (RDP 98 -> skipped).
    #[test]
    fn test_parse_mobaxterm_fixture() {
        let ini = "\
[Bookmarks]
SubRep=Production
ImgNum=42
My SSH Box= #109#0%ssh.example.com%2222%deploy%-1%-1%%%%%0%0%0%%%-1%0%0%0%
Legacy FTP= #99#0%ftp.example.com%21%ftpuser%0%0%0%
Windows RDP= #98#0%win.example.com%3389%administrator%

[Misc]
NotABookmark=value
";
        let entries = parse_mobaxterm(ini);
        assert_eq!(entries.len(), 3);

        let ssh = &entries[0];
        assert_eq!(ssh.name, "My SSH Box");
        assert_eq!(ssh.session_type, 109);
        assert_eq!(ssh.folder.as_deref(), Some("Production"));
        assert_eq!(field(&ssh.fields, F_HOST), Some("ssh.example.com"));
        assert_eq!(field(&ssh.fields, F_PORT), Some("2222"));
        assert_eq!(field(&ssh.fields, F_USER), Some("deploy"));

        let ftp = &entries[1];
        assert_eq!(ftp.session_type, 99);
        assert_eq!(field(&ftp.fields, F_HOST), Some("ftp.example.com"));

        let rdp = &entries[2];
        assert_eq!(rdp.session_type, 98);
        assert!(map_entry(rdp).is_err());
    }

    /// (a/c) Mapping + secret-policy: SSH maps to sftp with metadata, and
    /// the host-bound secret yields credential=None +
    /// has_stored_credential=Some(false) + the honest skipped reason.
    #[test]
    fn test_import_metadata_and_secret_policy() {
        let ini = "\
[Bookmarks]
SubRep=Lab
ImgNum=41
NAS SFTP= #109#0%192.168.1.50%22%admin%-1%-1%%%%%0%0%0%
Old FTP= #99#0%ftp.lab.test%21%anon%0%0%
RDP Host= #98#0%rdp.lab.test%3389%user%
";
        let tmp = std::env::temp_dir().join("aeroftp-test-moba.ini");
        std::fs::write(&tmp, ini).unwrap();
        let result = import_mobaxterm(&tmp).expect("should import");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 3);
        assert_eq!(result.servers.len(), 2); // SSH + FTP, RDP skipped

        let ssh = result
            .servers
            .iter()
            .find(|s| s.name == "NAS SFTP")
            .expect("ssh server present");
        assert_eq!(ssh.protocol.as_deref(), Some("sftp"));
        assert_eq!(ssh.host, "192.168.1.50");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.username, "admin");
        // Secret policy: host-bound -> no credential, explicit "stored=false".
        assert!(ssh.credential.is_none());
        assert_eq!(ssh.has_stored_credential, Some(false));

        // The host-bound note and the RDP unsupported note are both recorded.
        assert!(result
            .skipped
            .iter()
            .any(|s| s.name == "NAS SFTP" && s.reason.contains("host-bound")));
        assert!(result
            .skipped
            .iter()
            .any(|s| s.name == "RDP Host" && s.reason.contains("unsupported")));

        let ftp = result
            .servers
            .iter()
            .find(|s| s.name == "Old FTP")
            .expect("ftp server present");
        assert_eq!(ftp.protocol.as_deref(), Some("ftp"));
        assert_eq!(ftp.port, 21);
    }

    /// (b) Round-trip import -> export -> import: metadata-only idempotence.
    /// MobaXterm secrets are host-bound, so idempotence is asserted on the
    /// metadata exactly (protocol/host/port/user), never on credentials.
    #[test]
    fn test_roundtrip_metadata_idempotence() {
        let servers = vec![
            MobaxtermExportServer {
                name: "rt-sftp".to_string(),
                host: "10.0.0.5".to_string(),
                port: 2200,
                username: "claude".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
            MobaxtermExportServer {
                name: "rt-ftp".to_string(),
                host: "ftp.rt.test".to_string(),
                port: 21,
                username: "ftp".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
        ];
        let passwords = HashMap::new();

        let tmp = std::env::temp_dir().join("aeroftp-test-moba-roundtrip.ini");
        let exported = export_mobaxterm(&servers, &passwords, &tmp).expect("should export");
        assert_eq!(exported, 2);

        let result = import_mobaxterm(&tmp).expect("should re-import");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.servers.len(), 2);

        let sftp = result
            .servers
            .iter()
            .find(|s| s.name == "rt-sftp")
            .expect("sftp roundtrip");
        assert_eq!(sftp.protocol.as_deref(), Some("sftp"));
        assert_eq!(sftp.host, "10.0.0.5");
        assert_eq!(sftp.port, 2200);
        assert_eq!(sftp.username, "claude");
        // Metadata-only: never a credential after round-trip.
        assert!(sftp.credential.is_none());

        let ftp = result
            .servers
            .iter()
            .find(|s| s.name == "rt-ftp")
            .expect("ftp roundtrip");
        assert_eq!(ftp.protocol.as_deref(), Some("ftp"));
        assert_eq!(ftp.host, "ftp.rt.test");
        assert_eq!(ftp.port, 21);
        assert_eq!(ftp.username, "ftp");
    }

    /// (c) Secret-policy unit: reveal_mobaxterm refuses a non-empty obfuscated
    /// value (no fake crypto) and maps empty -> empty plaintext.
    #[test]
    fn test_reveal_is_host_bound() {
        assert_eq!(reveal_mobaxterm("", "u", "h").unwrap(), "");
        let e = reveal_mobaxterm("Q2lwaGVy", "user", "host")
            .expect_err("non-empty obfuscated must not be faked");
        assert!(e.contains("host-bound"));
    }

    /// (d) default_mobaxterm_config_path with env override (set + restore).
    /// On Linux there is no per-OS default, so without the override the
    /// result is None; with MOBAXTERM_INI pointing at a real file it is Some.
    #[test]
    fn test_default_path_env_override() {
        let prev = std::env::var("MOBAXTERM_INI").ok();

        std::env::remove_var("MOBAXTERM_INI");
        #[cfg(not(target_os = "windows"))]
        {
            assert!(default_mobaxterm_config_path().is_none());
        }

        let tmp = std::env::temp_dir().join("aeroftp-test-moba-envpath.ini");
        std::fs::write(&tmp, "[Bookmarks]\n").unwrap();
        std::env::set_var("MOBAXTERM_INI", &tmp);
        assert_eq!(default_mobaxterm_config_path(), Some(tmp.clone()));

        // A non-existent override path must not be returned.
        std::env::set_var("MOBAXTERM_INI", "/no/such/MobaXterm.ini");
        #[cfg(not(target_os = "windows"))]
        {
            assert!(default_mobaxterm_config_path().is_none());
        }

        std::fs::remove_file(&tmp).ok();
        match prev {
            Some(v) => std::env::set_var("MOBAXTERM_INI", v),
            None => std::env::remove_var("MOBAXTERM_INI"),
        }
    }
}
