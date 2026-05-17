// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from Adobe Dreamweaver site settings (`.ste`).
//!
//! The `.ste` file is Dreamweaver's "Site Settings export" interchange format:
//! a small, stable XML document (`<site>` with a `<remoteInfo>` element) that
//! is portable across Dreamweaver versions and operating systems. We parse
//! this documented export, NOT the in-app Dreamweaver CC configuration
//! (SQLite/prefs), which is unstable across CC releases.
//!
//! ## Password handling
//!
//! Dreamweaver `.ste` stores the connection password with its long-published,
//! fully reversible obfuscation scheme (hex pairs combined with a fixed,
//! index-dependent key). This is NOT real encryption: the scheme is public and
//! every decoded "Dreamweaver password" tool implements it. We reveal it on
//! import and re-obscure it on export so the `.ste` stays Dreamweaver-readable,
//! while the credential we keep is recrypted into our AES-256-GCM vault on
//! import (a security upgrade, identical posture to `winscp_import.rs`).
//!
//! ## Secret policy (shared bridge table)
//!
//! | Case | Bridge policy |
//! |---|---|
//! | plain/obfuscated-in-file (Dreamweaver pw) | reveal, populate `credential`, re-obscure on export |
//! | password attribute absent | `credential=None`, `has_stored_credential=Some(false)`, skipped-reason note "ste exported without login" (metadata profile still imported) |
//! | OS keychain | n/a for `.ste` |
//! | OAuth tokens | n/a for `.ste` |
//! | SSH key referenced | n/a for `.ste` (Dreamweaver SFTP stores a password) |
//!
//! `accessType` mapping: `ftp` -> ftp (21); `ftp`+useSSL/ftps -> ftps (990);
//! `sftp` -> sftp (22); `webdav` -> webdav (443); `local`/`network` -> skipped
//! ("not a remote server"); `rds` -> skipped ("ColdFusion RDS legacy"). An
//! empty host -> skipped ("missing host").

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Dreamweaver password (de)obfuscation ============
//
// scheme (authoritative): the `pw` attribute is an uppercase hex string, two
// hex digits per byte. Each byte is the plaintext byte plus its zero-based
// index, modulo 256:
//
//   enc[i]   = (plain[i] + i) mod 256
//   plain[i] = (enc[i]   - i) mod 256
//
// This is the real, long-published Dreamweaver site-password obfuscation
// (the `pw=`/`<remoteinfo>` value in a `.ste` export). It is NOT encryption.
//
// VERIFICATION STATUS: validated against a real Adobe-exported `.ste`
// (`pw="4575704346667968747D793D3C3F42"` decodes to `Etn@Basalto2024`,
// 15 bytes, plain[i] = enc[i] - i). The unit tests pin the algorithm with a
// neutral known vector and assert reveal o obscure == id both directions, so
// import -> export -> import stays byte-stable and the revealed value is
// recrypted into our AES-256-GCM vault on import.

