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
    /// Root matched AeroFTP v3.7.3..v4.2.7's own exporter signature
    /// (`version="3.67.1"` and `platform="*"`, which real FileZilla never writes).
    legacy_aeroftp_export: bool,
    /// `wchar_t` length unit implied by the root `<FileZilla3 platform="...">`
    /// attribute (`"windows"` -> UTF-16, `"mac"` / `"*nix"` -> scalar, unknown -> `None`).
    root_wchar_unit: Option<WcharUnit>,
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
    let mut xml_depth: usize = 0;
    let mut saw_root = false;
    let mut in_root_filezilla3 = false;
    let mut in_servers = false;
    let mut in_server = false;
    let mut server_depth: usize = 0;
    let mut current_fields: HashMap<String, String> = HashMap::new();
    let mut current_name = String::new();
    let mut current_tag = String::new();
    let mut current_parameter_name = String::new();
    let mut pass_encoding = String::new();
    let mut capped = false;
    let mut malformed = false;
    let mut root_closed = false;
    let mut legacy_aeroftp_export = false;
    let mut root_wchar_unit: Option<WcharUnit> = None;
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
                if in_servers && !in_server {
                    note_folder_text(&mut folder_stack, run.trim());
                }
                let tag = e.name().as_ref().to_string();
                let elem_depth = xml_depth;
                if elem_depth == 0 {
                    if saw_root {
                        // A second top-level root is malformed XML and must not
                        // overwrite the first root's platform or legacy metadata.
                        malformed = true;
                        break;
                    }
                    saw_root = true;
                }
                xml_depth += 1;
                match tag.as_str() {
                    // Require `<FileZilla3>` at document root (depth 0) so a
                    // nested `<FileZilla3>` inside another XML format cannot
                    // trigger legacy recovery or mark the parse complete.
                    "FileZilla3" if elem_depth == 0 => {
                        in_root_filezilla3 = true;
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
                        root_wchar_unit = match platform.as_deref() {
                            Some("windows") => Some(WcharUnit::Utf16),
                            Some("mac") | Some("*nix") => Some(WcharUnit::Scalar),
                            _ => None,
                        };
                        // Real FileZilla writes platform "windows", "mac", or "*nix"
                        // (`src/commonui/xml_file.cpp:177-185`). Only AeroFTP
                        // v3.7.3..v4.2.7 wrote `version="3.67.1"` with `platform="*"`.
                        if version.as_deref() == Some("3.67.1") && platform.as_deref() == Some("*")
                        {
                            legacy_aeroftp_export = true;
                        }
                    }
                    "Servers" if in_root_filezilla3 && elem_depth == 1 => {
                        in_servers = true;
                    }
                    // A site-specific bookmark is not part of the site. FileZilla 3.72.1
                    // `site_manager::Save` (`src/commonui/site_manager.cpp:71-89`) writes
                    // each `<Bookmark>` (its own `<Name>`, `<LocalDir>`, `<RemoteDir>`, ...)
                    // inside `<Server>` after the site's fields, and `ReadServerElement`
                    // (`site_manager.cpp:228-267`) reads the site from direct children of
                    // `<Server>` only. Skip the subtree so a bookmark's `<Name>` and
                    // `<RemoteDir>` do not replace the site's.
                    "Bookmark" if in_server && elem_depth == server_depth + 1 => {
                        current_tag.clear();
                        let mut bookmark = Vec::new();
                        if reader.read_to_end_into(e.name(), &mut bookmark).is_err() {
                            malformed = true;
                            break;
                        }
                        // `read_to_end_into` consumed the matching `</Bookmark>`.
                        xml_depth = elem_depth;
                    }
                    "Folder" if in_servers && !in_server => {
                        folder_stack.push((String::new(), servers.len()));
                    }
                    "Server" if in_servers && !in_server => {
                        if servers.len() >= MAX_SERVERS {
                            capped = true;
                            break;
                        }
                        in_server = true;
                        server_depth = elem_depth;
                        current_fields.clear();
                        current_name.clear();
                        pass_encoding.clear();
                        current_tag.clear();
                        current_parameter_name.clear();
                    }
                    _ if in_server && elem_depth == server_depth + 1 => {
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
                if xml_depth == 0 {
                    if saw_root {
                        malformed = true;
                        break;
                    }
                    saw_root = true;
                    if e.name().as_ref() == "FileZilla3" {
                        root_closed = true;
                    }
                }
                if !in_server {
                    if in_servers {
                        note_folder_text(&mut folder_stack, run.trim());
                    }
                    continue;
                }
                if xml_depth != server_depth + 1 {
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
                let raw_run = std::mem::take(&mut text);
                let run = raw_run.trim();
                let tag = e.name().as_ref().to_string();
                if xml_depth == 0 {
                    malformed = true;
                    break;
                }
                xml_depth -= 1;
                if in_root_filezilla3 && xml_depth == 0 && tag == "FileZilla3" {
                    in_root_filezilla3 = false;
                    in_servers = false;
                    root_closed = true;
                } else if in_servers && xml_depth == 1 && tag == "Servers" {
                    in_servers = false;
                } else if in_server && xml_depth == server_depth && tag == "Server" {
                    let display_name = if current_name.is_empty() {
                        current_fields
                            .get("Host")
                            .map(|h| crate::bridge_shared::safe_host_label(h, "unnamed"))
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
                } else if in_servers && !in_server && tag == "Folder" {
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
                } else if in_server && xml_depth == server_depth + 1 && tag == current_tag {
                    if current_tag == "Name" {
                        current_name = run.to_string();
                    } else if current_tag == "RemoteDir" {
                        // FileZilla 3.72.1 `site_manager::ReadBookmarkElement`
                        // (`src/commonui/site_manager.cpp:202`) reads `<RemoteDir>`
                        // with `GetTextElement` (untrimmed): a `GetSafePath` segment
                        // ending in a space must keep that trailing space.
                        current_fields.insert(current_tag.clone(), raw_run);
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
        root_wchar_unit,
    }
}

// The bounded, non-blocking reader is shared with the other importers.
#[cfg(test)]
use crate::bridge_shared::read_bounded_utf8;
use crate::bridge_shared::read_bounded_utf8_file;

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
    let mut saw_root = false;
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
                if depth == 0 {
                    if saw_root {
                        return None;
                    }
                    saw_root = true;
                    if name.as_ref() == "FileZilla3" {
                        in_root = true;
                    }
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
                if depth == 0 {
                    if saw_root {
                        return None;
                    }
                    saw_root = true;
                }
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
    remote_dir_warning: Option<String>,
}

/// Whether a server entry in a FileZilla XML document is an S3 profile written
/// by AeroFTP v3.7.3..v4.2.7 (which emitted `Protocol=6` under
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
    map_server_with_context(server, false, None).ok()
}

fn split_clean_host_and_port(clean: &str) -> (&str, Option<u32>) {
    if let Some(after_open) = clean.strip_prefix('[') {
        if let Some((inside, after_close)) = after_open.split_once(']') {
            let bracketed = &clean[..inside.len() + 2];
            let port = after_close
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|p| *p > 0)
                .map(u32::from);
            return (bracketed, port);
        }
        return (clean, None);
    }
    if let Some((h, p)) = clean.rsplit_once(':') {
        if !h.is_empty() && !h.contains(':') {
            if let Ok(port_u16) = p.parse::<u16>() {
                if port_u16 > 0 {
                    return (h, Some(u32::from(port_u16)));
                }
            }
        }
    }
    (clean, None)
}

fn map_server_with_context(
    server: &FileZillaServer,
    legacy_aeroftp_s3: bool,
    root_wchar_unit: Option<WcharUnit>,
) -> Result<MappedProfile, String> {
    let raw_host = server
        .fields
        .get("Host")
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "missing Host".to_string())?;
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

    let parsed_remote_dir = server
        .fields
        .get("RemoteDir")
        .map(|d| parse_filezilla_remote_dir_for_platform(d, root_wchar_unit))
        .unwrap_or(ParsedRemoteDir::Empty);

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
                let safe_name =
                    crate::bridge_shared::sanitize_warning_name(&server.name, "FileZilla site");
                log::info!(
                    "FileZilla server '{}': unsupported Protocol={}",
                    safe_name,
                    fz_protocol
                );
                return Err(format!(
                    "unsupported Protocol={}",
                    server.fields.get("Protocol").unwrap_or(&String::new())
                ));
            }
        }
    };

    // Never store raw URL credentials, query strings, or fragments in
    // `MappedProfile.host`: extract the safe host/port when `<Host>` is
    // URL-shaped or contains `@`, `?`, `#`, or `/`, and skip the site if the
    // value is ambiguous or has no valid host.
    let (host, inline_port) = if raw_host.contains("://")
        || raw_host.contains('@')
        || raw_host.contains('?')
        || raw_host.contains('#')
        || raw_host.contains('/')
    {
        let clean = crate::bridge_shared::try_endpoint_host(raw_host)
            .ok_or_else(|| "invalid or credential-bearing Host".to_string())?;
        let (h, p) = split_clean_host_and_port(&clean);
        (h.to_string(), p)
    } else {
        let (h, p) = split_clean_host_and_port(raw_host);
        (h.to_string(), p)
    };

    let actual_port = if port == 0 {
        inline_port.unwrap_or(default_port)
    } else {
        port
    };

    // Build protocol-specific options
    let mut options = serde_json::Map::new();
    let mut remote_dir_warning: Option<String> = None;
    let mut initial_path: Option<String> = match &parsed_remote_dir {
        ParsedRemoteDir::Empty => None,
        ParsedRemoteDir::SafePath(p) | ParsedRemoteDir::PlainPath(p) if !p.is_empty() => {
            Some(p.clone())
        }
        ParsedRemoteDir::SafePath(_) | ParsedRemoteDir::PlainPath(_) => None,
        ParsedRemoteDir::MalformedSafePath => {
            remote_dir_warning = Some(
                "FileZilla RemoteDir could not be decoded as a valid GetSafePath and was omitted; set the initial directory (and S3 bucket if applicable) before connecting".to_string(),
            );
            None
        }
        ParsedRemoteDir::AmbiguousSafePath => {
            remote_dir_warning = Some(
                "FileZilla RemoteDir length encoding is ambiguous without a known source platform (Windows UTF-16 vs macOS/Linux scalar lengths decode to different paths) and was omitted; set the initial directory before connecting".to_string(),
            );
            None
        }
    };

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
        // FileZilla Pro S3 connects over HTTPS. For non-AWS custom endpoints
        // (such as `objects.example.test:9000`), set an explicit `https://`
        // `options.endpoint` so `S3Config::from_provider_config` does not
        // mis-infer `http://` on ports like 9000. AWS stays implicit so virtual-
        // hosted bucket addressing is preserved.
        if let Some(clean_host) = crate::bridge_shared::try_endpoint_host(&host) {
            let aws =
                crate::bridge_shared::map_s3_provider_from_endpoint(&clean_host) == "amazon-s3";
            let default_aws_transport = aws && actual_port == 443;
            if !default_aws_transport {
                let has_explicit_port = if let Some((_, rest)) = clean_host.split_once(']') {
                    rest.starts_with(':')
                } else {
                    clean_host.contains(':')
                };
                let endpoint = if actual_port == 443 || has_explicit_port {
                    format!("https://{clean_host}")
                } else {
                    format!("https://{clean_host}:{actual_port}")
                };
                options.insert("endpoint".to_string(), serde_json::Value::String(endpoint));
            }
        }

        // FileZilla Pro S3 stores `/bucket` or `/bucket/prefix/...` in
        // `<RemoteDir>` as a `CServerPath::GetSafePath()`. Infer the bucket
        // only from a successfully decoded safe path (never from a failed or
        // ambiguous decode or a plain legacy path). Older AeroFTP
        // v3.7.3..v4.2.7 exports (`legacy_aeroftp_s3`) wrote only
        // `initial_path` without the bucket, so we must not infer a bucket
        // from their prefix either.
        if let ParsedRemoteDir::SafePath(ref decoded) = parsed_remote_dir {
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
        // `S3Config::from_provider_config` requires a non-empty `bucket`
        // before connecting, so warn when a native Protocol 7 S3 site carried
        // no decodable default RemoteDir bucket.
        if !options.contains_key("bucket") && remote_dir_warning.is_none() {
            remote_dir_warning = Some(
                "FileZilla S3 site has no default RemoteDir bucket; set the S3 bucket name before connecting".to_string(),
            );
        }
    }

    let options_val = if options.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(options))
    };

    Ok(MappedProfile {
        protocol: protocol.to_string(),
        host,
        port: actual_port,
        username,
        password,
        options: options_val,
        initial_path,
        remote_dir_warning,
    })
}

