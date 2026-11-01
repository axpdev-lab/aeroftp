// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from WinSCP configuration files.
//!
//! Parses `WinSCP.ini` or exported session files (INI format), maps WinSCP
//! `FSProtocol` + `Ftps` fields to AeroFTP ProviderType, and de-obfuscates
//! WinSCP passwords (XOR-based with well-known algorithm: NOT real encryption).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from WinSCP's reversible obfuscation to proper authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ WinSCP password de-obfuscation ============
// Source: WinSCP source code (Security.cpp / ScpPassword)
// This is NOT encryption: the algorithm is public and fully reversible.

const PWALG_SIMPLE_MAGIC: u8 = 0xA3;
const PWALG_SIMPLE_FLAG: u8 = 0xFF;

/// Parse a single hex character to its numeric value (0-15).
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decode the next byte from a WinSCP hex-nibble stream.
/// Consumes 2 characters from the iterator.
fn dec_next_char(nibbles: &mut impl Iterator<Item = u8>) -> Option<u8> {
    let a = nibbles.next()?;
    let b = nibbles.next()?;
    Some((!((a << 4) | b)) ^ PWALG_SIMPLE_MAGIC)
}

/// De-obfuscate a WinSCP password.
///
/// The `key` parameter is `username + hostname` (concatenated).
/// If `PasswordPlain` is available, use that directly instead.
pub fn decode_winscp_password(hex_str: &str, key: &str) -> Result<String, String> {
    if hex_str.is_empty() {
        return Ok(String::new());
    }

    // Parse hex string into nibble values
    let nibble_values: Vec<u8> = hex_str
        .bytes()
        .map(|b| hex_nibble(b).ok_or_else(|| format!("invalid hex char: {}", b as char)))
        .collect::<Result<Vec<_>, _>>()?;

    let mut nibbles = nibble_values.into_iter();

    // Read flag byte
    let flag = dec_next_char(&mut nibbles).ok_or("password too short: missing flag")?;

    let length: usize;

    if flag == PWALG_SIMPLE_FLAG {
        // Extended format: version byte + length
        let version = dec_next_char(&mut nibbles).ok_or("password too short: missing version")?;
        if version == 0x02 {
            // 16-bit length (big-endian)
            let hi = dec_next_char(&mut nibbles).ok_or("password too short: missing length hi")?;
            let lo = dec_next_char(&mut nibbles).ok_or("password too short: missing length lo")?;
            length = ((hi as usize) << 8) | (lo as usize);
        } else {
            // 8-bit length
            length =
                dec_next_char(&mut nibbles).ok_or("password too short: missing length")? as usize;
        }
    } else {
        length = flag as usize;
    }

    // Read and skip random padding
    let shift = dec_next_char(&mut nibbles).ok_or("password too short: missing shift")? as usize;
    for _ in 0..shift {
        // Skip `shift` decoded chars (each consumes 2 nibbles)
        if dec_next_char(&mut nibbles).is_none() {
            break;
        }
    }

    // Read the actual data
    let mut result = Vec::with_capacity(length);
    for _ in 0..length {
        match dec_next_char(&mut nibbles) {
            Some(ch) => result.push(ch),
            None => break,
        }
    }

    let decoded = String::from_utf8(result)
        .map_err(|_| "password contains invalid UTF-8 after de-obfuscation".to_string())?;

    // In extended format, the key (username+host) is prepended to the plaintext:
    // strip it. Both `key` and `decoded` come straight from the imported file, so
    // `key.len()` is NOT a guaranteed UTF-8 char boundary of `decoded`; a raw
    // byte-slice `decoded[key.len()..]` panics on a crafted WinSCP.ini whose
    // de-obfuscated password lands a multi-byte char across that index. Use a
    // boundary-safe slice and fall back to the raw decoded string when the prefix
    // does not line up (shorter than the key, or a mid-char boundary).
    // CLAUDE-AV-B9-01
    if flag == PWALG_SIMPLE_FLAG {
        match decoded.get(key.len()..) {
            Some(stripped) => Ok(stripped.to_string()),
            None => Ok(decoded),
        }
    } else {
        Ok(decoded)
    }
}

// ============ Session names ============
//
// WinSCP keeps a session under `[Sessions\<name>]`, where `<name>` is its
// `MungeStr` of the session name (`source/core/HierarchicalStorage.cpp` in
// WinSCP's source; `OpenSubKey` -> `MungeKeyName` -> `MungeStr` with
// `ForceAnsi` off), and reads it back with `UnMungeStr`.

