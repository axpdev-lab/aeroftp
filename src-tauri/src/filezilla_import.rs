// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from FileZilla configuration files.
//!
//! Parses `sitemanager.xml` (XML format), maps FileZilla protocol values to
//! AeroFTP ProviderType, and decodes base64-encoded passwords (FileZilla uses
//! plain base64: NOT encryption of any kind).
//!
//! Imported credentials are stored in our AES-256-GCM vault, upgrading security
//! from FileZilla's base64 encoding to proper authenticated encryption.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ XML Parser (minimal, no external crate) ============

/// A parsed FileZilla server entry.
struct FileZillaServer {
    fields: HashMap<String, String>,
    name: String,
}

/// Maximum number of servers to parse (defense against DoS).
const MAX_SERVERS: usize = 10_000;

/// Maximum file size (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Parse FileZilla sitemanager.xml and extract server entries.
/// Handles nested <Folder> elements for hierarchical names.
///
/// Event-driven (quick-xml) rather than line-oriented: FileZilla itself writes
/// one tag per line, but a sitemanager.xml that went through a minifier or was
/// written by another tool is still valid XML, and a line parser silently
/// imported nothing from it.
fn parse_sitemanager_xml(content: &str) -> Vec<FileZillaServer> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut servers: Vec<FileZillaServer> = Vec::new();
    // (folder name, index of the first server pushed inside it). FileZilla
    // writes a folder's name as a text node AFTER its children, so the name is
    // not known yet when the servers inside it are pushed: the path is stamped
    // onto them when the folder closes, innermost folder first.
    let mut folder_stack: Vec<(String, usize)> = Vec::new();
    let mut in_server = false;
    let mut current_fields: HashMap<String, String> = HashMap::new();
    let mut current_name = String::new();
    let mut current_tag = String::new();
    let mut current_text = String::new();
    let mut pass_encoding = String::new();
    let mut capped = false;

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    // Malformed XML ends iteration while preserving every server parsed
    // cleanly before the error.
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match tag.as_str() {
                    "Folder" if !in_server => {
                        folder_stack.push((String::new(), servers.len()));
                    }
                    "Server" if !in_server => {
                        if servers.len() >= MAX_SERVERS {
                            capped = true;
                            break;
                        }
                        in_server = true;
                        current_fields.clear();
                        current_name.clear();
                        pass_encoding.clear();
                        current_tag.clear();
                        current_text.clear();
                    }
                    _ if in_server => {
                        if tag == "Pass" {
                            pass_encoding = e
                                .try_get_attribute("encoding")
                                .ok()
                                .flatten()
                                .map(|a| String::from_utf8_lossy(&a.value).to_string())
                                .unwrap_or_default();
                        }
                        current_tag = tag;
                        current_text.clear();
                    }
                    _ => {}
                }
            }
            // Self-closing element, e.g. <Name/>: an empty value.
            Event::Empty(e) => {
                if !in_server {
                    continue;
                }
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "Pass" {
                    let enc = e
                        .try_get_attribute("encoding")
                        .ok()
                        .flatten()
                        .map(|a| String::from_utf8_lossy(&a.value).to_string())
                        .unwrap_or_default();
                    current_fields.insert("_pass_encoding".to_string(), enc);
                }
                if tag == "Name" {
                    current_name.clear();
                } else {
                    current_fields.insert(tag, String::new());
                }
            }
            Event::Text(e) => {
                let text = e
                    .xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|_| {
                        xml_unescape(&String::from_utf8_lossy(e.into_inner().as_ref()))
                    });
                if text.is_empty() {
                    continue;
                }
                if in_server {
                    if current_tag.is_empty() {
                        continue;
                    }
                    if !current_text.is_empty() {
                        current_text.push(' ');
                    }
                    current_text.push_str(&text);
                } else if let Some((name, _)) = folder_stack.last_mut() {
                    // FileZilla writes the folder name as a text node after the
                    // folder's children; the first one wins.
                    if name.is_empty() {
                        *name = text;
                    }
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if in_server && tag == "Server" {
                    let display_name = if current_name.is_empty() {
                        current_fields
                            .get("Host")
                            .cloned()
                            .unwrap_or_else(|| "unnamed".to_string())
                    } else {
                        current_name.clone()
                    };
                    servers.push(FileZillaServer {
                        fields: current_fields.clone(),
                        name: display_name,
                    });
                    in_server = false;
                    current_tag.clear();
                    current_text.clear();
                } else if !in_server && tag == "Folder" {
                    // The name is known only now: stamp it on every server this
                    // folder contains. Inner folders closed first, so prepending
                    // builds the path outermost-last.
                    if let Some((name, first_server)) = folder_stack.pop() {
                        if !name.is_empty() {
                            for server in servers.iter_mut().skip(first_server) {
                                let path = match server.fields.get("_folder") {
                                    Some(inner) => format!("{name}/{inner}"),
                                    None => name.clone(),
                                };
                                server.fields.insert("_folder".to_string(), path);
                            }
                        }
                    }
                } else if in_server && tag == current_tag {
                    if current_tag == "Name" {
                        current_name = current_text.clone();
                    } else {
                        current_fields.insert(current_tag.clone(), current_text.clone());
                    }
                    if current_tag == "Pass" {
                        current_fields.insert("_pass_encoding".to_string(), pass_encoding.clone());
                    }
                    current_tag.clear();
                    current_text.clear();
                }
            }
            _ => {}
        }
        buf.clear();
    }

    if capped {
        log::warn!("filezilla import: stopped at the {MAX_SERVERS}-server cap");
    }
    servers
}

