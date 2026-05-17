// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from lftp bookmarks.
//!
//! Parses lftp's `bookmarks` file (`~/.local/share/lftp/bookmarks`, legacy
//! `~/.lftp/bookmarks`). Each line is `name<whitespace/TAB>url` where the URL is
//! an lftp connection URL:
//! `proto://[user[:password]@]host[:port][/path]`. lftp may embed the password
//! directly in the bookmark URL, plain or percent-encoded. We decode it and
//! store it in our AES-256-GCM vault, which is a security upgrade over lftp's
//! plaintext-in-file storage.
//!
//! Scope note: lftp also supports an `rc` file (`~/.lftp/rc`,
//! `~/.config/lftp/rc`) with `open ...`/`bookmark add` directives. The `rc`
//! grammar is a full scripting surface (conditionals, includes, aliases,
//! per-command settings) and parsing it faithfully is fragile and out of scope
//! for v1. Only the `bookmarks` file is parsed here; `rc` is documented as
//! best-effort-not-implemented intentionally to avoid mis-parsing.
//!
//! Secret policy (APPENDIX-BRIDGE section 1):
//!
//! | Case | lftp form | Bridge policy |
//! |---|---|---|
//! | Secret in file (plain/obfuscated) | password in bookmark URL | decode, store in AES-256-GCM vault |
//! | Reversible obfuscation | n/a (lftp does not obfuscate) | n/a |
//! | Secret in OS keychain | n/a | n/a |
//! | OAuth token | n/a (lftp has no OAuth backends we map) | n/a |
//! | SSH key referenced | sftp without password | credential=None, has_stored_credential=Some(false) |

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ URL percent-decoding ============

/// Parse a single hex character to its numeric value (0-15).
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decode percent-encoded byte sequences (`%20` -> space, `%C3%A4` -> a-umlaut).
/// Multi-byte UTF-8 sequences are reassembled correctly. Mirrors the local
/// `url_decode` in `winscp_import.rs`.
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

/// Percent-encode a string for safe placement inside an lftp URL userinfo or
/// path segment. RFC 3986 unreserved characters pass through; everything else
/// is `%XX`. Used by the exporter so passwords round-trip through the URL.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

// ============ Bookmark URL parsing ============

struct MappedProfile {
    protocol: String,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    initial_path: Option<String>,
}

/// Map an lftp URL scheme to (AeroFTP protocol, default port).
///
/// lftp schemes seen in bookmarks: ftp, ftps, sftp, fish, http, https.
/// `fish` is SSH-tunnelled file access -> sftp. `http`/`https` -> webdav.
fn map_scheme(scheme: &str) -> Option<(&'static str, u32)> {
    match scheme.to_lowercase().as_str() {
        "ftp" => Some(("ftp", 21)),
        "ftps" => Some(("ftps", 990)),
        "sftp" | "fish" => Some(("sftp", 22)),
        "http" | "https" => Some(("webdav", 443)),
        _ => None,
    }
}