/// The UTF-8 BOM WinSCP puts in front of a non-ASCII session name, so
/// `UnMungeStr` reads what follows as UTF-8 and not in the ANSI code page.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Decode a WinSCP session name, as WinSCP's `UnMungeStr` does: each `%XX`
/// back to its byte, then, when the bytes start with the UTF-8 BOM, the
/// UTF-8 text after it (`%EF%BB%BFCaf%C3%A9` -> `Café`). Bytes without the
/// BOM are what WinSCP reads in the ANSI code page of the machine that wrote
/// them, which a file import cannot know: they are read as UTF-8 when they
/// are UTF-8 (ASCII always is) and lossily otherwise. A `%` not followed by
/// two hex digits is kept as written.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded_bytes = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                decoded_bytes.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        decoded_bytes.push(bytes[i]);
        i += 1;
    }
    let text = decoded_bytes
        .strip_prefix(UTF8_BOM)
        .unwrap_or(&decoded_bytes);
    String::from_utf8_lossy(text).into_owned()
}

/// A profile name as the WinSCP session name WinSCP itself writes for it:
/// WinSCP's `MungeStr` with `ForceAnsi` off. A name with a non-ASCII
/// character is its UTF-8 bytes after the UTF-8 BOM (`ToUnicode` adds the BOM
/// when the UTF-8 form is longer than the UTF-16 one, which is exactly when a
/// character is not ASCII), an ASCII name its own bytes. Those bytes then go
/// through PuTTY's `escape_registry_key`, which WinSCP calls: `%XX` for a
/// space, `\`, `*`, `?`, `%`, a byte below space or above `~` (the BOM
/// included) and a `.` in first position. `/` is kept, and WinSCP reads it as
/// a folder separator.
fn winscp_session_name(name: &str) -> String {
    let mut bytes = Vec::with_capacity(UTF8_BOM.len() + name.len());
    if !name.is_ascii() {
        bytes.extend_from_slice(UTF8_BOM);
    }
    bytes.extend_from_slice(name.as_bytes());

    let mut out = String::with_capacity(bytes.len());
    for (idx, &b) in bytes.iter().enumerate() {
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

// ============ INI Parser ============

/// A parsed WinSCP session: key/value pairs.
type WinScpSession = HashMap<String, String>;

/// Parse WinSCP.ini format, returning only `[Sessions\*]` sections.
fn parse_winscp_ini(content: &str) -> HashMap<String, WinScpSession> {
    let mut sessions: HashMap<String, WinScpSession> = HashMap::new();
    let mut current_section: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        // Section header: [Sessions\name]
        if line.starts_with('[') && line.ends_with(']') {
            let section = &line[1..line.len() - 1];
            if let Some(name) = section.strip_prefix("Sessions\\") {
                // Skip Default Settings
                let decoded = url_decode(name);
                if decoded != "Default Settings" && sessions.len() < MAX_SESSIONS {
                    sessions.entry(decoded.clone()).or_default();
                    current_section = Some(decoded);
                } else {
                    current_section = None;
                }
            } else {
                current_section = None;
            }
            continue;
        }

        // Key=Value (WinSCP uses = without spaces typically)
        if let Some(ref section) = current_section {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                if let Some(sec) = sessions.get_mut(section) {
                    sec.insert(key, value);
                }
            }
        }
    }

    sessions
}

// ============ Protocol Mapping ============

struct MappedProfile {
    protocol: String,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
}

