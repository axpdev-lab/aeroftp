// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from PuTTY's saved sessions.
//!
//! PuTTY does NOT use a config file: its sessions live in the Windows
//! registry under `HKCU\Software\SimonTatham\PuTTY\Sessions\<name>`. Each
//! session key carries `HostName`, `PortNumber` (REG_DWORD), `UserName`,
//! `Protocol` (`ssh`/`raw`/`telnet`/`rlogin`/`serial`; modern builds also
//! advertise `sftp` indirectly via SSH) and `PublicKeyFile` (path to a
//! `.ppk`/key file). Session names are URL-encoded with `%XX` escapes
//! (PuTTY's `unmungestr`/`mungestr`), so `My%20Server` decodes to
//! `My Server`.
//!
//! Because the source is the registry and not a file, `import_putty(path)`
//! and `export_putty(.., out)` accept their `Path`/`out` arguments only for
//! signature symmetry with the other bridge modules: on Windows the
//! registry is the single source of truth and those arguments are ignored
//! (documented per call site). On non-Windows the registry is unavailable,
//! so `import_putty` returns an empty `Ok` result with an info log and
//! `export_putty` returns `Ok(0)` with an info log; `default_putty_config_path`
//! is `None` on every OS (there is no file to point at).
//!
//! Secret policy for this source (paste-in-doc requirement):
//!
//! | Case                         | This module                                  |
//! |------------------------------|----------------------------------------------|
//! | plain/obfuscated-in-file     | n/a: PuTTY never stores passwords            |
//! | OS keychain (cyberduck-like) | n/a                                          |
//! | OAuth tokens                 | n/a                                          |
//! | SSH key referenced           | `options.private_key_path` = PublicKeyFile,  |
//! |                              | `credential = None`,                         |
//! |                              | `has_stored_credential = Some(false)`        |
//! | reversible obfuscation       | n/a                                          |
//!
//! PuTTY has no password store at all, so every imported profile has
//! `credential = None` and `has_stored_credential = Some(false)`. No secret
//! is ever read from or written to the registry by this module. WinSCP can
//! itself import PuTTY sessions and AeroFTP imports WinSCP, but the direct
//! importer removes that hop and is required independently.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Registry path (relative to `HKEY_CURRENT_USER`) holding PuTTY sessions.
#[cfg(target_os = "windows")]
const PUTTY_SESSIONS_KEY: &str = r"Software\SimonTatham\PuTTY\Sessions";

/// Defensive cap on the number of registry sessions parsed in one import.
#[cfg(target_os = "windows")]
const MAX_SESSIONS: usize = 10_000;

// ============ Pure helpers (testable on Linux CI) ============

/// Map a PuTTY `Protocol` value to an AeroFTP protocol token.
///
/// PuTTY stores `ssh`, `raw`, `telnet`, `rlogin`, `serial`. Only the SSH
/// family carries a usable file-transfer channel for us, so `ssh`/`sftp`
/// both fold to `"sftp"`; everything else has no AeroFTP equivalent and
/// returns `None` (caller records a skipped reason).
pub fn map_putty_protocol(proto: &str) -> Option<&'static str> {
    match proto.trim().to_lowercase().as_str() {
        "ssh" | "sftp" => Some("sftp"),
        _ => None,
    }
}