/// Parse a single lftp bookmark URL into a mapped profile.
///
/// Grammar: `proto://[user[:password]@]host[:port][/path]`. The userinfo and
/// path segments may be percent-encoded. Returns `None` for an unsupported
/// scheme or a missing host so the caller can record a skip reason.
fn parse_bookmark_url(url: &str) -> Option<MappedProfile> {
    let (scheme, rest) = url.split_once("://")?;
    let (protocol, default_port) = map_scheme(scheme)?;

    // Split optional userinfo from `host[:port]/path`. lftp uses the last '@'
    // before the first '/' as the userinfo delimiter so a password containing
    // an unencoded '@' (rare but legal) still splits at the right place.
    let (authority, path_part) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };

    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };

    let (username, password) = match userinfo {
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (
                url_decode(u),
                if p.is_empty() {
                    None
                } else {
                    Some(url_decode(p))
                },
            ),
            None => (url_decode(ui), None),
        },
        None => (String::new(), None),
    };

    // host[:port] -- IPv6 literals (`[::1]:port`) are kept verbatim in host.
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        // bracketed IPv6
        match stripped.split_once(']') {
            Some((h6, after)) => {
                let port = after
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u32>().ok())
                    .unwrap_or(default_port);
                (format!("[{}]", h6), port)
            }
            None => (hostport.to_string(), default_port),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => {
                let port = p.parse::<u32>().unwrap_or(default_port);
                (h.to_string(), port)
            }
            None => (hostport.to_string(), default_port),
        }
    };

    if host.is_empty() {
        return None;
    }

    let initial_path = {
        let p = url_decode(path_part);
        let trimmed = p.trim();
        if trimmed.is_empty() || trimmed == "/" {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    Some(MappedProfile {
        protocol: protocol.to_string(),
        host,
        port,
        username,
        password,
        initial_path,
    })
}

/// Parse an lftp `bookmarks` file into `name -> url` pairs, preserving file
/// order is not required (results are keyed by name like the rclone parser).
/// Each line is `name<TAB or spaces>url`. Lines without a separable URL are
/// returned with an empty URL so the caller can record the skip reason.
fn parse_bookmarks(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Name and URL are separated by the first run of whitespace (lftp
        // writes a single space, but a TAB or multiple spaces are tolerated).
        match line.split_once(char::is_whitespace) {
            Some((name, rest)) => {
                let name = name.trim().to_string();
                let url = rest.trim().to_string();
                if !name.is_empty() {
                    out.push((name, url));
                }
            }
            None => {
                // A bare token with no URL: keep it so it is reported skipped.
                out.push((line.to_string(), String::new()));
            }
        }
    }
    out
}

// ============ Default config path detection ============

/// Returns the default lftp `bookmarks` path for the current platform.
///
/// Chain: `$XDG_DATA_HOME/lftp/bookmarks` -> `$HOME/.local/share/lftp/bookmarks`
/// -> legacy `$HOME/.lftp/bookmarks`. lftp has no dedicated env var pointing at
/// the bookmarks file. Windows has no canonical lftp install location, so this
/// returns `None` there (the user browses to an exported file).
pub fn default_lftp_config_path() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                let path = PathBuf::from(xdg).join("lftp").join("bookmarks");
                if path.exists() {
                    return Some(path);
                }
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                let xdg_default = PathBuf::from(&home)
                    .join(".local")
                    .join("share")
                    .join("lftp")
                    .join("bookmarks");
                if xdg_default.exists() {
                    return Some(xdg_default);
                }
                let legacy = PathBuf::from(&home).join(".lftp").join("bookmarks");
                if legacy.exists() {
                    return Some(legacy);
                }
            }
        }
        None
    }

    #[cfg(not(unix))]
    {
        None
    }
}

// ============ Public API ============

/// Result of importing lftp bookmarks.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LftpImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<LftpSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// A bookmark that was skipped (unsupported scheme or malformed line).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LftpSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Maximum number of bookmarks to parse (defense against crafted files).
const MAX_BOOKMARKS: usize = 10_000;