/// Map a WinSCP session to an AeroFTP profile.
///
/// Protocol determination: `FSProtocol` + `Ftps` fields combined.
fn map_session(name: &str, fields: &WinScpSession) -> Option<MappedProfile> {
    let host = fields.get("HostName").filter(|h| !h.is_empty())?;
    let fs_protocol: u32 = fields
        .get("FSProtocol")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1); // default: SFTP
    let ftps: u32 = fields.get("Ftps").and_then(|v| v.parse().ok()).unwrap_or(0);
    let port: u32 = fields
        .get("PortNumber")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0); // 0 means "use default"
    let username = fields.get("UserName").cloned().unwrap_or_default();

    // Decode password
    let password = if let Some(plain) = fields.get("PasswordPlain") {
        if !plain.is_empty() {
            Some(plain.clone())
        } else {
            None
        }
    } else if let Some(hex) = fields.get("Password") {
        if !hex.is_empty() {
            let key = format!("{}{}", username, host);
            match decode_winscp_password(hex, &key) {
                Ok(pw) if !pw.is_empty() => Some(pw),
                Ok(_) => None,
                Err(e) => {
                    log::warn!("WinSCP password decode failed for '{}': {}", name, e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let remote_dir = fields
        .get("RemoteDirectory")
        .filter(|d| !d.is_empty())
        .cloned();

    let (protocol, default_port) = match (fs_protocol, ftps) {
        // WinSCP's 0 is SCP only, 1 SFTP with SCP fallback (its default)
        // and 2 SFTP only: SCP, and SFTP with the fallback on or off.
        (0, _) => ("scp", 22u32),
        (1, _) | (2, _) => ("sftp", 22),
        // FTP
        (5, 0) => ("ftp", 21),
        // FTPS implicit (port 990)
        (5, 1) => ("ftps", 990),
        // FTPS explicit SSL/TLS
        (5, 2) | (5, 3) => ("ftps", 21),
        // WebDAV
        (6, 0) => ("webdav", 80),
        (6, 1) => ("webdav", 443),
        // S3
        (7, _) => ("s3", 443),
        // Unknown protocol: skip
        _ => {
            log::info!(
                "WinSCP session '{}': unsupported FSProtocol={} Ftps={}",
                name,
                fs_protocol,
                ftps
            );
            return None;
        }
    };

    let actual_port = if port == 0 { default_port } else { port };

    // Build protocol-specific options
    let mut options = serde_json::Map::new();

    match protocol {
        "webdav"
            // Ftps=1 means HTTPS for WebDAV
            if ftps == 1 => {
                options.insert("useSsl".to_string(), serde_json::Value::Bool(true));
            }
        "ftps" => {
            // Implicit vs explicit, under `tlsMode`: the key the GUI stores
            // and the connection reads (an `ftpsMode` was read by nothing, so
            // an explicit site on port 21 connected as implicit FTPS).
            if ftps == 1 {
                options.insert(
                    "tlsMode".to_string(),
                    serde_json::Value::String("implicit".to_string()),
                );
            } else {
                options.insert(
                    "tlsMode".to_string(),
                    serde_json::Value::String("explicit".to_string()),
                );
            }
        }
        "sftp" if fs_protocol == 2 => {
            options.insert(
                "allowScpFallback".to_string(),
                serde_json::Value::Bool(false),
            );
        }
        "s3" => {
            if let Some(region) = fields.get("S3DefaultRegion").filter(|r| !r.is_empty()) {
                options.insert(
                    "region".to_string(),
                    serde_json::Value::String(region.clone()),
                );
            }
            let url_style = fields
                .get("S3UrlStyle")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            if url_style == 1 {
                options.insert("forcePathStyle".to_string(), serde_json::Value::Bool(true));
            }
        }
        _ => {}
    }

    let options_val = if options.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(options))
    };

    Some(MappedProfile {
        protocol: protocol.to_string(),
        host: host.clone(),
        port: actual_port,
        username,
        password,
        options: options_val,
        initial_path: remote_dir,
    })
}

// ============ Default config path detection ============

/// Returns the default WinSCP.ini path for the current platform.
///
/// WinSCP is Windows-only, but users might export sessions as .ini files
/// and import them on any platform.
pub fn default_winscp_config_path() -> Option<PathBuf> {
    // Primary: %APPDATA%\WinSCP\WinSCP.ini
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = PathBuf::from(appdata).join("WinSCP").join("WinSCP.ini");
            if path.exists() {
                return Some(path);
            }
        }
        // Portable: WinSCP.ini next to WinSCP.exe (common in portable installs)
        // We can't reliably detect this without user input
    }

    // On non-Windows, there's no default location: user must browse
    None
}

// ============ Public API ============

/// Result of importing WinSCP config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WinScpImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<WinScpSkippedSession>,
    pub source_path: String,
    pub total_sessions: usize,
}

/// A session that was skipped (unsupported protocol).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WinScpSkippedSession {
    pub name: String,
    pub fs_protocol: String,
    pub reason: String,
}

/// Maximum number of sessions to parse (defense against DoS with crafted files).
const MAX_SESSIONS: usize = 10_000;

