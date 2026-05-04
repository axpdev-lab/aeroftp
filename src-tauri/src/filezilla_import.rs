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
fn parse_sitemanager_xml(content: &str) -> Vec<FileZillaServer> {
    let mut servers = Vec::new();
    let mut folder_stack: Vec<String> = Vec::new();
    let mut in_server = false;
    let mut current_fields: HashMap<String, String> = HashMap::new();
    let mut current_name = String::new();
    let mut current_tag = String::new();
    let mut current_text = String::new();
    let mut pass_encoding = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Folder open: <Folder expanded="1">
        if trimmed.starts_with("<Folder") && !trimmed.starts_with("</Folder") {
            // Next text content before </Folder> is the folder name,
            // but FileZilla puts folder name as text AFTER child elements.
            // We'll handle it via a stack approach.
            folder_stack.push(String::new());
            continue;
        }

        // Folder close: </Folder>
        if trimmed == "</Folder>" {
            folder_stack.pop();
            continue;
        }

        // Folder name (text between <Folder> children and </Folder>)
        // FileZilla puts folder name as the last text node before </Folder>
        // We detect it as a bare text line when we're not in a server
        if !in_server && !trimmed.is_empty() && !trimmed.starts_with('<') {
            if let Some(last) = folder_stack.last_mut() {
                if last.is_empty() {
                    *last = xml_unescape(trimmed);
                }
            }
            continue;
        }

        // Server open
        if trimmed == "<Server>" {
            if servers.len() >= MAX_SERVERS {
                break;
            }
            in_server = true;
            current_fields.clear();
            current_name.clear();
            pass_encoding.clear();
            continue;
        }

        // Server close
        if trimmed == "</Server>" {
            if in_server {
                // Build hierarchical name from folder stack
                let folder_path: Vec<&str> = folder_stack
                    .iter()
                    .filter(|f| !f.is_empty())
                    .map(|f| f.as_str())
                    .collect();
                let display_name = if current_name.is_empty() {
                    current_fields
                        .get("Host")
                        .cloned()
                        .unwrap_or_else(|| "unnamed".to_string())
                } else {
                    current_name.clone()
                };
                // Store folder path for reference
                if !folder_path.is_empty() {
                    current_fields.insert("_folder".to_string(), folder_path.join("/"));
                }
                servers.push(FileZillaServer {
                    fields: current_fields.clone(),
                    name: display_name,
                });
            }
            in_server = false;
            continue;
        }

        if !in_server {
            continue;
        }

        // Parse tag open with optional attributes: <Pass encoding="base64">
        if trimmed.starts_with('<') && !trimmed.starts_with("</") {
            if let Some(tag_end) = trimmed.find('>') {
                let tag_content = &trimmed[1..tag_end];
                let (tag_name, attrs) = match tag_content.find(' ') {
                    Some(i) => (&tag_content[..i], &tag_content[i..]),
                    None => (tag_content, ""),
                };
                current_tag = tag_name.to_string();
                current_text.clear();

                // Check for encoding attribute on Pass
                if tag_name == "Pass" {
                    pass_encoding = if attrs.contains("base64") {
                        "base64".to_string()
                    } else {
                        String::new()
                    };
                }

                // Inline text: <Host>example.com</Host>
                let after_open = &trimmed[tag_end + 1..];
                if let Some(close_start) = after_open.find("</") {
                    let text = &after_open[..close_start];
                    let value = xml_unescape(text);

                    if current_tag == "Name" {
                        current_name = value;
                    } else {
                        current_fields.insert(current_tag.clone(), value);
                    }
                    // Store pass encoding info
                    if current_tag == "Pass" {
                        current_fields.insert("_pass_encoding".to_string(), pass_encoding.clone());
                    }
                    current_tag.clear();
                }
            }
            continue;
        }

        // Close tag
        if trimmed.starts_with("</") {
            if !current_tag.is_empty() && !current_text.is_empty() {
                let value = xml_unescape(&current_text);
                if current_tag == "Name" {
                    current_name = value;
                } else {
                    current_fields.insert(current_tag.clone(), value);
                }
                if current_tag == "Pass" {
                    current_fields.insert("_pass_encoding".to_string(), pass_encoding.clone());
                }
            }
            current_tag.clear();
            current_text.clear();
            continue;
        }

        // Text content between tags
        if !current_tag.is_empty() {
            if !current_text.is_empty() {
                current_text.push(' ');
            }
            current_text.push_str(trimmed);
        }
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
                    &uuid_v4()[..8]
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

    std::fs::write(output_path, xml).map_err(|e| format!("Write FileZilla config: {}", e))?;

    Ok(exported)
}

/// Simple UUID v4 generator using CSPRNG.
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
}