/// Import all supported bookmarks from an lftp `bookmarks` file.
pub fn import_lftp(path: &Path) -> Result<LftpImportResult, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Read lftp bookmarks: {}", e))?;

    let entries = parse_bookmarks(&content);
    let total_remotes = entries.len().min(MAX_BOOKMARKS);
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for (name, url) in entries.into_iter().take(MAX_BOOKMARKS) {
        // Record the scheme (best-effort) for the skipped "kind" field.
        let scheme = url
            .split_once("://")
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();

        if url.is_empty() {
            skipped.push(LftpSkippedRemote {
                name,
                kind: String::new(),
                reason: "no URL on bookmark line".to_string(),
            });
            continue;
        }

        match parse_bookmark_url(&url) {
            Some(mapped) => {
                let id = format!(
                    "lftp-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );

                let has_stored_credential = if mapped.password.is_some() {
                    None
                } else {
                    // No password embedded: the user must re-enter it (sftp
                    // key auth or interactive prompt). Signal it explicitly.
                    Some(false)
                };

                servers.push(ServerProfileExport {
                    id,
                    name: name.clone(),
                    host: mapped.host,
                    port: mapped.port,
                    username: mapped.username,
                    protocol: Some(mapped.protocol),
                    initial_path: mapped.initial_path,
                    local_initial_path: None,
                    color: None,
                    last_connected: None,
                    options: None,
                    provider_id: None,
                    credential: mapped.password,
                    has_stored_credential,
                    public_url_base: None,
                });
            }
            None => {
                let reason = if scheme.is_empty() {
                    "malformed bookmark URL (no scheme)".to_string()
                } else if map_scheme(&scheme).is_none() {
                    format!("unsupported lftp scheme: {}", scheme)
                } else {
                    "malformed bookmark URL (missing host)".to_string()
                };
                skipped.push(LftpSkippedRemote {
                    name,
                    kind: scheme,
                    reason,
                });
            }
        }
    }

    Ok(LftpImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to lftp bookmarks ============

/// A server profile to export as an lftp bookmark.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LftpExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Map an AeroFTP protocol back to an lftp URL scheme.
fn protocol_to_scheme(protocol: &str) -> Option<&'static str> {
    match protocol {
        "ftp" => Some("ftp"),
        "ftps" => Some("ftps"),
        "sftp" => Some("sftp"),
        "webdav" => Some("https"),
        _ => None,
    }
}

/// Export AeroFTP server profiles to an lftp `bookmarks` file.
///
/// Each line is `name url` with the password percent-encoded back into the URL
/// when present. The file carries secrets, so it is written atomically with
/// mode `0600` on unix (`atomic_write_600`).
pub fn export_lftp(
    servers: &[LftpExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut body = String::from("# lftp bookmarks exported by AeroFTP - https://aeroftp.app\n");
    let mut exported = 0;

    for server in servers {
        let proto = server.protocol.as_deref().unwrap_or("ftp");
        let Some(scheme) = protocol_to_scheme(proto) else {
            continue; // protocol without an lftp URL form
        };

        // Bookmark name: lftp uses the first whitespace as the name/url
        // delimiter, so a name with whitespace would corrupt the line.
        // Collapse internal whitespace to '_' to keep the bookmark parseable.
        let safe_name: String = server
            .name
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .filter(|c| *c != '\n' && *c != '\r')
            .collect();
        if safe_name.is_empty() {
            continue;
        }

        let mut url = String::from(scheme);
        url.push_str("://");

        let password = passwords.get(&server.name).filter(|p| !p.is_empty());
        if !server.username.is_empty() {
            url.push_str(&url_encode(&server.username));
            if let Some(pw) = password {
                url.push(':');
                url.push_str(&url_encode(pw));
            }
            url.push('@');
        }

        url.push_str(&server.host);

        // Only emit a port when it differs from the scheme default so the
        // bookmark stays clean and round-trips to the same parsed port.
        let default_port = match scheme {
            "ftp" => 21,
            "ftps" => 990,
            "sftp" => 22,
            _ => 443,
        };
        if server.port != 0 && server.port != default_port {
            url.push_str(&format!(":{}", server.port));
        }

        if let Some(path) = server.initial_path.as_deref().filter(|p| !p.is_empty()) {
            if !path.starts_with('/') {
                url.push('/');
            }
            url.push_str(path);
        }

        body.push_str(&format!("{} {}\n", safe_name, url));
        exported += 1;
    }

    crate::bridge_shared::atomic_write_600(out, body.as_bytes())?;
    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# lftp bookmarks
prod-ftp ftp://deploy:s3cr%40t@ftp.example.com:2121/var/www
nas-sftp sftp://admin@192.168.1.10:22/srv/share
cloud-dav https://user:pw123@dav.example.com/remote.php/dav
broken-line not-a-url-without-scheme
mystery gopher://old.example.com/0/
";

    #[test]
    fn test_parse_bookmarks_fixture() {
        let entries = parse_bookmarks(FIXTURE);
        // 4 well-formed lines + 1 "broken-line" + 1 "mystery"
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].0, "prod-ftp");
        assert_eq!(entries[0].1, "ftp://deploy:s3cr%40t@ftp.example.com:2121/var/www");
        assert_eq!(entries[3].0, "broken-line");
        assert_eq!(entries[3].1, "not-a-url-without-scheme");
    }

    #[test]
    fn test_parse_bookmark_url_ftp_with_userpass() {
        let m = parse_bookmark_url("ftp://deploy:s3cr%40t@ftp.example.com:2121/var/www")
            .expect("ftp url should parse");
        assert_eq!(m.protocol, "ftp");
        assert_eq!(m.host, "ftp.example.com");
        assert_eq!(m.port, 2121);
        assert_eq!(m.username, "deploy");
        // %40 percent-decodes to '@' inside the password
        assert_eq!(m.password.as_deref(), Some("s3cr@t"));
        assert_eq!(m.initial_path.as_deref(), Some("/var/www"));
    }

    #[test]
    fn test_parse_bookmark_url_sftp_no_password() {
        let m = parse_bookmark_url("sftp://admin@192.168.1.10:22/srv/share")
            .expect("sftp url should parse");
        assert_eq!(m.protocol, "sftp");
        assert_eq!(m.host, "192.168.1.10");
        assert_eq!(m.port, 22);
        assert_eq!(m.username, "admin");
        assert!(m.password.is_none());
    }

    #[test]
    fn test_parse_bookmark_url_https_to_webdav() {
        let m = parse_bookmark_url("https://user:pw123@dav.example.com/remote.php/dav")
            .expect("https url should parse");
        assert_eq!(m.protocol, "webdav");
        assert_eq!(m.host, "dav.example.com");
        assert_eq!(m.port, 443);
        assert_eq!(m.username, "user");
        assert_eq!(m.password.as_deref(), Some("pw123"));
    }

    #[test]
    fn test_parse_bookmark_url_fish_maps_sftp() {
        let m =
            parse_bookmark_url("fish://root@nas.local").expect("fish url should parse");
        assert_eq!(m.protocol, "sftp");
        assert_eq!(m.port, 22);
        assert_eq!(m.username, "root");
    }

    #[test]
    fn test_parse_bookmark_url_unsupported_scheme() {
        assert!(parse_bookmark_url("gopher://old.example.com/0/").is_none());
        assert!(parse_bookmark_url("not-a-url-without-scheme").is_none());
    }

    #[test]
    fn test_full_import_secret_policy() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-lftp-{}.bookmarks",
            crate::bridge_shared::uuid_v4()
        ));
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(FIXTURE.as_bytes()).unwrap();
        }
        let result = import_lftp(&tmp).expect("should import");
        std::fs::remove_file(&tmp).ok();

        // 3 mapped (ftp, sftp, https) + 2 skipped (broken-line, gopher)
        assert_eq!(result.servers.len(), 3);
        assert_eq!(result.skipped.len(), 2);

        // Embedded password -> credential populated.
        let ftp = result
            .servers
            .iter()
            .find(|s| s.name == "prod-ftp")
            .unwrap();
        assert_eq!(ftp.protocol.as_deref(), Some("ftp"));
        assert_eq!(ftp.credential.as_deref(), Some("s3cr@t"));
        assert_eq!(ftp.has_stored_credential, None);

        // No embedded password -> credential None + has_stored_credential false.
        let sftp = result
            .servers
            .iter()
            .find(|s| s.name == "nas-sftp")
            .unwrap();
        assert!(sftp.credential.is_none());
        assert_eq!(sftp.has_stored_credential, Some(false));

        // Skipped reasons.
        let unsupported = result
            .skipped
            .iter()
            .find(|s| s.name == "mystery")
            .unwrap();
        assert!(unsupported.reason.contains("unsupported lftp scheme"));
        let malformed = result
            .skipped
            .iter()
            .find(|s| s.name == "broken-line")
            .unwrap();
        assert!(malformed.reason.contains("no scheme"));
    }

    #[test]
    fn test_roundtrip_idempotence_with_password() {
        let servers = vec![
            LftpExportServer {
                name: "prod-ftp".to_string(),
                host: "ftp.example.com".to_string(),
                port: 2121,
                username: "deploy".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: Some("/var/www".to_string()),
            },
            LftpExportServer {
                name: "nas-sftp".to_string(),
                host: "192.168.1.10".to_string(),
                port: 22,
                username: "admin".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
        ];
        let mut passwords = HashMap::new();
        // Password with characters that MUST survive URL-encode round-trip.
        passwords.insert("prod-ftp".to_string(), "s3cr@t:/pass".to_string());

        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-test-lftp-export-{}.bookmarks",
            crate::bridge_shared::uuid_v4()
        ));
        let exported = export_lftp(&servers, &passwords, &tmp).expect("should export");
        assert_eq!(exported, 2);

        let result = import_lftp(&tmp).expect("should reimport");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.servers.len(), 2);

        let ftp = result
            .servers
            .iter()
            .find(|s| s.name == "prod-ftp")
            .unwrap();
        assert_eq!(ftp.protocol.as_deref(), Some("ftp"));
        assert_eq!(ftp.host, "ftp.example.com");
        assert_eq!(ftp.port, 2121);
        assert_eq!(ftp.username, "deploy");
        // The tricky password round-trips intact through percent-encoding.
        assert_eq!(ftp.credential.as_deref(), Some("s3cr@t:/pass"));
        assert_eq!(ftp.initial_path.as_deref(), Some("/var/www"));

        let sftp = result
            .servers
            .iter()
            .find(|s| s.name == "nas-sftp")
            .unwrap();
        assert_eq!(sftp.protocol.as_deref(), Some("sftp"));
        assert_eq!(sftp.host, "192.168.1.10");
        assert!(sftp.credential.is_none());
        assert_eq!(sftp.has_stored_credential, Some(false));
    }

    #[test]
    fn test_default_lftp_config_path_env_override() {
        // Build a temp HOME with a legacy ~/.lftp/bookmarks and verify the
        // chain resolves it, then verify XDG_DATA_HOME takes precedence.
        let base = std::env::temp_dir().join(format!(
            "aeroftp-lftp-pathtest-{}",
            crate::bridge_shared::uuid_v4()
        ));
        let home = base.join("home");
        let legacy_dir = home.join(".lftp");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("bookmarks"), b"x ftp://h/").unwrap();

        let xdg = base.join("xdg");
        let xdg_lftp = xdg.join("lftp");
        std::fs::create_dir_all(&xdg_lftp).unwrap();
        std::fs::write(xdg_lftp.join("bookmarks"), b"y ftp://h/").unwrap();

        let prev_home = std::env::var("HOME").ok();
        let prev_xdg = std::env::var("XDG_DATA_HOME").ok();

        std::env::remove_var("XDG_DATA_HOME");
        std::env::set_var("HOME", &home);
        #[cfg(unix)]
        {
            let p = default_lftp_config_path().expect("legacy path should resolve");
            assert_eq!(p, legacy_dir.join("bookmarks"));
        }

        std::env::set_var("XDG_DATA_HOME", &xdg);
        #[cfg(unix)]
        {
            let p = default_lftp_config_path().expect("xdg path should resolve");
            assert_eq!(p, xdg_lftp.join("bookmarks"));
        }

        // Restore environment.
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_url_encode_decode_roundtrip() {
        for s in &["plain", "a b@c", "s3cr@t:/pass", "unicode-\u{00e9}\u{00f1}", "%percent"] {
            let enc = url_encode(s);
            assert_eq!(&url_decode(&enc), s, "roundtrip failed for {}", s);
        }
    }
}
