//! Structural guard: no text built by the WebDAV, S3 and STS providers, the
//! local bridge probe or the CLI prints a URL that can carry a secret.
//!
//! A WebDAV URL can hold a credential in its path (MEGAcmd serves the account
//! below a random path token) or in its query, and an error or a log line
//! that names the full URL hands it to whoever reads the message. This test
//! reads the production code of those files and lists every formatting macro
//! (`format!`, `write!`, `tracing::info!`, ...) whose format string or
//! arguments name something URL-like. Each site must be in [`REVIEWED`] with
//! the reason it is safe: it prints only the origin, or what it prints can
//! carry no secret by construction. A new site, or an edit to the arguments
//! of a listed one, fails until it is reviewed and listed; a listed site that
//! no longer exists fails too, so the list stays the inventory.
//!
//! Errors from reqwest are covered apart: in WebDAV every one goes through
//! `describe_reqwest_error`, which drops the URL (`error_text_tests`); an S3
//! or STS request URL is the endpoint origin (no query, fragment or userinfo:
//! `S3Provider::new` refuses them), the bucket, the object key and the
//! operation's own query, since credentials travel in signed headers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/// The files scanned, by their path below `src/`, with their source.
const FILES: [(&str, &str); 5] = [
    ("providers/webdav.rs", include_str!("webdav.rs")),
    ("providers/s3.rs", include_str!("s3.rs")),
    ("providers/sts.rs", include_str!("sts.rs")),
    ("local_bridge.rs", include_str!("../local_bridge.rs")),
    ("bin/aeroftp_cli.rs", include_str!("../bin/aeroftp_cli.rs")),
];

/// Formatting macros whose output is text (an error, a log line, a string).
const MACROS: [&str; 13] = [
    "format",
    "format_args",
    "write",
    "writeln",
    "print",
    "println",
    "eprint",
    "eprintln",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
];

/// Identifier parts (split at `_` and `.`) that name a URL or a part of
/// one that can hold a path or a query. A bare host cannot, so `host` is not
/// one; `from` and `to` are, after `self.`, for [`InsecureRedirect`]-like
/// pairs.
///
/// [`InsecureRedirect`]: super::webdav
const URL_WORDS: [&str; 7] = [
    "url", "uri", "href", "location", "endpoint", "redirect", "urls",
];

/// True when `text` (format holes and arguments) names something URL-like.
fn names_a_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("self.from") || lower.contains("self.to") {
        return true;
    }
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric()))
        .any(|part| URL_WORDS.contains(&part))
}

/// A reviewed site: file, macro, format string, arguments (whitespace
/// collapsed), and why it carries no secret.
type Reviewed = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// URL builders: the value is a request URL or a request body, sent to the
/// server and never shown as text.
const BUILDS_A_REQUEST: &str = "builds a request URL or body, not text";
/// Prints the origin only (scheme, host, port), never a path, query or userinfo.
const ORIGIN_ONLY: &str = "prints the origin only";
/// An S3 request URL: endpoint origin without query, fragment or userinfo
/// (refused by `S3Provider::new`), bucket, key, operation query; credentials
/// are in signed headers.
const S3_URL: &str = "an S3 request URL, no secret by construction";
/// The S3 endpoint: no query, fragment or userinfo (refused by
/// `S3Provider::new`), and S3 authenticates by signed headers, so its path is
/// a gateway prefix and not a credential.
const S3_ENDPOINT: &str = "the S3 endpoint, no secret by construction";

