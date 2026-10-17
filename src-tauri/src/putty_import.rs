// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from PuTTY's saved sessions.
//!
//! PuTTY does NOT use a config file: its sessions live in the Windows
//! registry under `HKCU\Software\SimonTatham\PuTTY\Sessions\<name>`. Each
//! session key carries `HostName`, `PortNumber` (REG_DWORD), `UserName`,
//! `Protocol` (`ssh`/`raw`/`telnet`/`rlogin`/`serial`; modern builds also
//! advertise `sftp` indirectly via SSH) and `PublicKeyFile` (path to a
//! `.ppk`/key file). A session's key name is its name in the Windows ANSI
//! code page with some bytes written as `%XX` (PuTTY's
//! `escape_registry_key`/`unescape_registry_key`), so `My%20Server` is the
//! session `My Server`; see [`encode_putty_name`].
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

// ---- Session names ----
//
// PuTTY keeps a session under the key its `escape_registry_key` makes of the
// session name (`windows/utils/escape_registry_key.c` in PuTTY's source), and
// finds a session again the same way: loading or deleting a session escapes
// the name PuTTY shows and opens that key (`open_settings_r`, `del_settings`
// in `windows/storage.c`). A key written in any other form is still listed
// (the listing unescapes it), but under a name that escapes to a different
// key, so PuTTY loads it with its built-in defaults (no host) and cannot
// delete it.
//
// The name PuTTY escapes is not UTF-8. PuTTY reads it from the Saved Sessions
// box with `GetWindowTextA` (`windows/utils/getdlgitemtext_alloc.c`), is built
// without `UNICODE` and ships no UTF-8 `activeCodePage` manifest, so the
// bytes are in the system ANSI code page: Windows-1252 on a Western install,
// UTF-8 only where the "Beta: Use Unicode UTF-8" option is on. WinSCP reads
// PuTTY's names the same way (`PuttyStr`, an `AnsiString`, in
// `source/core/HierarchicalStorage.cpp`).
//
// The helpers are reached only from the Windows registry paths and the unit
// tests, so they are `#[cfg]`-gated to avoid dead-code warnings on a
// non-Windows lib build (the CI host).

/// The Windows ANSI code page PuTTY's session names are written in.
#[cfg(any(test, target_os = "windows"))]
trait AnsiCodePage {
    /// `s` in the code page, as an ANSI edit box hands it to PuTTY: a
    /// character the code page has no byte for becomes its best fit or the
    /// code page's default character (`?`).
    fn encode(&self, s: &str) -> Result<Vec<u8>, String>;
    /// Bytes in the code page, as text.
    fn decode(&self, bytes: &[u8]) -> Result<String, String>;
}

/// This system's ANSI code page (`CP_ACP`), the one PuTTY uses.
#[cfg(target_os = "windows")]
struct SystemAnsiCodePage;