/// What one FileZilla `CServerPath::GetSafePath()` length counts: one `wchar_t`,
/// a UTF-16 code unit in the Windows build and a Unicode scalar value in the
/// macOS and Linux builds. The two differ only for a character outside the
/// Basic Multilingual Plane (an emoji, a CJK Extension B ideograph).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WcharUnit {
    Scalar,
    Utf16,
}

impl WcharUnit {
    /// The unit FileZilla on this machine's OS reads an exported file with.
    fn native() -> Self {
        if cfg!(windows) {
            WcharUnit::Utf16
        } else {
            WcharUnit::Scalar
        }
    }

    fn width(self, c: char) -> usize {
        match self {
            WcharUnit::Scalar => 1,
            WcharUnit::Utf16 => c.len_utf16(),
        }
    }

    fn len(self, s: &str) -> usize {
        s.chars().map(|c| self.width(c)).sum()
    }
}

/// FileZilla 3.72.1 `ServerType` values with a path shape of their own
/// (`FileZilla_3.72.1_src/src/include/server.h:65-79`, wire values offset by +1).
const FZ_SERVER_TYPE_UNIX: u32 = 1;
const FZ_SERVER_TYPE_DOS: u32 = 3;
const FZ_SERVER_TYPE_DOS_VIRTUAL: u32 = 8;
const FZ_SERVER_TYPE_DOS_FWD_SLASHES: u32 = 10;
const FZ_SERVER_TYPE_MAX: u32 = 11;

/// The `CServerTypeTraits` row `CServerPath::GetPath()` renders a path with
/// (`FileZilla_3.72.1_src/src/engine/serverpath.cpp:19-30`, `traits[SERVERTYPE_MAX]`).
struct FileZillaPathTraits {
    separators: &'static str,
    has_root: bool,
    left_enclosure: Option<char>,
    right_enclosure: Option<char>,
    /// `prefixmode == 1`: the prefix is written after the segments (MVS).
    prefix_is_suffix: bool,
    separator_escape: Option<char>,
    separator_after_prefix: bool,
}

fn filezilla_path_traits(server_type: u32) -> Option<FileZillaPathTraits> {
    const fn row(
        separators: &'static str,
        has_root: bool,
        left_enclosure: Option<char>,
        right_enclosure: Option<char>,
        prefix_is_suffix: bool,
        separator_escape: Option<char>,
        separator_after_prefix: bool,
    ) -> FileZillaPathTraits {
        FileZillaPathTraits {
            separators,
            has_root,
            left_enclosure,
            right_enclosure,
            prefix_is_suffix,
            separator_escape,
            separator_after_prefix,
        }
    }
    Some(match server_type {
        // DEFAULT, UNIX, VXWORKS, ZVM
        0 | 1 | 5 | 6 => row("/", true, None, None, false, None, false),
        2 => row(".", false, Some('['), Some(']'), false, Some('^'), false), // VMS
        3 => row("\\/", false, None, None, false, None, false),              // DOS
        4 => row(".", false, Some('\''), Some('\''), true, None, false),     // MVS
        7 => row(".", false, None, None, false, None, false),                // HPNONSTOP
        8 => row("\\", true, None, None, false, None, false),                // DOS_VIRTUAL
        9 => row("/", true, None, None, false, None, true),                  // CYGWIN
        10 => row("/\\", false, None, None, false, None, false),             // DOS_FWD_SLASHES
        _ => return None,
    })
}

/// `CServerPath::GetPath()`: the path FileZilla sends to the server for a
/// decoded type, prefix and segment list.
fn render_filezilla_server_path(
    server_type: u32,
    prefix: Option<&str>,
    segments: &[String],
) -> Option<String> {
    let traits = filezilla_path_traits(server_type)?;
    let separator = traits.separators.chars().next()?;
    let mut path = String::new();
    if !traits.prefix_is_suffix {
        if let Some(prefix) = prefix {
            path.push_str(prefix);
        }
    }
    if let Some(left) = traits.left_enclosure {
        path.push(left);
    }
    if segments.is_empty()
        && (!traits.has_root || prefix.is_none() || traits.separator_after_prefix)
    {
        path.push(separator);
    }
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 || (traits.has_root && (prefix.is_none() || traits.separator_after_prefix)) {
            path.push(separator);
        }
        match traits.separator_escape {
            Some(escape) => {
                for c in segment.chars() {
                    if traits.separators.contains(c) {
                        path.push(escape);
                        path.push(separator);
                    } else {
                        path.push(c);
                    }
                }
            }
            None => path.push_str(segment),
        }
    }
    if traits.prefix_is_suffix {
        if let Some(prefix) = prefix {
            path.push_str(prefix);
        }
    }
    if let Some(right) = traits.right_enclosure {
        path.push(right);
    }
    // FileZilla: "C:" is the current directory on drive C, "C:\" its root.
    if matches!(
        server_type,
        FZ_SERVER_TYPE_DOS | FZ_SERVER_TYPE_DOS_FWD_SLASHES
    ) && segments.len() == 1
    {
        path.push(separator);
    }
    Some(path)
}