const REVIEWED: &[Reviewed] = &[
    ("providers/webdav.rs", "format", "{}:{}", ", method, uri", "the Digest A2 input, hashed and never shown"),
    ("providers/webdav.rs", "format", "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm=\"MD5\"", ", username, challenge.realm, challenge.nonce, uri, response", BUILDS_A_REQUEST),
    ("providers/webdav.rs", "format", "{}/", ", self.from.path()", "compares two paths, not text"),
    ("providers/webdav.rs", "write", "the server at {} redirected to {}, which leaves HTTPS, and AeroFTP does not follow a redirect from HTTPS to HTTP. \\ This is a server misconfiguration, typically a TLS-terminating reverse proxy whose backend builds http:// redirects", ", self.from.origin().ascii_serialization(), self.to.origin().ascii_serialization()", ORIGIN_ONLY),
    ("providers/webdav.rs", "format", "http://{endpoint}", "", BUILDS_A_REQUEST),
    ("providers/webdav.rs", "warn", "[WEBDAV] TLS certificate verification DISABLED for {}: connection is vulnerable to MITM attacks", ", crate::local_bridge::url_origin(&config.url)", ORIGIN_ONLY),
    ("providers/webdav.rs", "format", "{}/{}", ", base, url_tail", BUILDS_A_REQUEST),
    ("providers/webdav.rs", "format", "{}/{}{}", ", base, Self::encode_path(path), url_tail", BUILDS_A_REQUEST),
    ("providers/webdav.rs", "debug", "[WebDAV] Digest request: {} to {}", ", method.as_str(), crate::local_bridge::url_origin(&url)", ORIGIN_ONLY),
    ("providers/webdav.rs", "debug", "[WebDAV] Parsing XML from {}", ", crate::local_bridge::url_origin(&self.config.url)", ORIGIN_ONLY),
    ("providers/webdav.rs", "format", "{}|{}|{}|{}", ", NEXTCLOUD_HANDLE_PREFIX, self.transfer_id, self.user_id, self.total_size", "the multipart handle AeroFTP encodes: prefix, random transfer id, user id, size"),
    ("providers/webdav.rs", "format", "{}@{}", ", self.config.username, self.config .url .replace(\"https://\", \"\") .replace(\"http://\", \"\") .split('/') .next() .unwrap_or(&self.config.url)", "the connection label: user name and the authority up to the first /"),
    ("providers/webdav.rs", "info", "[WEBDAV] Filen bridge reconciled to {}", ", crate::local_bridge::url_origin(live)", ORIGIN_ONLY),
    ("providers/webdav.rs", "info", "[MEGAcmd] bridge serves the account below its root at {}; connecting to the served path", ", crate::local_bridge::url_origin(&url)", ORIGIN_ONLY),
    ("providers/webdav.rs", "warn", "[WEBDAV] Connection uses unencrypted HTTP ({}). Credentials and data will be sent in plaintext.", ", crate::local_bridge::url_origin(&self.config.url)", ORIGIN_ONLY),
    ("providers/webdav.rs", "info", "[WebDAV] Detected single-file resource at {} ({}, {} bytes); enabling single-file mode", ", crate::local_bridge::url_origin(&self.config.url), entry.name, entry.size", ORIGIN_ONLY),
    ("providers/webdav.rs", "info", "[WEBDAV] download non-success: status={} origin={} path={} headers=[{}] body=\\\"{}\\\"", ", status, crate::local_bridge::url_origin(&self.config.url), remote_path, headers_dump, body_snippet", "origin only; the path is relative to the base URL; a Location header is reduced to its origin and Set-Cookie is redacted (diagnostic_header_value)"),
    ("providers/s3.rs", "info", "[S3] Filen bridge endpoint reconciled to {}", ", crate::local_bridge::url_origin(&fixed)", ORIGIN_ONLY),
    ("providers/s3.rs", "format", "{}.{}", ", config.bucket, endpoint_host", BUILDS_A_REQUEST),
    ("providers/s3.rs", "debug", "[S3] new(): endpoint={:?} bucket={} region={} path_style={} verify_cert={}", ", config.endpoint, config.bucket, config.region, config.path_style, config.verify_cert,", S3_ENDPOINT),
    ("providers/s3.rs", "debug", "[S3] new(): endpoint_is_local={} accept_invalid_certs={}", ", endpoint_is_local, accept_invalid_certs,", "two booleans"),
    ("providers/s3.rs", "format", "{}/{}/", ", endpoint, bucket", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "{}/{}/{}", ", endpoint, bucket, encoded_key", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "{}://{}.{}", ", scheme, self.config.bucket, endpoint_without_scheme", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "{}://{}.{}/{}", ", scheme, self.config.bucket, endpoint_without_scheme, encoded_key", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "{}.{}", ", self.config.bucket, endpoint_host", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "{}/?IncludeStats=true", ", self.endpoint()", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "https://{endpoint}", "", BUILDS_A_REQUEST),
    ("providers/s3.rs", "ProviderError::NetworkError", "", "error.without_url().to_string()", "the reqwest error with its URL dropped"),
    ("providers/s3.rs", "format", "{}?{}", ", url, query", BUILDS_A_REQUEST),
    ("providers/s3.rs", "debug", "[S3] {} {} (bucket={} region={} path_style={})", ", method, url, self.config.bucket, self.config.region, self.config.path_style", S3_URL),
    ("providers/s3.rs", "warn", "S3 Response Status: {} for {} {}", ", status, method, url", S3_URL),
    ("providers/s3.rs", "format", "{}/", ", self.endpoint().trim_end_matches('/')", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "S3 Storage: {} - Bucket: {}", ", endpoint, self.config.bucket", S3_ENDPOINT),
    ("providers/s3.rs", "format", "{}?{}&X-Amz-Signature={}", ", url, query_params, signature", "the presigned share link, returned to the user who asked for it, never logged"),
    ("providers/s3.rs", "format", "{}?delete", ", self.build_url(\"\")", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "https://{endpoint}", "", BUILDS_A_REQUEST),
    ("providers/s3.rs", "format", "https://{endpoint}", "", BUILDS_A_REQUEST),
    ("bin/aeroftp_cli.rs", "format", "Invalid URL or unknown profile in '{}'. Use --profile <name> for saved profiles, protocol://host/path for direct URLs (supported: {}), or a local folder as /abs/path, ./rel/path or file:///abs/path.", ", url, SUPPORTED_URL_SCHEMES.join(\", \")", "echoes the argument typed, only when it has no scheme (not a URL)"),
    ("bin/aeroftp_cli.rs", "format", "Unsupported protocol: {}. Supported: {}", ", scheme, SUPPORTED_URL_SCHEMES.join(\", \")", "the scheme only"),
    ("bin/aeroftp_cli.rs", "writeln", "{lead}\\n{url}", "", "the Proton sign-in page, shown only on an interactive terminal by design (show_proton_sign_in_page)"),
    ("bin/aeroftp_cli.rs", "format", "<D:response><D:href>{}</D:href><D:propstat><D:prop>{}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>", ", escape_html(href), props", "a WebDAV response body the CLI server sends"),
    ("bin/aeroftp_cli.rs", "format", "{}/", ", self_href.trim_end_matches('/')", "a WebDAV response body the CLI server sends"),
    ("bin/aeroftp_cli.rs", "println", "{}", ", serde_json::json!({ \"status\": \"serving\", \"protocol\": \"ftp\", \"addr\": endpoint.addr, \"base_path\": base_path, \"auth_required\": auth_credentials.is_some(), \"auth_user\": auth_credentials.as_ref().map(|c| c.username.clone()), \"generated_auth_password\": auth_credentials .as_ref() .and_then(|c| if c.generated { Some(c.password.clone()) } else { None }), })", "the local address the CLI serves on"),
    ("bin/aeroftp_cli.rs", "eprintln", "Serving FTP on ftp://{}", ", endpoint.addr", "the local address the CLI serves on"),
    ("bin/aeroftp_cli.rs", "println", "{}", ", serde_json::json!({ \"status\": \"serving\", \"protocol\": \"sftp\", \"addr\": endpoint.addr, \"base_path\": base_path, \"auth_required\": auth_credentials.is_some(), \"auth_user\": auth_credentials.as_ref().map(|c| c.username.clone()), \"generated_auth_password\": auth_credentials .as_ref() .and_then(|c| if c.generated { Some(c.password.clone()) } else { None }), })", "the local address the CLI serves on"),
    ("bin/aeroftp_cli.rs", "eprintln", "Serving SFTP on sftp://{}", ", endpoint.addr", "the local address the CLI serves on"),
    ("bin/aeroftp_cli.rs", "println", "{}", ", result.url", "the share link the user asked for, printed as the command result"),
    ("bin/aeroftp_cli.rs", "format", "GET {}: {}", ", url, e", "the share link just created and printed as the result anyway"),
    ("bin/aeroftp_cli.rs", "writeln", "{} - {}://{}@{}:{}{}{}", ", sanitize_filename(&s.name), sanitize_filename(proto), sanitize_filename(&s.username), sanitize_filename(&s.host), s.port, cred, sanitize_filename(s.cleartext_endpoint_note())", "a saved profile as user@host:port, no path; the credential column is a note, not the secret"),
    ("bin/aeroftp_cli.rs", "println", "{} - {}://{}@{}:{}{}{}", ", s.name, proto, s.username, s.host, s.port, cred, s.cleartext_endpoint_note()", "a saved profile as user@host:port, no path; the credential column is a note, not the secret"),
    ("bin/aeroftp_cli.rs", "println", "{} - {}://{}@{}:{}{}{}", ", s.name, proto, s.username, s.host, s.port, cred, s.cleartext_endpoint_note()", "a saved profile as user@host:port, no path; the credential column is a note, not the secret"),
    ("bin/aeroftp_cli.rs", "format", "Connect failed for {}", ", redact_url_for_display(url)", "the URL the user typed, password removed, to that user's terminal"),
    ("bin/aeroftp_cli.rs", "format", "{},{},{},{},{:.2},{:.2},{},{},{},{},{:.1},{}\\n", ", e.rank, csv_cell_safe(&e.url), csv_cell_safe(&r.protocol), r.test_size, r.upload_mbps, r.download_mbps, r.download_ttfb_ms .map(|v| v.to_string()) .unwrap_or_default(), integrity, r.cleanup_ok as u8, r.trash_purged as u8, e.score * 100.0, \"\",", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "format", ",{},,,,,,,,,,{}\\n", ", csv_cell_safe(&e.url), csv_cell_safe(e.error.as_deref().unwrap_or(\"\")),", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "format", "| {} | {} | {} | {:.2} | {:.2} | {} | {} | {} | {} | {:.0} |\\n", ", e.rank, md_cell_safe(&e.url), md_cell_safe(&r.protocol.to_uppercase()), r.download_mbps, r.upload_mbps, r.download_ttfb_ms .map(|v| v.to_string()) .unwrap_or_else(|| \"-\".into()), integ, if r.cleanup_ok { \"✓\" } else { \"✗\" }, purged, e.score * 100.0,", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "format", "|: | {} |: |: |: |: |: |: |: | error: {} |\\n", ", md_cell_safe(&e.url), md_cell_safe(e.error.as_deref().unwrap_or(\"\")),", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "println", "{:>3} {:<48} {:<7} {:>12} {:>12} {:>8} {:>5}", ", \"#\", \"URL\", \"PROTO\", \"DOWN Mbps\", \"UP Mbps\", \"TTFB ms\", \"SCORE\"", "a column title"),
    ("bin/aeroftp_cli.rs", "format", "{}...", ", &e.url[..e.url.floor_char_boundary(45)]", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "println", "{:>3} {:<48} {:<7} {:>12.2} {:>12.2} {:>8} {:>5.0}", ", e.rank, url_disp, r.protocol.to_uppercase(), r.download_mbps, r.upload_mbps, r.download_ttfb_ms .map(|v| v.to_string()) .unwrap_or_else(|| \"-\".into()), e.score * 100.0,", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "println", "ERR {} -> {}", ", e.url, e.error.clone().unwrap_or_default()", "a URL the user passed to speed compare, to that user's output"),
    ("bin/aeroftp_cli.rs", "eprintln", "Mapping drive: net use {} {}", ", drive, webdav_url", "the local address the CLI serves on"),
    ("bin/aeroftp_cli.rs", "eprintln", "Watching {} -> {} (direction: {}, cooldown: {}s, rescan: {}s)", ", local, if url == \"_\" { \"(profile)\" } else { url }, direction, watch_cooldown, watch_rescan,", "the URL argument the user typed, to that user's terminal"),
    ("bin/aeroftp_cli.rs", "format", "Token limit exceeded: {} > {} (input {}, output {}, cache read {}, cache written {})", ", self.total_tokens, limit, self.input_tokens, self.output_tokens, self.cache_read_tokens, self.cache_write_tokens,", "token counters, not a URL"),
    ("bin/aeroftp_cli.rs", "eprintln", "\\x1b[1;37mAeroFTP\\x1b[0m {g}v{}{r} {b}|{r} {} protocols, {} native integrations, {} via direct URL {b}|{r} pget {b}|{r} mcp {b}|{r} ai agent {b}|{r} vault profiles", ", env!(\"CARGO_PKG_VERSION\"), protocols, integrations, SUPPORTED_URL_SCHEMES.len()", "the banner, not a URL"),
    ("bin/aeroftp_cli.rs", "eprintln", "AeroFTP v{} | {} protocols, {} native integrations, {} via direct URL | pget | mcp | ai agent | vault profiles", ", env!(\"CARGO_PKG_VERSION\"), protocols, integrations, SUPPORTED_URL_SCHEMES.len()", "the banner, not a URL"),
];

