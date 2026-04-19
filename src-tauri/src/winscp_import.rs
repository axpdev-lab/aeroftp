// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from WinSCP configuration files.
//!
//! Parses `WinSCP.ini` or exported session files (INI format), maps WinSCP
//! `FSProtocol` + `Ftps` fields to AeroFTP ProviderType, and de-obfuscates
//! WinSCP passwords (XOR-based with well-known algorithm — NOT real encryption).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from WinSCP's reversible obfuscation to proper authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ WinSCP password de-obfuscation ============
// Source: WinSCP source code (Security.cpp / ScpPassword)
// This is NOT encryption — the algorithm is public and fully reversible.

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

    // In extended format, the key is prepended to the plaintext — strip it
    if flag == PWALG_SIMPLE_FLAG {
        if decoded.len() >= key.len() && decoded.starts_with(key) {
            Ok(decoded[key.len()..].to_string())
        } else if decoded.len() >= key.len() {
            // Key might not match exactly (e.g. different encoding), try stripping by length
            Ok(decoded[key.len()..].to_string())
        } else {
            // Decoded string is shorter than key — return empty or the raw decoded
            Ok(decoded)
        }
    } else {
        Ok(decoded)
    }
}

// ============ URL Decoding ============

/// Decode percent-encoded strings in WinSCP session names (%20 -> space, etc.)
/// Handles multi-byte UTF-8 sequences correctly (e.g. %C3%A4 -> ä).
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
    String::from_utf8(decoded_bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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
        // SCP, SFTP (all SSH-based)
        (0, _) | (1, _) | (2, _) => ("sftp", 22u32),
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
        // Unknown protocol — skip
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
            // Track implicit vs explicit
            if ftps == 1 {
                options.insert(
                    "ftpsMode".to_string(),
                    serde_json::Value::String("implicit".to_string()),
                );
            } else {
                options.insert(
                    "ftpsMode".to_string(),
                    serde_json::Value::String("explicit".to_string()),
                );
            }
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

    // On non-Windows, there's no default location — user must browse
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
                    &uuid_v4()[..8]
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

    for server in servers {
        let protocol = server.protocol.as_deref().unwrap_or("ftp");

        // Map AeroFTP protocol to WinSCP FSProtocol + Ftps
        let (fs_protocol, ftps) = match protocol {
            "sftp" => (2u32, 0u32),
            "ftp" => (5, 0),
            "ftps" => {
                let mode = server
                    .options
                    .as_ref()
                    .and_then(|o| o.get("ftpsMode"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("explicit");
                if mode == "implicit" {
                    (5, 1)
                } else {
                    (5, 3)
                }
            }
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

        // URL-encode session name
        let encoded_name = server
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c.to_string()
                } else if c == ' ' {
                    "%20".to_string()
                } else {
                    format!("%{:02X}", c as u32)
                }
            })
            .collect::<String>();

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
        if let Some(password) = passwords.get(&server.name) {
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

    std::fs::write(output_path, ini).map_err(|e| format!("Write WinSCP config: {}", e))?;

    Ok(exported)
}

/// Simple UUID v4 generator using CSPRNG (avoid pulling uuid crate).
fn uuid_v4() -> String {
    let random_bytes = crate::crypto::random_bytes(16);

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&random_bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("Hello%20World"), "Hello World");
        assert_eq!(url_decode("test%25value"), "test%value");
        assert_eq!(url_decode("no-encoding"), "no-encoding");
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
}
