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
#[cfg(test)]
fn parse_sitemanager_xml(content: &str) -> Vec<FileZillaServer> {
    parse_sitemanager_xml_checked(content).servers
}

/// What [`parse_sitemanager_xml_checked`] read, and whether it read all of it.
struct SiteManagerParse {
    servers: Vec<FileZillaServer>,
    /// The document closed its `<FileZilla3>` root without a parse error. A
    /// file that stops early (a crash while saving, a cut copy) keeps the
    /// sites before the break, and the sites after it are not in the result.
    complete: bool,
    /// Parsing stopped at [`MAX_SERVERS`].
    capped: bool,
    /// Root matched AeroFTP v4.1.0..v4.2.7's own exporter signature
    /// (`version="3.67.1"` and `platform="*"`, which real FileZilla never writes).
    legacy_aeroftp_export: bool,
}

fn parse_sitemanager_xml_checked(content: &str) -> SiteManagerParse {
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
    let mut malformed = false;
    let mut root_closed = false;
    let mut legacy_aeroftp_export = false;
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
    // cleanly before the error; the caller is told the file was not read whole.
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(_) => {
                malformed = true;
                break;
            }
        };
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
                    "FileZilla3" if !in_server => {
                        let version = e
                            .try_get_attribute("version")
                            .ok()
                            .flatten()
                            .map(|a| a.value.into_owned());
                        let platform = e
                            .try_get_attribute("platform")
                            .ok()
                            .flatten()
                            .map(|a| a.value.into_owned());
                        // Real FileZilla writes platform "windows", "mac", or "*nix"
                        // (`src/commonui/xml_file.cpp:177-185`). Only AeroFTP
                        // v4.1.0..v4.2.7 wrote `version="3.67.1"` with `platform="*"`.
                        if version.as_deref() == Some("3.67.1") && platform.as_deref() == Some("*")
                        {
                            legacy_aeroftp_export = true;
                        }
                    }
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
                if !in_server && e.name().as_ref() == "FileZilla3" {
                    root_closed = true;
                }
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
                if !in_server && tag == "FileZilla3" {
                    root_closed = true;
                }
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
    SiteManagerParse {
        servers,
        // A capped parse stops before the root closes; that is reported as the cap.
        complete: !malformed && (root_closed || capped),
        capped,
        legacy_aeroftp_export,
    }
}

/// Read at most `max_bytes` UTF-8 bytes from `reader`, refusing oversized input
/// without reading past `max_bytes + 1` bytes.
fn read_bounded_utf8<R: std::io::Read>(reader: R, max_bytes: u64) -> Result<String, String> {
    use std::io::Read;

    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "File too large (max {} MB)",
            max_bytes.div_ceil(1024 * 1024)
        ));
    }
    String::from_utf8(bytes).map_err(|e| format!("invalid UTF-8: {e}"))
}

/// Read a regular UTF-8 file from a single opened handle, enforcing `max_bytes`
/// during the read so a file growing or swapped after a metadata check cannot
/// consume unbounded memory.
fn read_bounded_utf8_file(path: &Path, max_bytes: u64) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("metadata: {e}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "File too large (max {} MB)",
            max_bytes.div_ceil(1024 * 1024)
        ));
    }
    read_bounded_utf8(file, max_bytes)
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
    filezilla_global_settings_with_limit(sitemanager_path, MAX_FILE_SIZE)
}