/// Byte spans of `source` that are test code: an item under `#[cfg(test)]`.
fn test_spans(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(found) = source[from..].find("#[cfg(test)]") {
        let start = from + found;
        let mut i = start + "#[cfg(test)]".len();
        let mut end = None;
        while i < bytes.len() {
            match bytes[i] {
                b';' => {
                    end = Some(i + 1);
                    break;
                }
                b'{' => {
                    end = Some(matching(bytes, i, b'{', b'}'));
                    break;
                }
                _ => i = skip(bytes, i),
            }
        }
        let end = end.unwrap_or(bytes.len());
        spans.push((start, end));
        from = end;
    }
    spans
}

/// The index after the literal or comment starting at `i`, or `i + 1`.
fn skip(bytes: &[u8], i: usize) -> usize {
    match bytes[i] {
        b'/' if bytes.get(i + 1) == Some(&b'/') => {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            j
        }
        b'/' if bytes.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            j + 2
        }
        b'r' if matches!(bytes.get(i + 1), Some(b'#') | Some(b'"'))
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) =>
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) != Some(&b'"') {
                return i + 1;
            }
            j += 1;
            loop {
                if j >= bytes.len() {
                    return j;
                }
                if bytes[j] == b'"'
                    && bytes[j + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&b| b == b'#')
                        .count()
                        == hashes
                {
                    return j + 1 + hashes;
                }
                j += 1;
            }
        }
        b'"' => {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            j + 1
        }
        b'\'' => {
            // A char literal ('x', '\n', '\''); otherwise a lifetime.
            if bytes.get(i + 1) == Some(&b'\\') {
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'\'' {
                    j += 1;
                }
                j + 1
            } else if let Some(c) = source_char_len(bytes, i + 1) {
                if bytes.get(i + 1 + c) == Some(&b'\'') {
                    i + 2 + c
                } else {
                    i + 1
                }
            } else {
                i + 1
            }
        }
        _ => i + 1,
    }
}