/// Import all supported sessions from a WinSCP.ini or exported sessions file.
pub fn import_winscp(config_path: &Path) -> Result<WinScpImportResult, String> {
    let content =
        std::fs::read_to_string(config_path).map_err(|e| format!("Read WinSCP config: {}", e))?;

    let sessions = parse_winscp_ini(&content);
    let total_sessions = sessions.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for (name, fields) in &sessions {
        let fs_protocol_str = fields.get("FSProtocol").cloned().unwrap_or_default();

        match map_session(name, fields) {
            Some(mapped) => {
                let id = format!(
                    "winscp-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                // Extract display name: last segment after '/' (folder hierarchy)
                let display_name = name.rsplit('/').next().unwrap_or(name).to_string();

                servers.push(ServerProfileExport {
                    id,
                    name: display_name,
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
                    has_stored_credential: None,
                    public_url_base: None,
                    ..Default::default()
                });
            }
            None => {
                let reason = format!("unsupported FSProtocol={}", fs_protocol_str);
                skipped.push(WinScpSkippedSession {
                    name: name.clone(),
                    fs_protocol: fs_protocol_str,
                    reason,
                });
            }
        }
    }

    Ok(WinScpImportResult {
        servers,
        skipped,
        source_path: config_path.display().to_string(),
        total_sessions,
    })
}

// ============ WinSCP password obfuscation (for export) ============

/// Encode a single byte as two hex nibbles using WinSCP's algorithm.
fn enc_next_char(byte: u8) -> String {
    let encoded = (!byte) ^ PWALG_SIMPLE_MAGIC;
    format!("{:02X}", encoded)
}

/// Obfuscate a password using WinSCP's algorithm (for export).
///
/// The `key` parameter is `username + hostname` (concatenated).
pub fn obfuscate_winscp_password(password: &str, key: &str) -> String {
    let full_text = format!("{}{}", key, password);
    let mut result = String::new();

    // Flag byte: 0xFF (extended format)
    result.push_str(&enc_next_char(PWALG_SIMPLE_FLAG));

    if full_text.len() > 255 {
        // Version 0x02: 16-bit length (big-endian)
        result.push_str(&enc_next_char(0x02));
        result.push_str(&enc_next_char((full_text.len() >> 8) as u8));
        result.push_str(&enc_next_char((full_text.len() & 0xFF) as u8));
    } else {
        // Version 0x00: 8-bit length
        result.push_str(&enc_next_char(0x00));
        result.push_str(&enc_next_char(full_text.len() as u8));
    }

    // Random shift (0 for deterministic output)
    let shift: u8 = 0;
    result.push_str(&enc_next_char(shift));

    // Encode the full text (key + password)
    for byte in full_text.bytes() {
        result.push_str(&enc_next_char(byte));
    }

    result
}

// ============ Export ============

/// Server data for WinSCP export.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WinScpExportServer {
    /// The profile's id: the key of its secret in the passwords map
    /// ([`crate::bridge_shared::export_secret`]).
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub initial_path: Option<String>,
}

