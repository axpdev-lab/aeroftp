// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared primitives for the profile-bridge importers/exporters.
//!
//! Refactor 6 (APPENDIX-BRIDGE): the canonical helpers (`uuid_v4`,
//! `map_s3_provider`, `map_webdav_vendor`) used to be private duplicates
//! inside `rclone_import.rs` / `winscp_import.rs` / `filezilla_import.rs`.
//! Every bridge module (existing and new: aws, ssh, mc, cyberduck, s3cmd,
//! lftp, putty, mobaxterm, dreamweaver, kopia, duplicacy, restic) now points
//! here, so the provider tables are defined exactly once.
//!
//! Dependency philosophy of the repo: no extra crate where a focused scanner
//! suffices. The INI/XML helpers below intentionally replace `ini`/`plist`/
//! `quick-xml` for the regular subsets these tools emit.

use std::collections::HashMap;
use std::path::Path;

// ============ Identifiers ============

/// Simple UUID v4 generator (avoid pulling the `uuid` crate just for this).
///
/// Canonical implementation: the three legacy importers carried byte-identical
/// copies of this. Kept here as the single source.
pub(crate) fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let random_bytes = crate::crypto::random_bytes(16);

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&random_bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx

    let time_bytes = (seed as u64).to_le_bytes();
    for (i, &b) in time_bytes.iter().enumerate() {
        bytes[i] ^= b;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // re-set version after XOR
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // re-set variant after XOR

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

// ============ Provider tables ============

/// Map an rclone-style S3 `provider` token to our providerId.
pub(crate) fn map_s3_provider(provider: &str) -> &'static str {
    match provider.to_lowercase().as_str() {
        "aws" | "amazon" => "amazon-s3",
        "cloudflare" | "r2" => "cloudflare-r2",
        "digitalocean" | "digitaloceanspaces" => "digitalocean-spaces",
        "wasabi" => "wasabi",
        "backblaze" | "b2" => "backblaze-b2",
        "linode" | "linodeobjectstorage" => "linode-object-storage",
        "scaleway" => "scaleway",
        "stackpath" => "stackpath",
        "storj" => "storj",
        "idrive" | "idrivee2" => "idrive-e2",
        "minio" => "minio",
        "ceph" => "custom-s3",
        "arvancloud" => "custom-s3",
        "huaweiobs" | "obs" => "custom-s3",
        "tencentcos" | "cos" => "custom-s3",
        "alicloud" | "oss" => "custom-s3",
        "ibmcos" => "custom-s3",
        "ionos" => "ionos-s3",
        "petabox" => "custom-s3",
        "seaweedfs" => "custom-s3",
        "netease" => "custom-s3",
        "qiniu" => "custom-s3",
        _ => "custom-s3",
    }
}

/// Infer the S3 providerId from an endpoint URL or bare host. Used by every
/// importer whose source records an endpoint rather than a provider token
/// (aws, mc, s3cmd, kopia, duplicacy, restic).
pub(crate) fn map_s3_provider_from_endpoint(endpoint: &str) -> &'static str {
    let h = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_lowercase();
    if h.is_empty() || h.contains("amazonaws.com") {
        return "amazon-s3";
    }
    if h.contains("r2.cloudflarestorage.com") {
        return "cloudflare-r2";
    }
    if h.contains("digitaloceanspaces.com") {
        return "digitalocean-spaces";
    }
    if h.contains("wasabisys.com") {
        return "wasabi";
    }
    if h.contains("backblazeb2.com") || h.contains(".b2-") {
        return "backblaze-b2";
    }
    if h.contains("linodeobjects.com") {
        return "linode-object-storage";
    }
    if h.contains("scw.cloud") {
        return "scaleway";
    }
    if h.contains("storjshare.io") || h.contains("gateway.storj") {
        return "storj";
    }
    if h.contains("idrivee2") {
        return "idrive-e2";
    }
    if h.contains("ionos") {
        return "ionos-s3";
    }
    if h.contains("min.io") || h.contains("minio") {
        return "minio";
    }
    "custom-s3"
}

/// Map an rclone-style WebDAV `vendor` token to our providerId.
pub(crate) fn map_webdav_vendor(vendor: &str) -> &'static str {
    match vendor.to_lowercase().as_str() {
        "nextcloud" => "nextcloud",
        "owncloud" => "owncloud",
        "sharepoint" | "sharepoint-ntlm" => "custom-webdav",
        "fastmail" => "custom-webdav",
        _ => "custom-webdav",
    }
}

// ============ URL / host helpers ============

/// Strip scheme and path from an endpoint, keeping `host[:port]`.
pub(crate) fn endpoint_host(endpoint: &str) -> String {
    endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .trim_end_matches('/')
        .to_string()
}