/// The UTF-8 length of the character at `i`.
fn source_char_len(bytes: &[u8], i: usize) -> Option<usize> {
    let b = *bytes.get(i)?;
    Some(match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    })
}

/// The index after the bracket matching the one at `open_at`.
fn matching(bytes: &[u8], open_at: usize, open: u8, close: u8) -> usize {
    let mut depth = 0usize;
    let mut i = open_at;
    while i < bytes.len() {
        if bytes[i] == open {
            depth += 1;
            i += 1;
        } else if bytes[i] == close {
            depth -= 1;
            i += 1;
            if depth == 0 {
                return i;
            }
        } else {
            i = skip(bytes, i);
        }
    }
    bytes.len()
}

/// The first string literal of a macro body and the text after it.
fn split_format(body: &str) -> Option<(String, String)> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"'
            || (bytes[i] == b'r' && matches!(bytes.get(i + 1), Some(b'#') | Some(b'"')))
        {
            let end = skip(bytes, i);
            if body[..i].trim_end().ends_with("target:") {
                // `tracing::info!(target: "name", "format", ...)`.
                i = end;
                continue;
            }
            let literal = &body[i..end];
            let open = literal.find('"')?;
            let close = literal.rfind('"')?;
            return Some((
                literal[open + 1..close].to_string(),
                body[end..].to_string(),
            ));
        }
        i = skip(bytes, i);
    }
    None
}