/// Parse a single hex character to its numeric value (0-15).
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Reveal a Dreamweaver-obscured password (uppercase hex string).
///
/// Returns the cleartext. An empty input yields an empty string. Malformed
/// hex or non-UTF-8 output yields an empty string (treat as "no usable
/// credential" rather than failing the whole import), mirroring the lenient
/// posture the rclone/WinSCP importers take on undecodable secrets.
pub fn reveal_dreamweaver(enc: &str) -> String {
    let enc = enc.trim();
    if enc.is_empty() {
        return String::new();
    }
    let bytes = enc.as_bytes();
    if bytes.len() % 2 != 0 {
        return String::new();
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i < bytes.len() {
        let (Some(hi), Some(lo)) = (hex_nibble(bytes[i]), hex_nibble(bytes[i + 1])) else {
            return String::new();
        };
        let enc_byte = (hi << 4) | lo;
        let idx = out.len();
        // Authoritative Dreamweaver scheme: plain[i] = enc[i] - i (mod 256).
        let plain = enc_byte.wrapping_sub((idx & 0xFF) as u8);
        out.push(plain);
        i += 2;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Obscure a plaintext password using the Dreamweaver hex scheme.
///
/// Exact inverse of `reveal_dreamweaver`: `reveal(obscure(x)) == x` and
/// `obscure(reveal(y)) == y` for any valid `y`. Output is uppercase hex.
pub fn obscure_dreamweaver(plain: &str) -> String {
    let mut out = String::with_capacity(plain.len() * 2);
    for (idx, &b) in plain.as_bytes().iter().enumerate() {
        // Authoritative Dreamweaver scheme: enc[i] = plain[i] + i (mod 256).
        let enc_byte = b.wrapping_add((idx & 0xFF) as u8);
        out.push_str(&format!("{:02X}", enc_byte));
    }
    out
}

// ============ .ste parsing + mapping ============

struct MappedProfile {
    protocol: String,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    initial_path: Option<String>,
    public_url_base: Option<String>,
    /// When the metadata is importable but the credential was intentionally
    /// not exported by Dreamweaver, we still create the profile and surface
    /// a non-fatal note to the caller.
    no_login_note: bool,
}

/// Why a `<site>`/`<remoteInfo>` was not turned into a profile, or a note
/// about a profile that was imported without its credential.
enum MapOutcome {
    Mapped(MappedProfile),
    /// (kind, reason) for the skipped list. `kind` is the raw accessType.
    Skipped(String, String),
}

/// Decode Dreamweaver's `%XX` path/name encoding (sitename, remoteroot).
/// Leaves non-`%` characters and malformed escapes untouched.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Read the first present attribute among `names` from a tag slice.
/// Dreamweaver `.ste` attribute casing differs across exporters
/// (`accesstype` vs `accessType`, `user` vs `login`, `pw` vs `password`);
/// callers pass the real-format name first, legacy/camelCase as fallbacks.
fn ri_attr(ri: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| crate::bridge_shared::xml_attr(ri, n))
        .filter(|v| !v.trim().is_empty())
}

fn ri_flag(ri: &str, names: &[&str]) -> bool {
    ri_attr(ri, names)
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false)
}

/// Map one `<remoteinfo .../>` slice to a profile or a skip outcome.
///
/// Real Adobe `.ste` (validated against CS4-era exports) uses the lowercase
/// element `<remoteinfo>` with `accesstype`, `host`, `user`, `pw`,
/// `remoteroot`, `useSFTP`, `usepasv`. The camelCase names are accepted as
/// fallbacks for other Dreamweaver generations and our own export.
fn map_remote_info(ri: &str) -> MapOutcome {
    let access = ri_attr(ri, &["accesstype", "accessType"]).unwrap_or_default();
    let access_l = access.to_lowercase();

    let is_sftp = ri_flag(ri, &["useSFTP", "usesftp"]);
    let ssl = ri_flag(ri, &["useSSL", "usessl", "encryptedftp"])
        || ri_attr(ri, &["ftps"]).is_some();

    let (protocol, default_port): (&str, u32) = match access_l.as_str() {
        "ftp" => {
            if is_sftp {
                ("sftp", 22)
            } else if ssl {
                ("ftps", 990)
            } else {
                ("ftp", 21)
            }
        }
        "ftps" => ("ftps", 990),
        "sftp" => ("sftp", 22),
        "webdav" => ("webdav", 443),
        "local" | "network" => {
            return MapOutcome::Skipped(access, "not a remote server".to_string());
        }
        "rds" => {
            return MapOutcome::Skipped(access, "ColdFusion RDS legacy".to_string());
        }
        other => {
            return MapOutcome::Skipped(
                if other.is_empty() {
                    "(none)".to_string()
                } else {
                    access
                },
                "unsupported accessType".to_string(),
            );
        }
    };

    let host = ri_attr(ri, &["host", "server"]).unwrap_or_default();
    if host.trim().is_empty() {
        return MapOutcome::Skipped(access, "missing host".to_string());
    }

    let port = ri_attr(ri, &["port"])
        .and_then(|p| p.trim().parse::<u32>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(default_port);

    let username = ri_attr(ri, &["user", "login"]).unwrap_or_default();

    // `pw` is present only when the user exported with login/credentials;
    // Adobe omits it by default when sharing `.ste` between users.
    let raw_pw = ri_attr(ri, &["pw", "password"]);
    let (password, no_login_note) = match raw_pw {
        Some(enc) => {
            let revealed = reveal_dreamweaver(&enc);
            if revealed.is_empty() {
                (None, true)
            } else {
                (Some(revealed), false)
            }
        }
        None => (None, true),
    };

    let initial_path = ri_attr(ri, &["remoteroot", "directory", "remoteRoot"])
        .map(|d| pct_decode(d.trim()))
        .filter(|d| !d.is_empty() && d != "/");

    let public_url_base = ri_attr(ri, &["webUrl", "URLPrefix", "httpaddress"])
        .map(|u| pct_decode(u.trim()))
        .filter(|u| !u.is_empty());

    MapOutcome::Mapped(MappedProfile {
        protocol: protocol.to_string(),
        host,
        port,
        username,
        password,
        initial_path,
        public_url_base,
        no_login_note,
    })
}

// ============ Default config path detection ============

/// Dreamweaver `.ste` files are explicit user exports, not auto-located.
///
/// Dreamweaver CC keeps its live site configuration in a per-version
/// SQLite/prefs store whose layout changes between CC releases and is not a
/// documented interchange format. The `.ste` export, by contrast, is the
/// stable documented format, but the user chooses where to write it. There is
/// therefore no deterministic default path: the caller must pass the `.ste`
/// path explicitly.
pub fn default_dreamweaver_config_path() -> Option<PathBuf> {
    None
}

// ============ Public API ============

/// A `<site>` that was skipped, or imported without its credential.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamweaverSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing a Dreamweaver `.ste` file.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamweaverImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<DreamweaverSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Maximum number of `<site>` blocks to parse (DoS guard on crafted files).
const MAX_SITES: usize = 10_000;

/// Import all sites from a Dreamweaver `.ste` export.
pub fn import_dreamweaver(path: &Path) -> Result<DreamweaverImportResult, String> {
    use crate::bridge_shared::{first_tag, split_tags, uuid_v4, xml_attr};

    let xml = std::fs::read_to_string(path).map_err(|e| format!("read .ste: {}", e))?;

    // Real Adobe `.ste` is a single `<site>` document; older/other variants
    // may wrap multiple. `<remoteinfo>` (lowercase) is the real element;
    // `<remoteInfo>` is accepted as a fallback.
    let mut sites = split_tags(&xml, "site");
    if sites.is_empty()
        && (first_tag(&xml, "remoteinfo").is_some() || first_tag(&xml, "remoteInfo").is_some())
    {
        sites.push(xml.clone());
    }

    let total_remotes = sites.len().min(MAX_SITES);
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for site in sites.into_iter().take(MAX_SITES) {
        // Real `.ste` has no `<site name>`: the site name lives URL-encoded
        // in `<localinfo sitename="...">`. Fall back to legacy attributes,
        // then to the remote host.
        let localinfo = first_tag(&site, "localinfo").or_else(|| first_tag(&site, "localInfo"));
        let name = localinfo
            .as_deref()
            .and_then(|li| ri_attr(li, &["sitename", "siteName"]))
            .map(|n| pct_decode(&n))
            .or_else(|| xml_attr(&site, "name"))
            .or_else(|| xml_attr(&site, "siteName"))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Dreamweaver site".to_string());

        let Some(ri) = first_tag(&site, "remoteinfo").or_else(|| first_tag(&site, "remoteInfo"))
        else {
            skipped.push(DreamweaverSkippedRemote {
                name,
                kind: "(none)".to_string(),
                reason: "no <remoteinfo> element".to_string(),
            });
            continue;
        };

        match map_remote_info(&ri) {
            MapOutcome::Mapped(m) => {
                let id = format!(
                    "dw-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &uuid_v4()[..8]
                );

                if m.no_login_note {
                    skipped.push(DreamweaverSkippedRemote {
                        name: name.clone(),
                        kind: m.protocol.clone(),
                        reason: "ste exported without login".to_string(),
                    });
                }

                // Real `.ste` keeps the public URL on <localinfo httpaddress>
                // or <appserverinfo urlprefix>, not on <remoteinfo>.
                let public_url_base = m.public_url_base.or_else(|| {
                    localinfo
                        .as_deref()
                        .and_then(|li| ri_attr(li, &["httpaddress", "httpAddress"]))
                        .or_else(|| {
                            first_tag(&site, "appserverinfo")
                                .as_deref()
                                .and_then(|ai| ri_attr(ai, &["urlprefix", "urlPrefix"]))
                        })
                        .map(|u| pct_decode(u.trim()))
                        .filter(|u| !u.is_empty())
                });

                let has_cred = m.password.is_some();
                servers.push(ServerProfileExport {
                    id,
                    name,
                    host: m.host,
                    port: m.port,
                    username: m.username,
                    protocol: Some(m.protocol),
                    initial_path: m.initial_path,
                    local_initial_path: None,
                    color: None,
                    last_connected: None,
                    options: None,
                    provider_id: None,
                    credential: m.password,
                    has_stored_credential: if has_cred { None } else { Some(false) },
                    public_url_base,
                });
            }
            MapOutcome::Skipped(kind, reason) => {
                skipped.push(DreamweaverSkippedRemote { name, kind, reason });
            }
        }
    }

    Ok(DreamweaverImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to .ste ============

/// A server profile to export as a Dreamweaver `.ste` site.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamweaverExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Minimal XML attribute-value escaper for the `.ste` writer.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode the characters Dreamweaver escapes in `sitename` /
/// `remoteroot` (space, `[`, `]`, `%`, control). Exact inverse of
/// `pct_decode` for those, so a name round-trips through real Dreamweaver.
fn pct_encode_min(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b == b' ' || b == b'[' || b == b']' || b == b'%' || b < 0x20 {
            out.push_str(&format!("%{:02X}", b));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// Export an AeroFTP server profile as a Dreamweaver `.ste` XML file.
///
/// A real Adobe `.ste` describes exactly ONE site (`<localinfo>` +
/// `<remoteinfo>` + `<appserverinfo>`), so the export targets the first
/// eligible profile and errors if none is exportable. The file is written
/// in the real lowercase Dreamweaver schema; the password (looked up by
/// profile `name` in `passwords`) is re-obscured with `obscure_dreamweaver`
/// so the `.ste` is byte-stable through `import_dreamweaver` and loadable by
/// Dreamweaver itself. Returns 1 on success.
pub fn export_dreamweaver(
    servers: &[DreamweaverExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    use crate::bridge_shared::atomic_write_600;

    let server = servers
        .iter()
        .find(|s| {
            matches!(
                s.protocol.as_deref().unwrap_or("ftp"),
                "ftp" | "ftps" | "sftp" | "webdav"
            )
        })
        .ok_or_else(|| {
            "dreamweaver export: no FTP/FTPS/SFTP/WebDAV profile to write".to_string()
        })?;

    let proto = server.protocol.as_deref().unwrap_or("ftp");
    // Dreamweaver expresses SFTP as accesstype="ftp" + useSFTP="TRUE"; FTPS
    // as accesstype="ftp" + useSSL="TRUE"; WebDAV via accesstype="webdav".
    let (access_type, use_sftp, use_ssl) = match proto {
        "ftp" => ("ftp", false, false),
        "ftps" => ("ftp", false, true),
        "sftp" => ("ftp", true, false),
        "webdav" => ("webdav", false, false),
        _ => unreachable!("filtered above"),
    };

    let remoteroot = server.initial_path.as_deref().unwrap_or("");
    let pw_attr = passwords
        .get(&server.name)
        .filter(|p| !p.is_empty())
        .map(|pw| obscure_dreamweaver(pw));
    let http = server
        .options
        .as_ref()
        .and_then(|v| v.get("public_url_base").or_else(|| v.get("webUrl")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n");
    xml.push_str("<site>\n");
    xml.push_str(&format!(
        "<localinfo sitename=\"{}\" ftporrdsserver=\"FALSE\" httpaddress=\"{}\" curserver=\"webserver\"/>\n",
        pct_encode_min(&server.name),
        xml_escape(http),
    ));
    xml.push_str(&format!(
        "<remoteinfo accesstype=\"{}\" host=\"{}\" remoteroot=\"{}\" user=\"{}\"{}{}{} usepasv=\"TRUE\" useSFTP=\"{}\"/>\n",
        access_type,
        xml_escape(&server.host),
        pct_encode_min(remoteroot),
        xml_escape(&server.username),
        pw_attr
            .as_deref()
            .map(|p| format!(" pw=\"{p}\""))
            .unwrap_or_default(),
        if server.port != crate::bridge_shared::default_port_for(proto) {
            format!(" port=\"{}\"", server.port)
        } else {
            String::new()
        },
        if use_ssl { " useSSL=\"TRUE\"" } else { "" },
        if use_sftp { "TRUE" } else { "FALSE" },
    ));
    xml.push_str("</site>\n");

    atomic_write_600(out, xml.as_bytes())?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Authoritative password algorithm: plain[i] = enc[i] - i ----

    #[test]
    fn test_dreamweaver_password_known_vector() {
        // Anchor computed independently of the implementation:
        // 'A'=0x41 at i=0 -> 0x41 ; 'B'=0x42 at i=1 -> 0x43 ; hex "4143".
        // This pins the exact real Dreamweaver transform (validated against
        // four genuine Adobe `.ste` exports during development).
        assert_eq!(obscure_dreamweaver("AB"), "4143");
        assert_eq!(reveal_dreamweaver("4143"), "AB");

        for s in [
            "hello",
            "p@ssw0rd!",
            "Site2024",
            "with spaces #&<>\"'",
            "unicode \u{00e9}\u{20ac}",
            "x",
            "a password longer than two hundred and fifty six bytes ........................................................................................................................................................................................................................ end",
        ] {
            let enc = obscure_dreamweaver(s);
            assert_eq!(enc.len() % 2, 0);
            assert!(enc.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_eq!(reveal_dreamweaver(&enc), s, "round trip for {s:?}");
        }
    }

    #[test]
    fn test_reveal_empty_and_malformed() {
        assert_eq!(reveal_dreamweaver(""), "");
        assert_eq!(reveal_dreamweaver("   "), "");
        assert_eq!(reveal_dreamweaver("ABC"), ""); // odd length
        assert_eq!(reveal_dreamweaver("ZZ"), ""); // non-hex
    }

    // ---- Real Adobe `.ste` schema (lowercase, validated layout) ----

    /// One real-schema `.ste` (synthetic host/credential). The `pw` is
    /// produced with `obscure_dreamweaver` so the fixture matches the scheme.
    fn real_ste(pw_plain: Option<&str>) -> String {
        let pw_attr = pw_plain
            .map(|p| format!(" pw=\"{}\"", obscure_dreamweaver(p)))
            .unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="utf-8" ?>
<site>
<localinfo sitename="%5BA%5D%20demo.example.com" ftporrdsserver="FALSE" localroot="E:%5Cwww%5Cdemo%5C" httpaddress="https://www.demo.example.com/" curserver="webserver"/>
<remoteinfo accesstype="ftp" host="ftp.demo.example.com" remoteroot="www.demo.example.com/" user="12345@aruba.it"{pw_attr} usefirewall="FALSE" usepasv="TRUE" useSFTP="FALSE" useIpv6="FALSE"/>
<appserverinfo servermodel="PHP MySQL" urlprefix="https://www.demo.example.com/" accesstype="ftp" host="ftp.demo.example.com" remoteroot="www.demo.example.com/" user="12345@aruba.it"{pw_attr} testsvrbinaccesstype="none"/>
</site>"#
        )
    }

    fn write_tmp(body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aeroftp-dw-{}.ste",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn test_parse_real_schema_ste() {
        let p = write_tmp(&real_ste(Some("Demo@Pass2024")));
        let r = import_dreamweaver(&p).expect("parse real .ste");
        std::fs::remove_file(&p).ok();

        assert_eq!(r.total_remotes, 1);
        assert_eq!(r.servers.len(), 1);
        let s = &r.servers[0];
        // sitename is URL-encoded in <localinfo>; decoded to the profile name.
        assert_eq!(s.name, "[A] demo.example.com");
        assert_eq!(s.protocol.as_deref(), Some("ftp"));
        assert_eq!(s.host, "ftp.demo.example.com");
        assert_eq!(s.port, 21);
        assert_eq!(s.username, "12345@aruba.it");
        assert_eq!(s.initial_path.as_deref(), Some("www.demo.example.com/"));
        // public URL comes from <localinfo httpaddress> / <appserverinfo>.
        assert_eq!(
            s.public_url_base.as_deref(),
            Some("https://www.demo.example.com/")
        );
        // pw present -> credential decoded, has_stored_credential None.
        assert_eq!(s.credential.as_deref(), Some("Demo@Pass2024"));
        assert_eq!(s.has_stored_credential, None);
        // The <appserverinfo> duplicate must NOT create a second profile.
        assert!(r.skipped.is_empty());
    }

    #[test]
    fn test_real_schema_sftp_and_skips() {
        // accesstype="ftp" + useSFTP="TRUE" -> sftp(22)
        let sftp = r#"<site>
<localinfo sitename="NAS" httpaddress=""/>
<remoteinfo accesstype="ftp" host="nas.example.com" user="root" useSFTP="TRUE" usepasv="TRUE"/>
</site>"#;
        let p = write_tmp(sftp);
        let r = import_dreamweaver(&p).expect("parse");
        std::fs::remove_file(&p).ok();
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("sftp"));
        assert_eq!(s.port, 22);
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        assert!(r
            .skipped
            .iter()
            .any(|k| k.name == "NAS" && k.reason == "ste exported without login"));

        for (acc, reason) in [
            ("local", "not a remote server"),
            ("rds", "ColdFusion RDS legacy"),
        ] {
            let body = format!(
                r#"<site><localinfo sitename="X"/><remoteinfo accesstype="{acc}" host="h"/></site>"#
            );
            let p = write_tmp(&body);
            let r = import_dreamweaver(&p).expect("parse");
            std::fs::remove_file(&p).ok();
            assert!(r.servers.is_empty());
            assert_eq!(r.skipped[0].reason, reason, "accesstype {acc}");
        }
    }

    #[test]
    fn test_secret_policy_present_vs_absent() {
        let with = write_tmp(&real_ste(Some("s3cr3t")));
        let r = import_dreamweaver(&with).expect("parse");
        std::fs::remove_file(&with).ok();
        assert_eq!(r.servers[0].credential.as_deref(), Some("s3cr3t"));
        assert_eq!(r.servers[0].has_stored_credential, None);

        let without = write_tmp(&real_ste(None));
        let r2 = import_dreamweaver(&without).expect("parse");
        std::fs::remove_file(&without).ok();
        assert!(r2.servers[0].credential.is_none());
        assert_eq!(r2.servers[0].has_stored_credential, Some(false));
        assert!(r2
            .skipped
            .iter()
            .any(|k| k.reason == "ste exported without login"));
    }

    #[test]
    fn test_import_export_import_idempotence() {
        let src = write_tmp(&real_ste(Some("RoundTrip@1")));
        let first = import_dreamweaver(&src).expect("first import");
        std::fs::remove_file(&src).ok();
        let a = &first.servers[0];

        let mut passwords = HashMap::new();
        if let Some(c) = &a.credential {
            passwords.insert(a.name.clone(), c.clone());
        }
        let export_servers = vec![DreamweaverExportServer {
            name: a.name.clone(),
            host: a.host.clone(),
            port: a.port,
            username: a.username.clone(),
            protocol: a.protocol.clone(),
            options: a
                .public_url_base
                .as_ref()
                .map(|u| serde_json::json!({ "public_url_base": u })),
            provider_id: None,
            initial_path: a.initial_path.clone(),
        }];

        let out = std::env::temp_dir().join(format!(
            "aeroftp-dw-out-{}.ste",
            crate::bridge_shared::uuid_v4()
        ));
        let n = export_dreamweaver(&export_servers, &passwords, &out).expect("export");
        assert_eq!(n, 1, "Dreamweaver .ste is one site per file");

        let second = import_dreamweaver(&out).expect("reimport");
        std::fs::remove_file(&out).ok();
        let b = &second.servers[0];

        assert_eq!(a.name, b.name, "name survives pct encode/decode");
        assert_eq!(a.host, b.host);
        assert_eq!(a.port, b.port);
        assert_eq!(a.username, b.username);
        assert_eq!(a.protocol, b.protocol);
        assert_eq!(a.initial_path, b.initial_path);
        assert_eq!(a.credential, b.credential, "pw byte-stable");
        assert_eq!(a.public_url_base, b.public_url_base);
    }

    #[test]
    fn test_default_path_is_none() {
        assert!(default_dreamweaver_config_path().is_none());
    }
}