/// `CServerPath::DoSetSafePath()` (`src/engine/serverpath.cpp`):
/// `<type> <prefix_len>[ <prefix>][ <len> <segment>]...`, every length counted
/// in `unit`. Stricter than FileZilla in one way: each prefix and segment must
/// end on a space or at the end of the input, which is what tells a Windows
/// (UTF-16) count from a macOS / Linux (scalar) count apart. Nothing is
/// reserved from a declared length; a segment holds only characters present.
fn decode_filezilla_safe_path(encoded: &str, unit: WcharUnit) -> Option<String> {
    let chars: Vec<char> = encoded.chars().collect();
    let number = |pos: &mut usize, limit: u32| -> Option<u32> {
        let start = *pos;
        let mut value: u32 = 0;
        while let Some(digit) = chars.get(*pos).and_then(|c| c.to_digit(10)) {
            value = value * 10 + digit;
            if value > limit {
                return None;
            }
            *pos += 1;
        }
        (*pos > start).then_some(value)
    };
    let take = |pos: &mut usize, len: usize| -> Option<String> {
        let mut out = String::new();
        let mut counted = 0usize;
        while counted < len {
            let c = *chars.get(*pos)?;
            counted += unit.width(c);
            out.push(c);
            *pos += 1;
        }
        (counted == len).then_some(out)
    };
    // After a prefix or a segment: a space, or the end of the input.
    let separator = |pos: &mut usize| -> Option<bool> {
        match chars.get(*pos) {
            None => Some(false),
            Some(' ') => {
                *pos += 1;
                Some(true)
            }
            Some(_) => None,
        }
    };

    let mut pos = 0usize;
    let server_type = number(&mut pos, FZ_SERVER_TYPE_MAX - 1)?;
    if chars.get(pos) != Some(&' ') {
        return None;
    }
    pos += 1;
    let prefix_len = number(&mut pos, 32767)? as usize;
    let mut prefix = None;
    let mut segments = Vec::new();
    let mut more = separator(&mut pos)?;
    if !more && prefix_len != 0 {
        return None;
    }
    if more && prefix_len != 0 {
        prefix = Some(take(&mut pos, prefix_len)?);
        more = separator(&mut pos)?;
    }
    while more && pos < chars.len() {
        let segment_len = number(&mut pos, 32767)? as usize;
        if segment_len == 0 || chars.get(pos) != Some(&' ') {
            return None;
        }
        pos += 1;
        segments.push(take(&mut pos, segment_len)?);
        more = separator(&mut pos)?;
    }
    render_filezilla_server_path(server_type, prefix.as_deref(), &segments)
}

/// Result of parsing a `<RemoteDir>` element.
#[derive(Debug, PartialEq, Eq)]
enum ParsedRemoteDir {
    Empty,
    SafePath(String),
    PlainPath(String),
    MalformedSafePath,
    AmbiguousSafePath,
}

/// Whether `s` begins with FileZilla's `<type> <prefix_len>` safe-path header
/// (two space-separated decimal integers), so a truncated or corrupt safe path
/// like `1 0 5 med` is distinguished from a plain legacy path like `/var/www`.
fn looks_like_filezilla_safe_path(s: &str) -> bool {
    let mut parts = s.split(' ');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && !second.is_empty()
        && second.bytes().all(|b| b.is_ascii_digit())
}

fn decode_safe_path_with_unit(encoded: &str, trimmed: &str, unit: WcharUnit) -> Option<String> {
    decode_filezilla_safe_path(encoded, unit)
        .or_else(|| (trimmed != encoded).then(|| decode_filezilla_safe_path(trimmed, unit))?)
}

/// Decode `<RemoteDir>` using the document root's `platform` attribute when
/// known (`"windows"` -> UTF-16, `"mac"`/`"*nix"` -> scalar). When the source
/// platform is unknown and both units decode to different paths, report
/// [`ParsedRemoteDir::AmbiguousSafePath`] instead of silently picking one.
fn parse_filezilla_remote_dir_for_platform(
    encoded: &str,
    root_wchar_unit: Option<WcharUnit>,
) -> ParsedRemoteDir {
    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return ParsedRemoteDir::Empty;
    }
    if let Some(unit) = root_wchar_unit {
        // When the root `platform` attribute declares the source OS, decode
        // strictly with that OS's `wchar_t` unit rather than guessing another
        // unit on failure.
        if let Some(path) = decode_safe_path_with_unit(encoded, trimmed, unit) {
            return ParsedRemoteDir::SafePath(path);
        }
    } else {
        let scalar = decode_safe_path_with_unit(encoded, trimmed, WcharUnit::Scalar);
        let utf16 = decode_safe_path_with_unit(encoded, trimmed, WcharUnit::Utf16);
        match (scalar, utf16) {
            (Some(a), Some(b)) if a == b => return ParsedRemoteDir::SafePath(a),
            (Some(_), Some(_)) => return ParsedRemoteDir::AmbiguousSafePath,
            (Some(path), None) | (None, Some(path)) => return ParsedRemoteDir::SafePath(path),
            (None, None) => {}
        }
    }
    if looks_like_filezilla_safe_path(trimmed) {
        ParsedRemoteDir::MalformedSafePath
    } else {
        ParsedRemoteDir::PlainPath(trimmed.to_string())
    }
}

/// `<RemoteDir>` as AeroFTP's start path (test helper without platform context).
#[cfg(test)]
fn parse_filezilla_remote_dir(encoded: &str) -> String {
    match parse_filezilla_remote_dir_for_platform(encoded, None) {
        ParsedRemoteDir::Empty => String::new(),
        ParsedRemoteDir::SafePath(p) | ParsedRemoteDir::PlainPath(p) => p,
        ParsedRemoteDir::MalformedSafePath | ParsedRemoteDir::AmbiguousSafePath => {
            encoded.trim().to_string()
        }
    }
}

/// `CServerPath::GetSafePath()` with no prefix: `<type> 0[ <len> <segment>]...`.
fn encode_filezilla_safe_path<S: AsRef<str>>(
    server_type: u32,
    segments: &[S],
    unit: WcharUnit,
) -> String {
    let mut out = format!("{server_type} 0");
    for segment in segments {
        let segment = segment.as_ref();
        out.push(' ');
        out.push_str(&unit.len(segment).to_string());
        out.push(' ');
        out.push_str(segment);
    }
    out
}

/// A non-S3 start path as FileZilla would store it had the user typed it in
/// the Site Manager: the server type `CServerPath::SetPath` detects for a
/// DEFAULT-type site (`X:\` or `X:/` is DOS, a leading `\` is DOS_VIRTUAL, a
/// leading `/` is UNIX) and the segments its `Segmentize` keeps (no empty
/// segment, `.` dropped, `..` removes the previous one). `None` when FileZilla
/// cannot hold the path as a default remote directory: a relative path, or a
/// VMS, MVS or VxWorks form, which this encoder does not model.
fn format_filezilla_remote_dir(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let (server_type, separators): (u32, &[char]) = if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        (FZ_SERVER_TYPE_DOS, &['\\', '/'])
    } else if path.starts_with('/') {
        // Check absolute Unix paths before VMS `:[...]` so a Unix directory
        // such as `/data/snapshot:[2026]` is exported instead of dropped.
        (FZ_SERVER_TYPE_UNIX, &['/'])
    } else if (path.contains(":[") && path.ends_with(']'))
        || path.starts_with(':')
        || path.starts_with('\u{DC}')
    {
        return None;
    } else if path.starts_with('\\') {
        (FZ_SERVER_TYPE_DOS_VIRTUAL, &['\\'])
    } else {
        return None;
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split(separators) {
        match segment {
            "" | "." => {}
            // Never collapse `..` locally: if the preceding component is a
            // server-side symlink or junction, popping it would export a
            // different directory than the saved path.
            ".." => return None,
            segment => segments.push(segment),
        }
    }
    Some(encode_filezilla_safe_path(
        server_type,
        &segments,
        WcharUnit::native(),
    ))
}

