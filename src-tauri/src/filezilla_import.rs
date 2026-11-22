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
    let mut current_parameter_name = String::new();
    let mut pass_encoding = String::new();
    let mut capped = false;
    // Text since the last tag. quick-xml hands it over in pieces: a character
    // or entity reference is its own `GeneralRef` event between two `Text`
    // fragments, and a CDATA section is a `CData` event. The pieces are joined
    // here and the whole run is trimmed once, at the next tag, so the spaces
    // around a reference (`R &amp; D`) survive.
    let mut text = String::new();

    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    // Malformed XML ends iteration while preserving every server parsed
    // cleanly before the error.
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Eof => break,
            Event::Text(e) => text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0)),
            Event::CData(e) => text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0)),
            Event::GeneralRef(e) => {
                match crate::providers::xml_text::xml_entity_to_str(&e) {
                    Some(expansion) => text.push_str(&expansion),
                    // An unknown reference stays as written, as FileZilla's
                    // own reader leaves it, instead of vanishing from a name
                    // or a password.
                    None => {
                        text.push('&');
                        text.push_str(&e);
                        text.push(';');
                    }
                }
            }
            Event::Start(e) => {
                let run = std::mem::take(&mut text);
                if !in_server {
                    note_folder_text(&mut folder_stack, run.trim());
                }
                let tag = e.name().as_ref().to_string();
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
                        current_parameter_name.clear();
                    }
                    _ if in_server => {
                        if tag == "Pass" {
                            pass_encoding = e
                                .try_get_attribute("encoding")
                                .ok()
                                .flatten()
                                .map(|a| a.value.to_string())
                                .unwrap_or_default();
                        } else if tag == "Parameter" {
                            current_parameter_name = e
                                .try_get_attribute("Name")
                                .ok()
                                .flatten()
                                .map(|a| a.value.to_string())
                                .unwrap_or_default();
                        }
                        current_tag = tag;
                    }
                    _ => {}
                }
            }
            // Self-closing element, e.g. <Name/>: an empty value.
            Event::Empty(e) => {
                let run = std::mem::take(&mut text);
                if !in_server {
                    note_folder_text(&mut folder_stack, run.trim());
                    continue;
                }
                let tag = e.name().as_ref().to_string();
                if tag == "Pass" {
                    let enc = e
                        .try_get_attribute("encoding")
                        .ok()
                        .flatten()
                        .map(|a| a.value.to_string())
                        .unwrap_or_default();
                    current_fields.insert("_pass_encoding".to_string(), enc);
                }
                if tag == "Name" {
                    current_name.clear();
                } else {
                    current_fields.insert(tag, String::new());
                }
            }
            Event::End(e) => {
                let run = std::mem::take(&mut text);
                let run = run.trim();
                let tag = e.name().as_ref().to_string();
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
                } else if !in_server && tag == "Folder" {
                    note_folder_text(&mut folder_stack, run);
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
                        current_name = run.to_string();
                    } else if current_tag == "Parameter" && !current_parameter_name.is_empty() {
                        current_fields.insert(
                            format!("_parameter_{current_parameter_name}"),
                            run.to_string(),
                        );
                        current_parameter_name.clear();
                    } else {
                        current_fields.insert(current_tag.clone(), run.to_string());
                    }
                    if current_tag == "Pass" {
                        current_fields.insert("_pass_encoding".to_string(), pass_encoding.clone());
                    }
                    current_tag.clear();
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

/// Text found directly inside a `<Folder>`, outside its servers, is the
/// folder's name. FileZilla writes it after the folder's children; the first
/// non-empty run wins.
fn note_folder_text(folder_stack: &mut [(String, usize)], run: &str) {
    if let Some((name, _)) = folder_stack.last_mut() {
        if name.is_empty() && !run.is_empty() {
            *name = run.to_string();
        }
    }
}

struct FileZillaGlobalSettings {
    generic_type: u32,
    ftp_type: u32,
    use_pasv_mode: u32,
}

impl Default for FileZillaGlobalSettings {
    fn default() -> Self {
        Self {
            generic_type: 0,
            ftp_type: 0,
            use_pasv_mode: 1,
        }
    }
}