/// Every formatting site in the production code of `source` that names
/// something URL-like: (macro, format string, arguments).
fn url_sites(source: &str) -> Vec<(String, String, String)> {
    let bytes = source.as_bytes();
    let tests = test_spans(source);
    let mut sites = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(&(_, end)) = tests.iter().find(|(start, _)| *start == i) {
            i = end;
            continue;
        }
        let after_ident = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let raw_string = bytes[i] == b'r' && matches!(bytes.get(i + 1), Some(b'#') | Some(b'"'));
        let is_ident_start = (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') && !after_ident;
        if !is_ident_start || raw_string {
            i = skip(bytes, i);
            continue;
        }
        let mut j = i;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        let name = &source[i..j];
        if bytes.get(j) == Some(&b'!') && bytes.get(j + 1) == Some(&b'(') && MACROS.contains(&name)
        {
            let end = matching(bytes, j + 1, b'(', b')');
            let body = &source[j + 2..end - 1];
            if let Some((format, args)) = split_format(body) {
                let args = args.split_whitespace().collect::<Vec<_>>().join(" ");
                let holes: String = format
                    .split('{')
                    .skip(1)
                    .filter_map(|hole| hole.split('}').next())
                    .collect::<Vec<_>>()
                    .join(" ");
                if names_a_url(&format!("{holes} {args}")) {
                    let format = format.split_whitespace().collect::<Vec<_>>().join(" ");
                    sites.push((name.to_string(), format, args));
                }
            }
            i = end;
            continue;
        }
        // A `ProviderError::Variant(...)` built from a value rather than
        // through `format!` (scanned above): its text is that value.
        if name == "ProviderError" && source[j..].starts_with("::") {
            let mut k = j + 2;
            while k < bytes.len() && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_') {
                k += 1;
            }
            if bytes.get(k) == Some(&b'(') {
                let end = matching(bytes, k, b'(', b')');
                let body = source[k + 1..end - 1]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !body.starts_with("format!(") && !body.starts_with('"') && names_a_url(&body) {
                    sites.push((source[i..k].to_string(), String::new(), body));
                }
                i = k + 1;
                continue;
            }
        }
        i = j;
    }
    sites
}