/// An S3 bucket and key prefix as the `<RemoteDir>` FileZilla Pro reads:
/// `/bucket/prefix...` (ServerType UNIX), the first segment being the bucket.
/// FileZilla stores no empty segment (`DoSetSafePath` rejects a zero length),
/// so each empty segment of the prefix travels as a `/` at the start of the
/// next segment, or at the end of the last one when the prefix ends with it.
/// The bucket segment is never changed, and `parse_filezilla_remote_dir`
/// joins the segments back into the same prefix.
fn format_filezilla_s3_remote_dir(bucket: &str, prefix: &str) -> String {
    let key_prefix = prefix.strip_prefix('/').unwrap_or(prefix);
    let mut segments = vec![bucket.to_string()];
    if !key_prefix.is_empty() {
        let mut pending = String::new();
        let mut any_segment = false;
        for token in key_prefix.split('/') {
            if token.is_empty() {
                pending.push('/');
            } else {
                segments.push(format!("{pending}{token}"));
                pending.clear();
                any_segment = true;
            }
        }
        if !pending.is_empty() {
            if any_segment {
                if let Some(last) = segments.last_mut() {
                    last.push_str(&pending);
                }
            } else {
                // A prefix made of slashes only.
                segments.push(key_prefix.to_string());
            }
        }
    }
    encode_filezilla_safe_path(FZ_SERVER_TYPE_UNIX, &segments, WcharUnit::native())
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

        match map_server_with_context(fz_server, legacy_aeroftp_s3, parsed.root_wchar_unit) {
            Ok(mapped) => {
                let warn_name =
                    crate::bridge_shared::sanitize_warning_name(&fz_server.name, "FileZilla site");
                if let Some(reason) = mapped.remote_dir_warning {
                    warnings.push(FileZillaImportWarning {
                        name: warn_name.clone(),
                        reason,
                    });
                }
                if legacy_aeroftp_s3 {
                    warnings.push(FileZillaImportWarning {
                        name: warn_name.clone(),
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
                            name: warn_name,
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
                    crate::bridge_shared::sanitize_warning_name(&fz_server.name, "FileZilla site",)
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect::<String>(),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                servers.push(ServerProfileExport {
                    id,
                    name: crate::bridge_shared::sanitize_warning_name(
                        &fz_server.name,
                        "FileZilla site",
                    ),
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
            Err(reason) => {
                skipped.push(FileZillaSkippedServer {
                    name: crate::bridge_shared::sanitize_warning_name(
                        &fz_server.name,
                        "FileZilla site",
                    ),
                    protocol: fz_protocol,
                    reason,
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
    // Export with version 3.72.1 and the platform matching `WcharUnit::native()`
    // (`FileZilla_3.72.1_src/src/commonui/xml_file.cpp:177-185`) so Protocol=6
    // (plain FTP), Protocol=7 (S3), and non-BMP `RemoteDir` lengths stay
    // unambiguous from older AeroFTP v3.7.3..v4.2.7 `3.67.1` exports.
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "*nix"
    };
    xml.push_str(&format!(
        "<FileZilla3 version=\"3.72.1\" platform=\"{platform}\">\n"
    ));
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

        // When an S3 profile sets an explicit `options.endpoint` override,
        // export its effective endpoint host and port instead of stale
        // `server.host` / `server.port` fields.
        let (export_host, export_port) = if protocol == "s3"
            && server
                .options
                .as_ref()
                .and_then(|o| o.get("endpoint"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty())
        {
            match crate::bridge_shared::resolve_export_endpoint(
                "s3",
                &server.host,
                server.port,
                &server.username,
                server.options.as_ref(),
                None,
            ) {
                Ok(Some(ep)) => {
                    // FileZilla Pro S3 (`Protocol=7`) encodes only `<Host>`, `<Port>`,
                    // and `<RemoteDir>` (bucket + key prefix); it cannot represent an
                    // S3 endpoint base path such as `/gateway`. Skip the profile so it
                    // is not silently exported as a root endpoint.
                    if !ep.path.is_empty() {
                        continue;
                    }
                    let port = ep.port.map(u32::from).unwrap_or(if ep.scheme == "http" {
                        80
                    } else {
                        443
                    });
                    (ep.host, port)
                }
                Ok(None) => (server.host.clone(), server.port),
                Err(_) => continue,
            }
        } else {
            (server.host.clone(), server.port)
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
        xml.push_str(&format!("      <Host>{}</Host>\n", sanitize(&export_host)));
        xml.push_str(&format!("      <Port>{}</Port>\n", export_port));
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

        // Remote directory, in FileZilla's `CServerPath::GetSafePath()` format:
        // FileZilla loads `<RemoteDir>` only through `SetSafePath`. An S3 site
        // keeps `/<bucket>` as its first segment. An S3 profile without a
        // bucket writes none: its start path is a prefix inside a bucket the
        // profile does not name, and written alone FileZilla would open the
        // prefix's first segment as the bucket. That site starts at the bucket
        // list instead.
        let exported_remote_dir: Option<String> = if protocol == "s3" {
            server
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(|bucket| {
                    format_filezilla_s3_remote_dir(
                        bucket,
                        server.initial_path.as_deref().unwrap_or(""),
                    )
                })
        } else {
            server
                .initial_path
                .as_deref()
                .filter(|p| !p.is_empty())
                .and_then(format_filezilla_remote_dir)
        };
        if let Some(ref safe_path) = exported_remote_dir {
            xml.push_str(&format!(
                "      <RemoteDir>{}</RemoteDir>\n",
                sanitize(safe_path)
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

    /// A FIFO blocks `open(2)` until a writer appears: the sidecar used to be
    /// type-checked before it was opened, and must still be refused at once.
    #[cfg(unix)]
    #[test]
    fn a_fifo_sidecar_or_site_file_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            r#"<?xml version="1.0"?><FileZilla3><Servers/></FileZilla3>"#,
        )
        .unwrap();
        let sidecar = dir.path().join("filezilla.xml");
        let fifo_site = dir.path().join("fifo-sitemanager.xml");
        for fifo in [&sidecar, &fifo_site] {
            assert!(std::process::Command::new("mkfifo")
                .arg(fifo)
                .status()
                .unwrap()
                .success());
        }

        let returns_promptly = |fifo: &std::path::Path, run: Box<dyn FnOnce() + Send>| {
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                run();
                let _ = done.send(());
            });
            let outcome = finished.recv_timeout(std::time::Duration::from_secs(5));
            if outcome.is_err() {
                // Release the reader still blocked in open(2) so the thread ends.
                use std::os::unix::fs::OpenOptionsExt;
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo);
            }
            outcome.is_ok()
        };

        let probe = sitemanager.clone();
        assert!(
            returns_promptly(
                &sidecar,
                Box::new(move || assert!(filezilla_global_settings(&probe).is_none()))
            ),
            "a FIFO filezilla.xml blocked the import"
        );
        let site = fifo_site.clone();
        assert!(
            returns_promptly(
                &fifo_site,
                Box::new(move || assert!(import_filezilla(&site).is_err()))
            ),
            "a FIFO sitemanager.xml blocked the import"
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
    fn remote_dir_decoder_reads_every_server_type_and_windows_wchar_lengths() {
        // Windows FileZilla counts wchar_t in UTF-16 units: U+1F4C1 is two.
        assert_eq!(
            parse_filezilla_remote_dir("1 0 6 \u{1F4C1}docs 3 www"),
            "/\u{1F4C1}docs/www"
        );
        // macOS and Linux FileZilla count Unicode scalar values.
        assert_eq!(
            parse_filezilla_remote_dir("1 0 5 \u{1F4C1}docs 3 www"),
            "/\u{1F4C1}docs/www"
        );
        // `CServerPath::GetPath()` for the other server types.
        assert_eq!(parse_filezilla_remote_dir("3 0 2 C: 4 data"), "C:\\data");
        assert_eq!(parse_filezilla_remote_dir("3 0 2 C:"), "C:\\");
        assert_eq!(parse_filezilla_remote_dir("10 0 2 D: 3 www"), "D:/www");
        assert_eq!(parse_filezilla_remote_dir("8 0 4 data"), "\\data");
        assert_eq!(parse_filezilla_remote_dir("9 0 3 var 3 www"), "/var/www");
        assert_eq!(
            parse_filezilla_remote_dir("2 9 DISK$USR: 4 DIR1 4 DIR2"),
            "DISK$USR:[DIR1.DIR2]"
        );
        // A length that does not end on a separator is not a GetSafePath string.
        assert_eq!(parse_filezilla_remote_dir("1 0 2 abc"), "1 0 2 abc");
        // A segment ending in a space must decode untrimmed (FileZilla `site_manager.cpp:202`).
        assert_eq!(parse_filezilla_remote_dir("1 0 4 foo "), "/foo ");
        // Older AeroFTP exports wrote plain paths.
        assert_eq!(parse_filezilla_remote_dir("/var/www"), "/var/www");
    }

    #[test]
    fn remote_dir_trailing_space_segment_survives_xml_import() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server>"#,
                r#"<Host>ftp.example.com</Host><Port>21</Port><Protocol>0</Protocol><Type>0</Type>"#,
                r#"<User>u</User><Logontype>0</Logontype><Name>TrailingSpace</Name>"#,
                r#"<RemoteDir>1 0 4 foo </RemoteDir>"#,
                r#"</Server></Servers></FileZilla3>"#,
            ),
        )
        .unwrap();

        let imported = import_filezilla(&sitemanager).unwrap();
        assert_eq!(imported.servers.len(), 1);
        assert_eq!(imported.servers[0].initial_path.as_deref(), Some("/foo "));
    }

    #[test]
    fn export_remote_dir_matches_filezilla_segmentation_and_keeps_the_s3_bucket_segment() {
        let row = |name: &str, protocol: &str, options: Option<serde_json::Value>, path: &str| {
            FileZillaExportServer {
                id: name.to_string(),
                name: name.to_string(),
                host: "files.example.com".to_string(),
                port: 0,
                username: "u".to_string(),
                protocol: Some(protocol.to_string()),
                options,
                initial_path: Some(path.to_string()),
            }
        };
        let media = || Some(serde_json::json!({ "bucket": "media" }));
        let rows = [
            row("TrailingSlash", "ftp", None, "/var/www/"),
            row("Relative", "sftp", None, "public_html"),
            row("Windows", "ftp", None, "C:\\inetpub\\wwwroot"),
            row("SlashPrefix", "s3", media(), "//2026"),
            row("TrailingPrefix", "s3", media(), "/reports//2026/"),
            row("NoBucket", "s3", None, "/media/2026"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        assert_eq!(
            export_filezilla(&rows, &HashMap::new(), &path),
            Ok(rows.len())
        );

        let raw = parse_sitemanager_xml(&std::fs::read_to_string(&path).unwrap());
        let raw_dir = |name: &str| {
            raw.iter()
                .find(|s| s.name == name)
                .and_then(|s| s.fields.get("RemoteDir").cloned())
        };
        // FileZilla's own `Segmentize` keeps no empty segment for FTP/SFTP.
        assert_eq!(raw_dir("TrailingSlash").as_deref(), Some("1 0 3 var 3 www"));
        // FileZilla cannot hold a relative default remote directory.
        assert_eq!(raw_dir("Relative"), None);
        assert_eq!(
            raw_dir("Windows").as_deref(),
            Some("3 0 2 C: 7 inetpub 7 wwwroot")
        );
        // The bucket segment is never altered.
        assert_eq!(
            raw_dir("SlashPrefix").as_deref(),
            Some("1 0 5 media 5 /2026")
        );
        assert_eq!(
            raw_dir("TrailingPrefix").as_deref(),
            Some("1 0 5 media 7 reports 6 /2026/")
        );
        // No bucket: no prefix written as if its first segment were one.
        assert_eq!(raw_dir("NoBucket"), None);

        let back = import_filezilla(&path).unwrap();
        let by_name = |name: &str| back.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            by_name("TrailingSlash").initial_path.as_deref(),
            Some("/var/www")
        );
        assert_eq!(
            by_name("Windows").initial_path.as_deref(),
            Some("C:\\inetpub\\wwwroot")
        );
        for (name, prefix) in [
            ("SlashPrefix", "//2026"),
            ("TrailingPrefix", "/reports//2026/"),
        ] {
            let s3 = by_name(name);
            assert_eq!(
                s3.options.as_ref().and_then(|o| o.get("bucket")),
                Some(&serde_json::json!("media")),
                "{name}"
            );
            assert_eq!(s3.initial_path.as_deref(), Some(prefix), "{name}");
        }
    }

    #[test]
    fn export_counts_segment_lengths_in_the_local_filezilla_wchar_unit() {
        let expected = if cfg!(windows) {
            "1 0 6 \u{1F4C1}docs"
        } else {
            "1 0 5 \u{1F4C1}docs"
        };
        assert_eq!(
            format_filezilla_remote_dir("/\u{1F4C1}docs").as_deref(),
            Some(expected)
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

    #[test]
    fn site_bookmarks_do_not_replace_the_site_name_or_start_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        // The shape `CSiteManager::Save` writes: the site's own fields, then
        // each site-specific bookmark inside the same `<Server>`.
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server>"#,
                r#"<Host>ftp.example.com</Host><Port>21</Port><Protocol>0</Protocol><Type>0</Type>"#,
                r#"<User>u</User><Logontype>0</Logontype><Name>Production</Name><Comments />"#,
                r#"<LocalDir /><RemoteDir>1 0 3 var 3 www</RemoteDir>"#,
                r#"<SyncBrowsing>0</SyncBrowsing><DirectoryComparison>0</DirectoryComparison>"#,
                r#"<Bookmark><Name>Logs</Name><LocalDir /><RemoteDir>1 0 3 var 3 log</RemoteDir>"#,
                r#"<SyncBrowsing>0</SyncBrowsing><DirectoryComparison>0</DirectoryComparison></Bookmark>"#,
                r#"</Server></Servers></FileZilla3>"#,
            ),
        )
        .unwrap();

        let imported = import_filezilla(&sitemanager).unwrap();
        assert_eq!(imported.servers.len(), 1);
        assert_eq!(imported.servers[0].name, "Production");
        assert_eq!(
            imported.servers[0].initial_path.as_deref(),
            Some("/var/www")
        );
    }

    #[test]
    fn unnamed_site_warnings_never_leak_host_userinfo_or_query_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let user = "fzuser";
        let pw = "fzTopSecretPw";
        let tok = "fzSecretTok";

        // Active FTP site without <Name> and with userinfo + query in <Host>.
        let active_path = dir.path().join("active.xml");
        std::fs::write(
            &active_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server><Host>ftp://{user}:{pw}@ftp.example.com?token={tok}</Host><Port>21</Port><Protocol>0</Protocol><Type>0</Type><Parameter Name="data_mode_override">active</Parameter></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();
        let active = import_filezilla(&active_path).unwrap();
        assert_eq!(active.warnings.len(), 1);
        assert_eq!(active.warnings[0].name, "ftp.example.com");
        assert_eq!(active.servers[0].name, "ftp.example.com");
        assert_eq!(active.servers[0].host, "ftp.example.com");
        let active_json =
            serde_json::to_string(&(&active.servers, &active.skipped, &active.warnings)).unwrap();
        for secret in [user, pw, tok] {
            assert!(
                !active_json.contains(secret),
                "{secret} leaked: {active_json}"
            );
        }

        // Legacy AeroFTP Protocol=6 S3 site without <Name> and with ambiguous userinfo in <Host>.
        let legacy_dir = dir.path().join("legacy");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("sitemanager.xml");
        let amb_pw = "pa/ssSecret";
        std::fs::write(
            &legacy_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.67.1" platform="*"><Servers><Server><Host>https://{user}:{amb_pw}@s3.example.com</Host><Port>443</Port><Protocol>6</Protocol><User>DEMOKEY</User><RemoteDir>/archive</RemoteDir></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        std::fs::write(
            legacy_dir.join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();
        let legacy = import_filezilla(&legacy_path).unwrap();
        assert!(legacy.servers.is_empty());
        assert_eq!(legacy.skipped.len(), 1);
        assert_eq!(legacy.skipped[0].name, "unnamed");
        let legacy_json =
            serde_json::to_string(&(&legacy.servers, &legacy.skipped, &legacy.warnings)).unwrap();
        for secret in [user, "pa/ss", "ssSecret"] {
            assert!(
                !legacy_json.contains(secret),
                "{secret} leaked: {legacy_json}"
            );
        }

        // Explicit URL-shaped <Name> with credentials is also redacted in warnings.
        let named_path = dir.path().join("named.xml");
        std::fs::write(
            &named_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server><Host>ftp.example.com</Host><Port>21</Port><Protocol>0</Protocol><Type>0</Type><Name>ftp://{user}:{pw}@ftp.example.com?token={tok}</Name><Parameter Name="data_mode_override">active</Parameter></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        let named = import_filezilla(&named_path).unwrap();
        assert_eq!(named.warnings.len(), 1);
        assert_eq!(named.warnings[0].name, "ftp.example.com");
        assert_eq!(named.servers[0].name, "ftp.example.com");
        let named_json =
            serde_json::to_string(&(&named.servers, &named.skipped, &named.warnings)).unwrap();
        for secret in [user, pw, tok] {
            assert!(
                !named_json.contains(secret),
                "{secret} leaked: {named_json}"
            );
        }
    }

    #[test]
    fn wrapped_filezilla3_document_is_not_accepted_as_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();

        // A `<FileZilla3>` nested inside `<Other>` must not trigger legacy S3
        // recovery or import any sites.
        let wrapped = dir.path().join("wrapped.xml");
        std::fs::write(
            &wrapped,
            r#"<?xml version="1.0"?><Other><FileZilla3 version="3.67.1" platform="*"><Servers><Server><Host>s3.example.com</Host><Port>443</Port><Protocol>6</Protocol><User>DEMO</User><Name>Wrapped</Name></Server></Servers></FileZilla3></Other>"#,
        )
        .unwrap();
        let res = import_filezilla(&wrapped).unwrap();
        assert!(
            res.servers.is_empty(),
            "wrapped document must import 0 sites"
        );
        assert!(
            res.warnings
                .iter()
                .any(|w| w.reason.contains("ends before its closing tag")),
            "wrapped document must not be marked complete: {:?}",
            res.warnings.iter().map(|w| &w.reason).collect::<Vec<_>>()
        );
    }

    #[test]
    fn source_platform_disambiguates_wchar_remote_dir_and_unknown_warns() {
        // `1 0 4 😀😀 3 1 a`:
        // - Windows UTF-16: "😀😀" has length 4, next segment length 3 is "1 a" -> `/😀😀/1 a`
        // - *nix/mac scalar: "😀😀 3" has length 4, next segment length 1 is "a" -> `/😀😀 3/a`
        let ambiguous_safe_path = "1 0 4 \u{1F600}\u{1F600} 3 1 a";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();

        let win_path = dir.path().join("win.xml");
        std::fs::write(
            &win_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="windows"><Servers><Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>Win</Name><RemoteDir>{ambiguous_safe_path}</RemoteDir></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        let win = import_filezilla(&win_path).unwrap();
        assert_eq!(
            win.servers[0].initial_path.as_deref(),
            Some("/\u{1F600}\u{1F600}/1 a")
        );
        assert!(win.warnings.is_empty());

        let nix_path = dir.path().join("nix.xml");
        std::fs::write(
            &nix_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>Nix</Name><RemoteDir>{ambiguous_safe_path}</RemoteDir></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        let nix = import_filezilla(&nix_path).unwrap();
        assert_eq!(
            nix.servers[0].initial_path.as_deref(),
            Some("/\u{1F600}\u{1F600} 3/a")
        );
        assert!(nix.warnings.is_empty());

        let unk_path = dir.path().join("unk.xml");
        std::fs::write(
            &unk_path,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1"><Servers><Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>Unk</Name><RemoteDir>{ambiguous_safe_path}</RemoteDir></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        let unk = import_filezilla(&unk_path).unwrap();
        assert_eq!(unk.servers[0].initial_path, None);
        assert_eq!(unk.warnings.len(), 1);
        assert!(
            unk.warnings[0].reason.contains("ambiguous"),
            "{}",
            unk.warnings[0].reason
        );

        // Export writes a platform matching WcharUnit::native().
        let exp_path = dir.path().join("exported.xml");
        let row = FileZillaExportServer {
            id: "e1".to_string(),
            name: "E1".to_string(),
            host: "sftp.example.com".to_string(),
            port: 22,
            username: "u".to_string(),
            protocol: Some("sftp".to_string()),
            options: None,
            initial_path: Some("/\u{1F600}\u{1F600}/1 a".to_string()),
        };
        assert_eq!(export_filezilla(&[row], &HashMap::new(), &exp_path), Ok(1));
        let exp_xml = std::fs::read_to_string(&exp_path).unwrap();
        let expected_platform = if cfg!(windows) {
            r#"platform="windows""#
        } else if cfg!(target_os = "macos") {
            r#"platform="mac""#
        } else {
            r#"platform="*nix""#
        };
        assert!(exp_xml.contains(expected_platform), "{exp_xml}");
    }

    #[test]
    fn native_s3_protocol_7_rejects_malformed_safe_path_and_connects_custom_endpoint_port() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();
        let sitemanager = dir.path().join("sitemanager.xml");
        std::fs::write(
            &sitemanager,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="*nix"><Servers>"#,
                r#"<Server><Host>s3.amazonaws.com</Host><Port>443</Port><Protocol>7</Protocol><User>DEMO1</User><Name>BadSafePath</Name><RemoteDir>1 0 5 med</RemoteDir></Server>"#,
                r#"<Server><Host>objects.example.test</Host><Port>9000</Port><Protocol>7</Protocol><User>DEMO2</User><Name>CustomS3</Name><RemoteDir>1 0 5 media</RemoteDir></Server>"#,
                r#"</Servers></FileZilla3>"#,
            ),
        )
        .unwrap();

        let res = import_filezilla(&sitemanager).unwrap();
        assert_eq!(res.servers.len(), 2);

        let bad = res
            .servers
            .iter()
            .find(|s| s.name == "BadSafePath")
            .unwrap();
        assert!(
            bad.options.as_ref().and_then(|o| o.get("bucket")).is_none(),
            "malformed safe path must never become options.bucket: {:?}",
            bad.options
        );
        assert_eq!(bad.initial_path, None);
        assert!(
            res.warnings
                .iter()
                .any(|w| w.name == "BadSafePath" && w.reason.contains("GetSafePath")),
            "{:?}",
            res.warnings
                .iter()
                .map(|w| (&w.name, &w.reason))
                .collect::<Vec<_>>()
        );

        let custom = res.servers.iter().find(|s| s.name == "CustomS3").unwrap();
        assert_eq!(
            custom.options.as_ref().and_then(|o| o.get("bucket")),
            Some(&serde_json::json!("media"))
        );
        let mut extra = HashMap::new();
        if let Some(obj) = custom.options.as_ref().and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(sv) = v.as_str() {
                    extra.insert(k.clone(), sv.to_string());
                }
            }
        }
        let provider_cfg = crate::providers::types::ProviderConfig {
            name: custom.name.clone(),
            provider_type: crate::providers::types::ProviderType::S3,
            host: custom.host.clone(),
            port: Some(custom.port as u16),
            username: Some(custom.username.clone()),
            password: custom.credential.clone(),
            initial_path: custom.initial_path.clone(),
            extra,
        };
        let s3_cfg = crate::providers::types::S3Config::from_provider_config(&provider_cfg)
            .expect("custom S3 Protocol 7 must build valid S3Config");
        assert_eq!(s3_cfg.bucket, "media");
        assert_eq!(
            s3_cfg.endpoint.as_deref(),
            Some("https://objects.example.test:9000")
        );
        assert!(s3_cfg.path_style);
    }

    #[test]
    fn unix_path_with_colon_bracket_round_trips_while_vms_stays_omitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();
        let rows = [
            FileZillaExportServer {
                id: "u1".to_string(),
                name: "UnixBracket".to_string(),
                host: "sftp.example.com".to_string(),
                port: 22,
                username: "u".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                initial_path: Some("/data/snapshot:[2026]".to_string()),
            },
            FileZillaExportServer {
                id: "v1".to_string(),
                name: "VmsPath".to_string(),
                host: "ftp.example.com".to_string(),
                port: 21,
                username: "u".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                initial_path: Some("DISK$USR:[DIR1.DIR2]".to_string()),
            },
        ];
        let out = dir.path().join("sitemanager.xml");
        assert_eq!(export_filezilla(&rows, &HashMap::new(), &out), Ok(2));

        let imported = import_filezilla(&out).unwrap();
        let unix = imported
            .servers
            .iter()
            .find(|s| s.name == "UnixBracket")
            .unwrap();
        assert_eq!(unix.initial_path.as_deref(), Some("/data/snapshot:[2026]"));
        let vms = imported
            .servers
            .iter()
            .find(|s| s.name == "VmsPath")
            .unwrap();
        assert_eq!(vms.initial_path, None);
    }

    #[test]
    fn second_top_level_root_and_wrong_platform_length_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3>"#,
        )
        .unwrap();

        // #06: A second top-level `<FileZilla3>` with `*nix` platform must not
        // reinterpret a site already read from the first `windows` root.
        let ambiguous = "1 0 4 \u{1F600}\u{1F600} 3 1 a";
        let two_roots = dir.path().join("two-roots.xml");
        std::fs::write(
            &two_roots,
            format!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="windows"><Servers><Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>WinFirst</Name><RemoteDir>{ambiguous}</RemoteDir></Server></Servers></FileZilla3><FileZilla3 version="3.72.1" platform="*nix"><Servers><Server><Host>other.example.com</Host><Protocol>1</Protocol><Name>Second</Name></Server></Servers></FileZilla3>"#
            ),
        )
        .unwrap();
        let res = import_filezilla(&two_roots).unwrap();
        assert_eq!(res.servers.len(), 1);
        assert_eq!(res.servers[0].name, "WinFirst");
        assert_eq!(
            res.servers[0].initial_path.as_deref(),
            Some("/\u{1F600}\u{1F600}/1 a")
        );
        assert!(res
            .warnings
            .iter()
            .any(|w| w.reason.contains("ends before its closing tag")));

        // #06 sidecar: a second top-level root in filezilla.xml is rejected.
        let sidecar_dir = dir.path().join("two-root-sidecar");
        std::fs::create_dir_all(&sidecar_dir).unwrap();
        let sm = sidecar_dir.join("sitemanager.xml");
        std::fs::write(
            &sm,
            r#"<?xml version="1.0"?><FileZilla3><Servers/></FileZilla3>"#,
        )
        .unwrap();
        std::fs::write(
            sidecar_dir.join("filezilla.xml"),
            r#"<?xml version="1.0"?><FileZilla3><Settings/></FileZilla3><FileZilla3><Settings><Setting name="Proxy type">4</Setting></Settings></FileZilla3>"#,
        )
        .unwrap();
        assert!(filezilla_global_settings(&sm).is_none());

        // #11: On `platform="windows"`, scalar length 5 for `😀docs` (UTF-16 length 6)
        // must warn as malformed rather than retrying scalar; length 6 succeeds.
        let win_strict = dir.path().join("win-strict.xml");
        std::fs::write(
            &win_strict,
            concat!(
                r#"<?xml version="1.0"?><FileZilla3 version="3.72.1" platform="windows"><Servers>"#,
                "<Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>BadCount</Name><RemoteDir>1 0 5 \u{1F600}docs</RemoteDir></Server>",
                "<Server><Host>sftp.example.com</Host><Protocol>1</Protocol><Name>GoodCount</Name><RemoteDir>1 0 6 \u{1F600}docs</RemoteDir></Server>",
                r#"</Servers></FileZilla3>"#,
            ),
        )
        .unwrap();
        let strict = import_filezilla(&win_strict).unwrap();
        let bad = strict
            .servers
            .iter()
            .find(|s| s.name == "BadCount")
            .unwrap();
        assert_eq!(bad.initial_path, None);
        assert!(strict
            .warnings
            .iter()
            .any(|w| w.name == "BadCount" && w.reason.contains("GetSafePath")));
        let good = strict
            .servers
            .iter()
            .find(|s| s.name == "GoodCount")
            .unwrap();
        assert_eq!(good.initial_path.as_deref(), Some("/\u{1F600}docs"));
        assert!(!strict.warnings.iter().any(|w| w.name == "GoodCount"));
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

    #[test]
    fn url_shaped_hosts_are_sanitized_without_leaking_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        let s3_secret = "FzS3HostSecret99";
        let query_token = "FzQueryToken88";
        let bad_secret = "FzAmbiguousSecret77";
        let s3_url = format!(
            "https://{}:{}@objects.example.test:9000/bucket?token={}",
            "access", s3_secret, query_token
        );
        let bad_url = format!("https://user:{}/slash@objects.example.test", bad_secret);
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3 version="3.72.1" platform="*">
  <Servers>
    <Server>
      <Host>{s3_url}</Host>
      <Port>0</Port>
      <Protocol>7</Protocol>
      <User>DEMOKEY</User>
      <Name>CredS3</Name>
      <RemoteDir>1 0 5 media</RemoteDir>
    </Server>
    <Server>
      <Host>{bad_url}</Host>
      <Port>443</Port>
      <Protocol>7</Protocol>
      <User>DEMOKEY2</User>
      <Name>{bad_url}</Name>
    </Server>
  </Servers>
</FileZilla3>"#
        );
        std::fs::write(&path, xml).unwrap();
        let res = import_filezilla(&path).unwrap();
        assert_eq!(res.servers.len(), 1);
        assert_eq!(res.servers[0].host, "objects.example.test");
        assert_eq!(res.servers[0].port, 9000);
        assert_eq!(
            res.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("endpoint"))
                .and_then(|v| v.as_str()),
            Some("https://objects.example.test:9000")
        );
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(res.skipped[0].name, "FileZilla site");
        assert!(res.skipped[0].reason.contains("Host"));
        let serialized =
            serde_json::to_string(&(&res.servers, &res.skipped, &res.warnings)).unwrap();
        for secret in [s3_secret, query_token, bad_secret] {
            assert!(
                !serialized.contains(secret),
                "secret {secret} leaked in serialized import result: {serialized}"
            );
        }
    }

    #[test]
    fn native_s3_protocol_7_without_remote_dir_warns_missing_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        std::fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3 version="3.72.1" platform="*">
  <Servers>
    <Server>
      <Host>s3.amazonaws.com</Host>
      <Port>443</Port>
      <Protocol>7</Protocol>
      <User>DEMO1</User>
      <Name>NoBucketS3</Name>
    </Server>
    <Server>
      <Host>s3.amazonaws.com</Host>
      <Port>443</Port>
      <Protocol>7</Protocol>
      <User>DEMO2</User>
      <Name>WithBucketS3</Name>
      <RemoteDir>1 0 6 photos</RemoteDir>
    </Server>
  </Servers>
</FileZilla3>"#,
        )
        .unwrap();
        let res = import_filezilla(&path).unwrap();
        assert_eq!(res.servers.len(), 2);
        assert!(res
            .warnings
            .iter()
            .any(|w| { w.name == "NoBucketS3" && w.reason.contains("bucket") }));
        assert!(!res.warnings.iter().any(|w| w.name == "WithBucketS3"));
    }

    #[test]
    fn export_omits_remote_dir_with_parent_dotdot_segments() {
        assert_eq!(format_filezilla_remote_dir("/srv/link/../incoming"), None);
        assert_eq!(format_filezilla_remote_dir(r"C:\work\..\data"), None);
        assert_eq!(
            format_filezilla_remote_dir("/srv/incoming"),
            Some("1 0 3 srv 8 incoming".to_string())
        );
    }

    #[test]
    fn export_filezilla_uses_s3_endpoint_override_host_and_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        let servers = vec![
            FileZillaExportServer {
                id: "s3-override".into(),
                name: "OverrideS3".into(),
                host: "stale.example.test".into(),
                port: 443,
                username: "DEMOKEY".into(),
                protocol: Some("s3".into()),
                initial_path: Some("/archive".into()),
                options: Some(serde_json::json!({
                    "bucket": "media",
                    "endpoint": "https://objects.example.test:9000"
                })),
            },
            FileZillaExportServer {
                id: "s3-plain".into(),
                name: "PlainS3".into(),
                host: "s3.amazonaws.com".into(),
                port: 443,
                username: "DEMOKEY2".into(),
                protocol: Some("s3".into()),
                initial_path: None,
                options: Some(serde_json::json!({
                    "bucket": "photos"
                })),
            },
        ];
        let count = export_filezilla(&servers, &HashMap::new(), &path).unwrap();
        assert_eq!(count, 2);
        let back = import_filezilla(&path).unwrap();
        let overridden = back
            .servers
            .iter()
            .find(|s| s.name == "OverrideS3")
            .unwrap();
        assert_eq!(overridden.host, "objects.example.test");
        assert_eq!(overridden.port, 9000);
        let plain = back.servers.iter().find(|s| s.name == "PlainS3").unwrap();
        assert_eq!(plain.host, "s3.amazonaws.com");
        assert_eq!(plain.port, 443);
    }

    #[test]
    fn windows_s3_remote_dir_preserves_non_ascii_and_empty_prefix_segments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<FileZilla3 version=\"3.72.1\" platform=\"windows\">\n\
  <Servers>\n\
    <Server>\n\
      <Host>s3.amazonaws.com</Host>\n\
      <Port>443</Port>\n\
      <Protocol>7</Protocol>\n\
      <User>DEMOKEY</User>\n\
      <Name>WinS3</Name>\n\
      <RemoteDir>1 0 6 photos 3 \u{00E9}t\u{00E9} 6 /2026/</RemoteDir>\n\
    </Server>\n\
  </Servers>\n\
</FileZilla3>",
        )
        .unwrap();
        let imported = import_filezilla(&path).unwrap();
        assert_eq!(imported.servers.len(), 1);
        let srv = &imported.servers[0];
        assert_eq!(
            srv.options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|v| v.as_str()),
            Some("photos")
        );
        assert_eq!(
            srv.initial_path.as_deref(),
            Some("/\u{00E9}t\u{00E9}//2026/")
        );

        let out = dir.path().join("roundtrip.xml");
        let export_item = FileZillaExportServer {
            id: srv.id.clone(),
            name: srv.name.clone(),
            host: srv.host.clone(),
            port: srv.port,
            username: srv.username.clone(),
            protocol: srv.protocol.clone(),
            initial_path: srv.initial_path.clone(),
            options: srv.options.clone(),
        };
        assert_eq!(
            export_filezilla(&[export_item], &HashMap::new(), &out).unwrap(),
            1
        );
        let roundtrip = import_filezilla(&out).unwrap();
        assert_eq!(
            roundtrip.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("bucket"))
                .and_then(|v| v.as_str()),
            Some("photos")
        );
        assert_eq!(
            roundtrip.servers[0].initial_path.as_deref(),
            Some("/\u{00E9}t\u{00E9}//2026/")
        );
    }

    #[test]
    fn native_s3_import_preserves_non_default_port_for_aws_and_keeps_443_implicit() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3 version="3.72.1" platform="*nix">
  <Servers>
    <Server>
      <Host>s3.amazonaws.com</Host>
      <Port>9443</Port>
      <Protocol>7</Protocol>
      <User>AKIACUSTOMPORT</User>
      <Pass encoding="base64">c2VjcmV0</Pass>
      <Logontype>1</Logontype>
      <RemoteDir>1 0 6 photos</RemoteDir>
      <Name>AWS Custom Port</Name>
    </Server>
    <Server>
      <Host>s3.amazonaws.com</Host>
      <Port>443</Port>
      <Protocol>7</Protocol>
      <User>AKIADEFAULTPORT</User>
      <Pass encoding="base64">c2VjcmV0</Pass>
      <Logontype>1</Logontype>
      <RemoteDir>1 0 6 photos</RemoteDir>
      <Name>AWS Default Port</Name>
    </Server>
  </Servers>