/// Basic XML entity unescaping.
fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

// ============ Password Decoding ============

/// Decode a FileZilla password.
/// FileZilla uses plain base64 encoding: not encryption at all.
fn decode_filezilla_password(encoded: &str, encoding: &str) -> Option<String> {
    if encoded.is_empty() {
        return None;
    }

    if encoding == "base64" {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        match STANDARD.decode(encoded) {
            Ok(bytes) => String::from_utf8(bytes).ok().filter(|s| !s.is_empty()),
            Err(_) => None,
        }
    } else {
        // Plain text (older FileZilla versions or no encoding attribute)
        Some(encoded.to_string())
    }
}

/// Encode a password in FileZilla's base64 format (for export).
fn encode_filezilla_password(plaintext: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD.encode(plaintext.as_bytes())
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

/// Map a FileZilla server entry to an AeroFTP profile.
fn map_server(server: &FileZillaServer) -> Option<MappedProfile> {
    let host = server.fields.get("Host").filter(|h| !h.is_empty())?;
    let fz_protocol: u32 = server
        .fields
        .get("Protocol")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let port: u32 = server
        .fields
        .get("Port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let username = server.fields.get("User").cloned().unwrap_or_default();

    // Decode password
    let encoding = server
        .fields
        .get("_pass_encoding")
        .map(|s| s.as_str())
        .unwrap_or("");
    let password = server
        .fields
        .get("Pass")
        .and_then(|p| decode_filezilla_password(p, encoding));

    let remote_dir = server
        .fields
        .get("RemoteDir")
        .filter(|d| !d.is_empty())
        .map(|d| parse_filezilla_remote_dir(d));

    // FileZilla protocol values:
    // 0 = FTP, 1 = SFTP, 3 = FTPS implicit, 4 = FTPS explicit, 6 = S3
    let (protocol, default_port) = match fz_protocol {
        0 => ("ftp", 21u32),
        1 => ("sftp", 22),
        3 => ("ftps", 990), // implicit
        4 => ("ftps", 21),  // explicit
        6 => ("s3", 443),
        _ => {
            log::info!(
                "FileZilla server '{}': unsupported Protocol={}",
                server.name,
                fz_protocol
            );
            return None;
        }
    };

    let actual_port = if port == 0 { default_port } else { port };

    // Build protocol-specific options
    let mut options = serde_json::Map::new();

    if protocol == "ftps" {
        if fz_protocol == 3 {
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

/// Parse FileZilla's encoded remote directory format.
/// FileZilla uses a custom format: "1 0 <len> <path>" or just a plain path.
fn parse_filezilla_remote_dir(encoded: &str) -> String {
    // FileZilla encodes paths as: "1 0 <length> <path> 0"
    // Example: "1 0 4 /var 0 4 /www 0" -> "/var/www"
    // Simple paths are just the path string
    if encoded.starts_with("1 0") {
        // Parse the encoded format
        let parts: Vec<&str> = encoded.split_whitespace().collect();
        let mut path = String::new();
        let mut i = 2; // Skip "1" and "0"
        while i + 1 < parts.len() {
            if let Ok(len) = parts[i].parse::<usize>() {
                if i + 1 < parts.len() {
                    let segment = parts[i + 1];
                    if segment.len() == len || segment == "0" {
                        if segment != "0" {
                            path.push_str(segment);
                        }
                    } else {
                        path.push_str(segment);
                    }
                }
                i += 2;
            } else {
                break;
            }
        }
        if path.is_empty() {
            return String::new();
        }
        if !path.starts_with('/') {
            format!("/{}", path)
        } else {
            path
        }
    } else if !encoded.is_empty() {
        encoded.to_string()
    } else {
        String::new()
    }
}

// ============ Default config path detection ============

/// Returns the default sitemanager.xml path for the current platform.
pub fn default_filezilla_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = PathBuf::from(appdata)
                .join("FileZilla")
                .join("sitemanager.xml");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/filezilla/sitemanager.xml");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/filezilla/sitemanager.xml");
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let path = PathBuf::from(xdg).join("filezilla/sitemanager.xml");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Public API ============

/// Result of importing FileZilla config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileZillaImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<FileZillaSkippedServer>,
    pub source_path: String,
    pub total_servers: usize,
}

/// A server that was skipped (unsupported protocol).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileZillaSkippedServer {
    pub name: String,
    pub protocol: String,
    pub reason: String,
}

/// Import all supported servers from a FileZilla sitemanager.xml file.
pub fn import_filezilla(config_path: &Path) -> Result<FileZillaImportResult, String> {
    // Check file size before reading
    let metadata = std::fs::metadata(config_path)
        .map_err(|e| format!("Read FileZilla config metadata: {}", e))?;
    if metadata.len() > MAX_FILE_SIZE {
        return Err("File too large (max 10 MB)".to_string());
    }

    let content = std::fs::read_to_string(config_path)
        .map_err(|e| format!("Read FileZilla config: {}", e))?;

    let fz_servers = parse_sitemanager_xml(&content);
    let total_servers = fz_servers.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for fz_server in &fz_servers {
        let fz_protocol = fz_server
            .fields
            .get("Protocol")
            .cloned()
            .unwrap_or_default();

        match map_server(fz_server) {
            Some(mapped) => {
                let id = format!(
                    "filezilla-{}-{}",
                    fz_server
                        .name
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect::<String>(),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                servers.push(ServerProfileExport {
                    id,
                    name: fz_server.name.clone(),
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
                skipped.push(FileZillaSkippedServer {
                    name: fz_server.name.clone(),
                    protocol: fz_protocol,
                    reason: format!(
                        "unsupported Protocol={}",
                        fz_server.fields.get("Protocol").unwrap_or(&String::new())
                    ),
                });
            }
        }
    }

    Ok(FileZillaImportResult {
        servers,
        skipped,
        source_path: config_path.display().to_string(),
        total_servers,
    })
}

// ============ Export ============

/// Server data for FileZilla export.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileZillaExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub initial_path: Option<String>,
}

/// Export AeroFTP server profiles to FileZilla sitemanager.xml format.
pub fn export_filezilla(
    servers: &[FileZillaExportServer],
    passwords: &HashMap<String, String>,
    output_path: &Path,
) -> Result<usize, String> {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<FileZilla3 version=\"3.67.1\" platform=\"*\">\n");
    xml.push_str("  <Servers>\n");
    let mut exported = 0;

    for server in servers {
        let protocol = server.protocol.as_deref().unwrap_or("ftp");

        // Map AeroFTP protocol to FileZilla Protocol value
        let fz_protocol = match protocol {
            "ftp" => 0,
            "sftp" => 1,
            "ftps" => {
                let mode = server
                    .options
                    .as_ref()
                    .and_then(|o| o.get("ftpsMode"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("explicit");
                if mode == "implicit" {
                    3
                } else {
                    4
                }
            }
            "s3" => 6,
            _ => continue,
        };

        // Sanitize values for XML
        let sanitize = |s: &str| -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };

        xml.push_str("    <Server>\n");
        xml.push_str(&format!("      <Host>{}</Host>\n", sanitize(&server.host)));
        xml.push_str(&format!("      <Port>{}</Port>\n", server.port));
        xml.push_str(&format!("      <Protocol>{}</Protocol>\n", fz_protocol));
        xml.push_str(&format!(
            "      <User>{}</User>\n",
            sanitize(&server.username)
        ));

        // Password
        if let Some(password) = passwords.get(&server.name) {
            let encoded = encode_filezilla_password(password);
            xml.push_str(&format!(
                "      <Pass encoding=\"base64\">{}</Pass>\n",
                encoded
            ));
            xml.push_str("      <Logontype>1</Logontype>\n"); // Normal (user+pass)
        } else {
            xml.push_str("      <Logontype>0</Logontype>\n"); // Anonymous
        }

        // Remote directory
        if let Some(ref path) = server.initial_path {
            if !path.is_empty() {
                xml.push_str(&format!(
                    "      <RemoteDir>{}</RemoteDir>\n",
                    sanitize(path)
                ));
            }
        }

        xml.push_str(&format!("      <Name>{}</Name>\n", sanitize(&server.name)));
        xml.push_str("    </Server>\n");
        exported += 1;
    }

    xml.push_str("  </Servers>\n");
    xml.push_str("</FileZilla3>\n");

    crate::bridge_shared::atomic_write_600(output_path, xml.as_bytes())
        .map_err(|e| format!("Write FileZilla config: {}", e))?;

    Ok(exported)
}

// uuid_v4 now lives in `crate::bridge_shared` (Refactor 6).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_server() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3>
  <Servers>
    <Server>
      <Host>ftp.example.com</Host>
      <Port>21</Port>
      <Protocol>0</Protocol>
      <User>admin</User>
      <Pass encoding="base64">c2VjcmV0</Pass>
      <Name>My FTP Server</Name>
    </Server>
  </Servers>
</FileZilla3>"#;
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "My FTP Server");
        assert_eq!(servers[0].fields.get("Host").unwrap(), "ftp.example.com");
        assert_eq!(servers[0].fields.get("Protocol").unwrap(), "0");
    }

    #[test]
    fn test_map_ftp() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: [
                ("Host".to_string(), "ftp.test.com".to_string()),
                ("Port".to_string(), "21".to_string()),
                ("Protocol".to_string(), "0".to_string()),
                ("User".to_string(), "user".to_string()),
            ]
            .into(),
        };
        let mapped = map_server(&server).unwrap();
        assert_eq!(mapped.protocol, "ftp");
        assert_eq!(mapped.port, 21);
    }

    #[test]
    fn test_map_sftp() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: [
                ("Host".to_string(), "ssh.test.com".to_string()),
                ("Protocol".to_string(), "1".to_string()),
            ]
            .into(),
        };
        let mapped = map_server(&server).unwrap();
        assert_eq!(mapped.protocol, "sftp");
        assert_eq!(mapped.port, 22);
    }

    #[test]
    fn test_map_ftps_implicit() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: [
                ("Host".to_string(), "secure.test.com".to_string()),
                ("Protocol".to_string(), "3".to_string()),
            ]
            .into(),
        };
        let mapped = map_server(&server).unwrap();
        assert_eq!(mapped.protocol, "ftps");
        assert_eq!(mapped.port, 990);
    }

    #[test]
    fn test_map_ftps_explicit() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: [
                ("Host".to_string(), "secure.test.com".to_string()),
                ("Protocol".to_string(), "4".to_string()),
            ]
            .into(),
        };
        let mapped = map_server(&server).unwrap();
        assert_eq!(mapped.protocol, "ftps");
        assert_eq!(mapped.port, 21);
    }

    #[test]
    fn test_map_s3() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: [
                ("Host".to_string(), "s3.amazonaws.com".to_string()),
                ("Protocol".to_string(), "6".to_string()),
                ("User".to_string(), "AKIAEXAMPLE".to_string()),
            ]
            .into(),
        };
        let mapped = map_server(&server).unwrap();
        assert_eq!(mapped.protocol, "s3");
        assert_eq!(mapped.port, 443);
    }

    #[test]
    fn test_password_base64_decode() {
        assert_eq!(
            decode_filezilla_password("c2VjcmV0", "base64"),
            Some("secret".to_string())
        );
        assert_eq!(decode_filezilla_password("", "base64"), None);
    }

    #[test]
    fn test_password_roundtrip() {
        let original = "MyP@ssw0rd!123";
        let encoded = encode_filezilla_password(original);
        let decoded = decode_filezilla_password(&encoded, "base64");
        assert_eq!(decoded, Some(original.to_string()));
    }

    #[test]
    fn test_no_host_returns_none() {
        let server = FileZillaServer {
            name: "test".to_string(),
            fields: HashMap::new(),
        };
        assert!(map_server(&server).is_none());
    }

    #[test]
    fn test_xml_unescape() {
        assert_eq!(xml_unescape("foo &amp; bar"), "foo & bar");
        assert_eq!(xml_unescape("a &lt; b &gt; c"), "a < b > c");
    }

    #[test]
    fn test_multiple_servers() {
        let xml = r#"<?xml version="1.0"?>
<FileZilla3>
  <Servers>
    <Server>
      <Host>server1.com</Host>
      <Protocol>0</Protocol>
      <Name>Server 1</Name>
    </Server>
    <Server>
      <Host>server2.com</Host>
      <Protocol>1</Protocol>
      <Name>Server 2</Name>
    </Server>
  </Servers>
</FileZilla3>"#;
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "Server 1");
        assert_eq!(servers[1].name, "Server 2");
    }

    /// CLAUDE-AV-B9-01: the parser used to be line-oriented, so a well-formed
    /// sitemanager.xml whose tags were not one-per-line (an XML minifier, a
    /// third-party writer, a hand edit) imported ZERO sites and reported
    /// success, with no skip reason. Same document, no newlines.
    #[test]
    fn parses_sitemanager_written_on_a_single_line() {
        let xml = concat!(
            r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
            r#"<Server><Host>h1</Host><Port>21</Port><Protocol>0</Protocol>"#,
            r#"<User>u</User><Pass encoding="base64">QUJD</Pass>"#,
            r#"<Name>one-liner</Name></Server>"#,
            r#"</Servers></FileZilla3>"#,
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1, "single-line XML must still import");
        assert_eq!(servers[0].name, "one-liner");
        assert_eq!(
            servers[0].fields.get("Host").map(String::as_str),
            Some("h1")
        );
        assert_eq!(
            servers[0].fields.get("_pass_encoding").map(String::as_str),
            Some("base64"),
            "the Pass encoding attribute must survive the event parser"
        );
        assert_eq!(
            decode_filezilla_password("QUJD", "base64").as_deref(),
            Some("ABC")
        );
    }

    /// FileZilla writes a folder's name as a text node AFTER its children, so
    /// the name is not yet known when the servers inside it are parsed. The
    /// path is stamped on when the folder closes; before this it always came
    /// out empty.
    #[test]
    fn stamps_the_folder_path_on_the_servers_it_contains() {
        let xml = concat!(
            r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
            r#"<Folder expanded="1"><Server><Host>h1</Host><Name>inner</Name></Server>"#,
            r#"Clients</Folder>"#,
            r#"<Server><Host>h2</Host><Name>outside</Name></Server>"#,
            r#"</Servers></FileZilla3>"#,
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "inner");
        assert_eq!(
            servers[0].fields.get("_folder").map(String::as_str),
            Some("Clients")
        );
        assert_eq!(
            servers[1].fields.get("_folder"),
            None,
            "a server outside the folder must not inherit its path"
        );
    }

    /// Nested folders compose outermost-first.
    #[test]
    fn nested_folder_paths_compose_in_order() {
        let xml = concat!(
            r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
            r#"<Folder><Folder><Server><Host>h1</Host><Name>deep</Name></Server>"#,
            r#"Inner</Folder>Outer</Folder>"#,
            r#"</Servers></FileZilla3>"#,
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].fields.get("_folder").map(String::as_str),
            Some("Outer/Inner")
        );
    }

    /// A truncated document must keep the servers that parsed cleanly instead
    /// of discarding the whole file.
    #[test]
    fn keeps_servers_parsed_before_a_malformed_tail() {
        let xml = concat!(
            r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
            r#"<Server><Host>h1</Host><Name>good</Name></Server>"#,
            r#"<Server><Host>h2</Host><Name>trunc"#,
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "good");
    }
}