/// Default TCP port for a protocol token.
pub(crate) fn default_port_for(protocol: &str) -> u32 {
    match protocol {
        "ftp" => 21,
        "ftps" => 990,
        "sftp" => 22,
        // s3 / webdav / https endpoints all default to TLS 443
        _ => 443,
    }
}

// ============ serde_json helpers ============

/// Build a serde_json object from `(key, Option<String>)` pairs, skipping
/// `None` and empty values so optional metadata never produces noise keys.
pub(crate) fn json_map(pairs: &[(&str, Option<String>)]) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        if let Some(val) = v {
            if !val.is_empty() {
                m.insert((*k).to_string(), serde_json::Value::String(val.clone()));
            }
        }
    }
    m
}

// ============ Atomic secret-bearing write ============

/// Atomic write (temp + rename) with `0600` on unix. Mandatory for any
/// exported file that carries a secret (restic env script, duplicacy
/// preferences, kopia storage block, aws/mc/s3cmd native files).
/// Template: `profile_export.rs:109-117`.
pub(crate) fn atomic_write_600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        "{}.aerotmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bridge-export".to_string())
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write temp: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into place: {e}")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ============ INI parser (aws, s3cmd, ...) ============

/// Section-keyed INI parser. Mirrors the exact shape of
/// `rclone_import::parse_rclone_conf`: `[section]` headers, `key = value`
/// pairs, `#`/`;` comments, keys lowercased. An `[profile foo]` header
/// (AWS config style) collapses to `foo` so `~/.aws/config` augments
/// `~/.aws/credentials` cleanly.
pub(crate) fn parse_ini_sections(content: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let raw = line[1..line.len() - 1].trim();
            let name = raw.strip_prefix("profile ").unwrap_or(raw).trim().to_string();
            if !name.is_empty() {
                out.entry(name.clone()).or_default();
                cur = Some(name);
            }
            continue;
        }
        if let (Some(sec), Some((k, v))) = (cur.as_ref(), line.split_once('=')) {
            if let Some(s) = out.get_mut(sec) {
                s.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    out
}

// ============ XML scanner (cyberduck .duck, dreamweaver .ste) ============

/// Decode the small set of XML/HTML entities these config files use.
pub(crate) fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let rest = &s[i..];
        let Some(semi) = rest.find(';') else {
            out.push(c);
            continue;
        };
        let ent = &rest[1..semi];
        let decoded = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => {
                ent[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                for _ in 0..semi {
                    chars.next();
                }
            }
            None => out.push(c),
        }
    }
    out
}

/// Extract the value of an Apple-plist style `<key>NAME</key><…>VALUE</…>`
/// pair (Cyberduck `.duck`). Accepts `<string>`, `<integer>`, and the bare
/// `<true/>`/`<false/>` booleans.
pub(crate) fn plist_field(xml: &str, key: &str) -> Option<String> {
    let kt = format!("<key>{key}</key>");
    let i = xml.find(&kt)? + kt.len();
    let rest = xml[i..].trim_start();
    if rest.starts_with("<true/>") {
        return Some("true".to_string());
    }
    if rest.starts_with("<false/>") {
        return Some("false".to_string());
    }
    for tag in ["string", "integer", "real"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(s) = rest.strip_prefix(&open) {
            let e = s.find(&close)?;
            return Some(html_unescape(s[..e].trim()));
        }
    }
    None
}

/// All substrings `<tag ...>...</tag>` (or self-closing `<tag .../>`).
pub(crate) fn split_tags(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut hay = xml;
    while let Some(start) = hay.find(&open) {
        let after = &hay[start..];
        // self-closing within the opening tag
        if let Some(gt) = after.find('>') {
            if after.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
                out.push(after[..=gt].to_string());
                hay = &after[gt + 1..];
                continue;
            }
        }
        if let Some(end) = after.find(&close) {
            out.push(after[..end + close.len()].to_string());
            hay = &after[end + close.len()..];
        } else {
            break;
        }
    }
    out
}

/// First `<tag ...>` element slice (opening tag through its `>` or, if a
/// matching close exists, through `</tag>`).
pub(crate) fn first_tag(xml: &str, tag: &str) -> Option<String> {
    split_tags(xml, tag).into_iter().next()
}