#[cfg(target_os = "windows")]
impl AnsiCodePage for SystemAnsiCodePage {
    fn encode(&self, s: &str) -> Result<Vec<u8>, String> {
        use windows::core::PCSTR;
        use windows::Win32::Globalization::{WideCharToMultiByte, CP_ACP};

        let wide: Vec<u16> = s.encode_utf16().collect();
        if wide.is_empty() {
            return Ok(Vec::new());
        }
        // Flags 0: best fit and the default character, the conversion an
        // ANSI edit box applies to what is typed into it.
        // SAFETY: the wrapper passes `wide` with its own length, no output
        // buffer (a size query) and null default-character pointers.
        let len = unsafe { WideCharToMultiByte(CP_ACP, 0, &wide, None, PCSTR::null(), None) };
        if len <= 0 {
            return Err(format!(
                "convert to the Windows code page: {}",
                windows::core::Error::from_win32()
            ));
        }
        let mut out = vec![0u8; len as usize];
        // SAFETY: as above, with `out` passed with its own length.
        let written =
            unsafe { WideCharToMultiByte(CP_ACP, 0, &wide, Some(&mut out), PCSTR::null(), None) };
        if written <= 0 {
            return Err(format!(
                "convert to the Windows code page: {}",
                windows::core::Error::from_win32()
            ));
        }
        out.truncate(written as usize);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<String, String> {
        use windows::Win32::Globalization::{
            MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
        };

        if bytes.is_empty() {
            return Ok(String::new());
        }
        // SAFETY: the wrapper passes `bytes` with its own length and no
        // output buffer (a size query).
        let len =
            unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
        if len <= 0 {
            return Err(format!(
                "convert from the Windows code page: {}",
                windows::core::Error::from_win32()
            ));
        }
        let mut wide = vec![0u16; len as usize];
        // SAFETY: as above, with `wide` passed with its own length.
        let written = unsafe {
            MultiByteToWideChar(
                CP_ACP,
                MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                bytes,
                Some(&mut wide),
            )
        };
        if written <= 0 {
            return Err(format!(
                "convert from the Windows code page: {}",
                windows::core::Error::from_win32()
            ));
        }
        wide.truncate(written as usize);
        Ok(String::from_utf16_lossy(&wide))
    }
}

/// PuTTY's `escape_registry_key`: the bytes of a session name as the key
/// name PuTTY keeps it under. A byte becomes `%XX` (uppercase hex) when it
/// is a space, `\`, `*`, `?`, `%`, below a space, above `~` (DEL and every
/// byte of a non-ASCII character) or a `.` in first position. `/` is kept:
/// a registry key name may hold it, and PuTTY does not escape it.
#[cfg(any(test, target_os = "windows"))]
fn escape_registry_key(name: &[u8]) -> String {
    let mut out = String::with_capacity(name.len());
    for (idx, &b) in name.iter().enumerate() {
        let escape = matches!(b, b' ' | b'\\' | b'*' | b'?' | b'%')
            || !(b' '..=b'~').contains(&b)
            || (idx == 0 && b == b'.');
        if escape {
            out.push_str(&format!("%{:02X}", b));
        } else {
            // What is left is printable ASCII: one byte is one char.
            out.push(char::from(b));
        }
    }
    out
}

/// PuTTY's `unescape_registry_key`: each `%XX` back to its byte, every
/// other byte as it is. PuTTY decodes any `%` followed by two characters
/// without checking that they are hex digits; this decodes only real hex
/// escapes and keeps anything else as written. The two agree on every key
/// name `escape_registry_key` writes.
#[cfg(any(test, target_os = "windows"))]
fn unescape_registry_key(key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(key.len());
    let mut i = 0;
    while i < key.len() {
        if key[i] == b'%' && i + 2 < key.len() {
            let hi = char::from(key[i + 1]).to_digit(16);
            let lo = char::from(key[i + 2]).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(key[i]);
        i += 1;
    }
    out
}

/// The registry key name PuTTY keeps the session `name` under, the one
/// PuTTY's own load, save and delete of that name address: `name` in the
/// ANSI code page `cp`, then [`escape_registry_key`]. The result is
/// printable ASCII.
///
/// A character `cp` has no byte for cannot be part of a PuTTY session name
/// (PuTTY's Saved Sessions box could not hold it either): it is written as
/// `cp`'s best fit or `?`, and the session comes back from PuTTY under that
/// name.
#[cfg(any(test, target_os = "windows"))]
fn encode_putty_name(name: &str, cp: &dyn AnsiCodePage) -> Result<String, String> {
    Ok(escape_registry_key(&cp.encode(name)?))
}

/// The session name PuTTY shows for the registry key name `key`. PuTTY
/// lists its sessions through the ANSI registry API, so the key name is
/// first taken in the code page (a key name PuTTY wrote is ASCII, which no
/// code page changes), then unescaped, then read in the code page.
#[cfg(any(test, target_os = "windows"))]
fn decode_putty_name(key: &str, cp: &dyn AnsiCodePage) -> Result<String, String> {
    cp.decode(&unescape_registry_key(&cp.encode(key)?))
}

/// The key name AeroFTP's exporter wrote for `name` before it followed
/// PuTTY's escape, when that is not `current` (the key name it writes now).
///
/// The old form escaped `/` and left space, `*`, `?`, DEL and non-ASCII
/// unescaped, the last as one Latin-1 character per UTF-8 byte (`Café`
/// became `CafÃ©`). Where the two forms differ, the old one holds a raw
/// space, `*`, `?`, DEL, a non-ASCII character or a `%2F`, none of which
/// `escape_registry_key` writes: PuTTY never wrote that key, and lists it
/// under a name that escapes to another key, so it can neither load nor
/// delete it.
#[cfg(any(test, target_os = "windows"))]
fn legacy_putty_key_name(name: &str, current: &str) -> Option<String> {
    let mut old = String::with_capacity(name.len());
    for (idx, b) in name.bytes().enumerate() {
        if b < 0x20 || b == b'%' || b == b'\\' || b == b'/' || (idx == 0 && b == b'.') {
            old.push_str(&format!("%{:02X}", b));
        } else {
            // The old defect, reproduced on purpose to find its keys.
            old.push(char::from(b));
        }
    }
    (!old.eq_ignore_ascii_case(current)).then_some(old)
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
        ..Default::default()
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

            let decoded = match decode_putty_name(&sub_name, &SystemAnsiCodePage) {
                Ok(name) => name,
                Err(e) => {
                    skipped.push(PuttySkippedRemote {
                        name: sub_name,
                        kind: "session".to_string(),
                        reason: format!("session name unreadable: {}", e),
                    });
                    continue;
                }
            };
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
/// written to `PublicKeyFile` by reference (no key bytes). Each session is
/// written under the key PuTTY itself uses for its name
/// ([`encode_putty_name`]), and the key an earlier AeroFTP export wrote for
/// that name in the old form, which PuTTY lists but can neither load nor
/// delete ([`legacy_putty_key_name`]), is removed. On non-Windows hosts this
/// is a no-op returning `Ok(0)` with an info log so the file compiles on the
/// Linux CI host. Returns the number of sessions written.
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

            let key_name = encode_putty_name(&server.name, &SystemAnsiCodePage)
                .map_err(|e| format!("session name '{}': {}", server.name, e))?;
            match decode_putty_name(&key_name, &SystemAnsiCodePage) {
                Ok(shown) if shown == server.name => {}
                Ok(shown) => log::warn!(
                    "[putty export] '{}' has characters the Windows code page cannot hold; \
                     PuTTY lists it as '{}'",
                    server.name,
                    shown
                ),
                Err(e) => log::warn!(
                    "[putty export] cannot tell how PuTTY lists '{}': {}",
                    server.name,
                    e
                ),
            }
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

            if let Some(legacy) = legacy_putty_key_name(&server.name, &key_name) {
                match sessions.delete_subkey(&legacy) {
                    Ok(()) => log::info!(
                        "[putty export] removed '{}', the key an earlier export wrote for '{}'; \
                         PuTTY could not open it",
                        legacy,
                        server.name
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => log::warn!(
                        "[putty export] could not remove '{}', the key an earlier export wrote \
                         for '{}': {}",
                        legacy,
                        server.name,
                        e
                    ),
                }
            }
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

    /// Windows-1252, the ANSI code page of a Western install, for the
    /// characters these tests use. No best-fit table: anything else is `?`.
    struct Cp1252;

    impl AnsiCodePage for Cp1252 {
        fn encode(&self, s: &str) -> Result<Vec<u8>, String> {
            Ok(s.chars()
                .map(|c| match c {
                    '\u{0}'..='\u{7f}' | '\u{a0}'..='\u{ff}' => c as u8,
                    '\u{20ac}' => 0x80,
                    _ => b'?',
                })
                .collect())
        }

        fn decode(&self, bytes: &[u8]) -> Result<String, String> {
            Ok(bytes
                .iter()
                .map(|&b| match b {
                    0x80 => '\u{20ac}',
                    0x81..=0x9f => char::REPLACEMENT_CHARACTER,
                    _ => char::from(b),
                })
                .collect())
        }
    }

    /// The UTF-8 code page (65001), set where Windows' "Beta: Use Unicode
    /// UTF-8" option is on.
    struct Utf8CodePage;

    impl AnsiCodePage for Utf8CodePage {
        fn encode(&self, s: &str) -> Result<Vec<u8>, String> {
            Ok(s.as_bytes().to_vec())
        }

        fn decode(&self, bytes: &[u8]) -> Result<String, String> {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
    }

    /// The bytes PuTTY's `escape_registry_key` escapes, each on its own, and
    /// every other printable ASCII byte kept as it is.
    #[test]
    fn test_escape_registry_key_escapes_what_putty_escapes() {
        for (byte, key) in [
            (b' ', "%20"),
            (b'\\', "%5C"),
            (b'*', "%2A"),
            (b'?', "%3F"),
            (b'%', "%25"),
            (0x00, "%00"),
            (b'\t', "%09"),
            (0x1f, "%1F"),
            (0x7f, "%7F"),
            (0x80, "%80"),
            (0xe9, "%E9"),
            (0xff, "%FF"),
        ] {
            assert_eq!(escape_registry_key(&[b'a', byte]), format!("a{key}"));
        }
        for byte in 0x21..=0x7eu8 {
            if matches!(byte, b'\\' | b'*' | b'?' | b'%') {
                continue;
            }
            assert_eq!(
                escape_registry_key(&[byte, byte]),
                format!("{0}{0}", char::from(byte)).replacen('.', "%2E", 1),
                "byte {byte:#04x}"
            );
        }
        // `/` is kept: PuTTY writes the session `a/b` as the key `a/b`.
        assert_eq!(escape_registry_key(b"a/b"), "a/b");
    }

    /// Only a `.` in first position is escaped, and only that one.
    #[test]
    fn test_escape_registry_key_leading_dot() {
        assert_eq!(escape_registry_key(b".hidden"), "%2Ehidden");
        assert_eq!(escape_registry_key(b"..x"), "%2E.x");
        assert_eq!(escape_registry_key(b"prod.db"), "prod.db");
        assert_eq!(escape_registry_key(b"."), "%2E");
    }

    #[test]
    fn test_unescape_registry_key() {
        assert_eq!(unescape_registry_key(b"My%20Server"), b"My Server");
        assert_eq!(unescape_registry_key(b"plain"), b"plain");
        assert_eq!(unescape_registry_key(b"a/b"), b"a/b");
        assert_eq!(unescape_registry_key(b"100%25"), b"100%");
        assert_eq!(unescape_registry_key(b"Caf%E9"), b"Caf\xe9");
        assert_eq!(unescape_registry_key(b"%2Ehidden"), b".hidden");
        // Not a hex escape: kept as written.
        assert_eq!(unescape_registry_key(b"bad%zz"), b"bad%zz");
        assert_eq!(unescape_registry_key(b"trailing%2"), b"trailing%2");
    }

    /// A non-ASCII name is written in the system's ANSI code page, the bytes
    /// PuTTY would have saved for it: `Caf%E9` on Windows-1252, not the
    /// `CafÃ©` the old encoder wrote.
    #[test]
    fn test_encode_putty_name_uses_the_ansi_code_page() {
        assert_eq!(encode_putty_name("Café", &Cp1252).unwrap(), "Caf%E9");
        assert_eq!(encode_putty_name("€ box", &Cp1252).unwrap(), "%80%20box");
        assert_eq!(
            encode_putty_name("Café", &Utf8CodePage).unwrap(),
            "Caf%C3%A9"
        );
        for cp in [&Cp1252 as &dyn AnsiCodePage, &Utf8CodePage] {
            let key = encode_putty_name("Ünïcødé ñame", cp).unwrap();
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'*' | b'?')),
                "a key name is printable ASCII without space, \\, * or ?: {key:?}"
            );
        }
    }

    /// A character the code page has no byte for cannot be in a PuTTY
    /// session name: it is written as `?`, which is what PuTTY's Saved
    /// Sessions box would have made of it, and PuTTY lists it that way.
    #[test]
    fn test_encode_putty_name_outside_the_code_page() {
        let key = encode_putty_name("日本 box", &Cp1252).unwrap();
        assert_eq!(key, "%3F%3F%20box");
        assert_eq!(decode_putty_name(&key, &Cp1252).unwrap(), "?? box");
        let key = encode_putty_name("日本 box", &Utf8CodePage).unwrap();
        assert_eq!(decode_putty_name(&key, &Utf8CodePage).unwrap(), "日本 box");
    }

    /// Export then import gives the name back, non-ASCII included, and the
    /// key name is the one PuTTY's own load and save of that name address:
    /// escaping the name PuTTY shows gives the same key again.
    #[test]
    fn test_encode_decode_name_roundtrip() {
        for original in [
            "My Server",
            "prod.db",
            ".dotfirst",
            "a/b\\c",
            "100% sure",
            "what?*",
            "tab\tname",
            "Café",
            "Ünïcødé ñame",
            "€ box",
            "lab-01",
        ] {
            for cp in [&Cp1252 as &dyn AnsiCodePage, &Utf8CodePage] {
                let key = encode_putty_name(original, cp).unwrap();
                assert_eq!(
                    decode_putty_name(&key, cp).unwrap(),
                    original,
                    "round-trip failed for {original:?} (key {key:?})"
                );
                assert_eq!(
                    encode_putty_name(&decode_putty_name(&key, cp).unwrap(), cp).unwrap(),
                    key,
                    "PuTTY would address another key for {original:?}"
                );
            }
        }
    }

    /// The key the old encoder wrote, found so the export can remove it, and
    /// only where it differs from the key written now.
    #[test]
    fn test_legacy_putty_key_name() {
        let legacy =
            |name: &str| legacy_putty_key_name(name, &encode_putty_name(name, &Cp1252).unwrap());
        assert_eq!(legacy("My Server").as_deref(), Some("My Server"));
        assert_eq!(legacy("a/b").as_deref(), Some("a%2Fb"));
        assert_eq!(legacy("what?").as_deref(), Some("what?"));
        assert_eq!(legacy("Café").as_deref(), Some("CafÃ©"));
        assert_eq!(legacy("plain"), None);
        assert_eq!(legacy(".dot%x"), None);
        assert_eq!(legacy("tab\tname"), None);
    }

    /// The property the export's removal rests on: a key the old encoder
    /// wrote where the forms differ is one PuTTY cannot open, because the
    /// name PuTTY lists for it escapes to another key. So it is never a key
    /// PuTTY wrote, nor the one written now.
    #[test]
    fn test_legacy_putty_key_is_one_putty_cannot_open() {
        for name in [
            "My Server",
            "a/b",
            "what?*",
            "Café",
            "Ünïcødé ñame",
            "x\u{7f}y",
        ] {
            for cp in [&Cp1252 as &dyn AnsiCodePage, &Utf8CodePage] {
                let current = encode_putty_name(name, cp).unwrap();
                let old = legacy_putty_key_name(name, &current).expect("forms differ");
                let putty_lookup =
                    encode_putty_name(&decode_putty_name(&old, cp).unwrap(), cp).unwrap();
                assert!(
                    !putty_lookup.eq_ignore_ascii_case(&old),
                    "PuTTY can open {old:?} for {name:?}"
                );
                assert!(!old.eq_ignore_ascii_case(&current));
            }
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