fn filezilla_global_settings_with_limit(
    sitemanager_path: &Path,
    max_bytes: u64,
) -> Option<FileZillaGlobalSettings> {
    use quick_xml::events::Event;

    let path = sitemanager_path.with_file_name("filezilla.xml");
    let Ok(content) = read_bounded_utf8_file(&path, max_bytes) else {
        return None;
    };
    let mut reader = quick_xml::Reader::from_str(&content);
    let mut settings = FileZillaGlobalSettings::default();
    let mut depth: usize = 0;
    let mut in_root = false;
    let mut in_settings = false;
    let mut saw_completed_settings = false;
    let mut saw_completed_root = false;
    let mut setting_name = None;
    let mut setting_value = String::new();
    let mut buf = Vec::new();
    loop {
        let Ok(event) = reader.read_event_into(&mut buf) else {
            return None;
        };
        match event {
            Event::Eof => {
                return (depth == 0
                    && saw_completed_root
                    && saw_completed_settings
                    && setting_name.is_none())
                .then_some(settings);
            }
            Event::Start(e) => {
                let name = e.name();
                if depth == 0 && name.as_ref() == "FileZilla3" {
                    in_root = true;
                } else if in_root && depth == 1 && name.as_ref() == "Settings" {
                    in_settings = true;
                } else if in_settings && depth == 2 && name.as_ref() == "Setting" {
                    setting_name = e
                        .try_get_attribute("name")
                        .ok()
                        .flatten()
                        .map(|a| a.value.into_owned());
                    setting_value.clear();
                }
                depth += 1;
            }
            Event::Empty(e) => {
                let name = e.name();
                if in_root && depth == 1 && name.as_ref() == "Settings" {
                    saw_completed_settings = true;
                } else if in_settings && depth == 2 && name.as_ref() == "Setting" {
                    if let Some(attr) = e
                        .try_get_attribute("name")
                        .ok()
                        .flatten()
                        .map(|a| a.value.into_owned())
                    {
                        match attr.as_str() {
                            "Proxy type" => settings.generic_type = 0,
                            "FTP Proxy type" => settings.ftp_type = 0,
                            "Use Pasv mode" => settings.use_pasv_mode = 1,
                            _ => {}
                        }
                    }
                }
            }
            Event::Text(e) if depth == 3 && setting_name.is_some() => {
                setting_value.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::CData(e) if depth == 3 && setting_name.is_some() => {
                setting_value.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(e) if depth == 3 && setting_name.is_some() => {
                match crate::providers::xml_text::xml_entity_to_str(&e) {
                    Some(expansion) => setting_value.push_str(&expansion),
                    None => {
                        setting_value.push('&');
                        setting_value.push_str(&e);
                        setting_value.push(';');
                    }
                }
            }
            Event::End(e) => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let name = e.name();
                if in_settings && depth == 2 && name.as_ref() == "Setting" {
                    let value = setting_value.trim();
                    match setting_name.as_deref() {
                        Some("Proxy type") => settings.generic_type = value.parse().unwrap_or(0),
                        Some("FTP Proxy type") => settings.ftp_type = value.parse().unwrap_or(0),
                        Some("Use Pasv mode") => {
                            settings.use_pasv_mode = value.parse().unwrap_or(1)
                        }
                        _ => {}
                    }
                    setting_name = None;
                    setting_value.clear();
                } else if in_settings && depth == 1 && name.as_ref() == "Settings" {
                    in_settings = false;
                    saw_completed_settings = true;
                } else if in_root && depth == 0 && name.as_ref() == "FileZilla3" {
                    in_root = false;
                    saw_completed_root = true;
                }
            }
            _ => {}
        }
        buf.clear();
    }
}

/// FileZilla 3.72 `ProxyType` (`src/engine/proxy.h:7-15`):
/// `NONE = 0, HTTP = 1, SOCKS5 = 2, SOCKS4 = 3, SSH = 4, count = 5`.
/// Both `controlsocket.cpp:848` and `ftp/logon.cpp:52` treat only
/// `NONE < proxy_type < count` as an active generic proxy; out-of-range
/// values do not force passive mode or suppress the FTP proxy.
fn is_known_generic_proxy(proxy_type: u32) -> bool {
    (1..=4).contains(&proxy_type)
}

fn generic_proxy_kind(proxy_type: u32) -> String {
    match proxy_type {
        1 => "HTTP CONNECT".to_string(),
        2 => "SOCKS5".to_string(),
        3 => "SOCKS4".to_string(),
        4 => "SSH".to_string(),
        other => format!("unknown proxy type {other}"),
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

/// Whether a server entry in a FileZilla XML document is an S3 profile written
/// by AeroFTP v4.1.0..v4.2.7 (which emitted `Protocol=6` under
/// `<FileZilla3 version="3.67.1" platform="*">` without any native FileZilla
/// site fields such as `Type`, `PasvMode`, `EncodingType`, `BypassProxy`, or
/// `Parameter`). Real FileZilla never writes `platform="*"`.
fn is_legacy_aeroftp_s3_server(legacy_aeroftp_export: bool, server: &FileZillaServer) -> bool {
    if !legacy_aeroftp_export || server.fields.get("Protocol").map(String::as_str) != Some("6") {
        return false;
    }
    const NATIVE_FILEZILLA_FIELDS: &[&str] = &[
        "Type",
        "PasvMode",
        "EncodingType",
        "BypassProxy",
        "TimezoneOffset",
        "MaximumMultipleConnections",
        "SyncBrowsing",
        "DirectoryComparison",
    ];
    !NATIVE_FILEZILLA_FIELDS
        .iter()
        .any(|key| server.fields.contains_key(*key))
        && !server
            .fields
            .keys()
            .any(|key| key.starts_with("_parameter_"))
}

/// Map a FileZilla server entry to an AeroFTP profile.
#[cfg(test)]
fn map_server(server: &FileZillaServer) -> Option<MappedProfile> {
    map_server_with_context(server, false)
}

fn map_server_with_context(
    server: &FileZillaServer,
    legacy_aeroftp_s3: bool,
) -> Option<MappedProfile> {
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
        .map(|d| parse_filezilla_remote_dir(d))
        .filter(|d| !d.is_empty());

    // FileZilla 3.72 `ServerProtocol` (`src/include/server.h:11-24`, values
    // never change): 0 = FTP (explicit TLS if available), 1 = SFTP, 2 = HTTP,
    // 3 = FTPS implicit, 4 = FTPES explicit, 5 = HTTPS, 6 = INSECURE_FTP
    // (plain FTP only), 7 = S3.
    let (protocol, default_port) = if legacy_aeroftp_s3 {
        ("s3", 443u32)
    } else {
        match fz_protocol {
            0 => ("ftp", 21u32),
            1 => ("sftp", 22),
            3 => ("ftps", 990), // implicit
            4 => ("ftps", 21),  // explicit
            6 => ("ftp", 21),   // plain FTP only
            7 => ("s3", 443),
            _ => {
                log::info!(
                    "FileZilla server '{}': unsupported Protocol={}",
                    server.name,
                    fz_protocol
                );
                return None;
            }
        }
    };

    let actual_port = if port == 0 { default_port } else { port };

    // Build protocol-specific options
    let mut options = serde_json::Map::new();
    let mut initial_path = remote_dir;

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
    } else if protocol == "ftp" && fz_protocol == 6 {
        // "Only use plain FTP (insecure)": no AUTH TLS attempt.
        options.insert(
            "tlsMode".to_string(),
            serde_json::Value::String("none".to_string()),
        );
    } else if protocol == "s3" && !legacy_aeroftp_s3 {
        // FileZilla Pro S3 stores `/bucket` or `/bucket/prefix/...` in
        // `<RemoteDir>`. AeroFTP's `S3Config::from_provider_config` reads the
        // bucket from `options.bucket` and starts inside `initial_path`.
        // Split the bucket off once so empty interior segments (e.g.
        // `/reports//2026`) survive verbatim. Older AeroFTP v4.1.0..v4.2.7
        // exports (`legacy_aeroftp_s3`) wrote only `initial_path` without the
        // bucket, so we must not infer a bucket from their prefix.
        if let Some(ref decoded) = initial_path {
            let trimmed_root = decoded.strip_prefix('/').unwrap_or(decoded.as_str());
            if !trimmed_root.is_empty() {
                match trimmed_root.split_once('/') {
                    Some((bucket, rest)) if !bucket.is_empty() => {
                        options.insert(
                            "bucket".to_string(),
                            serde_json::Value::String(bucket.to_string()),
                        );
                        initial_path = if rest.is_empty() {
                            None
                        } else {
                            Some(format!("/{rest}"))
                        };
                    }
                    None => {
                        options.insert(
                            "bucket".to_string(),
                            serde_json::Value::String(trimmed_root.to_string()),
                        );
                        initial_path = None;
                    }
                    _ => {}
                }
            }
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
        initial_path,
    })
}

/// Encode a POSIX path into FileZilla's `CServerPath::GetSafePath()` format
/// (`FileZilla_3.72.1_src/src/commonui/site_manager.cpp:63-65, 202` and
/// `src/engine/serverpath.cpp:290-446`): `"1 0"` for `/` or
/// `"1 0 <len> <segment>..."` (ServerType::UNIX = 1, prefix_len = 0).
/// Because FileZilla's `DoSetSafePath` rejects zero-length segments, an empty
/// segment produced by `//` or a trailing `/` is folded as a trailing `/` onto
/// the preceding segment so exact S3 prefixes like `/reports//2026` round-trip.
fn format_filezilla_remote_dir(path: &str) -> String {
    let without_root = path.strip_prefix('/').unwrap_or(path);
    if without_root.is_empty() {
        return "1 0".to_string();
    }
    let mut segments: Vec<String> = Vec::new();
    for raw in without_root.split('/') {
        if raw.is_empty() {
            if let Some(prev) = segments.last_mut() {
                prev.push('/');
            }
        } else {
            segments.push(raw.to_string());
        }
    }
    if segments.is_empty() {
        return "1 0".to_string();
    }
    let mut out = String::from("1 0");
    for seg in &segments {
        out.push(' ');
        out.push_str(&seg.chars().count().to_string());
        out.push(' ');
        out.push_str(seg);
    }
    out
}

/// Parse FileZilla's encoded remote directory format (`CServerPath::GetSafePath`,
/// `src/engine/serverpath.cpp:290-446`): `"1 0"` (root) or
/// `"1 0 <len> <segment> <len> <segment>..."`, or a plain path (for legacy
/// AeroFTP exports). Never pre-allocates the declared segment length before
/// verifying it fits in the remaining input.
fn parse_filezilla_remote_dir(encoded: &str) -> String {
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed == "1 0" {
        return "/".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("1 0 ") {
        let mut chars = rest.chars().peekable();
        let mut segments: Vec<String> = Vec::new();
        let mut parsed_ok = true;

        while chars.peek().is_some() {
            while matches!(chars.peek(), Some(' ')) {
                chars.next();
            }
            if chars.peek().is_none() {
                break;
            }
            let mut len_str = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_digit() {
                    len_str.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
            let Ok(seg_len) = len_str.parse::<usize>() else {
                parsed_ok = false;
                break;
            };
            if seg_len == 0 || seg_len > rest.len() {
                parsed_ok = false;
                break;
            }
            if chars.next() != Some(' ') {
                parsed_ok = false;
                break;
            }
            let mut segment = String::new();
            for _ in 0..seg_len {
                match chars.next() {
                    Some(ch) => segment.push(ch),
                    None => {
                        parsed_ok = false;
                        break;
                    }
                }
            }
            if !parsed_ok {
                break;
            }
            if !segment.is_empty() {
                segments.push(segment);
            }
        }

        if parsed_ok && !segments.is_empty() {
            return format!("/{}", segments.join("/"));
        }
    }
    trimmed.to_string()
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
    let content = read_bounded_utf8_file(config_path, MAX_FILE_SIZE)
        .map_err(|e| format!("Read FileZilla config: {}", e))?;

    let parsed = parse_sitemanager_xml_checked(&content);
    let fz_servers = parsed.servers;
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
        let legacy_aeroftp_s3 =
            is_legacy_aeroftp_s3_server(parsed.legacy_aeroftp_export, fz_server);

        match map_server_with_context(fz_server, legacy_aeroftp_s3) {
            Some(mapped) => {
                if legacy_aeroftp_s3 {
                    warnings.push(FileZillaImportWarning {
                        name: fz_server.name.clone(),
                        reason: "Recovered as S3 from an older AeroFTP FileZilla export that wrote S3 as Protocol=6 without exporting the S3 bucket; set the bucket name before connecting (RemoteDir was kept as the start path)".to_string(),
                    });
                }
                // FileZilla 3.72 reads BypassProxy as an integer
                // (`src/commonui/xml_file.cpp:541`: `GetTextElementInt(node, "BypassProxy", false) == 1`).
                let bypass_proxy = fz_server
                    .fields
                    .get("BypassProxy")
                    .and_then(|value| value.trim().parse::<i64>().ok())
                    == Some(1);
                let uses_generic_proxy = !bypass_proxy
                    && global_settings
                        .as_ref()
                        .is_some_and(|settings| is_known_generic_proxy(settings.generic_type));
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
                let proxy_label = generic_proxy_kind(settings.generic_type);
                let reason = if is_known_generic_proxy(settings.generic_type) {
                    format!(
                        "FileZilla's global {proxy_label} proxy is not imported; {} affected {} will connect directly through AeroFTP",
                        generic_proxy_sites,
                        if generic_proxy_sites == 1 { "site" } else { "sites" },
                    )
                } else {
                    format!(
                        "FileZilla's global settings specify {proxy_label}, which is not imported; {} affected {} will connect directly through AeroFTP",
                        generic_proxy_sites,
                        if generic_proxy_sites == 1 { "site" } else { "sites" },
                    )
                };
                warnings.push(FileZillaImportWarning {
                    name: "FileZilla global settings".to_string(),
                    reason,
                });
            }
            if !is_known_generic_proxy(settings.generic_type)
                && settings.ftp_type != 0
                && ftp_proxy_sites > 0
            {
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

    if !parsed.complete {
        warnings.push(FileZillaImportWarning {
            name: "FileZilla site manager".to_string(),
            reason: format!(
                "sitemanager.xml is malformed or ends before its closing tag; {} {} read before that point, and any sites after it were not imported",
                total_servers,
                if total_servers == 1 { "site was" } else { "sites were" },
            ),
        });
    }
    if parsed.capped {
        warnings.push(FileZillaImportWarning {
            name: "FileZilla site manager".to_string(),
            reason: format!(
                "the import stopped at the {MAX_SERVERS}-site limit; later sites were not imported"
            ),
        });
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
    // Export with version 3.72.1 so Protocol=6 (plain FTP) and Protocol=7 (S3)
    // stay unambiguous from older AeroFTP v4.1.0..v4.2.7 `3.67.1` exports.
    xml.push_str("<FileZilla3 version=\"3.72.1\" platform=\"*\">\n");
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
            // 4 explicit, 6 plain FTP only, 0 explicit if the server offers it.
            "ftp" | "ftps" => match crate::bridge_shared::ftp_tls_mode_for_export(
                protocol,
                server.options.as_ref(),
            ) {
                Some("implicit") => 3,
                Some("explicit") => 4,
                Some("none") => 6,
                _ => 0,
            },
            // FileZilla's S3 is 7; 6 is INSECURE_FTP.
            "s3" => 7,
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

        // Remote directory: encode in FileZilla's `CServerPath::GetSafePath()`
        // format for all protocols (`site_manager.cpp:63-65, 202`), because
        // FileZilla loads `<RemoteDir>` only through `SetSafePath`. For S3,
        // prepend `/<bucket>` while preserving exact prefix separators (such
        // as `/reports//2026`).
        let exported_remote_dir: Option<String> = if protocol == "s3" {
            let bucket = server
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|b| !b.is_empty());
            match (bucket, server.initial_path.as_deref()) {
                (Some(b), Some(p)) if !p.is_empty() => {
                    let suffix = p.strip_prefix('/').unwrap_or(p);
                    if suffix.is_empty() {
                        Some(format!("/{b}"))
                    } else {
                        Some(format!("/{b}/{suffix}"))
                    }
                }
                (Some(b), _) => Some(format!("/{b}")),
                (None, Some(p)) if !p.is_empty() => Some(p.to_string()),
                _ => None,
            }
        } else {
            server
                .initial_path
                .as_ref()
                .filter(|p| !p.is_empty())
                .cloned()
        };
        if let Some(ref path) = exported_remote_dir {
            let safe_path = format_filezilla_remote_dir(path);
            xml.push_str(&format!(
                "      <RemoteDir>{}</RemoteDir>\n",
                sanitize(&safe_path)
            ));
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
                ("Protocol".to_string(), "7".to_string()),
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
        for malformed in [
            "",
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type""#,
            r#"<?xml version="1.0"?><FileZilla3><Settings>"#,
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">4</Setting></Settings>"#,
            r#"<?xml version="1.0"?><FileZilla3/>"#,
            r#"<?xml version="1.0"?><FileZilla3></FileZilla3>"#,
            r#"<?xml version="1.0"?><Other><Settings></Settings></Other>"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let sitemanager = dir.path().join("sitemanager.xml");
            std::fs::write(
                &sitemanager,
                r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Name>Default mode</Name></Server></Servers></FileZilla3>"#,
            )
            .unwrap();
            std::fs::write(dir.path().join("filezilla.xml"), malformed).unwrap();

            let result = import_filezilla(&sitemanager).unwrap();
            assert_eq!(result.warnings.len(), 1, "{malformed}");
            assert!(result.warnings[0]
                .reason
                .contains("proxy settings were not included"));
            assert!(result.warnings[0]
                .reason
                .contains("global transfer mode was not read"));
        }
    }

    #[test]
    fn complete_empty_settings_document_is_accepted_as_defaults() {
        for valid_empty in [
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
            r#"<?xml version="1.0"?><FileZilla3><Settings></Settings></FileZilla3>"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let sitemanager = dir.path().join("sitemanager.xml");
            std::fs::write(
                &sitemanager,
                r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Name>Default mode</Name></Server></Servers></FileZilla3>"#,
            )
            .unwrap();
            std::fs::write(dir.path().join("filezilla.xml"), valid_empty).unwrap();

            let result = import_filezilla(&sitemanager).unwrap();
            assert!(
                result.warnings.is_empty(),
                "expected no warnings for valid empty settings {valid_empty}: {:?}",
                result
                    .warnings
                    .iter()
                    .map(|w| (&w.name, &w.reason))
                    .collect::<Vec<_>>()
            );
        }
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

    #[test]
    fn insecure_ftp_protocol_6_imports_as_plain_ftp_with_its_transfer_mode_warning() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3><Servers><Server>"#,
                r#"<Host>ftp.example.com</Host><Protocol>6</Protocol>"#,
                r#"<Parameter Name="data_mode_override">active</Parameter>"#,
                r#"<BypassProxy>1</BypassProxy><Name>Plain FTP</Name></Server>"#,
                r#"</Servers></FileZilla3>"#,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings></Settings></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.servers.len(), 1);
        let server = &result.servers[0];
        assert_eq!(server.protocol.as_deref(), Some("ftp"));
        assert_eq!(server.port, 21);
        assert_eq!(
            server.options.as_ref().and_then(|o| o.get("tlsMode")),
            Some(&serde_json::json!("none"))
        );
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].reason.contains("active FTP"));
    }

    #[test]
    fn s3_is_filezilla_protocol_7_on_import_and_export() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>s3.amazonaws.com</Host><Protocol>7</Protocol><User>AKIAEXAMPLE</User><Name>Bucket</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        let result = import_filezilla(&sitemanager).unwrap();
        assert!(result.skipped.is_empty());
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].protocol.as_deref(), Some("s3"));

        let out = dir.path().join("export.xml");
        let servers = [
            FileZillaExportServer {
                id: "s3".to_string(),
                name: "S3".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "AKIAEXAMPLE".to_string(),
                protocol: Some("s3".to_string()),
                options: None,
                initial_path: None,
            },
            FileZillaExportServer {
                id: "plain".to_string(),
                name: "Plain".to_string(),
                host: "ftp.example.com".to_string(),
                port: 21,
                username: "u".to_string(),
                protocol: Some("ftp".to_string()),
                options: Some(serde_json::json!({ "tlsMode": "none" })),
                initial_path: None,
            },
        ];
        assert_eq!(export_filezilla(&servers, &HashMap::new(), &out), Ok(2));
        let written = parse_sitemanager_xml(&std::fs::read_to_string(&out).unwrap());
        let protocol_of = |name: &str| {
            written
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.fields.get("Protocol").cloned())
        };
        assert_eq!(protocol_of("S3").as_deref(), Some("7"));
        assert_eq!(protocol_of("Plain").as_deref(), Some("6"));
    }

    #[test]
    fn a_cut_sitemanager_reports_that_later_sites_were_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3><Servers>"#,
                r#"<Server><Host>h1</Host><Protocol>1</Protocol><BypassProxy>1</BypassProxy><Name>good</Name></Server>"#,
                r#"<Server><Host>h2</Host><Name>trunc"#,
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].name, "FileZilla site manager");
        assert!(result.warnings[0].reason.contains("not imported"));
    }

    #[test]
    fn bypass_proxy_is_read_as_an_integer_like_filezilla() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Parameter Name="data_mode_override">active</Parameter><BypassProxy>01</BypassProxy><Name>Bypass</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">2</Setting></Settings></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].reason.contains("active FTP"));
    }

    #[test]
    fn out_of_range_generic_proxy_warns_without_hiding_active_or_ftp_proxy_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers><Server><Host>ftp.example.com</Host><Protocol>0</Protocol><Parameter Name="data_mode_override">active</Parameter><Name>Active FTP</Name></Server></Servers></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings><Setting name="Proxy type">99</Setting><Setting name="FTP Proxy type">2</Setting></Settings></FileZilla3>"#,
        )
        .unwrap();

        let result = import_filezilla(&sitemanager).unwrap();
        assert_eq!(result.warnings.len(), 3);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.name == "Active FTP" && w.reason.contains("active FTP")));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.reason.contains("unknown proxy type 99")));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.reason.contains("FTP SITE proxy")));
    }

    #[test]
    fn sidecar_size_limit_is_enforced_during_read() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        let valid_empty = r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#;
        std::fs::write(&sitemanager, valid_empty).unwrap();
        let sidecar = dir.path().join("filezilla.xml");
        std::fs::write(&sidecar, valid_empty).unwrap();

        let exact_len = valid_empty.len() as u64;
        assert!(filezilla_global_settings_with_limit(&sitemanager, exact_len).is_some());
        assert!(filezilla_global_settings_with_limit(&sitemanager, exact_len - 1).is_none());
        assert!(read_bounded_utf8_file(&sidecar, exact_len).is_ok());
        assert!(read_bounded_utf8_file(&sidecar, exact_len - 1).is_err());
        // Read-time enforcement independent of metadata.len() (growing file).
        assert!(read_bounded_utf8(std::io::Cursor::new(valid_empty.as_bytes()), exact_len).is_ok());
        assert!(
            read_bounded_utf8(std::io::Cursor::new(valid_empty.as_bytes()), exact_len - 1).is_err()
        );
    }

    #[test]
    fn legacy_aeroftp_protocol_6_s3_export_recovers_as_s3_while_native_protocol_6_stays_ftp() {
        let dir = tempfile::tempdir().unwrap();
        // Exact shape produced by AeroFTP v4.1.0..v4.2.7 `export_filezilla`:
        // `options.bucket` was never written, and `<RemoteDir>` carried only
        // `initial_path` (e.g. `/media/2026` inside bucket `archive`).
        let legacy_xml = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<FileZilla3 version=\"3.67.1\" platform=\"*\">\n",
            "  <Servers>\n",
            "    <Server>\n",
            "      <Host>s3.amazonaws.com</Host>\n",
            "      <Port>443</Port>\n",
            "      <Protocol>6</Protocol>\n",
            "      <User>DEMOACCESSKEY</User>\n",
            "      <Pass encoding=\"base64\">c2VjcmV0</Pass>\n",
            "      <Logontype>1</Logontype>\n",
            "      <RemoteDir>/media/2026</RemoteDir>\n",
            "      <Name>Legacy S3</Name>\n",
            "    </Server>\n",
            "  </Servers>\n",
            "</FileZilla3>\n",
        );
        let legacy_path = dir.path().join("legacy-sitemanager.xml");
        std::fs::write(&legacy_path, legacy_xml).unwrap();
        let legacy = import_filezilla(&legacy_path).unwrap();
        assert_eq!(legacy.servers.len(), 1);
        assert_eq!(legacy.servers[0].protocol.as_deref(), Some("s3"));
        assert_eq!(legacy.servers[0].port, 443);
        assert_eq!(
            legacy.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("bucket")),
            None,
            "legacy v4.1.0..v4.2.7 exports carry only initial_path and must not infer a bucket"
        );
        assert_eq!(
            legacy.servers[0].initial_path.as_deref(),
            Some("/media/2026")
        );
        assert!(legacy.warnings.iter().any(|w| w.name == "Legacy S3"
            && w.reason.contains("Recovered as S3")
            && w.reason.contains("bucket")));

        // Real FileZilla (platform="*nix") or a site with native FileZilla fields stays plain FTP.
        for native_xml in [
            r#"<?xml version="1.0"?><FileZilla3 version="3.67.1" platform="*nix"><Servers><Server><Host>ftp.example.com</Host><Port>21</Port><Protocol>6</Protocol><User>u</User><Name>Real FZ</Name></Server></Servers></FileZilla3>"#,
            r#"<?xml version="1.0"?><FileZilla3 version="3.67.1" platform="*"><Servers><Server><Host>ftp.example.com</Host><Port>21</Port><Protocol>6</Protocol><BypassProxy>0</BypassProxy><Name>Native Field</Name></Server></Servers></FileZilla3>"#,
            r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*"><Servers><Server><Host>ftp.example.com</Host><Port>21</Port><Protocol>6</Protocol><Name>New Export Plain</Name></Server></Servers></FileZilla3>"#,
        ] {
            let native_path = dir.path().join("native.xml");
            std::fs::write(&native_path, native_xml).unwrap();
            let native = import_filezilla(&native_path).unwrap();
            assert_eq!(native.servers.len(), 1, "{native_xml}");
            assert_eq!(
                native.servers[0].protocol.as_deref(),
                Some("ftp"),
                "{native_xml}"
            );
            assert_eq!(
                native.servers[0]
                    .options
                    .as_ref()
                    .and_then(|o| o.get("tlsMode")),
                Some(&serde_json::json!("none")),
                "{native_xml}"
            );
        }
    }

    #[test]
    fn filezilla_s3_protocol_7_preserves_bucket_and_prefix_from_remote_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server>"#,
                r#"<Host>s3.amazonaws.com</Host><Port>443</Port><Protocol>7</Protocol>"#,
                r#"<User>DEMOACCESSKEY</User><Pass encoding="base64">c2VjcmV0</Pass>"#,
                r#"<RemoteDir>1 0 5 media 4 2026 6 photos</RemoteDir>"#,
                r#"<Name>Photos S3</Name></Server></Servers></FileZilla3>"#,
            ),
        )
        .unwrap();

        let imported = import_filezilla(&sitemanager).unwrap();
        assert_eq!(imported.servers.len(), 1);
        let s3 = &imported.servers[0];
        assert_eq!(s3.protocol.as_deref(), Some("s3"));
        assert_eq!(
            s3.options.as_ref().and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        assert_eq!(s3.initial_path.as_deref(), Some("/2026/photos"));

        // Verify the imported S3 profile satisfies S3Config::from_provider_config.
        let mut extra = HashMap::new();
        if let Some(obj) = s3.options.as_ref().and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(sv) = v.as_str() {
                    extra.insert(k.clone(), sv.to_string());
                }
            }
        }
        let provider_cfg = crate::providers::types::ProviderConfig {
            name: s3.name.clone(),
            provider_type: crate::providers::types::ProviderType::S3,
            host: s3.host.clone(),
            port: Some(s3.port as u16),
            username: Some(s3.username.clone()),
            password: s3.credential.clone(),
            initial_path: s3.initial_path.clone(),
            extra,
        };
        let s3_cfg = crate::providers::types::S3Config::from_provider_config(&provider_cfg)
            .expect("S3Config must accept imported bucket");
        assert_eq!(s3_cfg.bucket, "media");

        // Round-trip export -> import preserves bucket and prefix.
        let exported_path = dir.path().join("exported.xml");
        let export_row = FileZillaExportServer {
            id: "s3-rt".to_string(),
            name: s3.name.clone(),
            host: s3.host.clone(),
            port: s3.port,
            username: s3.username.clone(),
            protocol: s3.protocol.clone(),
            options: s3.options.clone(),
            initial_path: s3.initial_path.clone(),
        };
        assert_eq!(
            export_filezilla(&[export_row], &HashMap::new(), &exported_path),
            Ok(1)
        );
        let roundtripped = import_filezilla(&exported_path).unwrap();
        assert_eq!(roundtripped.servers.len(), 1);
        assert_eq!(roundtripped.servers[0].protocol.as_deref(), Some("s3"));
        assert_eq!(
            roundtripped.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        assert_eq!(
            roundtripped.servers[0].initial_path.as_deref(),
            Some("/2026/photos")
        );
    }

    #[test]
    fn remote_dir_decoder_rejects_oversized_segment_length_without_allocating() {
        let oversized = "1 0 9223372036854775808 x";
        assert_eq!(parse_filezilla_remote_dir(oversized), oversized);
        assert_eq!(
            parse_filezilla_remote_dir("1 0 5 media 4 2026"),
            "/media/2026"
        );
    }

    #[test]
    fn s3_prefix_with_empty_segments_round_trips_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let exported_path = dir.path().join("s3-empty-seg.xml");
        let export_rows = [
            FileZillaExportServer {
                id: "s3-double-slash".to_string(),
                name: "DoubleSlash".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "DEMOACCESSKEY".to_string(),
                protocol: Some("s3".to_string()),
                options: Some(serde_json::json!({ "bucket": "media" })),
                initial_path: Some("/reports//2026".to_string()),
            },
            FileZillaExportServer {
                id: "s3-ordinary".to_string(),
                name: "Ordinary".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "DEMOACCESSKEY".to_string(),
                protocol: Some("s3".to_string()),
                options: Some(serde_json::json!({ "bucket": "media" })),
                initial_path: Some("/2026/photos".to_string()),
            },
        ];
        assert_eq!(
            export_filezilla(&export_rows, &HashMap::new(), &exported_path),
            Ok(2)
        );
        let imported = import_filezilla(&exported_path).unwrap();
        assert_eq!(imported.servers.len(), 2);
        let double_slash = imported
            .servers
            .iter()
            .find(|s| s.name == "DoubleSlash")
            .unwrap();
        assert_eq!(
            double_slash.options.as_ref().and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        assert_eq!(double_slash.initial_path.as_deref(), Some("/reports//2026"));

        let ordinary = imported
            .servers
            .iter()
            .find(|s| s.name == "Ordinary")
            .unwrap();
        assert_eq!(
            ordinary.options.as_ref().and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        assert_eq!(ordinary.initial_path.as_deref(), Some("/2026/photos"));
    }

    #[test]
    fn export_writes_filezilla_get_safe_path_remote_dir_for_all_protocols() {
        let dir = tempfile::tempdir().unwrap();
        let exported_path = dir.path().join("all-protocols.xml");
        let export_rows = [
            FileZillaExportServer {
                id: "ftp".to_string(),
                name: "FTP".to_string(),
                host: "ftp.example.com".to_string(),
                port: 21,
                username: "u".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                initial_path: Some("/var/www".to_string()),
            },
            FileZillaExportServer {
                id: "sftp".to_string(),
                name: "SFTP".to_string(),
                host: "sftp.example.com".to_string(),
                port: 22,
                username: "u".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                initial_path: Some("/".to_string()),
            },
            FileZillaExportServer {
                id: "s3".to_string(),
                name: "S3".to_string(),
                host: "s3.amazonaws.com".to_string(),
                port: 443,
                username: "DEMOACCESSKEY".to_string(),
                protocol: Some("s3".to_string()),
                options: Some(serde_json::json!({ "bucket": "media" })),
                initial_path: Some("/2026/photos".to_string()),
            },
        ];
        assert_eq!(
            export_filezilla(&export_rows, &HashMap::new(), &exported_path),
            Ok(3)
        );
        let raw_xml = std::fs::read_to_string(&exported_path).unwrap();
        let raw_parsed = parse_sitemanager_xml(&raw_xml);
        let raw_dir = |name: &str| {
            raw_parsed
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.fields.get("RemoteDir").cloned())
        };
        assert_eq!(raw_dir("FTP").as_deref(), Some("1 0 3 var 3 www"));
        assert_eq!(raw_dir("SFTP").as_deref(), Some("1 0"));
        assert_eq!(
            raw_dir("S3").as_deref(),
            Some("1 0 5 media 4 2026 6 photos")
        );

        let reimported = import_filezilla(&exported_path).unwrap();
        assert_eq!(reimported.servers.len(), 3);
        let by_name = |name: &str| reimported.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(by_name("FTP").initial_path.as_deref(), Some("/var/www"));
        assert_eq!(by_name("SFTP").initial_path.as_deref(), Some("/"));
        assert_eq!(
            by_name("S3").options.as_ref().and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        assert_eq!(by_name("S3").initial_path.as_deref(), Some("/2026/photos"));
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