/// Decode a PuTTY-munged session name. PuTTY escapes characters it cannot
/// store verbatim in a registry key name as `%XX` (uppercase hex), where
/// `%` itself is `%25`. Invalid escapes are passed through unchanged.
pub fn decode_putty_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode a session name back into PuTTY's munged registry-key form.
/// Mirrors PuTTY's `mungestr`: control characters, `%`, and a leading `.`
/// are percent-escaped; everything else (including spaces) is preserved,
/// matching what PuTTY itself writes.
///
/// Reached only from the Windows registry export path and the unit tests,
/// so it is `#[cfg]`-gated to avoid a dead-code warning on a non-Windows
/// lib build (the CI host).
#[cfg(any(test, target_os = "windows"))]
fn encode_putty_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (idx, b) in name.bytes().enumerate() {
        let needs_escape =
            b < 0x20 || b == b'%' || b == b'\\' || b == b'/' || (idx == 0 && b == b'.');
        if needs_escape {
            out.push_str(&format!("%{:02X}", b));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Build a `ServerProfileExport` from one PuTTY session's key/value map.
///
/// Pure and OS-independent so it can be unit-tested on the Linux CI host
/// with a synthetic `kv` map. `kv` keys are the PuTTY value names
/// (`HostName`, `PortNumber`, `UserName`, `Protocol`, `PublicKeyFile`) and
/// `name` is the already-decoded session name. Returns `None` for sessions
/// whose protocol is not bridgeable or that have no host (caller records a
/// skipped reason).
///
/// Secret invariant: `credential` is ALWAYS `None` and
/// `has_stored_credential` is ALWAYS `Some(false)` here, because PuTTY
/// never stores a password. A referenced key file becomes
/// `options.private_key_path` (path only, never key bytes).
///
/// Called from the Windows registry import path and the unit tests, so it
/// is `#[cfg]`-gated to avoid a dead-code warning on a non-Windows build.
#[cfg(any(test, target_os = "windows"))]
fn map_putty_session(name: &str, kv: &HashMap<String, String>) -> Option<ServerProfileExport> {
    let proto_raw = kv.get("Protocol").map(|s| s.as_str()).unwrap_or("ssh");
    let protocol = map_putty_protocol(proto_raw)?;

    let host = kv
        .get("HostName")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let port = kv
        .get("PortNumber")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(22);

    let username = kv
        .get("UserName")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut options = serde_json::Map::new();
    if let Some(key_file) = kv
        .get("PublicKeyFile")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        options.insert(
            "private_key_path".into(),
            serde_json::Value::String(key_file.to_string()),
        );
    }

    Some(ServerProfileExport {
        id: format!(
            "putty-{}-{}",
            name.to_lowercase().replace(' ', "-"),
            &crate::bridge_shared::uuid_v4()[..8]
        ),
        name: name.to_string(),
        host,
        port,
        username,
        protocol: Some(protocol.to_string()),
        initial_path: None,
        local_initial_path: None,
        color: None,
        last_connected: None,
        options: if options.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(options))
        },
        provider_id: None,
        // PuTTY never stores a password. SSH key auth is bridged by
        // reference only (options.private_key_path above).
        credential: None,
        has_stored_credential: Some(false),
        public_url_base: None,
    })
}

// ============ Default config path detection ============

/// PuTTY has no config file: sessions live in the Windows registry.
///
/// Returns `None` on every platform. The CLI/UI must not prompt for a path
/// for this source; `import_putty` reads `HKCU` directly on Windows and is a
/// no-op elsewhere.
pub fn default_putty_config_path() -> Option<PathBuf> {
    None
}

// ============ Public API ============