</FileZilla3>"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        std::fs::write(&path, xml).unwrap();
        let res = import_filezilla(&path).unwrap();
        assert_eq!(res.servers.len(), 2);

        let custom = res
            .servers
            .iter()
            .find(|s| s.name == "AWS Custom Port")
            .unwrap();
        assert_eq!(custom.host, "s3.amazonaws.com");
        assert_eq!(custom.port, 9443);
        let custom_ep = custom
            .options
            .as_ref()
            .and_then(|o| o.get("endpoint"))
            .and_then(|v| v.as_str());
        assert_eq!(custom_ep, Some("https://s3.amazonaws.com:9443"));
        let mut custom_extra = HashMap::new();
        custom_extra.insert("bucket".to_string(), "photos".to_string());
        custom_extra.insert("endpoint".to_string(), custom_ep.unwrap().to_string());
        let custom_cfg = crate::providers::types::S3Config::from_provider_config(
            &crate::providers::types::ProviderConfig {
                name: custom.name.clone(),
                provider_type: crate::providers::types::ProviderType::S3,
                host: custom.host.clone(),
                port: Some(custom.port as u16),
                username: Some(custom.username.clone()),
                password: custom.credential.clone(),
                initial_path: None,
                extra: custom_extra,
            },
        )
        .expect("custom AWS port builds S3Config");
        assert_eq!(
            custom_cfg.endpoint.as_deref(),
            Some("https://s3.amazonaws.com:9443")
        );

        let default_aws = res
            .servers
            .iter()
            .find(|s| s.name == "AWS Default Port")
            .unwrap();
        assert_eq!(default_aws.host, "s3.amazonaws.com");
        assert_eq!(default_aws.port, 443);
        assert_eq!(
            default_aws.options.as_ref().and_then(|o| o.get("endpoint")),
            None,
            "default AWS port 443 must stay implicit"
        );
        let mut default_extra = HashMap::new();
        default_extra.insert("bucket".to_string(), "photos".to_string());
        let default_cfg = crate::providers::types::S3Config::from_provider_config(
            &crate::providers::types::ProviderConfig {
                name: default_aws.name.clone(),
                provider_type: crate::providers::types::ProviderType::S3,
                host: default_aws.host.clone(),
                port: Some(default_aws.port as u16),
                username: Some(default_aws.username.clone()),
                password: default_aws.credential.clone(),
                initial_path: None,
                extra: default_extra,
            },
        )
        .expect("default AWS port builds S3Config");
        assert_eq!(default_cfg.endpoint, None);
    }

    #[test]
    fn native_s3_bare_host_with_inline_port_prefers_nonzero_port_field_and_keeps_inline_when_zero()
    {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3 version="3.72.1" platform="*nix">
  <Servers>
    <Server>
      <Host>objects.example.test:9000</Host>
      <Port>9443</Port>
      <Protocol>7</Protocol>
      <User>AKIABAREOVERRIDE</User>
      <Pass encoding="base64">c2VjcmV0</Pass>
      <Logontype>1</Logontype>
      <RemoteDir>1 0 6 photos</RemoteDir>
      <Name>Bare Inline Port Override</Name>
    </Server>
    <Server>
      <Host>objects.example.test:9000</Host>
      <Port>0</Port>
      <Protocol>7</Protocol>
      <User>AKIABAREZERO</User>
      <Pass encoding="base64">c2VjcmV0</Pass>
      <Logontype>1</Logontype>
      <RemoteDir>1 0 6 photos</RemoteDir>
      <Name>Bare Inline Port Fallback</Name>
    </Server>
  </Servers>
</FileZilla3>"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        std::fs::write(&path, xml).unwrap();
        let res = import_filezilla(&path).unwrap();
        assert_eq!(res.servers.len(), 2);

        let overridden = res
            .servers
            .iter()
            .find(|s| s.name == "Bare Inline Port Override")
            .unwrap();
        assert_eq!(overridden.host, "objects.example.test");
        assert_eq!(overridden.port, 9443);
        let overridden_ep = overridden
            .options
            .as_ref()
            .and_then(|o| o.get("endpoint"))
            .and_then(|v| v.as_str());
        assert_eq!(overridden_ep, Some("https://objects.example.test:9443"));
        let mut overridden_extra = HashMap::new();
        overridden_extra.insert("bucket".to_string(), "photos".to_string());
        overridden_extra.insert("endpoint".to_string(), overridden_ep.unwrap().to_string());
        let overridden_cfg = crate::providers::types::S3Config::from_provider_config(
            &crate::providers::types::ProviderConfig {
                name: overridden.name.clone(),
                provider_type: crate::providers::types::ProviderType::S3,
                host: overridden.host.clone(),
                port: Some(overridden.port as u16),
                username: Some(overridden.username.clone()),
                password: overridden.credential.clone(),
                initial_path: None,
                extra: overridden_extra,
            },
        )
        .expect("overridden port builds S3Config");
        assert_eq!(
            overridden_cfg.endpoint.as_deref(),
            Some("https://objects.example.test:9443")
        );

        let fallback = res
            .servers
            .iter()
            .find(|s| s.name == "Bare Inline Port Fallback")
            .unwrap();
        assert_eq!(fallback.host, "objects.example.test");
        assert_eq!(fallback.port, 9000);
        let fallback_ep = fallback
            .options
            .as_ref()
            .and_then(|o| o.get("endpoint"))
            .and_then(|v| v.as_str());
        assert_eq!(fallback_ep, Some("https://objects.example.test:9000"));
    }

    #[test]
    fn export_filezilla_skips_path_based_s3_endpoint_and_keeps_root_https_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemanager.xml");
        let servers = vec![
            FileZillaExportServer {
                id: "s3-path-endpoint".into(),
                name: "GatewayS3".into(),
                host: "stale.example.test".into(),
                port: 443,
                username: "AKIAGATEWAY".into(),
                protocol: Some("s3".into()),
                initial_path: Some("/archive".into()),
                options: Some(serde_json::json!({
                    "bucket": "media",
                    "endpoint": "https://objects.example.test:9443/gateway"
                })),
            },
            FileZillaExportServer {
                id: "s3-root-endpoint".into(),
                name: "RootHttpsS3".into(),
                host: "stale2.example.test".into(),
                port: 443,
                username: "AKIAROOT".into(),
                protocol: Some("s3".into()),
                initial_path: Some("/archive".into()),
                options: Some(serde_json::json!({
                    "bucket": "media",
                    "endpoint": "https://objects.example.test:9443"
                })),
            },
        ];
        let count = export_filezilla(&servers, &HashMap::new(), &path).unwrap();
        assert_eq!(
            count, 1,
            "path-based S3 endpoint must not be exported as a root endpoint"
        );
        let xml = std::fs::read_to_string(&path).unwrap();
        assert!(!xml.contains("GatewayS3"), "{xml}");
        assert!(xml.contains("RootHttpsS3"), "{xml}");

        let back = import_filezilla(&path).unwrap();
        assert_eq!(back.servers.len(), 1);
        assert_eq!(back.servers[0].name, "RootHttpsS3");
        assert_eq!(back.servers[0].host, "objects.example.test");
        assert_eq!(back.servers[0].port, 9443);

        // When only a path-based S3 profile is passed, export counts 0 and writes no file.
        let solo_path = dir.path().join("solo.xml");
        let solo_count = export_filezilla(&servers[..1], &HashMap::new(), &solo_path).unwrap();
        assert_eq!(solo_count, 0);
        assert!(!solo_path.exists());
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