#[test]
fn every_text_that_names_a_url_is_reviewed() {
    let mut unreviewed = Vec::new();
    let mut found = Vec::new();
    for (file, source) in FILES {
        for (name, format, args) in url_sites(source) {
            let listed = REVIEWED
                .iter()
                .any(|(f, m, fmt, a, _)| *f == file && *m == name && *fmt == format && *a == args);
            if !listed {
                unreviewed.push(format!(
                    "({file:?}, {name:?}, {format:?}, {args:?}, <reason>),"
                ));
            }
            found.push((file, name, format, args));
        }
    }
    let stale: Vec<_> = REVIEWED
        .iter()
        .filter(|(f, m, fmt, a, _)| {
            !found.iter().any(|(file, name, format, args)| {
                file == f && name == m && format == fmt && args == a
            })
        })
        .map(|(f, m, fmt, _, _)| format!("{f} {m}!({fmt:?})"))
        .collect();
    assert!(
        unreviewed.is_empty(),
        "these sites format something URL-like and are not reviewed: make each print only the \
         origin (crate::local_bridge::url_origin) or show why it carries no secret, then list it:\n{}",
        unreviewed.join("\n")
    );
    assert!(
        stale.is_empty(),
        "these reviewed sites no longer exist:\n{}",
        stale.join("\n")
    );
}

/// The scanner sees a site in production code, skips test code, string and
/// comment contents, and matches a URL word in a hole or an argument.
#[test]
fn the_scanner_finds_url_sites_and_only_in_production_code() {
    let source = r##"
fn a() {
    let _ = format!("{}", config.url);
    let _ = format!("{url}");
    let _ = format!("plain {}", count);
    // format!("{}", commented.url);
    let _ = "format!(\"{}\", quoted.url)";
    tracing::warn!("x {}", "y");
}
#[cfg(test)]
mod tests {
    fn b() { let _ = format!("{}", test_only.url); }
}
"##;
    let sites = url_sites(source);
    assert_eq!(
        sites,
        vec![
            (
                "format".to_string(),
                "{}".to_string(),
                ", config.url".to_string()
            ),
            ("format".to_string(), "{url}".to_string(), String::new()),
        ]
    );
}