/// Read an attribute value from a tag slice: `attr="VALUE"` or `attr='VALUE'`.
pub(crate) fn xml_attr(tag: &str, attr: &str) -> Option<String> {
    // Match the attribute name as a token (preceded by whitespace or '<tag')
    // so `host=` does not also match `vhost=`.
    let bytes = tag.as_bytes();
    let needle = format!("{attr}=");
    let mut search_from = 0;
    while let Some(rel) = tag[search_from..].find(&needle) {
        let pos = search_from + rel;
        let ok_boundary = pos == 0
            || matches!(bytes[pos - 1], b' ' | b'\t' | b'\n' | b'\r' | b'<');
        if !ok_boundary {
            search_from = pos + needle.len();
            continue;
        }
        let i = pos + needle.len();
        let q = *bytes.get(i)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let rest = &tag[i + 1..];
        let end = rest.find(q as char)?;
        return Some(html_unescape(&rest[..end]));
    }
    None
}

// ============ Bridge source metadata (single source of truth) ============
//
// These tables describe the 12 generically-dispatched bridge sources
// (the three legacy importers rclone/winscp/filezilla keep their own
// dedicated Tauri commands and are intentionally not listed here).
// Both the CLI (`cmd_export_bridge`) and the GUI Tauri commands
// (`bridge_commands`) read from here so the protocol filter and the
// export file format never drift between the two surfaces.

/// Canonical id list for the generically-dispatched sources.
pub const BRIDGE_GENERIC_SOURCES: &[&str] = &[
    "aws",
    "ssh",
    "mc",
    "cyberduck",
    "s3cmd",
    "lftp",
    "putty",
    "mobaxterm",
    "dreamweaver",
    "kopia",
    "duplicacy",
    "restic",
];

/// Protocol families a given source can carry. An empty slice means the
/// source is unknown to the generic bridge (legacy/own-command sources
/// return empty here so the CLI keeps its prior `_ => &[]` behaviour).
pub fn bridge_supported_protocols(src: &str) -> &'static [&'static str] {
    match src {
        "aws" | "mc" => &["s3"],
        "s3cmd" | "kopia" | "duplicacy" | "restic" => &["s3", "sftp", "webdav"],
        "lftp" => &["ftp", "ftps", "sftp", "webdav"],
        "ssh" | "putty" => &["sftp"],
        "cyberduck" | "mobaxterm" | "dreamweaver" => {
            &["ftp", "ftps", "sftp", "webdav", "s3"]
        }
        _ => &[],
    }
}

/// `(extension, human label)` for the file an export writes. `None` for
/// sources outside the generic bridge.
pub fn bridge_export_format(src: &str) -> Option<(&'static str, &'static str)> {
    let pair = match src {
        "aws" => ("ini", "AWS credentials"),
        "ssh" => ("conf", "ssh_config"),
        "mc" => ("json", "mc config.json"),
        "cyberduck" => ("duck", "Cyberduck bookmarks"),
        "s3cmd" => ("cfg", ".s3cfg"),
        "lftp" => ("rc", "lftp rc"),
        "putty" => ("reg", "PuTTY sessions"),
        "mobaxterm" => ("ini", "MobaXterm.ini"),
        "dreamweaver" => ("ste", "Dreamweaver site"),
        "kopia" => ("conf", "kopia repository config"),
        "duplicacy" => ("json", "duplicacy preferences"),
        "restic" => ("sh", "restic env script"),
        _ => return None,
    };
    Some(pair)
}