/// A PuTTY session that was skipped (unsupported protocol or no host).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PuttySkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing PuTTY saved sessions.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PuttyImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<PuttySkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import all bridgeable PuTTY sessions.
///
/// The `_path` argument exists only for signature symmetry with the other
/// bridge importers (`import_<src>(path: &Path)`); PuTTY's source is the
/// Windows registry, so the argument is ignored. On non-Windows hosts there
/// is no registry to read: this returns an empty result with an info log so
/// the bridge surface stays uniform and the file still compiles on the
/// Linux CI host (no `winreg` symbol is referenced outside `cfg(windows)`).
pub fn import_putty(_path: &Path) -> Result<PuttyImportResult, String> {
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        use winreg::RegKey;

        let source_path = format!(r"HKCU\{}", PUTTY_SESSIONS_KEY);
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let sessions = match hkcu.open_subkey_with_flags(PUTTY_SESSIONS_KEY, KEY_READ) {
            Ok(k) => k,
            Err(_) => {
                // No PuTTY sessions registered for this user: not an error.
                log::info!("[putty import] no PuTTY Sessions key in HKCU; nothing to import");
                return Ok(PuttyImportResult {
                    servers: Vec::new(),
                    skipped: Vec::new(),
                    source_path,
                    total_remotes: 0,
                });
            }
        };

        let mut servers = Vec::new();
        let mut skipped = Vec::new();
        let mut total_remotes = 0usize;

        for sub_name in sessions.enum_keys().flatten() {
            if total_remotes >= MAX_SESSIONS {
                log::warn!(
                    "[putty import] session cap {} reached; remaining sessions ignored",
                    MAX_SESSIONS
                );
                break;
            }
            total_remotes += 1;

            let decoded = decode_putty_name(&sub_name);
            let sub = match sessions.open_subkey_with_flags(&sub_name, KEY_READ) {
                Ok(s) => s,
                Err(e) => {
                    skipped.push(PuttySkippedRemote {
                        name: decoded,
                        kind: "session".to_string(),
                        reason: format!("registry read failed: {}", e),
                    });
                    continue;
                }
            };

            let mut kv: HashMap<String, String> = HashMap::new();
            // PortNumber is REG_DWORD; the rest are REG_SZ strings.
            if let Ok(v) = sub.get_value::<String, _>("HostName") {
                kv.insert("HostName".into(), v);
            }
            if let Ok(v) = sub.get_value::<u32, _>("PortNumber") {
                kv.insert("PortNumber".into(), v.to_string());
            }
            if let Ok(v) = sub.get_value::<String, _>("UserName") {
                kv.insert("UserName".into(), v);
            }
            if let Ok(v) = sub.get_value::<String, _>("Protocol") {
                kv.insert("Protocol".into(), v);
            }
            if let Ok(v) = sub.get_value::<String, _>("PublicKeyFile") {
                kv.insert("PublicKeyFile".into(), v);
            }

            match map_putty_session(&decoded, &kv) {
                Some(profile) => servers.push(profile),
                None => {
                    let proto = kv
                        .get("Protocol")
                        .map(|s| s.as_str())
                        .unwrap_or("ssh")
                        .to_string();
                    let reason = if kv
                        .get("HostName")
                        .map(|h| h.trim().is_empty())
                        .unwrap_or(true)
                    {
                        "session has no HostName".to_string()
                    } else {
                        format!("unsupported PuTTY protocol: {}", proto)
                    };
                    skipped.push(PuttySkippedRemote {
                        name: decoded,
                        kind: "session".to_string(),
                        reason,
                    });
                }
            }
        }

        Ok(PuttyImportResult {
            servers,
            skipped,
            source_path,
            total_remotes,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        log::info!(
            "[putty import] PuTTY stores sessions in the Windows registry; \
             not available on this OS, returning empty result"
        );
        Ok(PuttyImportResult {
            servers: Vec::new(),
            skipped: Vec::new(),
            source_path: r"HKCU\Software\SimonTatham\PuTTY\Sessions".to_string(),
            total_remotes: 0,
        })
    }
}

// ============ Export to PuTTY registry ============

/// A server profile to write back as a PuTTY saved session.
///
/// Mirrors `rclone_import::RcloneExportServer`. PuTTY never persists a
/// password, so the shared `passwords` map is accepted for API symmetry
/// but never consulted: no secret is ever written to the registry.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PuttyExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Write server profiles back as PuTTY saved sessions under
/// `HKCU\Software\SimonTatham\PuTTY\Sessions`.
///
/// The `passwords` map and `_out` path are accepted only for signature
/// symmetry with the other bridge exporters: PuTTY's destination is the
/// registry (not a file) and PuTTY never stores passwords, so neither is
/// used. Only SFTP/SSH profiles are written (`Protocol = ssh`); other
/// protocols are skipped. A referenced `options.private_key_path` is
/// written to `PublicKeyFile` by reference (no key bytes). On non-Windows
/// hosts this is a no-op returning `Ok(0)` with an info log so the file
/// compiles on the Linux CI host. Returns the number of sessions written.
pub fn export_putty(
    servers: &[PuttyExportServer],
    _passwords: &HashMap<String, String>,
    _out: &Path,
) -> Result<usize, String> {
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (sessions, _) = hkcu
            .create_subkey(PUTTY_SESSIONS_KEY)
            .map_err(|e| format!("open/create PuTTY Sessions key: {}", e))?;

        let mut written = 0usize;
        for server in servers {
            // Only the SSH family maps to a PuTTY session we can drive.
            let proto = server.protocol.as_deref().unwrap_or("sftp");
            if map_putty_protocol(proto).is_none() {
                continue;
            }
            if server.host.trim().is_empty() {
                continue;
            }

            let key_name = encode_putty_name(&server.name);
            let (sub, _) = sessions
                .create_subkey(&key_name)
                .map_err(|e| format!("create session '{}': {}", server.name, e))?;

            sub.set_value("HostName", &server.host)
                .map_err(|e| format!("write HostName for '{}': {}", server.name, e))?;
            // PortNumber is a REG_DWORD in PuTTY.
            sub.set_value("PortNumber", &server.port)
                .map_err(|e| format!("write PortNumber for '{}': {}", server.name, e))?;
            sub.set_value("Protocol", &"ssh".to_string())
                .map_err(|e| format!("write Protocol for '{}': {}", server.name, e))?;
            if !server.username.trim().is_empty() {
                sub.set_value("UserName", &server.username)
                    .map_err(|e| format!("write UserName for '{}': {}", server.name, e))?;
            }
            if let Some(key_file) = server
                .options
                .as_ref()
                .and_then(|o| o.get("private_key_path"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                sub.set_value("PublicKeyFile", &key_file.to_string())
                    .map_err(|e| format!("write PublicKeyFile for '{}': {}", server.name, e))?;
            }
            // No password is ever written: PuTTY has no password store.
            written += 1;
        }
        Ok(written)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = servers;
        log::info!(
            "[putty export] PuTTY sessions live in the Windows registry; \
             export is a no-op on this OS"
        );
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Pure-function tests: run on the Linux CI host ----

    #[test]
    fn test_map_putty_protocol() {
        assert_eq!(map_putty_protocol("ssh"), Some("sftp"));
        assert_eq!(map_putty_protocol("SSH"), Some("sftp"));
        assert_eq!(map_putty_protocol("sftp"), Some("sftp"));
        assert_eq!(map_putty_protocol(" ssh "), Some("sftp"));
        assert_eq!(map_putty_protocol("raw"), None);
        assert_eq!(map_putty_protocol("telnet"), None);
        assert_eq!(map_putty_protocol("rlogin"), None);
        assert_eq!(map_putty_protocol("serial"), None);
    }

    #[test]
    fn test_decode_putty_name() {
        assert_eq!(decode_putty_name("My%20Server"), "My Server");
        assert_eq!(decode_putty_name("plain"), "plain");
        assert_eq!(decode_putty_name("a%2Fb"), "a/b");
        assert_eq!(decode_putty_name("100%25"), "100%");
        // Invalid escape is passed through unchanged.
        assert_eq!(decode_putty_name("bad%zz"), "bad%zz");
        assert_eq!(decode_putty_name("trailing%2"), "trailing%2");
    }

    #[test]
    fn test_encode_decode_name_roundtrip() {
        for original in ["My Server", "prod.db", "a/b\\c", "100% sure", "lab-01"] {
            let encoded = encode_putty_name(original);
            assert_eq!(
                decode_putty_name(&encoded),
                original,
                "round-trip failed for {:?} (encoded {:?})",
                original,
                encoded
            );
        }
    }

    /// Parser unit test on a minimal realistic key/value map (the registry
    /// equivalent of a fixture), exercising the SSH key-by-reference path.
    #[test]
    fn test_map_putty_session_ssh_with_keyfile() {
        let mut kv = HashMap::new();
        kv.insert("HostName".to_string(), "192.168.1.50".to_string());
        kv.insert("PortNumber".to_string(), "2222".to_string());
        kv.insert("UserName".to_string(), "admin".to_string());
        kv.insert("Protocol".to_string(), "ssh".to_string());
        kv.insert(
            "PublicKeyFile".to_string(),
            r"C:\keys\id_ed25519.ppk".to_string(),
        );

        let p = map_putty_session("My NAS", &kv).expect("ssh session must map");
        assert_eq!(p.protocol.as_deref(), Some("sftp"));
        assert_eq!(p.host, "192.168.1.50");
        assert_eq!(p.port, 2222);
        assert_eq!(p.username, "admin");
        assert!(p.id.starts_with("putty-my-nas-"));

        // Secret-policy invariant: PuTTY never stores passwords, the key
        // is bridged by reference only.
        assert!(
            p.credential.is_none(),
            "PuTTY profile must have no credential"
        );
        assert_eq!(p.has_stored_credential, Some(false));
        let opts = p.options.expect("options with private_key_path");
        assert_eq!(
            opts.get("private_key_path").and_then(|v| v.as_str()),
            Some(r"C:\keys\id_ed25519.ppk")
        );
    }

    #[test]
    fn test_map_putty_session_default_port_and_no_key() {
        let mut kv = HashMap::new();
        kv.insert("HostName".to_string(), "ssh.example.com".to_string());
        kv.insert("Protocol".to_string(), "ssh".to_string());
        // No PortNumber, no UserName, no PublicKeyFile.

        let p = map_putty_session("Bare", &kv).expect("must map with defaults");
        assert_eq!(p.port, 22);
        assert_eq!(p.username, "");
        assert!(p.options.is_none());
        assert!(p.credential.is_none());
        assert_eq!(p.has_stored_credential, Some(false));
    }

    /// Secret-policy / protocol-filter test: a telnet session is skipped
    /// (no AeroFTP equivalent) and never yields a credential-bearing
    /// profile.
    #[test]
    fn test_map_putty_session_telnet_skipped() {
        let mut kv = HashMap::new();
        kv.insert("HostName".to_string(), "legacy.example.com".to_string());
        kv.insert("Protocol".to_string(), "telnet".to_string());
        assert!(
            map_putty_session("Legacy", &kv).is_none(),
            "telnet must be skipped"
        );
    }

    #[test]
    fn test_map_putty_session_missing_host_skipped() {
        let mut kv = HashMap::new();
        kv.insert("Protocol".to_string(), "ssh".to_string());
        assert!(
            map_putty_session("NoHost", &kv).is_none(),
            "ssh session without HostName must be skipped"
        );
    }

    /// On the Linux CI host the registry is unavailable: import returns an
    /// empty Ok result (no winreg symbol is reachable here).
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_import_putty_empty_on_non_windows() {
        let result =
            import_putty(Path::new("/ignored")).expect("non-windows import must be Ok(empty)");
        assert_eq!(result.servers.len(), 0);
        assert_eq!(result.skipped.len(), 0);
        assert_eq!(result.total_remotes, 0);
        assert!(result.source_path.contains("PuTTY"));
    }

    /// Export is a no-op on non-Windows and never errors.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_export_putty_noop_on_non_windows() {
        let servers = vec![PuttyExportServer {
            name: "x".to_string(),
            host: "h".to_string(),
            port: 22,
            username: "u".to_string(),
            protocol: Some("sftp".to_string()),
            options: None,
            provider_id: None,
            initial_path: None,
        }];
        let written =
            export_putty(&servers, &HashMap::new(), Path::new("/ignored")).expect("noop ok");
        assert_eq!(written, 0);
    }

    #[test]
    fn test_default_putty_config_path_is_none() {
        // Registry-based source: there is no file to point at on any OS.
        assert!(default_putty_config_path().is_none());
    }

    /// Round-trip idempotence for a registry source is metadata-only (we
    /// assert exactly that): map a synthetic session, run it through the
    /// export shape, and confirm host/port/user/protocol/key-path survive
    /// with no credential ever materialising.
    #[test]
    fn test_metadata_roundtrip_idempotence() {
        let mut kv = HashMap::new();
        kv.insert("HostName".to_string(), "10.0.0.9".to_string());
        kv.insert("PortNumber".to_string(), "22".to_string());
        kv.insert("UserName".to_string(), "root".to_string());
        kv.insert("Protocol".to_string(), "ssh".to_string());
        kv.insert("PublicKeyFile".to_string(), "/home/u/key.ppk".to_string());

        let p1 = map_putty_session("lab box", &kv).expect("map");
        let exp = PuttyExportServer {
            name: p1.name.clone(),
            host: p1.host.clone(),
            port: p1.port,
            username: p1.username.clone(),
            protocol: p1.protocol.clone(),
            options: p1.options.clone(),
            provider_id: p1.provider_id.clone(),
            initial_path: p1.initial_path.clone(),
        };

        // Reconstruct the kv that export_putty would write, then re-map.
        let mut kv2 = HashMap::new();
        kv2.insert("HostName".to_string(), exp.host.clone());
        kv2.insert("PortNumber".to_string(), exp.port.to_string());
        kv2.insert("UserName".to_string(), exp.username.clone());
        kv2.insert("Protocol".to_string(), "ssh".to_string());
        if let Some(k) = exp
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str())
        {
            kv2.insert("PublicKeyFile".to_string(), k.to_string());
        }
        let p2 = map_putty_session(&exp.name, &kv2).expect("re-map");

        assert_eq!(p1.host, p2.host);
        assert_eq!(p1.port, p2.port);
        assert_eq!(p1.username, p2.username);
        assert_eq!(p1.protocol, p2.protocol);
        assert_eq!(p1.name, p2.name);
        assert_eq!(
            p1.options.as_ref().and_then(|o| o.get("private_key_path")),
            p2.options.as_ref().and_then(|o| o.get("private_key_path"))
        );
        // Metadata-only idempotence: no credential on either side.
        assert!(p1.credential.is_none() && p2.credential.is_none());
        assert_eq!(p1.has_stored_credential, p2.has_stored_credential);
    }
}