fn filezilla_global_settings(sitemanager_path: &Path) -> Option<FileZillaGlobalSettings> {
    use quick_xml::events::Event;

    let path = sitemanager_path.with_file_name("filezilla.xml");
    let Ok(metadata) = std::fs::metadata(&path) else {
        return None;
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
        return None;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return None;
    };
    let mut reader = quick_xml::Reader::from_str(&content);
    let mut settings = FileZillaGlobalSettings::default();
    let mut setting_name = None;
    let mut setting_value = String::new();
    let mut buf = Vec::new();
    loop {
        let Ok(event) = reader.read_event_into(&mut buf) else {
            return None;
        };
        match event {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref() == "Setting" => {
                setting_name = e
                    .try_get_attribute("name")
                    .ok()
                    .flatten()
                    .map(|a| a.value.into_owned());
                setting_value.clear();
            }
            Event::Text(e) if setting_name.is_some() => {
                setting_value.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::CData(e) if setting_name.is_some() => {
                setting_value.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(e) if setting_name.is_some() => {
                match crate::providers::xml_text::xml_entity_to_str(&e) {
                    Some(expansion) => setting_value.push_str(&expansion),
                    None => {
                        setting_value.push('&');
                        setting_value.push_str(&e);
                        setting_value.push(';');
                    }
                }
            }
            Event::End(e) if e.name().as_ref() == "Setting" => {
                let value = setting_value.trim();
                match setting_name.as_deref() {
                    Some("Proxy type") => settings.generic_type = value.parse().unwrap_or(0),
                    Some("FTP Proxy type") => settings.ftp_type = value.parse().unwrap_or(0),
                    Some("Use Pasv mode") => settings.use_pasv_mode = value.parse().unwrap_or(1),
                    _ => {}
                }
                setting_name = None;
                setting_value.clear();
            }
            _ => {}
        }
        buf.clear();
    }
    Some(settings)
}

fn generic_proxy_kind(proxy_type: u32) -> String {
    match proxy_type {
        1 => "HTTP CONNECT".to_string(),
        2 => "SOCKS5".to_string(),
        3 => "SOCKS4".to_string(),
        4 => "SSH".to_string(),
        other => format!("type {other}"),
    }
}

fn ftp_proxy_kind(proxy_type: u32) -> String {
    match proxy_type {
        1 => "USER@HOST".to_string(),
        2 => "SITE".to_string(),
        3 => "OPEN".to_string(),
        4 => "custom".to_string(),
        other => format!("type {other}"),
    }
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

    // Implicit vs explicit, under `tlsMode`: the key the GUI stores and the
    // connection reads (an `ftpsMode` was read by nothing, so an explicit site
    // on port 21 connected as implicit FTPS).
    if protocol == "ftps" {
        if fz_protocol == 3 {
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
    pub warnings: Vec<FileZillaImportWarning>,
    pub source_path: String,
    pub total_servers: usize,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileZillaImportWarning {
    pub name: String,
    pub reason: String,
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
    let global_settings = filezilla_global_settings(config_path);
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    let mut generic_proxy_sites = 0usize;
    let mut ftp_proxy_sites = 0usize;
    let mut default_mode_ftp_sites = 0usize;

    for fz_server in &fz_servers {
        let fz_protocol = fz_server
            .fields
            .get("Protocol")
            .cloned()
            .unwrap_or_default();

        match map_server(fz_server) {
            Some(mapped) => {
                let bypass_proxy =
                    fz_server.fields.get("BypassProxy").map(String::as_str) == Some("1");
                let uses_generic_proxy = !bypass_proxy
                    && global_settings
                        .as_ref()
                        .is_some_and(|settings| settings.generic_type != 0);
                if matches!(mapped.protocol.as_str(), "ftp" | "ftps") {
                    let data_mode = match fz_server
                        .fields
                        .get("_parameter_data_mode_override")
                        .map(String::as_str)
                    {
                        Some("active") => Some("active"),
                        Some("passive") => Some("passive"),
                        _ => match fz_server.fields.get("PasvMode").map(String::as_str) {
                            Some("MODE_ACTIVE") => Some("active"),
                            Some("MODE_PASSIVE") => Some("passive"),
                            _ => None,
                        },
                    };
                    if data_mode.is_none() {
                        default_mode_ftp_sites += 1;
                    }
                    let global_active = global_settings
                        .as_ref()
                        .is_some_and(|settings| settings.use_pasv_mode == 0);
                    if !uses_generic_proxy
                        && (data_mode == Some("active") || (data_mode.is_none() && global_active))
                    {
                        warnings.push(FileZillaImportWarning {
                            name: fz_server.name.clone(),
                            reason: "FileZilla requires active FTP for this site; AeroFTP supports passive FTP only".to_string(),
                        });
                    }
                }
                if !bypass_proxy {
                    generic_proxy_sites += 1;
                    if matches!(mapped.protocol.as_str(), "ftp" | "ftps") {
                        ftp_proxy_sites += 1;
                    }
                }
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

    match global_settings {
        Some(settings) => {
            if settings.generic_type != 0 && generic_proxy_sites > 0 {
                warnings.push(FileZillaImportWarning {
                    name: "FileZilla global settings".to_string(),
                    reason: format!(
                        "FileZilla's global {} proxy is not imported; {} affected {} will connect directly through AeroFTP",
                        generic_proxy_kind(settings.generic_type),
                        generic_proxy_sites,
                        if generic_proxy_sites == 1 { "site" } else { "sites" },
                    ),
                });
            }
            if settings.generic_type == 0 && settings.ftp_type != 0 && ftp_proxy_sites > 0 {
                warnings.push(FileZillaImportWarning {
                    name: "FileZilla global settings".to_string(),
                    reason: format!(
                        "FileZilla's global FTP {} proxy is not imported; {} affected {} will connect directly through AeroFTP",
                        ftp_proxy_kind(settings.ftp_type),
                        ftp_proxy_sites,
                        if ftp_proxy_sites == 1 { "site" } else { "sites" },
                    ),
                });
            }
        }
        None if generic_proxy_sites > 0 || default_mode_ftp_sites > 0 => {
            let mut reasons = Vec::new();
            if generic_proxy_sites > 0 {
                reasons.push(format!(
                    "FileZilla's global proxy settings were not included in this import; {} {} may use a proxy, but AeroFTP will connect directly",
                    generic_proxy_sites,
                    if generic_proxy_sites == 1 { "site" } else { "sites" },
                ));
            }
            if default_mode_ftp_sites > 0 {
                reasons.push(format!(
                    "FileZilla's global transfer mode was not read for {} default-mode FTP {}; AeroFTP uses passive FTP",
                    default_mode_ftp_sites,
                    if default_mode_ftp_sites == 1 { "site" } else { "sites" },
                ));
            }
            warnings.push(FileZillaImportWarning {
                name: "FileZilla global settings".to_string(),
                reason: reasons.join(" "),
            });
        }
        None => {}
    }

    Ok(FileZillaImportResult {
        servers,
        skipped,
        warnings,
        source_path: config_path.display().to_string(),
        total_servers,
    })
}

// ============ Export ============

/// Server data for FileZilla export.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileZillaExportServer {
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
    // FileZilla addresses a site by its name (`--site 0/<name>`): two of one
    // name leave the second unreachable that way.
    let mut names = crate::bridge_shared::ExportEntryNames::exact(servers.iter().map(|s| &s.name));

    for server in servers {
        let protocol = server.protocol.as_deref().unwrap_or("ftp");

        // Map AeroFTP protocol to FileZilla Protocol value
        let fz_protocol = match protocol {
            "sftp" => 1,
            // FTP follows the TLS mode the connection uses
            // (`bridge_shared::ftp_tls_mode_for_export`): 3 implicit,
            // 4 explicit, 0 explicit if the server offers it.
            "ftp" | "ftps" => match crate::bridge_shared::ftp_tls_mode_for_export(
                protocol,
                server.options.as_ref(),
            ) {
                Some("implicit") => 3,
                Some("explicit") => 4,
                _ => 0,
            },
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
        if let Some(password) = crate::bridge_shared::export_secret(passwords, &server.id) {
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

        xml.push_str(&format!(
            "      <Name>{}</Name>\n",
            sanitize(&names.claim(&server.name))
        ));
        xml.push_str("    </Server>\n");
        exported += 1;
    }

    xml.push_str("  </Servers>\n");
    xml.push_str("</FileZilla3>\n");

    crate::bridge_shared::write_export(exported, output_path, xml.as_bytes())
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
        // Stored under the key the connection reads, or the site on port 21
        // would be opened as implicit FTPS.
        assert_eq!(
            mapped.options.as_ref().and_then(|o| o.get("tlsMode")),
            Some(&serde_json::json!("explicit"))
        );
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

    /// quick-xml hands an entity reference over as its own event between two
    /// text fragments. The parser dropped the reference and joined the
    /// fragments with a space, so `R&amp;D` imported as "R D" and a plain-text
    /// password `p&amp;ss` as "p ss": a wrong password, with no warning.
    #[test]
    fn entity_references_survive_in_values_names_and_folders() {
        let xml = concat!(
            r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
            r#"<Folder expanded="1"><Server><Host>h1</Host><User>a&amp;b</User>"#,
            r#"<Pass>p&amp;ss</Pass><Name>R&amp;D</Name></Server>"#,
            r#"A&amp;B</Folder>"#,
            r#"<Server><Host>h2</Host><Pass>it&#39;s &lt;ok&gt;</Pass>"#,
            r#"<Name>R &amp; D</Name></Server>"#,
            r#"<Server><Host>h3</Host><Name>a&nbsp;b</Name></Server>"#,
            r#"</Servers></FileZilla3>"#,
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 3);
        assert_eq!(servers[0].name, "R&D");
        assert_eq!(
            servers[0].fields.get("User").map(String::as_str),
            Some("a&b")
        );
        assert_eq!(
            servers[0].fields.get("Pass").map(String::as_str),
            Some("p&ss")
        );
        assert_eq!(
            servers[0].fields.get("_folder").map(String::as_str),
            Some("A&B")
        );
        assert_eq!(servers[1].name, "R & D", "spaces around a reference stay");
        assert_eq!(
            servers[1].fields.get("Pass").map(String::as_str),
            Some("it's <ok>")
        );
        assert_eq!(
            servers[2].name, "a&nbsp;b",
            "an unknown reference is kept as written, as FileZilla reads it"
        );
    }

    /// A CDATA section is element content like any other text, and a value
    /// spread over several lines is trimmed as a whole.
    #[test]
    fn cdata_and_indented_values_are_read() {
        let xml = concat!(
            "<?xml version=\"1.0\"?><FileZilla3><Servers><Server>",
            "<Host>h1</Host><Pass encoding=\"base64\">\n      QUJD\n    </Pass>",
            "<Name><![CDATA[a<b & c]]></Name></Server></Servers></FileZilla3>",
        );
        let servers = parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "a<b & c");
        assert_eq!(
            servers[0].fields.get("Pass").map(String::as_str),
            Some("QUJD")
        );
    }

    /// What `export_filezilla` escapes, the importer reads back unchanged.
    #[test]
    fn exported_names_and_users_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sitemanager.xml");
        let servers = [FileZillaExportServer {
            id: "R&D <\"prod\"> it's".to_string(),
            name: "R&D <\"prod\"> it's".to_string(),
            host: "ftp.example.com".to_string(),
            port: 21,
            username: "a&b".to_string(),
            protocol: Some("sftp".to_string()),
            options: None,
            initial_path: None,
        }];
        let passwords = HashMap::from([(servers[0].name.clone(), "p&ss".to_string())]);
        assert_eq!(export_filezilla(&servers, &passwords, &path), Ok(1));
        let written = std::fs::read_to_string(&path).expect("read export");
        let imported = parse_sitemanager_xml(&written);
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "R&D <\"prod\"> it's");
        assert_eq!(
            imported[0].fields.get("User").map(String::as_str),
            Some("a&b")
        );
        let pass = imported[0].fields.get("Pass").expect("Pass");
        let encoding = imported[0].fields.get("_pass_encoding").expect("encoding");
        assert_eq!(
            decode_filezilla_password(pass, encoding).as_deref(),
            Some("p&ss")
        );
    }

    #[test]
    fn filezilla_372_sites_import_with_active_mode_and_ssh_proxy_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1"><Servers>"#,
                r#"<Server><Host>active.example.com</Host><Protocol>0</Protocol>"#,
                r#"<Parameter Name="data_mode_override">active</Parameter>"#,
                r#"<BypassProxy>1</BypassProxy><Name>Active through SSH</Name></Server>"#,
                r#"<Server><Host>passive.example.com</Host><Protocol>0</Protocol>"#,
                r#"<PasvMode>MODE_PASSIVE</PasvMode><BypassProxy>0</BypassProxy>"#,
                r#"<Name>Passive through SSH</Name></Server>"#,
                r#"</Servers></FileZilla3>"#,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1"><Settings>"#,
                r#"<Setting name="Proxy type">4</Setting>"#,
                r#"<Setting name="Proxy host">bastion.example.com</Setting>"#,
                r#"</Settings></FileZilla3>"#,
            ),
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.total_servers, 2);
        assert_eq!(result.servers.len(), 2);
        assert!(result.skipped.is_empty());
        assert_eq!(result.warnings.len(), 2);
        assert_eq!(result.warnings[0].name, "Active through SSH");
        assert!(result.warnings[0].reason.contains("active FTP"));
        assert_eq!(result.warnings[1].name, "FileZilla global settings");
        assert!(result.warnings[1].reason.contains("SSH proxy"));
        assert!(result.warnings[1].reason.contains("1 affected site"));
    }

    #[test]
    fn every_filezilla_global_proxy_kind_is_named_in_one_file_level_warning() {
        let cases = [
            ("Proxy type", "2", "SOCKS5", 1),
            ("Proxy type", "1", "HTTP CONNECT", 1),
            ("Proxy type", "3", "SOCKS4", 1),
            ("Proxy type", "4", "SSH", 1),
            ("FTP Proxy type", "1", "USER@HOST", 0),
            ("FTP Proxy type", "2", "SITE", 0),
            ("FTP Proxy type", "3", "OPEN", 0),
            ("FTP Proxy type", "4", "custom", 0),
        ];
        for (setting, value, kind, protocol) in cases {
            let dir = tempfile::tempdir().unwrap();
            let sitemanager = dir.path().join("sitemanager.xml");
            std::fs::write(
                &sitemanager,
                format!(
                    r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>proxy.example.com</Host><Protocol>{protocol}</Protocol><Name>Proxy site</Name></Server></Servers></FileZilla3>"#
                ),
            )
            .unwrap();
            std::fs::write(
                dir.path().join("filezilla.xml"),
                format!(
                    r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="{setting}">{value}</Setting></Settings></FileZilla3>"#
                ),
            )
            .unwrap();

            let result = import_filezilla(&sitemanager).unwrap();
            assert_eq!(result.servers.len(), 1, "{setting}={value}");
            assert_eq!(result.warnings.len(), 1, "{setting}={value}");
            assert_eq!(result.warnings[0].name, "FileZilla global settings");
            assert!(
                result.warnings[0].reason.contains(kind),
                "{setting}={value}: {}",
                result.warnings[0].reason
            );
            assert!(result.warnings[0].reason.contains("1 affected site"));
        }
    }

    #[test]
    fn generic_proxy_takes_precedence_over_the_ftp_proxy_setting() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Name>FTP site</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">1</Setting><Setting name="FTP Proxy type">4</Setting></Settings></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].reason.contains("HTTP CONNECT"));
        assert!(!result.warnings[0].reason.contains("custom"));
    }

    #[test]
    fn malformed_global_settings_produce_proxy_and_transfer_mode_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Name>Default mode</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type""#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0]
            .reason
            .contains("proxy settings were not included"));
        assert!(result.warnings[0]
            .reason
            .contains("global transfer mode was not read"));
    }

    #[test]
    fn plain_cdata_and_entity_proxy_values_parse_once_at_setting_end() {
        for value in ["4", "<![CDATA[4]]>\n", "&#52;"] {
            let dir = tempfile::tempdir().unwrap();
            let sitemanager = dir.path().join("sitemanager.xml");
            std::fs::write(
                &sitemanager,
                r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>Proxy site</Name></Server></Servers></FileZilla3>"#,
            )
            .unwrap();
            std::fs::write(
                dir.path().join("filezilla.xml"),
                format!(
                    r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">{value}</Setting></Settings></FileZilla3>"#
                ),
            )
            .unwrap();

            let result = import_filezilla(&sitemanager).unwrap();
            assert_eq!(result.warnings.len(), 1, "{value}");
            assert!(result.warnings[0].reason.contains("SSH proxy"), "{value}");
        }
    }

    #[test]
    fn global_active_mode_warns_only_for_default_mode_sites() {
        for (use_pasv, override_mode, expected_warnings) in
            [("0", None, 1), ("1", None, 0), ("0", Some("passive"), 0)]
        {
            let dir = tempfile::tempdir().unwrap();
            let sitemanager = dir.path().join("sitemanager.xml");
            let parameter = override_mode
                .map(|mode| format!(r#"<Parameter Name="data_mode_override">{mode}</Parameter>"#))
                .unwrap_or_default();
            std::fs::write(
                &sitemanager,
                format!(
                    r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol>{parameter}<Name>Transfer mode site</Name></Server></Servers></FileZilla3>"#
                ),
            )
            .unwrap();
            std::fs::write(
                dir.path().join("filezilla.xml"),
                format!(
                    r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Use Pasv mode">{use_pasv}</Setting></Settings></FileZilla3>"#
                ),
            )
            .unwrap();

            let result = import_filezilla(&sitemanager).unwrap();
            assert_eq!(
                result.warnings.len(),
                expected_warnings,
                "Use Pasv mode={use_pasv}, override={override_mode:?}"
            );
            if expected_warnings == 1 {
                assert!(result.warnings[0].reason.contains("active FTP"));
            }
        }
    }

    #[test]
    fn generic_proxy_forces_passive_instead_of_an_active_mode_warning() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Parameter Name="data_mode_override">active</Parameter><Name>Proxied active preference</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">4</Setting><Setting name="Use Pasv mode">0</Setting></Settings></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].reason.contains("SSH proxy"));
        assert!(!result.warnings[0].reason.contains("active FTP"));
    }

    #[test]
    fn standalone_sitemanager_warns_that_global_settings_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3><Servers><Server>"#,
                r#"<Host>ftp.example.com</Host><Protocol>0</Protocol>"#,
                r#"<PasvMode>MODE_PASSIVE</PasvMode><BypassProxy>0</BypassProxy>"#,
                r#"<Name>Old PC export</Name></Server></Servers></FileZilla3>"#,
            ),
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].name, "FileZilla global settings");
        assert!(result.warnings[0].reason.contains("1 site may use a proxy"));
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// two sites of one name, both with the second one's password.
    #[test]
    fn two_profiles_of_one_name_export_as_two_sites_each_with_its_own_password() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<
            FileZillaExportServer,
        >(
            serde_json::json!({"host": "a.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
            serde_json::json!({"host": "b.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        assert_eq!(export_filezilla(&servers, &passwords, &path).unwrap(), 2);
        let back = import_filezilla(&path).unwrap();
        assert_eq!(
            crate::bridge_shared::names_hosts_secrets(&back.servers),
            crate::bridge_shared::owned3(&[
                ("Lab", "a.example.com", "secret-a"),
                ("Lab-2", "b.example.com", "secret-b"),
            ])
        );
    }
}

#[cfg(test)]
mod recorded_sitemanager_fixture {
    #[test]
    fn parses_recorded_sitemanager() {
        let xml = include_str!("providers/fixtures/quickxml/filezilla-sitemanager.xml");
        let servers = super::parse_sitemanager_xml(xml);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "My FTP Server");
        assert_eq!(
            servers[0].fields.get("Host").map(String::as_str),
            Some("ftp.example.com")
        );
        assert_eq!(
            servers[0].fields.get("Protocol").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            servers[0].fields.get("_pass_encoding").map(String::as_str),
            Some("base64")
        );
        assert_eq!(
            servers[0].fields.get("_folder").map(String::as_str),
            Some("Work")
        );
    }
}