/// File-picker hint for an import: `(filter name, accepted extensions)`.
/// Many of these configs are conventionally extensionless (`credentials`,
/// `config`, `.s3cfg`), so the extension list is a UX convenience only:
/// the server side validates by canonicalisation/size, not by extension.
pub fn bridge_import_filter(src: &str) -> (&'static str, &'static [&'static str]) {
    match src {
        "aws" => ("AWS credentials", &["ini", "conf", "txt"]),
        "ssh" => ("ssh config", &["conf", "config", "txt"]),
        "mc" => ("mc config", &["json"]),
        "cyberduck" => ("Cyberduck bookmark", &["duck", "plist", "xml"]),
        "s3cmd" => ("s3cmd config", &["cfg", "s3cfg", "ini", "conf"]),
        "lftp" => ("lftp rc", &["rc", "conf", "txt"]),
        "putty" => ("PuTTY registry export", &["reg"]),
        "mobaxterm" => ("MobaXterm config", &["ini"]),
        "dreamweaver" => ("Dreamweaver site export", &["ste"]),
        "kopia" => ("kopia repository config", &["config", "conf", "json"]),
        "duplicacy" => ("duplicacy preferences", &["json", "txt"]),
        "restic" => ("restic env script", &["sh", "env", "txt"]),
        _ => ("Configuration", &["*"]),
    }
}

/// How much of a usable secret an import can realistically recover.
/// `"full"`: plaintext/obfuscated secret present, upgraded into the
/// vault. `"limited"`: only part of the secret material (host-bound or
/// optional). `"metadata"`: connection metadata only (secret lives in an
/// OS keychain / SSH agent / interactive prompt).
pub fn bridge_secret_policy(src: &str) -> &'static str {
    match src {
        "aws" | "mc" | "s3cmd" | "dreamweaver" | "kopia" | "restic" => "full",
        "lftp" | "mobaxterm" | "duplicacy" => "limited",
        "ssh" | "cyberduck" | "putty" => "metadata",
        _ => "metadata",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_tables_are_consistent() {
        for src in BRIDGE_GENERIC_SOURCES {
            // Every generic source must have a non-empty protocol set,
            // an export format and a recognised secret policy.
            assert!(
                !bridge_supported_protocols(src).is_empty(),
                "missing protocols for {src}"
            );
            assert!(
                bridge_export_format(src).is_some(),
                "missing export format for {src}"
            );
            assert!(
                matches!(bridge_secret_policy(src), "full" | "limited" | "metadata"),
                "bad secret policy for {src}"
            );
            let (_, exts) = bridge_import_filter(src);
            assert!(!exts.is_empty(), "missing import filter for {src}");
        }
        // Legacy / unknown sources fall through cleanly.
        assert!(bridge_supported_protocols("rclone").is_empty());
        assert!(bridge_export_format("winscp").is_none());
        assert_eq!(bridge_secret_policy("filezilla"), "metadata");
    }

    #[test]
    fn uuid_shape() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4'); // version nibble
        assert_eq!(&u[8..9], "-");
    }

    #[test]
    fn s3_provider_from_endpoint() {
        assert_eq!(map_s3_provider_from_endpoint(""), "amazon-s3");
        assert_eq!(
            map_s3_provider_from_endpoint("https://s3.us-east-1.amazonaws.com"),
            "amazon-s3"
        );
        assert_eq!(
            map_s3_provider_from_endpoint("https://abc.r2.cloudflarestorage.com"),
            "cloudflare-r2"
        );
        assert_eq!(
            map_s3_provider_from_endpoint("https://s3.wasabisys.com/bucket"),
            "wasabi"
        );
        assert_eq!(
            map_s3_provider_from_endpoint("http://192.168.1.10:9000"),
            "custom-s3"
        );
        assert_eq!(
            map_s3_provider_from_endpoint("https://play.min.io"),
            "minio"
        );
    }

    #[test]
    fn endpoint_host_strips_scheme_and_path() {
        assert_eq!(
            endpoint_host("https://s3.lab.example.com:9000/bucket/x"),
            "s3.lab.example.com:9000"
        );
        assert_eq!(endpoint_host("plain.host"), "plain.host");
    }

    #[test]
    fn json_map_skips_none_and_empty() {
        let m = json_map(&[
            ("a", Some("v".into())),
            ("b", None),
            ("c", Some(String::new())),
        ]);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("a").unwrap(), "v");
    }

    #[test]
    fn ini_collapses_profile_prefix() {
        let s = parse_ini_sections("[profile foo]\nregion = eu-west-1\n[bar]\nk=v\n");
        assert_eq!(s.get("foo").unwrap().get("region").unwrap(), "eu-west-1");
        assert_eq!(s.get("bar").unwrap().get("k").unwrap(), "v");
    }

    #[test]
    fn xml_helpers() {
        assert_eq!(html_unescape("a&amp;b&lt;c&#65;"), "a&b<cA");
        let duck = "<key>Hostname</key><string>ftp.example.com</string><key>Port</key><integer>21</integer><key>Secure</key><true/>";
        assert_eq!(plist_field(duck, "Hostname").as_deref(), Some("ftp.example.com"));
        assert_eq!(plist_field(duck, "Port").as_deref(), Some("21"));
        assert_eq!(plist_field(duck, "Secure").as_deref(), Some("true"));
        let ste = r#"<site name="My Site"><remoteInfo accessType="ftp" host="h.example.com" port="21" login="u"/></site>"#;
        let ri = first_tag(ste, "remoteInfo").unwrap();
        assert_eq!(xml_attr(&ri, "host").as_deref(), Some("h.example.com"));
        assert_eq!(xml_attr(&ri, "accessType").as_deref(), Some("ftp"));
        // token boundary: "ost=" must not satisfy a request for "host"
        assert_eq!(xml_attr(r#"<x vhost="bad" host="good"/>"#, "host").as_deref(), Some("good"));
    }

    #[test]
    fn atomic_write_sets_mode() {
        let dir = std::env::temp_dir().join(format!("bridge-aw-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("secret.env");
        atomic_write_600(&f, b"export X=1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "export X=1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