/// Export AeroFTP server profiles to WinSCP.ini format.
///
/// Returns the number of successfully exported servers.
pub fn export_winscp(
    servers: &[WinScpExportServer],
    passwords: &std::collections::HashMap<String, String>,
    output_path: &Path,
) -> Result<usize, String> {
    let mut ini = String::from("; WinSCP session file exported by AeroFTP\n\n");
    let mut exported = 0;
    // A second `[Sessions\<name>]` is not a second session, and
    // `[Sessions\Default%20Settings]` is WinSCP's default session, which the
    // import skips: a profile called that would set the defaults instead.
    let mut names = crate::bridge_shared::ExportEntryNames::ignoring_case(
        servers.iter().map(|s| winscp_session_name(&s.name)),
    )
    .with_taken([winscp_session_name("Default Settings")]);

    for server in servers {
        let protocol = server.protocol.as_deref().unwrap_or("ftp");

        // Map AeroFTP protocol to WinSCP FSProtocol + Ftps
        // FTP follows the TLS mode the connection uses
        // (`bridge_shared::ftp_tls_mode_for_export`): WinSCP's Ftps=1 is
        // implicit, 3 explicit. It has no "TLS if available", so that mode
        // requires TLS rather than export a session that sends the password
        // in clear where AeroFTP would have encrypted it.
        let (fs_protocol, ftps) = match protocol {
            "scp" => (0u32, 0u32),
            "sftp" => {
                let fallback_off = server
                    .options
                    .as_ref()
                    .and_then(|o| o.get("allowScpFallback"))
                    .and_then(|v| v.as_bool())
                    == Some(false);
                (if fallback_off { 2 } else { 1 }, 0)
            }
            "ftp" | "ftps" => match crate::bridge_shared::ftp_tls_mode_for_export(
                protocol,
                server.options.as_ref(),
            ) {
                Some("implicit") => (5, 1),
                Some("explicit") | Some("explicit_if_available") => (5, 3),
                _ => (5, 0),
            },
            "webdav" => {
                let use_ssl = server
                    .options
                    .as_ref()
                    .and_then(|o| o.get("useSsl"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(server.port == 443);
                if use_ssl {
                    (6, 1)
                } else {
                    (6, 0)
                }
            }
            "s3" => (7, 1),
            _ => continue, // Skip unsupported protocols
        };

        let encoded_name = names.claim(&winscp_session_name(&server.name));

        // Sanitize values to prevent INI injection (strip newlines and bracket chars)
        let sanitize = |s: &str| -> String {
            s.chars()
                .filter(|c| *c != '\n' && *c != '\r' && *c != '[' && *c != ']')
                .collect()
        };

        ini.push_str(&format!("[Sessions\\{}]\n", encoded_name));
        ini.push_str(&format!("HostName={}\n", sanitize(&server.host)));
        ini.push_str(&format!("PortNumber={}\n", server.port));
        ini.push_str(&format!("UserName={}\n", sanitize(&server.username)));
        ini.push_str(&format!("FSProtocol={}\n", fs_protocol));
        ini.push_str(&format!("Ftps={}\n", ftps));

        // Password
        if let Some(password) = crate::bridge_shared::export_secret(passwords, &server.id) {
            let key = format!("{}{}", server.username, server.host);
            let obfuscated = obfuscate_winscp_password(password, &key);
            ini.push_str(&format!("Password={}\n", obfuscated));
        }

        // Remote directory
        if let Some(ref path) = server.initial_path {
            if !path.is_empty() {
                ini.push_str(&format!("RemoteDirectory={}\n", sanitize(path)));
            }
        }

        // S3-specific options
        if protocol == "s3" {
            if let Some(ref opts) = server.options {
                if let Some(region) = opts.get("region").and_then(|v| v.as_str()) {
                    ini.push_str(&format!("S3DefaultRegion={}\n", region));
                }
                if opts
                    .get("forcePathStyle")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    ini.push_str("S3UrlStyle=1\n");
                }
            }
        }

        ini.push('\n');
        exported += 1;
    }

    crate::bridge_shared::atomic_write_600(output_path, ini.as_bytes())
        .map_err(|e| format!("Write WinSCP config: {}", e))?;

    Ok(exported)
}

// uuid_v4 now lives in `crate::bridge_shared` (Refactor 6).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("Hello%20World"), "Hello World");
        assert_eq!(url_decode("test%25value"), "test%value");
        assert_eq!(url_decode("no-encoding"), "no-encoding");
        assert_eq!(url_decode("bad%zz"), "bad%zz");
    }

    /// A non-ASCII name as WinSCP writes it carries the UTF-8 BOM, which is
    /// a marker and not part of the name: it used to come in as a leading
    /// U+FEFF.
    #[test]
    fn test_url_decode_strips_winscp_utf8_bom() {
        assert_eq!(url_decode("%EF%BB%BFCaf%C3%A9"), "Café");
        assert_eq!(
            url_decode("%EF%BB%BFProduction/%E6%97%A5%E6%9C%AC%20lab"),
            "Production/日本 lab"
        );
        // Without the BOM, UTF-8 bytes still read as UTF-8.
        assert_eq!(url_decode("Caf%C3%A9"), "Café");
    }

    /// The session name WinSCP's `MungeStr` writes: PuTTY's escape set on
    /// the bytes, and the UTF-8 BOM in front of a non-ASCII name only.
    #[test]
    fn test_winscp_session_name_is_what_winscp_writes() {
        for (c, escaped) in [
            (' ', "%20"),
            ('\\', "%5C"),
            ('*', "%2A"),
            ('?', "%3F"),
            ('%', "%25"),
            ('\t', "%09"),
            ('\n', "%0A"),
            ('\u{7f}', "%7F"),
        ] {
            assert_eq!(winscp_session_name(&format!("a{c}")), format!("a{escaped}"));
        }
        for byte in 0x21..=0x7eu8 {
            if matches!(byte, b'\\' | b'*' | b'?' | b'%') {
                continue;
            }
            let pair = format!("{0}{0}", char::from(byte));
            assert_eq!(
                winscp_session_name(&pair),
                pair.replacen('.', "%2E", 1),
                "byte {byte:#04x}"
            );
        }
        assert_eq!(winscp_session_name("My Server"), "My%20Server");
        assert_eq!(winscp_session_name("Production/Web"), "Production/Web");
        assert_eq!(winscp_session_name(".hidden"), "%2Ehidden");
        assert_eq!(winscp_session_name("prod.db"), "prod.db");
        assert_eq!(winscp_session_name("Café"), "%EF%BB%BFCaf%C3%A9");
        assert_eq!(winscp_session_name("€"), "%EF%BB%BF%E2%82%AC");
        assert_eq!(
            winscp_session_name("日本 lab"),
            "%EF%BB%BF%E6%97%A5%E6%9C%AC%20lab"
        );
        // The escape runs over the BOM too: a `.` after it is not first.
        assert_eq!(winscp_session_name(".é"), "%EF%BB%BF.%C3%A9");
    }

    /// Export then import gives each name back, non-ASCII and INI-looking
    /// characters included (a folder session comes back as its last segment,
    /// which is how the importer names it), and every section name is one
    /// line that starts with `[Sessions\` and ends with `]`.
    #[test]
    fn test_export_import_session_name_roundtrip() {
        let names = [
            "My Server",
            "Café",
            "日本 lab",
            "Production/Web Server",
            "x[1]=y;z",
            ".hidden",
            "100% *?",
            "tab\there",
            "back\\slash",
        ];
        let servers: Vec<WinScpExportServer> = names
            .iter()
            .enumerate()
            .map(|(i, n)| WinScpExportServer {
                id: format!("id-{i}"),
                name: n.to_string(),
                host: "sftp.example.com".to_string(),
                port: 22,
                username: "u".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                initial_path: None,
            })
            .collect();
        let out = std::env::temp_dir().join(format!(
            "aeroftp-winscp-names-{}.ini",
            crate::bridge_shared::uuid_v4()
        ));
        assert_eq!(
            export_winscp(&servers, &HashMap::new(), &out).unwrap(),
            names.len()
        );
        let ini = std::fs::read_to_string(&out).unwrap();
        let back = import_winscp(&out).unwrap();
        std::fs::remove_file(&out).ok();

        assert!(!ini.contains('\u{feff}') && ini.is_ascii(), "{ini}");
        assert_eq!(
            ini.lines().filter(|l| l.starts_with("[Sessions\\")).count(),
            names.len()
        );
        let mut got: Vec<&str> = back.servers.iter().map(|s| s.name.as_str()).collect();
        let mut want: Vec<&str> = names
            .iter()
            .map(|n| n.rsplit('/').next().unwrap())
            .collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    // CLAUDE-AV-B9-01: a crafted WinSCP.ini must not panic the importer.
    #[test]
    fn decode_winscp_password_non_boundary_key_does_not_panic() {
        // Emit a byte through WinSCP's public de-obfuscation so `dec_next_char`
        // reproduces it: `dec_next_char` returns `!enc ^ MAGIC`, so to get `out`
        // we store `enc = !(out ^ MAGIC)` as two hex nibbles.
        fn enc_stream(bytes: &[u8]) -> String {
            bytes
                .iter()
                .map(|out| format!("{:02X}", !(*out ^ PWALG_SIMPLE_MAGIC)))
                .collect()
        }
        // Extended format (flag 0xFF), version!=2 -> 8-bit length, zero padding
        // shift, then the 3-byte UTF-8 plaintext "xé" (78 C3 A9). With key "ab"
        // (len 2), byte index 2 is the continuation byte of 'é' -> not a char
        // boundary, so a raw `decoded[key.len()..]` slice would panic.
        let hex = enc_stream(&[0xFF, 0x00, 0x03, 0x00, 0x78, 0xC3, 0xA9]);
        let out = decode_winscp_password(&hex, "ab").expect("must return Ok, not panic");
        assert_eq!(out, "xé"); // prefix can't be stripped safely -> raw decoded
    }

    #[test]
    fn test_parse_winscp_ini_basic() {
        let ini = r#"
[Sessions\My%20Server]
HostName=example.com
PortNumber=22
UserName=admin
FSProtocol=2
Ftps=0

[Sessions\Default%20Settings]
HostName=
PortNumber=22
"#;
        let sessions = parse_winscp_ini(ini);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains_key("My Server"));
        let s = &sessions["My Server"];
        assert_eq!(s.get("HostName").unwrap(), "example.com");
        assert_eq!(s.get("UserName").unwrap(), "admin");
    }

    #[test]
    fn test_parse_winscp_ini_folders() {
        let ini = r#"
[Sessions\Production/Web%20Server]
HostName=web.example.com
PortNumber=22
UserName=deploy
FSProtocol=1
"#;
        let sessions = parse_winscp_ini(ini);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains_key("Production/Web Server"));
    }

    #[test]
    fn test_map_session_sftp() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "example.com".to_string());
        fields.insert("PortNumber".to_string(), "22".to_string());
        fields.insert("UserName".to_string(), "admin".to_string());
        fields.insert("FSProtocol".to_string(), "2".to_string());
        fields.insert("Ftps".to_string(), "0".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "sftp");
        assert_eq!(mapped.port, 22);
    }

    /// WinSCP's 0 is SCP only, 1 SFTP with SCP fallback (its default) and 2
    /// SFTP only; export writes each back as it came.
    #[test]
    fn winscp_scp_and_sftp_fallback_modes_round_trip() {
        let mapped = |fs: &str| {
            let mut fields = HashMap::new();
            fields.insert("HostName".to_string(), "router.lan".to_string());
            fields.insert("UserName".to_string(), "root".to_string());
            if !fs.is_empty() {
                fields.insert("FSProtocol".to_string(), fs.to_string());
            }
            map_session("s", &fields).unwrap()
        };
        let fallback = |m: &MappedProfile| {
            m.options
                .as_ref()
                .and_then(|o| o.get("allowScpFallback"))
                .and_then(|v| v.as_bool())
        };
        let scp = mapped("0");
        assert_eq!((scp.protocol.as_str(), scp.port), ("scp", 22));
        let with_fallback = mapped("1");
        assert_eq!(with_fallback.protocol, "sftp");
        assert_eq!(fallback(&with_fallback), None, "on is the default");
        assert_eq!(fallback(&mapped("")), None, "an absent FSProtocol is 1");
        let only = mapped("2");
        assert_eq!(only.protocol, "sftp");
        assert_eq!(fallback(&only), Some(false));

        let server = |protocol: &str, options: Option<serde_json::Value>| WinScpExportServer {
            id: format!("id-{protocol}-{}", options.is_some()),
            name: format!("{protocol} {}", options.is_some()),
            host: "router.lan".to_string(),
            port: 22,
            username: "root".to_string(),
            protocol: Some(protocol.to_string()),
            options,
            initial_path: None,
        };
        let servers = vec![
            server("scp", None),
            server("sftp", None),
            server(
                "sftp",
                Some(serde_json::json!({ "allowScpFallback": false })),
            ),
        ];
        let out = std::env::temp_dir().join(format!(
            "aeroftp-winscp-scp-{}.ini",
            crate::bridge_shared::uuid_v4()
        ));
        assert_eq!(export_winscp(&servers, &HashMap::new(), &out).unwrap(), 3);
        let ini = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_file(&out).ok();
        let protocols: Vec<&str> = ini
            .lines()
            .filter(|l| l.starts_with("FSProtocol="))
            .collect();
        assert_eq!(
            protocols,
            ["FSProtocol=0", "FSProtocol=1", "FSProtocol=2"],
            "{ini}"
        );
    }

    #[test]
    fn test_map_session_ftp() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "ftp.example.com".to_string());
        fields.insert("PortNumber".to_string(), "21".to_string());
        fields.insert("UserName".to_string(), "user".to_string());
        fields.insert("FSProtocol".to_string(), "5".to_string());
        fields.insert("Ftps".to_string(), "0".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "ftp");
        assert_eq!(mapped.port, 21);
    }

    #[test]
    fn test_map_session_ftps_explicit_stores_tls_mode() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "secure.example.com".to_string());
        fields.insert("FSProtocol".to_string(), "5".to_string());
        fields.insert("Ftps".to_string(), "3".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "ftps");
        assert_eq!(mapped.port, 21);
        // Stored under the key the connection reads, or the site on port 21
        // would be opened as implicit FTPS.
        assert_eq!(
            mapped.options.as_ref().and_then(|o| o.get("tlsMode")),
            Some(&serde_json::json!("explicit"))
        );
    }

    #[test]
    fn test_map_session_ftps_implicit() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "secure.example.com".to_string());
        fields.insert("FSProtocol".to_string(), "5".to_string());
        fields.insert("Ftps".to_string(), "1".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "ftps");
        assert_eq!(mapped.port, 990);
    }

    #[test]
    fn test_map_session_webdav_https() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "cloud.example.com".to_string());
        fields.insert("FSProtocol".to_string(), "6".to_string());
        fields.insert("Ftps".to_string(), "1".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "webdav");
        assert_eq!(mapped.port, 443);
    }

    #[test]
    fn test_map_session_s3() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "s3.amazonaws.com".to_string());
        fields.insert("UserName".to_string(), "AKIAIOSFODNN7EXAMPLE".to_string());
        fields.insert("FSProtocol".to_string(), "7".to_string());
        fields.insert("Ftps".to_string(), "1".to_string());
        fields.insert("S3DefaultRegion".to_string(), "us-east-1".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.protocol, "s3");
        assert_eq!(mapped.port, 443);
        let opts = mapped.options.unwrap();
        assert_eq!(opts.get("region").unwrap().as_str().unwrap(), "us-east-1");
    }

    #[test]
    fn test_map_session_default_port() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "example.com".to_string());
        fields.insert("FSProtocol".to_string(), "2".to_string());
        // No PortNumber -> should use default 22

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.port, 22);
    }

    #[test]
    fn test_map_session_no_host() {
        let fields = HashMap::new();
        // No HostName -> should return None
        assert!(map_session("test", &fields).is_none());
    }

    #[test]
    fn test_password_plain_preferred() {
        let mut fields = HashMap::new();
        fields.insert("HostName".to_string(), "example.com".to_string());
        fields.insert("UserName".to_string(), "user".to_string());
        fields.insert("FSProtocol".to_string(), "2".to_string());
        fields.insert("PasswordPlain".to_string(), "mysecret".to_string());
        fields.insert("Password".to_string(), "AABBCCDD".to_string());

        let mapped = map_session("test", &fields).unwrap();
        assert_eq!(mapped.password.unwrap(), "mysecret");
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// written as `[Sessions\Lab]` twice, both with the second one's
    /// password, and two sections of one name are one session.
    #[test]
    fn two_profiles_of_one_name_export_as_two_sessions_each_with_its_own_password() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<WinScpExportServer>(
            serde_json::json!({"host": "a.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
            serde_json::json!({"host": "b.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("WinSCP.ini");
        assert_eq!(export_winscp(&servers, &passwords, &path).unwrap(), 2);
        let back = import_winscp(&path).unwrap();
        assert_eq!(
            crate::bridge_shared::names_hosts_secrets(&back.servers),
            crate::bridge_shared::owned3(&[
                ("Lab", "a.example.com", "secret-a"),
                ("Lab-2", "b.example.com", "secret-b"),
            ])
        );
    }

    /// `[Sessions\Default%20Settings]` is WinSCP's default session, which the
    /// import skips: a profile called that, in any case, set the defaults
    /// instead of adding a session.
    #[test]
    fn a_profile_named_default_settings_is_not_written_onto_the_defaults() {
        let servers: Vec<WinScpExportServer> =
            [("id-a", "Default Settings"), ("id-b", "default settings")]
                .into_iter()
                .map(|(id, name)| {
                    serde_json::from_value(serde_json::json!({
                        "id": id, "name": name, "host": format!("{id}.example.com"),
                        "port": 21, "username": "u", "protocol": "ftp"
                    }))
                    .unwrap()
                })
                .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("WinSCP.ini");
        assert_eq!(export_winscp(&servers, &HashMap::new(), &path).unwrap(), 2);
        let ini = std::fs::read_to_string(&path).unwrap();
        assert!(
            !ini.to_lowercase()
                .contains("[sessions\\default%20settings]"),
            "{ini}"
        );
        let mut back: Vec<String> = import_winscp(&path)
            .unwrap()
            .servers
            .into_iter()
            .map(|s| s.name)
            .collect();
        back.sort();
        assert_eq!(back, ["Default Settings-2", "default settings-3"]);
    }
}
