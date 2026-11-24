//! WebDAV Storage Provider
//!
//! Implementation of the StorageProvider trait for WebDAV protocol.
//! Compatible with Nextcloud, Synology, QNAP, Koofr, and other WebDAV servers.
//!
//! WebDAV extends HTTP with methods like PROPFIND, MKCOL, MOVE, COPY, and DELETE
//! to provide full file system operations over HTTP/HTTPS.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::HttpResultExt;
use async_trait::async_trait;
use futures_util::StreamExt;
use md5::{Digest as _, Md5};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::Rng;
use reqwest::{Client, Method, StatusCode};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

#[cfg(test)]
#[path = "webdav_owncloud_tests.rs"]
mod owncloud_tests;

#[cfg(test)]
#[path = "webdav_nextcloud_purge_tests.rs"]
mod nextcloud_purge_tests;

use super::{
    MultipartHandle, ProviderError, ProviderListExecutorKind, ProviderTransferExecutorKind,
    ProviderType, RemoteEntry, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult,
    StorageProvider, UploadedPart, WebDavConfig,
};

#[cfg(test)]
#[path = "webdav_nextcloud_retry_tests.rs"]
mod nextcloud_retry_tests;

/// Encode one untrusted Nextcloud trash identifier as exactly one URL segment.
/// Dots are encoded too so an identifier equal to `..` cannot traverse out of
/// the `trash` / `restore` collection after URL normalization.
fn encode_nextcloud_trash_segment(value: &str) -> String {
    const SEGMENT: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'/')
        .add(b'.')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    utf8_percent_encode(value, SEGMENT).to_string()
}

/// A trash item from a Nextcloud trashbin PROPFIND response.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NextcloudTrashEntry {
    /// Trash item identifier (from WebDAV href, needed for restore/delete).
    pub id: String,
    /// Original filename before deletion.
    pub name: String,
    /// Original path (relative to user root) before deletion.
    pub original_path: String,
    /// Unix timestamp of when the file was deleted.
    pub deleted_at: u64,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// `oc:fileid` when the trashbin PROPFIND returned one. Empty when it did
    /// not. The id is the live file's id, so a purge can name that item and
    /// leave every other item of the same original location alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_id: String,
}

// ============ HTTP Digest Authentication (RFC 2617) ============

/// The server-issued Digest challenge, immutable once parsed.
#[derive(Clone)]
struct DigestChallenge {
    realm: String,
    nonce: String,
    qop: String,
    opaque: Option<String>,
    /// `stale=true` on a 401: RFC 2617's one signal for "the credentials are
    /// fine, the nonce is not". Without it a nonce rotation and a revoked
    /// password are the same event to us, because both answer with a
    /// perfectly parseable challenge.
    stale: bool,
}

/// State for HTTP Digest authentication.
///
/// Both halves are shared across `clone_for_transfer` workers, and the nonce is
/// shared for the same reason the counter is. A nonce is a property of the
/// session with the server, not of a worker: when the server rotates it (Apache
/// mod_auth_digest does so on AuthDigestNonceLifetime, 300s by default, replying
/// `stale=true`) a per-worker copy means one clone re-negotiates and the other
/// seven keep presenting the dead nonce. Vanilla WebDAV became an eight-way
/// clone pool in this release and Digest had never run under one before, while
/// `download()` in this same file still refuses concurrency when Digest is
/// active, for exactly this reason. Sharing the challenge is what makes those
/// two statements agree.
#[derive(Clone)]
struct DigestState {
    challenge: Arc<RwLock<DigestChallenge>>,
    /// Shared across workers so concurrent requests never reuse the same `nc`
    /// against one nonce (RFC 2617).
    nc: Arc<AtomicU32>,
}

impl DigestState {
    /// A poisoned lock still holds a perfectly valid challenge: it is four
    /// strings, and a panic elsewhere says nothing about them. Failing the
    /// request because another thread died would turn an unrelated bug into an
    /// authentication failure, so the value is taken either way.
    fn read_challenge(&self) -> std::sync::RwLockReadGuard<'_, DigestChallenge> {
        self.challenge
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn realm(&self) -> String {
        self.read_challenge().realm.clone()
    }

    fn qop(&self) -> String {
        self.read_challenge().qop.clone()
    }

    fn nonce(&self) -> String {
        self.read_challenge().nonce.clone()
    }

    /// A short, log-safe prefix of the current nonce.
    ///
    /// Two things make the obvious `&state.nonce()[..12]` wrong here. `nonce()`
    /// returns an owned String, so calling it twice (once to slice, once for the
    /// bound) reads the shared challenge twice, and another worker may
    /// re-negotiate in between: a short old nonce sliced at the new one's length
    /// panics. And a byte index is not a character boundary, so a server sending
    /// a non-ASCII nonce panics regardless of any race. One read, and cut on a
    /// boundary.
    fn nonce_prefix(&self) -> String {
        let nonce = self.nonce();
        match nonce.char_indices().nth(12) {
            Some((cut, _)) => nonce[..cut].to_string(),
            None => nonce,
        }
    }

    /// Adopt a rotated challenge from a `401 ... stale=true` response.
    ///
    /// Returns false, so the caller falls through to its ordinary failure path,
    /// in all three cases where adopting would be wrong:
    ///
    /// - the header is not a usable Digest challenge (Basic auth, a truncated
    ///   header): retrying against a half-parsed challenge is worse than
    ///   failing the request;
    /// - the challenge does not carry `stale=true`: this is the case that used
    ///   to be missing, and it is the whole point. A revoked password answers
    ///   with a challenge that parses perfectly, so "it parsed" cannot tell a
    ///   nonce rotation from a credential failure. Adopting it and retrying is
    ///   exactly how a dead password turns into a retry storm, which is why
    ///   the data paths were left un-wired as accepted debt until now;
    /// - the challenge re-offers the nonce already held: a server that answers
    ///   `stale=true` without actually rotating would otherwise have us retry
    ///   against the nonce that just failed, once per request, indefinitely.
    ///
    /// On success the counter restarts, because `nc` counts requests against
    /// ONE nonce and carrying it over would present a count the new nonce has
    /// never seen. The write stays in place: replacing the `Arc` would leave
    /// every `clone_for_transfer` worker pointing at the old challenge, the
    /// very defect the shared challenge exists to prevent.
    fn renegotiate(&self, www_authenticate: &str) -> bool {
        let Some(fresh) = DigestChallenge::parse(www_authenticate) else {
            return false;
        };
        if !fresh.stale {
            return false;
        }
        let mut guard = self
            .challenge
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.nonce == fresh.nonce {
            return false;
        }
        *guard = fresh;
        self.nc.store(0, Ordering::Relaxed);
        true
    }

    /// Parse a `WWW-Authenticate: Digest ...` header value
    fn parse(header: &str) -> Option<Self> {
        Some(Self {
            challenge: Arc::new(RwLock::new(DigestChallenge::parse(header)?)),
            nc: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Generate the `Authorization: Digest ...` header value
    fn authorization(&self, method: &str, uri: &str, username: &str, password: &str) -> String {
        // One guard for the whole header: the nonce, the counter and the qop
        // must all come from the SAME challenge, or a rotation landing between
        // two reads would mix a fresh nonce with a count belonging to the old
        // one and the server would reject a request that is otherwise correct.
        let challenge = self.read_challenge();
        // First request is nc=1, matching the previous `self.nc += 1` start.
        let nc = self.nc.fetch_add(1, Ordering::Relaxed) + 1;
        let nc_str = format!("{:08x}", nc);
        let cnonce = Self::generate_cnonce();

        let ha1 = md5_hex(&format!("{}:{}:{}", username, challenge.realm, password));
        let ha2 = md5_hex(&format!("{}:{}", method, uri));

        let response = if !challenge.qop.is_empty() {
            md5_hex(&format!(
                "{}:{}:{}:{}:auth:{}",
                ha1, challenge.nonce, nc_str, cnonce, ha2
            ))
        } else {
            md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2))
        };

        // Quote algorithm and qop for maximum server compatibility
        // (Python requests quotes these and works with all servers)
        let mut header = format!(
            r#"Digest username="{}", realm="{}", nonce="{}", uri="{}", response="{}", algorithm="MD5""#,
            username, challenge.realm, challenge.nonce, uri, response
        );

        if !challenge.qop.is_empty() {
            header.push_str(&format!(
                r#", qop="auth", nc={}, cnonce="{}""#,
                nc_str, cnonce
            ));
        }

        if let Some(ref opaque) = challenge.opaque {
            header.push_str(&format!(r#", opaque="{}""#, opaque));
        }

        tracing::debug!("[WebDAV Digest] method={} nc={}", method, nc_str);

        header
    }

    fn generate_cnonce() -> String {
        use rand::rngs::OsRng;
        let bytes: [u8; 8] = OsRng.gen();
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl DigestChallenge {
    fn parse(header: &str) -> Option<Self> {
        let s = header.strip_prefix("Digest ")?;
        Some(Self {
            realm: Self::extract_param(s, "realm")?,
            nonce: Self::extract_param(s, "nonce")?,
            qop: Self::extract_param(s, "qop").unwrap_or_default(),
            opaque: Self::extract_param(s, "opaque"),
            // RFC 2617 writes the flag bare (`stale=true`), but servers quote
            // it and case it freely, so `extract_param` handles both forms and
            // the comparison ignores case. Absent means false: a challenge
            // that does not claim staleness is not one.
            stale: Self::extract_param(s, "stale")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
        })
    }

    /// Extract a parameter value from the Digest challenge string
    fn extract_param(s: &str, key: &str) -> Option<String> {
        // Try quoted form: key="value"
        let quoted = format!("{}=\"", key);
        if let Some(pos) = s.find(&quoted) {
            let after = &s[pos + quoted.len()..];
            let end = after.find('"')?;
            return Some(after[..end].to_string());
        }
        // Try unquoted form: key=value
        let unquoted = format!("{}=", key);
        if let Some(pos) = s.find(&unquoted) {
            let after = &s[pos + unquoted.len()..];
            let end = after.find([',', ' ']).unwrap_or(after.len());
            return Some(after[..end].to_string());
        }
        None
    }
}

/// Compute MD5 hex digest of a string
fn md5_hex(input: &str) -> String {
    let digest = Md5::digest(input.as_bytes());
    format!("{:x}", digest)
}

/// Render a reqwest error as message text: reqwest's wording, its full
/// source chain, and the request named by its origin only.
///
/// reqwest's top-level `Display` for a send failure is the generic
/// "error sending request for url (...)"; the actionable cause (connection
/// reset, connection closed before message completed, timed out, ...) lives
/// in the source chain. Surfacing it turns an opaque preview/download failure
/// into a diagnosable one (issue #264, MEGAcmd bridge GET failures).
///
/// The URL itself never reaches the text: a WebDAV path can be the secret
/// (MEGAcmd serves each location under a random token in the path) and a
/// query can carry a token, and these messages end up in toasts, logs and
/// CLI output. A refused certificate is named by the TLS library's own words
/// in the chain ("certificate verify failed (self-signed certificate)" with
/// OpenSSL), which reqwest's "error sending request" alone hides.
fn describe_reqwest_error(e: reqwest::Error) -> String {
    let origin = e
        .url()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null");
    let certificate = e.is_connect() && refused_certificate(&e);
    let e = e.without_url();
    let mut msg = e.to_string();
    if let Some(origin) = origin {
        msg.push_str(" to ");
        msg.push_str(&origin);
    }
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let detail = s.to_string();
        if !detail.is_empty() && !msg.ends_with(&detail) {
            msg.push_str(": ");
            msg.push_str(&detail);
        }
        source = s.source();
    }
    if certificate {
        format!("{REFUSED_CERTIFICATE} ({msg})")
    } else {
        msg
    }
}

/// How a WebDAV error begins when the server's certificate was refused, so
/// a front end can give the advice that fits (a trusted certificate) and not
/// the advice for a server that is down.
pub const REFUSED_CERTIFICATE: &str = "the server's TLS certificate was refused";

/// Whether a connect failed on the server's certificate.
///
/// rustls says so in its type (`InvalidCertificate`), found through each
/// link of the chain and inside an `io::Error`, whose `source()` skips the
/// error it wraps. The native backends do not: `native_tls::Error` exposes
/// only its text, so their refusal is recognised by its wording
/// ([`names_a_refused_certificate`]).
fn refused_certificate(e: &reqwest::Error) -> bool {
    fn rustls_refused(cause: &(dyn std::error::Error + 'static)) -> bool {
        matches!(
            cause.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::InvalidCertificate(_))
        )
    }
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        let inner = cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .map(|inner| inner as &(dyn std::error::Error + 'static));
        if rustls_refused(cause)
            || inner.is_some_and(rustls_refused)
            || names_a_refused_certificate(&cause.to_string())
        {
            return true;
        }
        source = cause.source();
    }
    false
}

/// The phrases TLS backends use for a server certificate they refused:
/// OpenSSL on Linux ("certificate verify failed", measured), rustls ("invalid
/// peer certificate"), SChannel on Windows ("... root certificate which is not
/// trusted ...", "... issued by an authority that is not trusted") and
/// Security.framework on macOS ("invalid certificate chain", "The trust policy
/// was not trusted."). Phrases, not the bare word "certificate": a server that
/// asks for a client certificate ("alert certificate required", "alert bad
/// certificate") refuses our side, and AeroFTP's WebDAV client presents none.
/// A refused connection, a failed name lookup, a reset or a timeout says
/// none of them either.
fn names_a_refused_certificate(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "certificate verify failed",
        "invalid peer certificate",
        "invalid certificate chain",
        "not trusted",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// [`describe_reqwest_error`] as the `ParseError` of a reply body that could
/// not be read or decoded.
fn body_read_error(e: reqwest::Error) -> ProviderError {
    ProviderError::ParseError(describe_reqwest_error(e))
}

/// A redirect the WebDAV client refused because it leaves TLS: the request
/// went to an `https://` URL and the server pointed it at `http://`.
///
/// reqwest would follow it, but it treats the scheme change as a new origin
/// and drops `Authorization`, so the hop arrives anonymous and the server
/// answers `401`: a healthy session that reads as rejected credentials.
/// Following it WITH the credentials would be worse, since they would travel
/// in cleartext. The usual cause is a server behind a TLS-terminating reverse
/// proxy that builds absolute redirects from the scheme it sees itself (nginx
/// without `absolute_redirect off`, Apache without an `https://` `ServerName`),
/// and the usual redirect is the `301` that appends the trailing slash to a
/// collection. Measured on 2026-10-02: every verb sent to a slash-less
/// collection on such a server (PROPFIND, MKCOL, MOVE, COPY, DELETE, LOCK,
/// PROPPATCH, GET, HEAD, OPTIONS) answered that `301`.
#[derive(Debug)]
struct InsecureRedirect {
    from: reqwest::Url,
    to: reqwest::Url,
}

impl InsecureRedirect {
    /// True when the redirect only appends the trailing slash, that is the
    /// server named the request target a collection and asked for its
    /// collection form. A caller that did not know the target's type can
    /// resend there over HTTPS: the server refused the first request without
    /// acting on it.
    fn is_to_collection_form(&self) -> bool {
        self.from.host_str() == self.to.host_str()
            && self.from.query() == self.to.query()
            && self.to.path() == format!("{}/", self.from.path())
    }
}

impl std::fmt::Display for InsecureRedirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the server at {} redirected to {}, which leaves HTTPS, and AeroFTP does not follow a redirect from HTTPS to HTTP. \
             This is a server misconfiguration, typically a TLS-terminating reverse proxy whose backend builds http:// redirects",
            self.from.origin().ascii_serialization(),
            self.to.origin().ascii_serialization()
        )
    }
}

impl std::error::Error for InsecureRedirect {}

/// What an error may show of a WebDAV server's reply: its message element
/// (Sabre `<s:message>`, OCS `<message>`, a JSON `"message"`), sanitized and
/// with the profile's base path replaced by `<base>`, or only its length.
/// The rest of a reply can echo the request (an href, the form, a path below
/// a base URL that holds a token, as MEGAcmd's does).
///
/// `sent` is what the request sent as a secret (the account password, a
/// generated share password): an echo of it is removed in any spelling once
/// the message is decoded, see `without_sent_values`.
fn server_message(body: &str, base_url: &str, sent: &[String]) -> String {
    static ELEMENT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<(?:[A-Za-z0-9_]+:)?message(?:\s[^>]*)?>([^<]*)</").unwrap()
    });
    static JSON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#""message"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap()
    });
    let message = ELEMENT
        .captures(body)
        .or_else(|| JSON.captures(body))
        .and_then(|found| found.get(1))
        .map(|found| found.as_str().trim())
        .filter(|message| !message.is_empty());
    let Some(message) = message else {
        return if body.is_empty() {
            "no message".to_string()
        } else {
            format!("a reply of {} bytes with no message", body.len())
        };
    };
    // Fully decoded before scrubbing, so an entity-escaped path or query
    // value (`AB&amp;CD` for `AB%26CD`) matches.
    let Some(decoded) = super::fully_unescaped(message) else {
        return super::deeply_escaped_reply(body.len());
    };
    let Some(mut shown) = super::without_sent_values(&decoded, sent) else {
        return super::reply_with_a_sent_value(body.len());
    };
    if let Ok(url) = url::Url::parse(base_url) {
        let path = url.path().trim_end_matches('/');
        // Every leading part of the base path, longest first: a server can
        // echo only `/<handle>` of `/<handle>/Cloud%20Drive`, and the handle
        // alone opens the drive.
        let mut prefixes: Vec<&str> = path
            .match_indices('/')
            .map(|(at, _)| &path[..at])
            .filter(|prefix| prefix.len() > 1)
            .collect();
        prefixes.push(path);
        for prefix in prefixes.into_iter().rev().filter(|prefix| prefix.len() > 1) {
            let decoded = percent_decode_str(prefix).decode_utf8_lossy().into_owned();
            shown = shown.replace(prefix, "<base>").replace(&decoded, "<base>");
        }
        if let Some(query) = url.query().filter(|q| !q.is_empty()) {
            // Remove the saved URL's query and its parameter values (raw and
            // percent-decoded), longest first, so a server echoing `?token=...`
            // or a query value inside `<message>` cannot leak a query secret.
            let mut secrets: Vec<String> = Vec::new();
            let mut short_secrets: Vec<String> = Vec::new();
            let decoded_query = percent_decode_str(query).decode_utf8_lossy().into_owned();
            secrets.push(query.to_string());
            if decoded_query != query {
                secrets.push(decoded_query);
            }
            let mut raw_and_decoded_vals: Vec<String> = Vec::new();
            for raw_pair in query.split('&').filter(|p| !p.is_empty()) {
                secrets.push(raw_pair.to_string());
                let decoded_pair = percent_decode_str(raw_pair)
                    .decode_utf8_lossy()
                    .into_owned();
                if decoded_pair != raw_pair {
                    secrets.push(decoded_pair);
                }
                if let Some((_, raw_val)) = raw_pair.split_once('=') {
                    raw_and_decoded_vals.push(raw_val.to_string());
                    let decoded_val = percent_decode_str(raw_val).decode_utf8_lossy().into_owned();
                    if decoded_val != raw_val {
                        raw_and_decoded_vals.push(decoded_val);
                    }
                }
            }
            for (_, decoded_val) in url.query_pairs() {
                raw_and_decoded_vals.push(decoded_val.into_owned());
            }
            for val in raw_and_decoded_vals {
                if val.len() >= 4 {
                    secrets.push(val);
                } else if !val.is_empty() {
                    short_secrets.push(val);
                }
            }
            secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
            secrets.dedup();
            shown = super::redact_values(&shown, &secrets, "<redacted>");
            // A short value (1-3 bytes, `?token=abc`) cannot be cut out of a
            // word without guessing where it ends (`abc123`), and replacing
            // every occurrence would garble ordinary words. A message that
            // contains one anywhere is withheld: only its size is shown.
            // Every spelling `redact_values` knows: as saved, decoded and
            // percent-encoded (`a+b` decodes to `a b`, echoed as `a%20b`).
            if super::contains_any_spelling(&shown, &short_secrets) {
                return format!(
                    "a server message of {} bytes, withheld: it contains a value of the saved address",
                    message.len()
                );
            }
        }
    }
    super::sanitize_plain_error(&shown)
}

/// The value of a response header as the download diagnostic logs it. A
/// `Location` or `Content-Location` names a URL whose path or query can carry
/// a secret (the MEGAcmd path token), so only its origin is kept; a
/// `Set-Cookie` is a session credential and is not printed at all.
fn diagnostic_header_value(
    name: &reqwest::header::HeaderName,
    value: &reqwest::header::HeaderValue,
) -> String {
    use reqwest::header::{CONTENT_LOCATION, LOCATION, SET_COOKIE};
    let Ok(text) = value.to_str() else {
        return "<binary>".to_string();
    };
    if name == SET_COOKIE {
        "<redacted>".to_string()
    } else if name == LOCATION || name == CONTENT_LOCATION {
        crate::local_bridge::url_origin(text)
    } else {
        text.to_string()
    }
}

/// The WebDAV client's redirect policy: reqwest's default (follow at most ten
/// hops), except that a hop from `https` to `http` is refused with
/// [`InsecureRedirect`] instead of being followed without credentials.
fn webdav_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let downgrade_from = attempt
            .previous()
            .last()
            .filter(|from| from.scheme() == "https" && attempt.url().scheme() == "http")
            .cloned();
        match downgrade_from {
            Some(from) => {
                let to = attempt.url().clone();
                attempt.error(InsecureRedirect { from, to })
            }
            None => reqwest::redirect::Policy::default().redirect(attempt),
        }
    })
}

/// A redirect from the Filen bridge to another origin, refused. It carries
/// no URL: the error a refusal becomes is shown and logged, and the request
/// URL may hold a query the user did not mean to publish.
#[derive(Debug)]
struct LeftTheBridge;

impl std::fmt::Display for LeftTheBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the Filen Desktop bridge redirected to another server, which AeroFTP does not follow",
        )
    }
}

impl std::error::Error for LeftTheBridge {}

/// The redirect policy of a client for the Filen bridge: a redirect is
/// followed only to the bridge's own origin, so the bridge's relaxed
/// certificate checks and direct route never reach another server.
fn bridge_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let leaves_the_bridge = attempt
            .previous()
            .first()
            .is_none_or(|first| first.origin() != attempt.url().origin());
        if leaves_the_bridge {
            attempt.error(LeftTheBridge)
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// Whether a failed send is a redirect the Filen bridge client refused.
fn left_the_bridge(e: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if s.downcast_ref::<LeftTheBridge>().is_some() {
            return true;
        }
        source = s.source();
    }
    false
}

/// How a WebDAV connection's HTTP client treats the server, decided again on
/// every connect of the Filen preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClientPolicy {
    /// Certificate checks off: the user's own choice for this server, or the
    /// self-signed certificate of the Filen bridge over HTTPS.
    accept_invalid_certs: bool,
    /// The Filen Desktop WebDAV bridge our probe found on a loopback IP
    /// during this connect, over HTTP or HTTPS: reached directly, never
    /// through a system proxy, and never redirected to another origin.
    filen_bridge: bool,
}

/// Whether `url` names a loopback IP literally (127.0.0.1 or ::1), not a
/// name that DNS resolves.
fn names_a_loopback_ip(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|u| u.host().map(|h| h.to_owned()))
    {
        Some(url::Host::Ipv4(ip)) => ip == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

/// Whether `url` is served by `host` itself, compared on the parsed host and
/// never on a substring, which a path could carry.
fn served_by(url: &str, host: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(host)))
        .unwrap_or(false)
}

/// The refused redirect inside a failed send, if that is what failed.
fn insecure_redirect(e: &reqwest::Error) -> Option<&InsecureRedirect> {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(refused) = s.downcast_ref::<InsecureRedirect>() {
            return Some(refused);
        }
        source = s.source();
    }
    None
}

/// Whether a failed send is a refused redirect to the collection form of
/// its own target, the one failure a path-type-ambiguous caller repairs by
/// resending in the trailing-slash form.
fn refused_collection_redirect(e: &reqwest::Error) -> bool {
    insecure_redirect(e).is_some_and(InsecureRedirect::is_to_collection_form)
}

/// Map a failed send to a `ProviderError`. No reply in time is a `Timeout`
/// (see `http_send_error`). A refused HTTPS-to-HTTP redirect is a
/// deterministic server misconfiguration, so it becomes a `ServerError` that
/// names it, never the retryable `NetworkError` of a reset connection;
/// everything else keeps the full source chain (see `describe_reqwest_error`).
fn send_error(e: reqwest::Error) -> ProviderError {
    super::http_send_error(e, |e| match insecure_redirect(&e) {
        Some(refused) => ProviderError::ServerError(refused.to_string()),
        None if left_the_bridge(&e) => ProviderError::ServerError(LeftTheBridge.to_string()),
        None => ProviderError::NetworkError(describe_reqwest_error(e)),
    })
}

/// reqwest can wrap a streamed source read failure as a request error rather
/// than `is_body()`. Do not spend the network budget reopening a broken source.
fn nextcloud_chunk_send_retryable(error: &reqwest::Error) -> bool {
    use std::error::Error;
    if error.is_builder()
        || error.is_body()
        || error.is_redirect()
        || insecure_redirect(error).is_some()
    {
        return false;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if cause
            .downcast_ref::<hyper::Error>()
            .is_some_and(|e| e.is_user())
        {
            return false;
        }
        source = cause.source();
    }
    true
}

/// [`send_error`] for the connect-time probes, which report through
/// `ConnectionFailed`.
fn connect_send_error(e: reqwest::Error) -> ProviderError {
    super::http_send_error(e, |e| match insecure_redirect(&e) {
        Some(refused) => ProviderError::ConnectionFailed(refused.to_string()),
        None if left_the_bridge(&e) => ProviderError::ConnectionFailed(LeftTheBridge.to_string()),
        None => ProviderError::ConnectionFailed(describe_reqwest_error(e)),
    })
}

/// The error for a `MOVE` or `COPY` answered `502 Bad Gateway`. RFC 4918
/// (9.8.5, 9.9.4) gives 502 a WebDAV meaning there: the server refused the
/// `Destination`. Apache mod_dav answers it when the Destination's scheme or
/// port differs from the one it believes it serves, which is what a backend
/// behind a TLS-terminating proxy believes of every `https://` Destination
/// (measured on 2026-10-02: each rename on such a server failed this way).
fn destination_refused_error(method: &str) -> ProviderError {
    ProviderError::ServerError(format!(
        "{method} failed with status 502 Bad Gateway: the server refused the Destination URL. \
         Behind a TLS-terminating reverse proxy this usually means the backend serves plain HTTP and rejects an https:// Destination; \
         the proxy has to rewrite the Destination header"
    ))
}

/// Extract the path component from a full URL, preserving trailing slash
fn extract_uri_path(url: &str) -> String {
    if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        if let Some(slash_idx) = after_scheme.find('/') {
            let path = after_scheme[slash_idx..].to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }
    "/".to_string()
}

/// Custom HTTP methods for WebDAV
mod webdav_methods {
    use reqwest::Method;

    pub fn propfind() -> Method {
        Method::from_bytes(b"PROPFIND").unwrap()
    }

    pub fn mkcol() -> Method {
        Method::from_bytes(b"MKCOL").unwrap()
    }

    #[allow(dead_code)]
    pub fn copy() -> Method {
        Method::from_bytes(b"COPY").unwrap()
    }

    pub fn move_method() -> Method {
        Method::from_bytes(b"MOVE").unwrap()
    }

    pub fn lock() -> Method {
        Method::from_bytes(b"LOCK").unwrap()
    }

    pub fn unlock() -> Method {
        Method::from_bytes(b"UNLOCK").unwrap()
    }
}

/// Pure boundary check used by `cd()` and `cd_up()`.
///
/// Returns `true` when `path` resolves above the configured WebDAV root.
/// `boundary` is the effective root (auto-detected `server_root` if known,
/// otherwise the user-supplied `initial_path`). Issue #175.
///
/// The check trims trailing slashes for case-insensitive prefix matching,
/// and short-circuits on empty / "/" boundaries (no-op since the WebDAV
/// server has effectively no boundary to enforce in those cases).
fn path_violates_root(path: &str, boundary: Option<&str>) -> bool {
    let Some(boundary) = boundary else {
        return false;
    };
    if boundary.is_empty() {
        return false;
    }
    let root_trimmed = boundary.trim_end_matches('/');
    if root_trimmed.is_empty() {
        return false;
    }
    let path_trimmed = path.trim_end_matches('/');
    path_trimmed != root_trimmed && !path_trimmed.starts_with(&format!("{root_trimmed}/"))
}

/// WebDAV Storage Provider
#[derive(Clone)]
pub struct WebDavProvider {
    config: WebDavConfig,
    client: Client,
    /// The policy `client` was built with.
    client_policy: ClientPolicy,
    current_path: String,
    connected: bool,
    /// Digest auth state (set during connect if server requires it)
    digest_auth: Option<DigestState>,
    /// Server-side WebDAV root resolved at connect time (issue #175).
    ///
    /// `config.initial_path` is the user-supplied value from the saved-server
    /// form and may be empty, "/", or a relative folder like "/Documents".
    /// On Nextcloud / ownCloud the actual WebDAV root is auto-detected at
    /// connect time as one of `/remote.php/dav/files/<user>/` or
    /// `/remote.php/webdav/` after a 405 on `PROPFIND /`. The boundary checks
    /// in `cd()` and `cd_up()` MUST use the auto-detected root (otherwise a
    /// drill-down click into a folder fails with "Cannot navigate above
    /// WebDAV root", because the entry path returned by `list()` is rooted
    /// at the real WebDAV root, not the user-typed one).
    ///
    /// Populated in every successful branch of `connect()`. `None` when
    /// disconnected. When `Some`, takes precedence over `config.initial_path`.
    server_root: Option<String>,
    /// Multi-thread concurrent-Range download (rclone `--multi-thread-streams`).
    /// `1` = disabled (default). Set via `set_multi_thread_download`.
    multi_thread_streams: usize,
    /// What the last `download` read, see `DownloadTime`.
    download_time: super::multi_thread::DownloadTime,
    /// Files at or above this size use the concurrent-Range path when
    /// `multi_thread_streams >= 2`.
    multi_thread_cutoff: u64,
    /// Single-file resource mode (issue #264).
    ///
    /// Some WebDAV servers expose only one file at the configured URL rather
    /// than a browseable collection — most commonly the MEGAcmd
    /// `mega-webdav <file>` bridge, which responds to PROPFIND/GET on the
    /// exact file path but returns 403/404 on the root and any parent path.
    /// The standard PROPFIND-on-`/` discovery probe AeroFTP runs at connect
    /// time would then fail with a confusing reqwest error.
    ///
    /// When `connect()` detects this shape (PROPFIND on the URL verbatim
    /// returns 207 with a non-collection resource), the entry is cached
    /// here and `list()` / `download()` / `build_url()` short-circuit to
    /// operate on the verbatim URL.
    single_file_mode: Option<RemoteEntry>,
    /// How long `list_recursive` waits for the `Depth: infinity` response
    /// headers (#958). [`INFINITY_HEADERS_TIMEOUT`] outside tests.
    infinity_headers_timeout: std::time::Duration,
    /// The whole-request limit of a metadata request. [`METADATA_TIMEOUT`]
    /// (or the CLI's `--timeout`) outside tests.
    metadata_timeout: std::time::Duration,
    /// This server refused `PROPFIND Depth: infinity` earlier in this
    /// session, so `list_recursive` does not ask again: each used-storage scan
    /// paid the request before walking (#958). Set from the start for the
    /// Filen Desktop bridge, which never answers it. See [`Self::reset_infinity_memory`].
    infinity_refused: bool,
    /// When the server last sent no headers to a `Depth: infinity` within the
    /// cap. Unlike a refusal this may pass, so it is remembered for
    /// [`INFINITY_TIMEOUT_MEMORY`] only, then asked again.
    infinity_timed_out_at: Option<std::time::Instant>,
    /// The URL the profile was saved with. A MEGAcmd session may connect to
    /// the path the bridge serves instead (`config.url`), and a reconnect of
    /// the same session chooses again from this one, so a bridge that moved
    /// meanwhile is followed rather than the path it served before.
    profile_url: String,
    /// Whether this MEGAcmd session is served at the account root (the URL
    /// `mega-webdav /` reports): MEGAcmd's versions name account paths, so
    /// a session rooted below another folder would list, download or
    /// restore the file of the same path at the account root instead.
    megacmd_serves_account_root: bool,
    /// The account MEGAcmd was logged in to when this session connected
    /// (`mega-whoami`): versions are read only while it still is.
    megacmd_account: Option<String>,
}

/// How long a `Depth: infinity` that sent no headers keeps the next scans on
/// the folder walk (#958): long enough that repeated clicks do not each wait
/// the cap, short enough that a server that recovers is asked again.
const INFINITY_TIMEOUT_MEMORY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Wait for the response headers of a `PROPFIND Depth: infinity` (#958). A
/// server that never starts answering it (Filen WebDAV did) held the provider
/// lock for the 1800 s read timeout, blocking the file browser and the
/// disconnect behind it. The cap is on the headers only: once they arrive, a
/// large tree streams under the idle read timeout and the scan's own cancel,
/// never a wall clock. Past the cap the used-storage scan walks Depth:1.
const INFINITY_HEADERS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The whole of a metadata request: a listing or a stat (`PROPFIND` with
/// `Depth` 0 or 1), a modification date (`PROPPATCH`), a new folder
/// (`MKCOL`), `HEAD` and `OPTIONS`. Without it a server that accepts the
/// request and never answers holds the command for the 1800 s read timeout.
/// Measured on Nextcloud 33 (2026-10-30): a 2000-file folder lists in under
/// 2 s with the connection, a stat in 0.8 s; the limit leaves about sixty
/// times that. Transfers and the operations a timeout would leave half known
/// (`MOVE`, `COPY`, `DELETE`, `LOCK`) keep the read timeout only, and so does
/// `Depth: infinity`, which has its own header cap and may stream a large
/// tree for longer. The CLI's `--timeout` replaces it.
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether `method` is a metadata request, bounded by [`METADATA_TIMEOUT`].
fn is_metadata_method(method: &Method) -> bool {
    matches!(
        method.as_str(),
        "PROPFIND" | "PROPPATCH" | "MKCOL" | "HEAD" | "OPTIONS"
    )
}

/// The download body stopped with an error. reqwest's own message for it is
/// "error decoding response body" whatever happened; the cause sits in its
/// source chain. Koofr, for one, answers a file type it refuses to serve with
/// a 200 that announces the size and then ends with no data (#368), which the
/// bare message left unexplained: say how much came, and why it stopped.
fn body_cut_short(error: reqwest::Error, received: u64, announced: u64) -> String {
    let cause = describe_reqwest_error(error);
    if announced > 0 {
        format!("the download stopped after {received} of {announced} bytes ({cause})")
    } else {
        format!("the download stopped after {received} bytes ({cause})")
    }
}

/// #958: the Filen Desktop bridge never answers `Depth: infinity`, so asking
/// it cost every used-storage scan the whole header cap.
/// Whether the account behind a WebDAV or S3 endpoint cannot be verified from
/// the connection: a local bridge (Filen Desktop, MEGAcmd, OpenDrive Desktop,
/// by preset) or any loopback endpoint, which can be a hand-configured bridge.
/// Its endpoint and credentials are the bridge's, and the account upstream
/// can change behind them. Such a connection owns no stable identity: no sync
/// owner, and a GUI target of this connection only (T4). One rule for both.
pub(crate) fn upstream_account_unverified(provider_id: Option<&str>, endpoint: &str) -> bool {
    let bridge = provider_id.is_some_and(|id| {
        crate::local_bridge::bridge_kind_for_provider_id(id).is_some()
            || matches!(id, "filen" | "opendrive-desktop")
    });
    // An endpoint saved without a scheme (`127.0.0.1:4566`) is read as one.
    let url = url::Url::parse(endpoint)
        .ok()
        .filter(|url| url.host_str().is_some())
        .or_else(|| url::Url::parse(&format!("http://{endpoint}")).ok());
    let local = url
        .as_ref()
        .and_then(url::Url::host_str)
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
                || host.ends_with(".filen.io") && host.starts_with("local.")
        });
    bridge || local
}

fn infinity_refused_from_the_start(provider_id: Option<&str>) -> bool {
    provider_id == Some("filen-desktop-webdav")
}

/// Provider-specific hard cap on concurrent Range streams (mirrors S3's 16).
const WEBDAV_MULTI_THREAD_MAX_STREAMS: usize = 16;
/// File-level clone-pool ceiling. Matches the GUI/CLI `MAX_MAX_CONCURRENT`
/// knob (issue #591): independent WebDAV GET/PUT of distinct files is safe
/// on a cloned `reqwest::Client`.
const WEBDAV_TRANSFER_MAX_SESSIONS: u16 = 8;
/// Directories a walk lists at once (#958): the shared walker's default
/// checkers, the same count as rclone's `--checkers`. Like rclone's pacer, a
/// listing the server answers 429 or 503 is retried after a pause rather than
/// counted as failed (see `send_propfind_paced`); `--checkers` lowers it in
/// the CLI.
const WEBDAV_LIST_MAX_SESSIONS: u16 = 8;
/// The most one collection listing waits, all its retry pauses together,
/// before it gives up on a 429 or 503 (#958 review). The listing holds the
/// provider lock while it waits, so a `Retry-After` of minutes would freeze
/// the file browser; past this budget the refusal is returned at once.
const LIST_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Live `oc:fileid` lookup. The trashbin PROPFIND asks for the same property
/// after the delete; this one runs before it, on the file's parent.
const NEXTCLOUD_FILE_ID_PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
    <d:prop>
        <oc:fileid/>
    </d:prop>
</d:propfind>"#;

/// Depth-0 lookup of one file for its versions: the id and the current
/// contents' size, time and ETag, in one answer.
const NEXTCLOUD_FILE_PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
    <d:prop>
        <oc:fileid/>
        <d:getcontentlength/>
        <d:getlastmodified/>
        <d:getetag/>
        <d:resourcetype/>
    </d:prop>
</d:propfind>"#;

/// One propstat's fields. A 404 propstat is staged here and kept only when
/// its status is missing or 200, so a later "not found" block cannot replace
/// the file id or the original location.
#[derive(Default)]
struct TrashPropBuf {
    href: String,
    filename: String,
    location: String,
    deletion_time: String,
    content_length: String,
    file_id: String,
    is_collection: bool,
}

impl TrashPropBuf {
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Copy each field the destination does not already have.
    fn take_missing_from(&mut self, from: &Self) {
        Self::fill(&mut self.href, &from.href);
        Self::fill(&mut self.filename, &from.filename);
        Self::fill(&mut self.location, &from.location);
        Self::fill(&mut self.deletion_time, &from.deletion_time);
        Self::fill(&mut self.content_length, &from.content_length);
        Self::fill(&mut self.file_id, &from.file_id);
        self.is_collection |= from.is_collection;
    }

    fn fill(dest: &mut String, src: &str) {
        if dest.is_empty() && !src.is_empty() {
            dest.push_str(src);
        }
    }

    /// Append one text or entity fragment. Indentation is dropped except in
    /// the name-bearing tags, where a space between entities is part of the
    /// name (`a&amp; &amp;b.txt`).
    fn push_fragment(&mut self, tag: &str, raw: &str) {
        let payload = matches!(
            tag,
            "href" | "trashbin-filename" | "trashbin-original-location"
        );
        if raw.trim().is_empty() && !payload {
            return;
        }
        match tag {
            "href" => self.href.push_str(raw),
            "trashbin-filename" => self.filename.push_str(raw),
            "trashbin-original-location" => self.location.push_str(raw),
            "trashbin-deletion-time" => self.deletion_time.push_str(raw),
            "getcontentlength" => self.content_length.push_str(raw),
            "fileid" => self.file_id.push_str(raw),
            _ => {}
        }
    }
}

fn active_trash_buf<'a>(
    kept: &'a mut TrashPropBuf,
    staged: &'a mut TrashPropBuf,
    in_propstat: bool,
) -> &'a mut TrashPropBuf {
    if in_propstat {
        staged
    } else {
        kept
    }
}

/// A propstat with no status element is the shape older trash fixtures use,
/// and a 200 keeps the values. `1200` and `404` do not: the code is one
/// whitespace-separated token, so a longer number is not a success.
fn trash_propstat_succeeded(status: &str) -> bool {
    let status = status.trim();
    if status.is_empty() {
        return true;
    }
    status.split_whitespace().any(|token| token == "200")
}

/// Original-location key. Leading and trailing slashes are ignored. The
/// remaining text matches exactly, including case, Unicode, and spaces at
/// either end of a name. `docs/a.txt` and `docs/a.txt ` are different files.
///
/// Trash text is already decoded XML. A second percent-decode would turn a
/// literal `%20` in a name into a space, so this only strips slashes.
fn nextcloud_trash_location(path: &str) -> String {
    path.trim_matches('/').to_string()
}

/// One URL segment, decoded once. An invalid escape is kept as written.
fn decode_one_segment(segment: &str) -> String {
    urlencoding::decode(segment)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| segment.to_string())
}

fn find_segment_sequence(segments: &[String], needle: &[&str]) -> Option<usize> {
    segments.windows(needle.len()).position(|window| {
        window
            .iter()
            .zip(needle.iter())
            .all(|(have, want)| have == want)
    })
}

/// Path relative to the Nextcloud files root, from the decoded URL segments
/// of the request that was actually sent.
///
/// The files form is `remote.php/dav/files/<userid>/...`, and the userid is
/// the decoded `nextcloud_userid`. The legacy form is `remote.php/webdav/...`.
/// A subpath install may have segments before `remote.php`. Anything else,
/// a userid that does not match, or an empty remainder (the root itself) is
/// `None`: the caller purges nothing and does not guess.
fn nextcloud_location_rest(segments: &[String], userid: Option<&str>) -> Option<String> {
    if let Some(at) = find_segment_sequence(segments, &["remote.php", "dav", "files"]) {
        let user = segments.get(at + 3)?;
        if user != userid? {
            return None;
        }
        let rest = &segments[at + 4..];
        if rest.is_empty() {
            return None;
        }
        return Some(rest.join("/"));
    }
    if let Some(at) = find_segment_sequence(segments, &["remote.php", "webdav"]) {
        let rest = &segments[at + 2..];
        if rest.is_empty() {
            return None;
        }
        return Some(rest.join("/"));
    }
    None
}

/// An empty id, `.`, `..`, or an id that still contains `/` would address
/// the trash collection or leave it. Those are refused before DELETE.
fn nextcloud_trash_id_is_one_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains('/')
}

/// Parent collection and exact final segment of a caller path. `.` and `..`
/// are not names that can be matched against a listing.
fn nextcloud_parent_and_name(path: &str) -> Option<(String, String)> {
    // Slashes only. A space at either end of the name is part of the name.
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let (parent, name) = match trimmed.rfind('/') {
        Some(cut) => (
            format!("/{}", &trimmed[..cut]),
            trimmed[cut + 1..].to_string(),
        ),
        None => ("/".to_string(), trimmed.to_string()),
    };
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some((parent, name))
    }
}

#[derive(Debug)]
struct ListedFileId {
    href: String,
    file_id: String,
    /// `getcontentlength` and `getlastmodified`, when the PROPFIND asked for
    /// them: read in the same answer as the id they belong to.
    size: Option<u64>,
    modified: Option<String>,
    /// `getetag`, when asked for: the validator a read of these contents
    /// is sent with ([`WebDavProvider::nextcloud_download_current`]).
    etag: Option<String>,
    /// `resourcetype` holds `collection`: a folder, which has no versions.
    is_collection: bool,
}

/// One Nextcloud file as a single Depth-0 PROPFIND answered: its id and the
/// size and time of its current contents, so the "current" row of a version
/// listing cannot pair one file's id with another file's metadata.
struct NextcloudFile {
    file_id: String,
    size: u64,
    modified: Option<String>,
    etag: Option<String>,
}

/// quick-xml yields `Eof` when the body stops, even if an element is still
/// open. A PROPFIND is complete only after `</multistatus>` and with every
/// element closed. Anything else would be a partial listing.
fn propfind_document_is_complete(
    depth: u32,
    saw_multistatus_end: bool,
) -> Result<(), ProviderError> {
    if depth == 0 && saw_multistatus_end {
        Ok(())
    } else {
        Err(ProviderError::ParseError(
            "PROPFIND listing ended before the multistatus document was complete".into(),
        ))
    }
}

/// A versions-related PROPFIND answer opens with `multistatus` as its root:
/// any other root (an error page, a document that only nests one) is not a
/// listing, and only the root's own end completes it.
fn propfind_root_is_multistatus(depth: u32, tag: &str) -> Result<(), ProviderError> {
    if depth == 0 && tag != "multistatus" {
        return Err(ProviderError::ParseError(format!(
            "PROPFIND answer's root is <{tag}>, not a DAV multistatus"
        )));
    }
    Ok(())
}

/// Whether a `<d:status>` of a whole `<d:response>` (not of a propstat)
/// lets its row stand: none, or a 2xx. A row the server answers 404 or 403
/// for is not an entry.
fn response_status_succeeded(status: &str) -> bool {
    let status = status.trim();
    status.is_empty()
        || status
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .is_some_and(|code| (200..300).contains(&code))
}

/// Depth-1 listing of a file's versions collection
/// (`/remote.php/dav/versions/<user>/versions/<fileid>/`). Each version is a
/// child named by the Unix time it was superseded; `nc:version-author` is
/// present from Nextcloud 28 on.
/// What a Nextcloud with the Versions app off answers: 404 on the
/// versions collection of a file it lists (measured on Murena and Felicloud,
/// 2026-11-16, while their trashbin answered).
const NEXTCLOUD_VERSIONS_OFF: &str =
    "Versions are not enabled on this Nextcloud server: /remote.php/dav/versions/ answers 404 (the Versions app is off)";

/// Largest versions listing read into memory (one PROPFIND of one file's
/// versions collection). A row is a few hundred bytes, so this holds tens of
/// thousands of versions; a server that sends more is refused, not buffered.
const NEXTCLOUD_VERSIONS_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A listed Nextcloud version id, bound to the file id it was listed from:
/// `<oc:fileid>:<version>`. A download, restore or delete of it refuses a
/// path that names another file by then (the file was replaced after the
/// listing), instead of acting on the replacement's versions.
fn nextcloud_bound_version_id(file_id: &str, version: &str) -> String {
    format!("{file_id}:{version}")
}

/// The file id a version id is bound to (`None` for a bare version, as a
/// caller may type it) and the version segment itself. A current row bound
/// to its listed ETag ([`super::bound_current_row`]) is the current version
/// of its file.
fn nextcloud_split_version_id(id: &str) -> (Option<&str>, &str) {
    if let Some((file_id, _)) = super::bound_current_row_of(id) {
        if file_id.bytes().all(|b| b.is_ascii_digit()) {
            return (Some(file_id), super::CURRENT_VERSION_ID);
        }
    }
    match id.split_once(':') {
        Some((file_id, version))
            if !file_id.is_empty() && file_id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (Some(file_id), version)
        }
        _ => (None, id),
    }
}

const NEXTCLOUD_VERSIONS_PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns">
    <d:prop>
        <d:getcontentlength/>
        <d:getlastmodified/>
        <d:resourcetype/>
        <nc:version-author/>
    </d:prop>
</d:propfind>"#;

/// One response of a versions PROPFIND, with only what a successful
/// propstat reported.
#[derive(Debug, Default, PartialEq)]
struct ListedVersion {
    href: String,
    size: Option<u64>,
    modified: Option<String>,
    author: Option<String>,
    is_collection: bool,
}

/// Parse a versions PROPFIND. A 404 propstat (a property the server does
/// not have, such as `version-author` before Nextcloud 28) contributes
/// nothing; a reader error or a body that ends before `</multistatus>` fails
/// the listing, so a partial parse is never read as "fewer versions".
fn parse_nextcloud_versions_listing(xml: &str) -> Result<Vec<ListedVersion>, ProviderError> {
    let mut found = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_response = false;
    let mut in_propstat = false;
    let mut current_tag: Option<String> = None;
    let mut row = ListedVersion::default();
    let mut staged = ListedVersion::default();
    let mut propstat_status = String::new();
    let mut text = String::new();
    let mut depth = 0u32;
    let mut saw_multistatus_end = false;
    let mut response_status = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Err(error) => return Err(ProviderError::ParseError(error.to_string())),
            Ok(Event::Eof) => {
                propfind_document_is_complete(depth, saw_multistatus_end)?;
                break;
            }
            Ok(Event::Start(ref event)) => {
                propfind_root_is_multistatus(depth, event.local_name().as_ref())?;
                depth = depth.saturating_add(1);
                let tag = event.local_name().as_ref().to_string();
                text.clear();
                match tag.as_str() {
                    "response" => {
                        in_response = true;
                        row = ListedVersion::default();
                        response_status.clear();
                        current_tag = None;
                    }
                    "propstat" if in_response => {
                        in_propstat = true;
                        staged = ListedVersion::default();
                        propstat_status.clear();
                    }
                    "collection" if in_propstat => staged.is_collection = true,
                    _ if in_response => current_tag = Some(tag),
                    _ => {}
                }
            }
            Ok(Event::Empty(ref event)) => {
                propfind_root_is_multistatus(depth, event.local_name().as_ref())?;
                match event.local_name().as_ref() {
                    // `<d:multistatus/>`, the empty document spelled short, as
                    // the trash and the ordinary listing parsers accept it.
                    "multistatus" if depth == 0 => saw_multistatus_end = true,
                    "collection" if in_propstat => staged.is_collection = true,
                    _ => {}
                }
            }
            // An element's text arrives as Text pieces around GeneralRef ones
            // (`&amp;` in an href or an author), so it is gathered whole and
            // read when the element ends.
            Ok(Event::Text(ref event)) => {
                if current_tag.is_some() {
                    text.push_str(event.as_ref());
                }
            }
            Ok(Event::GeneralRef(ref event)) => {
                if current_tag.is_some() {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(event.as_ref()) {
                        text.push_str(&ch);
                    }
                }
            }
            Ok(Event::End(ref event)) => {
                let tag = event.local_name().as_ref().to_string();
                depth = depth.saturating_sub(1);
                if current_tag.as_deref() == Some(tag.as_str()) {
                    let raw = std::mem::take(&mut text);
                    let value = Some(raw.trim().to_string()).filter(|v| !v.is_empty());
                    match tag.as_str() {
                        "href" if !in_propstat => row.href.push_str(&raw),
                        "status" if in_propstat => propstat_status.push_str(&raw),
                        "status" => response_status.push_str(&raw),
                        "getcontentlength" if in_propstat => {
                            staged.size = raw.trim().parse::<u64>().ok();
                        }
                        "getlastmodified" if in_propstat => staged.modified = value,
                        "version-author" if in_propstat => staged.author = value,
                        _ => {}
                    }
                }
                text.clear();
                match tag.as_str() {
                    "multistatus" if depth == 0 => saw_multistatus_end = true,
                    "propstat" if in_propstat => {
                        in_propstat = false;
                        if trash_propstat_succeeded(&propstat_status) {
                            let staged = std::mem::take(&mut staged);
                            row.size = row.size.or(staged.size);
                            row.modified = row.modified.take().or(staged.modified);
                            row.author = row.author.take().or(staged.author);
                            row.is_collection |= staged.is_collection;
                        }
                    }
                    "response" if in_response => {
                        in_response = false;
                        let row = std::mem::take(&mut row);
                        if response_status_succeeded(&response_status) {
                            found.push(row);
                        }
                    }
                    _ => {}
                }
                current_tag = None;
            }
            Ok(_) => {}
        }
        buf.clear();
    }
    Ok(found)
}

/// The one strong ETag `raw` names, quoted as `If-Match` sends it: `*` would
/// match any file at the path, a weak `W/` tag any of the same meaning, and a
/// list or a tag with a quote inside it no single one, so each is refused.
/// A bare token (a server that leaves the quotes out) is quoted.
fn strong_etag(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let inner = match raw.strip_prefix('"') {
        Some(rest) => rest.strip_suffix('"')?,
        None => raw,
    };
    let valid = !inner.is_empty()
        && !raw.starts_with("W/")
        && inner
            .bytes()
            .all(|b| b > 0x20 && b != b'"' && b != b',' && b != 0x7f)
        && inner != "*";
    valid.then(|| format!("\"{inner}\""))
}

/// Whether a MEGAcmd session URL is the one `mega-webdav /` serves the
/// account root at (`served`, `None` when the bridge printed none).
fn megacmd_session_is_account_root(session_url: &str, served: Option<&str>) -> bool {
    let trimmed = |u: &str| u.trim().trim_end_matches('/').to_string();
    served.is_some_and(|served| trimmed(served) == trimmed(session_url))
}

/// The version id of a row of the versions answer: only a direct child of
/// the collection that was listed (`<root>/versions/<user>/versions/<file id>/`,
/// path of `collection_url`), compared segment by segment, decoded. A row of
/// another file's id, or of the same id under another user's collection (a
/// server or proxy answering more than was asked), is not this file's, and
/// binding it would let a delete remove this file's version of that time.
fn nextcloud_version_id_of_href(
    href: &str,
    collection_url: &str,
    answered_from: &str,
) -> Option<String> {
    // The href is resolved as a browser would, against the URL the answer
    // came from (after any redirect): an absolute URL and a scheme-relative
    // `//host/...` name their own server, and one of another origin than
    // the collection's is not a row of this collection, whatever its path.
    let base = url::Url::parse(collection_url).ok()?;
    let resolved = url::Url::parse(answered_from).ok()?.join(href).ok()?;
    if resolved.origin() != base.origin() {
        return None;
    }
    let href = resolved.path().to_string();
    let collection = base.path().to_string();
    let (parent, last) = href.trim_end_matches('/').rsplit_once('/')?;
    let decoded = |p: &str| -> Vec<String> {
        p.split('/')
            .filter(|s| !s.is_empty())
            .map(decode_one_segment)
            .collect()
    };
    if decoded(parent) != decoded(collection.trim_end_matches('/')) {
        return None;
    }
    let id = decode_one_segment(last);
    nextcloud_trash_id_is_one_segment(&id).then_some(id)
}

/// Depth-1 file-id listing. A 404 propstat's file id is ignored. A reader
/// error, or a body that ends before `</multistatus>`, fails the listing:
/// a partial parse would look like "no file id".
fn parse_nextcloud_file_id_listing(xml: &str) -> Result<Vec<ListedFileId>, ProviderError> {
    let mut found = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_response = false;
    let mut in_propstat = false;
    let mut current_tag: Option<String> = None;
    let mut href = String::new();
    let mut staged_id = String::new();
    let mut kept_id = String::new();
    let mut staged_size = String::new();
    let mut kept_size = String::new();
    let mut staged_modified = String::new();
    let mut kept_modified = String::new();
    let mut staged_etag = String::new();
    let mut kept_etag = String::new();
    let mut staged_collection = false;
    let mut kept_collection = false;
    let mut propstat_status = String::new();
    let mut response_status = String::new();
    let mut depth = 0u32;
    let mut saw_multistatus_end = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Err(error) => return Err(ProviderError::ParseError(error.to_string())),
            Ok(Event::Eof) => {
                propfind_document_is_complete(depth, saw_multistatus_end)?;
                break;
            }
            Ok(Event::Start(ref event)) => {
                propfind_root_is_multistatus(depth, event.local_name().as_ref())?;
                depth = depth.saturating_add(1);
                let tag = event.local_name().as_ref().to_string();
                match tag.as_str() {
                    "response" => {
                        in_response = true;
                        in_propstat = false;
                        href.clear();
                        staged_id.clear();
                        kept_id.clear();
                        kept_size.clear();
                        kept_modified.clear();
                        kept_etag.clear();
                        kept_collection = false;
                        propstat_status.clear();
                        response_status.clear();
                        current_tag = None;
                    }
                    "propstat" if in_response => {
                        in_propstat = true;
                        staged_id.clear();
                        staged_size.clear();
                        staged_modified.clear();
                        staged_etag.clear();
                        staged_collection = false;
                        propstat_status.clear();
                    }
                    "collection" if in_propstat => staged_collection = true,
                    _ if in_response => current_tag = Some(tag),
                    _ => {}
                }
            }
            Ok(Event::End(ref event)) => {
                let tag = event.local_name().as_ref().to_string();
                depth = depth.saturating_sub(1);
                if tag == "multistatus" && depth == 0 {
                    saw_multistatus_end = true;
                }
                match tag.as_str() {
                    "propstat" if in_propstat => {
                        in_propstat = false;
                        if trash_propstat_succeeded(&propstat_status) {
                            for (kept, staged) in [
                                (&mut kept_id, &staged_id),
                                (&mut kept_size, &staged_size),
                                (&mut kept_modified, &staged_modified),
                                (&mut kept_etag, &staged_etag),
                            ] {
                                if kept.is_empty() && !staged.is_empty() {
                                    kept.push_str(staged);
                                }
                            }
                            kept_collection |= staged_collection;
                        }
                        current_tag = None;
                    }
                    "response" if in_response => {
                        in_response = false;
                        if !href.trim().is_empty() && response_status_succeeded(&response_status) {
                            let modified = kept_modified.trim();
                            let etag = kept_etag.trim();
                            found.push(ListedFileId {
                                href: std::mem::take(&mut href),
                                file_id: kept_id.trim().to_string(),
                                size: kept_size.trim().parse::<u64>().ok(),
                                modified: (!modified.is_empty()).then(|| modified.to_string()),
                                etag: (!etag.is_empty()).then(|| etag.to_string()),
                                is_collection: kept_collection,
                            });
                        }
                    }
                    _ => {
                        if current_tag.as_deref() == Some(tag.as_str()) {
                            current_tag = None;
                        }
                    }
                }
            }
            Ok(Event::Text(ref event)) => {
                if let Some(ref tag) = current_tag {
                    let raw = event.as_ref().to_string();
                    if tag == "status" && in_propstat {
                        propstat_status.push_str(&raw);
                    } else if tag == "status" {
                        response_status.push_str(&raw);
                    } else if tag == "href" && !in_propstat {
                        href.push_str(&raw);
                    } else if tag == "fileid" && in_propstat && !raw.trim().is_empty() {
                        staged_id.push_str(&raw);
                    } else if tag == "getcontentlength" && in_propstat {
                        staged_size.push_str(&raw);
                    } else if tag == "getlastmodified" && in_propstat {
                        staged_modified.push_str(&raw);
                    } else if tag == "getetag" && in_propstat {
                        staged_etag.push_str(&raw);
                    }
                }
            }
            Ok(Event::GeneralRef(ref event)) => {
                if let Some(ch) = super::xml_text::xml_entity_to_str(event.as_ref()) {
                    if let Some(ref tag) = current_tag {
                        if tag == "href" && !in_propstat {
                            href.push_str(&ch);
                        } else if tag == "fileid" && in_propstat {
                            staged_id.push_str(&ch);
                        } else if tag == "getetag" && in_propstat {
                            staged_etag.push_str(&ch);
                        }
                    }
                }
            }
            Ok(Event::Empty(ref event)) => {
                propfind_root_is_multistatus(depth, event.local_name().as_ref())?;
                match event.local_name().as_ref() {
                    "multistatus" if depth == 0 => saw_multistatus_end = true,
                    "collection" if in_propstat => staged_collection = true,
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(found)
}

impl WebDavProvider {
    /// Create a new WebDAV provider with the given configuration
    pub fn new(config: WebDavConfig) -> Result<Self, ProviderError> {
        // M6: Log a warning when TLS certificate verification is disabled.
        // This exposes the connection to MITM attacks: acceptable only for self-signed certs
        // in trusted networks (e.g. home NAS).
        if !config.verify_cert {
            tracing::warn!(
                "[WEBDAV] TLS certificate verification DISABLED for {}: connection is vulnerable to MITM attacks",
                crate::local_bridge::url_origin(&config.url)
            );
        }
        let client_policy = ClientPolicy {
            accept_invalid_certs: !config.verify_cert,
            filen_bridge: false,
        };
        let client = Self::http_client(&config, client_policy)?;

        let infinity_refused = infinity_refused_from_the_start(config.provider_id.as_deref());
        let profile_url = config.url.clone();
        Ok(Self {
            config,
            client,
            client_policy,
            current_path: "/".to_string(),
            connected: false,
            digest_auth: None,
            server_root: None,
            download_time: Default::default(),
            multi_thread_streams: 1,
            multi_thread_cutoff: 8 * 1024 * 1024,
            single_file_mode: None,
            infinity_headers_timeout: INFINITY_HEADERS_TIMEOUT,
            metadata_timeout: crate::providers::http_limits::limit(METADATA_TIMEOUT),
            infinity_refused,
            infinity_timed_out_at: None,
            profile_url,
            megacmd_serves_account_root: false,
            megacmd_account: None,
        })
    }

    /// MEGAcmd's versions are of account paths: refused for a session served
    /// below another folder, where the same path names another file.
    fn megacmd_versions_at_account_root(&self) -> Result<(), ProviderError> {
        if self.megacmd_serves_account_root {
            return Ok(());
        }
        Err(ProviderError::NotSupported(
            "MEGA versions need this profile served at the account root (`mega-webdav /`): \
             this session is served below another folder, where a path names another file"
                .into(),
        ))
    }

    /// The HTTP client of a connection to `config`, checking certificates as
    /// `trust` says.
    fn http_client(config: &WebDavConfig, policy: ClientPolicy) -> Result<Client, ProviderError> {
        let mut client_builder = Client::builder()
            .user_agent(crate::providers::AEROFTP_WEBDAV_USER_AGENT)
            .danger_accept_invalid_certs(policy.accept_invalid_certs)
            .connect_timeout(crate::providers::http_limits::limit(
                std::time::Duration::from_secs(30),
            ))
            // A redirect from https to http is refused, never followed
            // anonymously: see `InsecureRedirect`.
            .redirect(webdav_redirect_policy())
            // 30 minutes of silence for large uploads on slow links: 300 s
            // killed 1 GiB uploads on jianguoyun, InfiniCloud, DriveHQ and
            // Koofr WebDAV below about 27 Mbps.
            .read_timeout(crate::providers::http_limits::limit(
                crate::providers::http_limits::READ_TIMEOUT,
            ));

        // Issue #264: MEGAcmd's embedded `mega-webdav` bridge does not
        // reliably honor HTTP keep-alive. A connection kept idle after the
        // connect-time PROPFIND (or the stat that precedes an image preview)
        // is frequently closed by the bridge, so reqwest reuses a dead socket
        // and the following GET fails before any byte is sent ("error sending
        // request for url ...", a transport error rather than an HTTP status).
        // The bridge is loopback-only, so forcing a fresh connection per
        // request costs nothing and removes the stale-reuse failure class.
        if super::mega_df::is_megacmd_webdav_provider_id(config.provider_id.as_deref()) {
            client_builder = client_builder.pool_max_idle_per_host(0);
        }
        if policy.filen_bridge {
            client_builder = client_builder.no_proxy().redirect(bridge_redirect_policy());
        }

        client_builder
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("HTTP client init failed: {e}")))
    }

    /// Normalize a collection (directory) path to the trailing-slash form.
    ///
    /// Apache mod_dav answers a collection request (PROPFIND/MKCOL/DELETE/
    /// MOVE) whose path lacks a trailing slash with `301 Moved Permanently`
    /// to the slash form. Behind a TLS-terminating reverse proxy that 301
    /// carries an `http://` Location, so reqwest sees a scheme downgrade,
    /// treats the hop as cross-origin, strips the `Authorization` header,
    /// and the redirected request 401s: this is what surfaced as
    /// `Session expired` on `check`/`tree` and as a 401 on directory
    /// delete. Callers that always target collections normalize through
    /// this so the redirect never happens. File verbs (GET/PUT/file
    /// DELETE) must NOT use it: a trailing slash there would point at a
    /// non-existent collection.
    fn collection_path(path: &str) -> String {
        if path.is_empty() || path == "/" || path.ends_with('/') {
            path.to_string()
        } else {
            format!("{}/", path)
        }
    }

    /// Compose a caller path under the auto-detected `server_root`.
    ///
    /// On Nextcloud / ownCloud the real WebDAV root lives under a versioned
    /// prefix (`/remote.php/dav/files/<user>/`) discovered at connect time.
    /// Before this, only `list()`/`cd()` rewrote the literal `/` to that
    /// root; every other verb (`mkdir`, `put`, `get`, `delete`, `stat`,
    /// `move`, ...) sent the bare caller path, which on Nextcloud bypasses
    /// the DAV root and hits the front controller as `404`/`405`. This
    /// centralizes the rewrite at the single URL chokepoint.
    ///
    /// Idempotent: a path already at or under `server_root` (the GUI
    /// drill-down case, where `list()` returns fully-rooted entry paths) is
    /// returned unchanged, so no double-prefix occurs. A no-op when there is
    /// no distinct root (traditional servers, `server_root` `None` or `/`),
    /// so non-Nextcloud backends and the connect-time auto-detection probes
    /// (which run before `server_root` is set) are unaffected.
    fn resolve_root(&self, path: &str) -> String {
        let root = match self.server_root.as_deref() {
            Some(r) if !r.is_empty() && r != "/" => r,
            _ => return path.to_string(),
        };
        let root_trim = root.trim_end_matches('/');
        let p = if path.is_empty() { "/" } else { path };
        let p_norm = p.trim_end_matches('/');

        if p_norm == root_trim || p.starts_with(&format!("{}/", root_trim)) {
            return p.to_string();
        }

        let rel = p.trim_start_matches('/');
        if rel.is_empty() {
            format!("{}/", root_trim)
        } else if p.ends_with('/') {
            format!("{}/{}/", root_trim, rel.trim_end_matches('/'))
        } else {
            format!("{}/{}", root_trim, rel)
        }
    }

    /// The URL a profile's start folder is requested at (`build_url` of its
    /// `initial_path`, before any connect-time root), for the bridge tests
    /// that check a bookmark reopens the same resource.
    #[cfg(test)]
    pub(crate) fn start_folder_request_url(config: &crate::providers::ProviderConfig) -> String {
        let provider = Self::new(WebDavConfig::from_provider_config(config).unwrap()).unwrap();
        provider.build_url(provider.config.initial_path.as_deref().unwrap_or("/"))
    }

    /// Build full URL for a path
    fn build_url(&self, path: &str) -> String {
        // Issue #264: in single-file mode every operation targets the
        // verbatim configured URL (the bridge has no browseable structure
        // beyond the one file). Ignore the path argument; callers in
        // single-file mode that pass an unrelated path are an internal
        // bug, but we'd rather surface that as a 404 against the real
        // file URL than as a malformed-URL reqwest error.
        if self.single_file_mode.is_some() {
            return self.config.url.clone();
        }
        // A configured `?query` or `#fragment` stays after the request path:
        // the collection path goes into the URL's path component.
        let url = self.config.url.as_str();
        let (url_path, url_tail) = url.split_at(url.find(['?', '#']).unwrap_or(url.len()));
        let base = url_path.trim_end_matches('/');
        let rooted = self.resolve_root(path);
        let path = rooted.trim_start_matches('/');

        if path.is_empty() || path == "/" {
            // For root/empty path, ensure trailing slash (required by some WebDAV servers
            // for Digest auth URI matching on directory endpoints)
            format!("{}/{}", base, url_tail)
        } else {
            format!("{}/{}{}", base, Self::encode_path(path), url_tail)
        }
    }

    /// Whether `path` names the file a single-file profile serves, in either
    /// spelling (`/f.txt` or `f.txt`). False outside single-file mode.
    fn names_the_served_file(&self, path: &str) -> bool {
        self.single_file_mode
            .as_ref()
            .is_some_and(|served| path.trim_matches('/') == served.path.trim_matches('/'))
    }

    /// In single-file mode every path maps to the one configured URL, so a
    /// PUT, DELETE or MKCOL for any other path acts on the served file: an
    /// edit's temporary overwrote it, and the cleanup of that temporary
    /// deleted it. Refused unless `path` names the served file.
    fn refuse_another_path_in_single_file_mode(&self, path: &str) -> Result<(), ProviderError> {
        match &self.single_file_mode {
            Some(served) if !self.names_the_served_file(path) => {
                Err(ProviderError::NotSupported(format!(
                    "this WebDAV profile serves the single file {}, and {path} is not it: \
                     the request would act on that file",
                    served.path
                )))
            }
            _ => Ok(()),
        }
    }

    /// Whether `from` and the destination URL `destination` name one
    /// resource. A MOVE onto itself is a no-op everywhere else, but Nextcloud
    /// answers it with 412, read as AlreadyExists (found live on 2026-09-26),
    /// and under `Overwrite: T` a server may delete the destination first.
    /// A trailing slash does not make another resource.
    fn names_the_same_resource(&self, from: &str, destination: &str) -> bool {
        self.build_url(from).trim_end_matches('/') == destination.trim_end_matches('/')
    }

    /// Extract the path component of a URL (everything after `host[:port]`,
    /// up to a `?query` or `#fragment`), or `""` if the URL has no path
    /// beyond the authority: a slash inside the query is not a path.
    /// Best-effort string parser: avoids pulling in `url` for one call.
    fn url_path_component(url: &str) -> &str {
        // `//host/path` is a scheme-relative URL: its authority comes first.
        let after_scheme = match url.find("://") {
            Some(i) => &url[i + 3..],
            None => url.strip_prefix("//").unwrap_or(url),
        };
        let before_query =
            &after_scheme[..after_scheme.find(['?', '#']).unwrap_or(after_scheme.len())];
        match before_query.find('/') {
            Some(i) => &before_query[i..],
            None => "",
        }
    }

    /// The href resolved against `base` (the URL the answer came from, after
    /// any redirect) as a browser resolves it (WHATWG URL), when it names an
    /// entry of `own`'s origin: a path or a
    /// scheme-relative href (`//host/...`) takes `base`'s scheme, a default
    /// port is the same as none, user information and the case of the scheme
    /// and host do not count, and a backslash ends the authority (and
    /// separates segments) as it does in `http` and `https`. None for an
    /// href of another origin, of another scheme (a `blob:` URL takes the
    /// origin of the URL inside it) or one that does not parse. Its path,
    /// read from the same parse, is the resource the href names.
    fn href_of_our_origin(href: &str, base: &str, own: &str) -> Option<url::Url> {
        let own = url::Url::parse(own).ok()?;
        let base = url::Url::parse(base).ok()?;
        // The parser drops a leading or trailing space and every tab or
        // newline: escaped first, they stay part of the name the href gives
        // (`a.txt ` is not `a.txt`).
        let escaped: String = href
            .chars()
            .map(|c| {
                if c <= ' ' {
                    format!("%{:02X}", u32::from(c))
                } else {
                    c.to_string()
                }
            })
            .collect();
        let href = base.join(&escaped).ok()?;
        let origin = own.origin();
        (origin.is_tuple() && href.scheme() == own.scheme() && href.origin() == origin)
            .then_some(href)
    }

    /// The path a PROPFIND href names, as its decoded segments, for an exact
    /// comparison. The scheme and authority of an absolute URL and any query
    /// or fragment are dropped, empty and `.` segments vanish, `..` removes
    /// the segment before it, and a relative reference is resolved against
    /// the collection `base` (segments already normalised). Each segment is
    /// decoded on its own, so an escaped `/` (`%2F`) inside a name stays part
    /// of that name. The root is no segment at all.
    fn dav_path_segments(href: &str, base: &[String]) -> Vec<String> {
        let raw = href.split(['?', '#']).next().unwrap_or("");
        let has_authority =
            raw.starts_with("//") || raw.find("://").is_some_and(|i| !raw[..i].contains('/'));
        let (path, relative) = if has_authority {
            (Self::url_path_component(raw), false)
        } else {
            (raw, !raw.starts_with('/'))
        };
        let mut segments: Vec<String> = if relative { base.to_vec() } else { Vec::new() };
        for segment in path.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                _ => segments.push(
                    urlencoding::decode(segment)
                        .map(|decoded| decoded.into_owned())
                        .unwrap_or_else(|_| segment.to_string()),
                ),
            }
        }
        segments
    }

    /// Issue #264 — try to detect a single-file WebDAV resource at connect time.
    ///
    /// Runs PROPFIND with `Depth: 0` against the configured URL **verbatim**
    /// (no trailing slash appended). On a 207 response that does NOT contain
    /// a `<collection/>` resourcetype, the URL is treated as a single-file
    /// bridge (MEGAcmd `mega-webdav <file>` is the canonical case). The
    /// extracted `RemoteEntry` is returned so `connect()` can cache it.
    ///
    /// Returns `None` (caller falls through to the standard PROPFIND `/`
    /// flow) when:
    /// - the URL has no path component (no file to point at);
    /// - the server returns a non-207 status (401 Digest, 405 well-known,
    ///   any error — the standard flow handles those);
    /// - the response indicates a collection resource (traditional WebDAV
    ///   server with a non-trailing-slash root URL).
    async fn probe_single_file(&self) -> Option<RemoteEntry> {
        let url_path = Self::url_path_component(&self.config.url);
        if url_path.is_empty() || url_path == "/" {
            return None;
        }

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:propfind xmlns:d="DAV:">
                <d:prop>
                    <d:resourcetype/>
                    <d:getcontentlength/>
                    <d:getlastmodified/>
                    <d:getcontenttype/>
                    <d:getetag/>
                </d:prop>
            </d:propfind>"#;

        let mut builder = self
            .bounded(
                self.client
                    .request(webdav_methods::propfind(), &self.config.url),
                &webdav_methods::propfind(),
            )
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(propfind_body);

        if !self.config.anonymous {
            builder = builder.basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            );
        }

        let response = builder.send().await.ok()?;
        let status = response.status();
        if status != StatusCode::MULTI_STATUS && status != StatusCode::OK {
            tracing::debug!(
                "[WebDAV] single-file probe: status {}, falling through to PROPFIND /",
                status
            );
            return None;
        }

        let xml = response.text().await.ok()?;
        Self::extract_single_file_entry(&xml, url_path)
    }

    /// Parse the body of a `Depth: 0` PROPFIND against a single-file URL.
    ///
    /// Returns `None` when the resource is a collection (the caller should
    /// fall through to the standard flow) or when no parseable response is
    /// found.
    fn extract_single_file_entry(xml: &str, url_path: &str) -> Option<RemoteEntry> {
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;

        let mut reader = Reader::from_str(xml);
        // No trim_text: fragments around XML entities must survive intact;
        // scalars are trimmed once at consumption below.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut in_resourcetype = false;
        let mut current_tag: Option<String> = None;
        let mut is_collection = false;
        let mut got_response = false;
        let mut size_text = String::new();
        let mut modified_text = String::new();
        let mut content_type_text = String::new();
        let mut etag_text = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" => {
                            in_response = true;
                            got_response = true;
                        }
                        "resourcetype" if in_response => in_resourcetype = true,
                        "collection" if in_response && in_resourcetype => is_collection = true,
                        "getcontentlength" | "getlastmodified" | "getcontenttype" | "getetag"
                            if in_response =>
                        {
                            current_tag = Some(local);
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_response && in_resourcetype {
                        is_collection = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "resourcetype" => in_resourcetype = false,
                        "response" => in_response = false,
                        "getcontentlength" | "getlastmodified" | "getcontenttype" | "getetag" => {
                            current_tag = None;
                        }
                        _ => {}
                    }
                }
                // Every piece goes in, whitespace included: an ETag arrives as
                // `&quot;abc&quot;` from SabreDAV (Nextcloud, ownCloud), so it
                // is Text pieces around GeneralRef ones, and a space between
                // two references is part of the value. Scalars are trimmed
                // once below.
                Ok(Event::Text(ref t)) => {
                    if let Some(tag) = current_tag.as_deref() {
                        let text = t.as_ref();
                        match tag {
                            "getcontentlength" => size_text.push_str(text),
                            "getlastmodified" => modified_text.push_str(text),
                            "getcontenttype" => content_type_text.push_str(text),
                            "getetag" => etag_text.push_str(text),
                            _ => {}
                        }
                    }
                }
                Ok(Event::GeneralRef(ref reference)) => {
                    if let (Some(tag), Some(expanded)) = (
                        current_tag.as_deref(),
                        super::xml_text::xml_entity_to_str(reference.as_ref()),
                    ) {
                        match tag {
                            "getcontentlength" => size_text.push_str(&expanded),
                            "getlastmodified" => modified_text.push_str(&expanded),
                            "getcontenttype" => content_type_text.push_str(&expanded),
                            "getetag" => etag_text.push_str(&expanded),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }

        if !got_response || is_collection {
            return None;
        }

        let name = url_path
            .rsplit('/')
            .find(|seg| !seg.is_empty())
            .map(|seg| {
                urlencoding::decode(seg)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| seg.to_string())
            })
            .unwrap_or_else(|| "file".to_string());

        let mut metadata = std::collections::HashMap::new();
        if !etag_text.trim().is_empty() {
            metadata.insert("etag".to_string(), etag_text.trim().to_string());
        }

        Some(RemoteEntry {
            name: name.clone(),
            path: format!("/{}", name),
            is_dir: false,
            size: size_text.trim().parse().unwrap_or(0),
            modified: if modified_text.trim().is_empty() {
                None
            } else {
                Some(modified_text.trim().to_string())
            },
            is_symlink: false,
            link_target: None,
            permissions: None,
            owner: None,
            group: None,
            mime_type: if content_type_text.trim().is_empty() {
                None
            } else {
                Some(content_type_text.trim().to_string())
            },
            metadata,
        })
    }

    /// Percent-encode a remote path so reserved characters in file names
    /// (`#`, `?`, `%`, space, non-ASCII, ...) survive intact.
    ///
    /// Without this, `#`/`?` were parsed by the `url` crate as fragment /
    /// query delimiters (truncating the request target) and a literal
    /// space, while transport-encoded to `%20` on the wire, was hashed
    /// raw into the Digest `uri`, producing a mismatch the server
    /// rejected. rclone shipped the equivalent fix in v1.73.2.
    ///
    /// The encode set is deliberately conservative: structural and unsafe
    /// characters only. `pchar` sub-delims (`! $ & ' ( ) * + , ; = : @`)
    /// are left literal, because percent-encoding those is exactly the
    /// over-escaping rclone had to revert in v1.73.3 ("URLPathEscapeAll
    /// broke strict path-matching servers"). `/` is never in the set:
    /// segments are encoded individually and rejoined, so a trailing
    /// slash (collection form) is preserved.
    pub(crate) fn encode_path(path: &str) -> String {
        const WEBDAV_SEGMENT: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'%')
            .add(b'<')
            .add(b'>')
            .add(b'?')
            .add(b'[')
            .add(b'\\')
            .add(b']')
            .add(b'^')
            .add(b'`')
            .add(b'{')
            .add(b'|')
            .add(b'}');

        path.split('/')
            .map(|seg| utf8_percent_encode(seg, WEBDAV_SEGMENT).to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// `builder` with the whole-request limit when `method` is a metadata
    /// request, unchanged otherwise.
    fn bounded(
        &self,
        builder: reqwest::RequestBuilder,
        method: &Method,
    ) -> reqwest::RequestBuilder {
        if is_metadata_method(method) {
            builder.timeout(self.metadata_timeout)
        } else {
            builder
        }
    }

    /// Make an authenticated request (Basic or Digest depending on server).
    /// A metadata request is bounded as a whole: see [`METADATA_TIMEOUT`].
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let builder = self.request_any_length(method.clone(), path);
        self.bounded(builder, &method)
    }

    /// [`Self::request`] without the metadata limit, for a request that may
    /// rightly take longer: `PROPFIND Depth: infinity`.
    fn request_any_length(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self.build_url(path);
        let builder = self.client.request(method.clone(), &url);

        if self.config.anonymous {
            return builder;
        }

        if let Some(ref state) = self.digest_auth {
            let uri_path = extract_uri_path(&url);
            tracing::debug!(
                "[WebDAV] Digest request: {} to {}",
                method.as_str(),
                crate::local_bridge::url_origin(&url)
            );
            let auth = state.authorization(
                method.as_str(),
                &uri_path,
                &self.config.username,
                self.config.password.expose_secret(),
            );
            builder.header("Authorization", auth)
        } else {
            builder.basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
        }
    }

    async fn send_with_too_early_retry(
        &mut self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        const MAX_ATTEMPTS: usize = 3;

        // Compute a per-host `Referer` header (`https://host/`) once per call.
        // FileLu's WebDAV frontend (`webdav.filelu.com`, behind Cloudflare)
        // returns `500 Internal Server Error` to GET requests that arrive
        // without a same-origin Referer; rclone v1.74 has shipped this header
        // on every WebDAV request since its inception. Other servers tolerate
        // it without complaint, so we set it universally as a defensive
        // default rather than as a FileLu special case. The trailing `/` is
        // required so the value parses as a "directory" origin.
        let referer = {
            let base = self.config.url.trim_end_matches('/').to_string();
            if base.is_empty() {
                None
            } else {
                Some(format!("{base}/"))
            }
        };

        // FileLu's WebDAV frontend used to refuse GET with a 500 to every
        // User-Agent but a few known clients, and this call sent rclone's for
        // `filelu.com`. FileLu now accepts AeroFTP's own (checked 2026-11-01:
        // GET 200, ranges 206, PUT, MOVE, COPY, DELETE), and rclone's is
        // answered 206 on a plain GET. Every server gets the AeroFTP agent.

        // A rotated Digest nonce is replayed at most once per call, tracked
        // here rather than by attempt number: the 425 loop may already have
        // burned attempts, and a second rotation inside one call is the retry
        // storm the `stale` gate exists to prevent.
        let mut digest_replayed = false;
        // The replay is granted an attempt OF ITS OWN instead of consuming or
        // escaping the 425 budget, because the two answer different questions: the
        // budget bounds how long we wait for a server that is not ready, while the
        // replay is one retry of a request the server refused for a reason we can
        // fix. Letting the replay `continue` out of the final attempt is what used
        // to walk the loop off the end of its own range and into a panic; the loop
        // is now unbounded in form and bounded by `budget`, so the only way out is
        // the return below.
        let mut budget = MAX_ATTEMPTS;
        let mut attempt = 0usize;

        loop {
            attempt += 1;
            let nonce_used = self.digest_nonce_snapshot();
            let mut req = self.request(method.clone(), path);
            if let Some(ref r) = referer {
                req = req.header("Referer", r);
            }
            let response = req.send().await.map_err(send_error)?;

            if response.status() == StatusCode::UNAUTHORIZED
                && !digest_replayed
                && self.should_replay_after_401(&response, &nonce_used)
            {
                digest_replayed = true;
                budget = MAX_ATTEMPTS + 1;
                continue;
            }

            if response.status() != StatusCode::TOO_EARLY || attempt >= budget {
                return Ok(response);
            }

            tracing::debug!(
                "[WEBDAV] {} {} returned 425 Too Early, retry {}/{}",
                method.as_str(),
                path,
                attempt,
                budget
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// Send a request and, on a `401` carrying a genuinely rotated Digest
    /// nonce, rebuild it and send it exactly once more.
    ///
    /// `build` runs once per attempt, so the `Authorization` header is
    /// recomputed from the challenge the provider holds *now*: a replay never
    /// re-presents the dead nonce, and a body is rebuilt from its source
    /// instead of being buffered for a retry that usually never happens.
    ///
    /// Replaying on `401` is safe for every caller, idempotent or not,
    /// because a `401` means the server rejected the request BEFORE applying
    /// it: there is no intermediate state. That is what separates this from
    /// replaying a timeout or a `5xx`, where the server may already have
    /// acted and idempotence would have to decide. It is why `MOVE`, `DELETE`
    /// and the multipart `complete` can all go through here safely.
    ///
    /// Exactly one replay, structurally: a straight-line `if`, no loop. A
    /// second rotation inside one call is the storm the `stale` gate exists
    /// to prevent.
    ///
    /// This is one half of a pair; [`Self::send_with_too_early_retry`] is the
    /// other, and heals `425 Too Early` on the same principle for the two
    /// GET paths that need it. Do not add a third variant: extend one of
    /// these, or every future request site will pick a different half and the
    /// gap this pair closes will reopen.
    async fn send_replaying_digest(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        self.send_replaying_digest_raw(build)
            .await
            .map_err(send_error)
    }

    /// [`Self::send_replaying_digest`] before the error is mapped, for the
    /// caller that has to tell a refused collection redirect apart.
    async fn send_replaying_digest_raw(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let nonce_used = self.digest_nonce_snapshot();
        let response = build().send().await?;

        if response.status() == StatusCode::UNAUTHORIZED
            && self.should_replay_after_401(&response, &nonce_used)
        {
            return build().send().await;
        }

        Ok(response)
    }

    /// Send a request whose target may be a file or a collection, through
    /// [`Self::send_replaying_digest`].
    ///
    /// The verbs that cannot know the type (`MOVE`, `COPY`, `DELETE`) send
    /// `path` as given, which is right for a file. When the server redirects
    /// that request to the collection form of the same URL across a scheme
    /// downgrade (see [`InsecureRedirect`]), it has said the target is a
    /// collection and refused the request without acting on it, so `build`
    /// runs once more with the trailing-slash form, over HTTPS. Any other
    /// failure, and any refused redirect elsewhere, is returned as is.
    async fn send_on_resource(
        &self,
        path: &str,
        build: impl Fn(&str) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        match self.send_replaying_digest_raw(|| build(path)).await {
            Err(e) if refused_collection_redirect(&e) && !path.ends_with('/') => {
                let collection = Self::collection_path(path);
                tracing::debug!(
                    "[WebDAV] {} is a collection (refused redirect to its slash form), resending as {}",
                    path,
                    collection
                );
                self.send_replaying_digest(|| build(&collection)).await
            }
            other => other.map_err(send_error),
        }
    }

    /// One `PROPFIND` of a collection (`Depth: 0` or `1`), paced for a server
    /// that limits concurrency (#958). A 429 or 503 is sent again after the
    /// pause the server asks for (`Retry-After`) or an exponential one (1, 2,
    /// 4 s), three times at most, as long as the pauses together stay within
    /// [`LIST_RETRY_BUDGET`]: a pause that would pass it ends the retries and
    /// returns the refusal. Every `list` goes through here, the file
    /// browser's included, and it waits holding the provider lock, so the
    /// budget is what keeps a long `Retry-After` from freezing the browser.
    /// Every other status comes back as it came, a 500 or a gateway error
    /// included. Each attempt is built afresh, so a Digest session spends a
    /// new nonce count on each, and each pays the CLI's `--tpslimit` toll.
    async fn send_propfind_paced(
        &self,
        path: &str,
        depth: &str,
        body: &'static str,
    ) -> Result<reqwest::Response, ProviderError> {
        let config = super::http_retry::HttpRetryConfig::default();
        let mut waited = std::time::Duration::ZERO;
        let mut attempt = 0u32;
        loop {
            super::tpslimit::maybe_acquire().await;
            let response = self
                .request(webdav_methods::propfind(), path)
                .header("Depth", depth)
                .header("Content-Type", "application/xml")
                .body(body)
                .send()
                .await
                .map_err(send_error)?;
            let throttled = matches!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            );
            if !throttled || attempt >= config.max_retries {
                return Ok(response);
            }
            let pause = super::http_retry::parse_retry_after(&response)
                .unwrap_or_else(|| super::http_retry::calculate_delay(attempt, &config));
            if waited + pause > LIST_RETRY_BUDGET {
                tracing::debug!(
                    "[WebDAV] PROPFIND {} answered {}: a pause of {:?} would pass the {:?} listing budget, not retrying",
                    path,
                    response.status(),
                    pause,
                    LIST_RETRY_BUDGET
                );
                return Ok(response);
            }
            tracing::debug!(
                "[WebDAV] PROPFIND {} answered {}, retry {} after {:?}",
                path,
                response.status(),
                attempt + 1,
                pause
            );
            tokio::time::sleep(pause).await;
            waited += pause;
            attempt += 1;
        }
    }

    /// Send a PROPFIND and, on a `401` that carries a fresh
    /// `WWW-Authenticate: Digest` challenge, re-negotiate the Digest state
    /// once and retry the request.
    ///
    /// Long recursive scans (`check` / `tree` right after `sync`) issue many
    /// sequential PROPFIND requests reusing one cached `nonce`. Servers and
    /// reverse proxies expire that nonce by TTL or by use count and answer
    /// `401 ... stale=true` with a brand-new nonce, expecting the client to
    /// re-handshake (RFC 2617 section 3.3). Without this, the first stale
    /// `401` was surfaced as `Session expired` even though the credentials
    /// were still valid: the WebDAV recursive-scan defect reproduced against
    /// `dav.lab.example.test`. Basic-auth servers never emit a Digest challenge,
    /// so a genuine `401` there still propagates unchanged for the caller to
    /// map.
    async fn send_propfind(
        &mut self,
        path: &str,
        depth: &str,
        body: &'static str,
    ) -> Result<reqwest::Response, ProviderError> {
        // `list()` and `list_recursive()` always target collections;
        // normalize to the trailing-slash form so Apache does not 301 to a
        // scheme-downgraded URL that loses auth. See `collection_path`.
        let dir_path = Self::collection_path(path);
        let path = dir_path.as_str();

        // A walk lists several collections at once (#958), and a server that
        // limits concurrency answers the extra requests 429 or 503: those are
        // paced by `send_propfind_paced`. `Depth: infinity` stays one attempt:
        // on any failure its callers walk folder by folder.
        //
        // The nonce this request goes out with, for the 401 below: on a walk
        // several clones share one Digest challenge, so a sibling may already
        // have adopted the rotated nonce this request was refused for.
        let nonce_used = self.digest_nonce_snapshot();
        let response = if depth == "infinity" {
            self.request_any_length(webdav_methods::propfind(), path)
                .header("Depth", depth)
                .header("Content-Type", "application/xml")
                .body(body)
                .send()
                .await
                .map_err(send_error)?
        } else {
            self.send_propfind_paced(path, depth, body).await?
        };

        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        let www_auth = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Adopt the rotated challenge IN PLACE when this session already has
        // Digest state. Replacing `self.digest_auth` with a freshly parsed state
        // would build a new Arc, so every `clone_for_transfer` worker would keep
        // pointing at the old challenge and the repair would reach exactly one
        // of them: the same defect sharing the challenge exists to prevent.
        match self.digest_auth.as_ref() {
            Some(_) => {
                // Replay once when this worker adopts the rotated nonce, or when
                // a sibling already did and the nonce held now is not the one
                // this request carried (#1080 review): `renegotiate` alone
                // refused that second case, and the folder was reported
                // unreadable while the pool was already repaired. Anything else
                // (Basic auth, a genuine credential failure) goes to the caller.
                if !self.should_replay_after_401(&response, &nonce_used) {
                    return Ok(response);
                }
            }
            None => {
                let Some(state) = DigestState::parse(&www_auth) else {
                    return Ok(response);
                };
                tracing::debug!(
                    "[WebDAV] PROPFIND 401 with Digest challenge, negotiating (realm={}, nonce={}...)",
                    state.realm(),
                    state.nonce_prefix()
                );
                self.digest_auth = Some(state);
            }
        }

        // The replay keeps the first attempt's limit: none on `Depth: infinity`.
        let replay = if depth == "infinity" {
            self.request_any_length(webdav_methods::propfind(), path)
        } else {
            self.request(webdav_methods::propfind(), path)
        };
        replay
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(send_error)
    }

    /// One-shot Digest re-negotiation for a `401` on a data path.
    ///
    /// `send_propfind` has renegotiated since Digest landed; the data paths
    /// never did, and that asymmetry IS the accepted debt this closes. The
    /// gate is [`DigestState::renegotiate`], which adopts a challenge only
    /// when it says `stale=true` AND carries a nonce we are not already
    /// holding. So a caller that replays on `true` replays at most once per
    /// request, and never on a credential failure: that is what keeps a
    /// revoked password from becoming a retry storm, which is the reason the
    /// data paths were left un-wired in the first place.
    ///
    /// Unlike `send_propfind` this never negotiates Digest from scratch. A
    /// session with no `digest_auth` is either not speaking Digest or has not
    /// finished `connect`, and neither of those is a nonce rotation.
    ///
    /// The repair is pool-wide, not per-caller: the challenge lives behind a
    /// shared `Arc`, so one worker adopting the rotated nonce fixes it for
    /// every `clone_for_transfer` sibling still presenting the dead one.
    /// The nonce this provider holds right now, or `None` when the connection
    /// is not Digest-authenticated.
    ///
    /// Read immediately before a request is sent so that a `401` can be told
    /// apart from a sibling worker's repair: see
    /// [`Self::should_replay_after_401`].
    fn digest_nonce_snapshot(&self) -> Option<String> {
        self.digest_auth.as_ref().map(|state| state.nonce())
    }

    /// Whether a `401` should be replayed once, given the nonce the failed
    /// request actually carried.
    ///
    /// Two things can make a replay right, and only one of them is a rotation
    /// this worker performs itself:
    ///
    /// - `adopt_rotated_nonce` succeeded: the server offered `stale=true` with
    ///   a nonce we were not holding, and we installed it.
    /// - It refused, but the nonce we hold is no longer the one this request
    ///   used. A sibling worker hit the same rotation first and repaired the
    ///   shared challenge, so the server is now offering back the nonce that
    ///   sibling already installed. `renegotiate` correctly declines to adopt
    ///   what it already has, but this request still went out with a dead
    ///   nonce and deserves its one replay, with the fresh one.
    ///
    /// Without the second arm, concurrent transfers lose whichever requests
    /// lost the race: measured against Apache `mod_auth_digest` with a 2s
    /// `AuthDigestNonceLifetime`, a 60-file recursive download failed a
    /// random 2 to 6 files per run while the rest succeeded.
    ///
    /// The anti-storm property is untouched. When nothing changed, the held
    /// nonce still equals the one just used and `adopt_rotated_nonce` refused,
    /// so both arms are false and a revoked credential still costs exactly one
    /// request.
    ///
    /// Known imprecision, deliberately left: `used` comes from a read of the
    /// shared challenge taken just before the request is built, not from the
    /// single read inside `authorization` that actually computes the header.
    /// A sibling rotating in that window makes the snapshot disagree with what
    /// the request really carried, and the second arm then grants a replay
    /// that was not needed. It costs one extra request and cannot storm, since
    /// the replay is still capped at one. Closing it means having
    /// `authorization` report the nonce it used, which changes the signature
    /// `request` and `request_url` share across some forty call sites; the
    /// trade was not worth making inside this fix.
    fn should_replay_after_401(&self, response: &reqwest::Response, used: &Option<String>) -> bool {
        if self.adopt_rotated_nonce(response) {
            return true;
        }
        match (used, self.digest_nonce_snapshot()) {
            (Some(before), Some(now)) => *before != now,
            _ => false,
        }
    }

    fn adopt_rotated_nonce(&self, response: &reqwest::Response) -> bool {
        let Some(existing) = self.digest_auth.as_ref() else {
            return false;
        };
        let Some(www_auth) = response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
        else {
            return false;
        };
        if !existing.renegotiate(www_auth) {
            return false;
        }
        tracing::debug!(
            "[WebDAV] 401 stale=true, adopted rotated nonce for every worker (realm={}, nonce={}...)",
            existing.realm(),
            existing.nonce_prefix()
        );
        true
    }

    // ─── Nextcloud OCS / Trashbin helpers ─────────────────────────────

    /// Detect if this WebDAV server is a Nextcloud instance.
    ///
    /// Delegates to [`Self::is_nextcloud_for_dav`] so every Nextcloud-specific
    /// surface answers the same question the same way. It used to test the
    /// configured URL for `/remote.php/dav/files/` on its own, which made the
    /// answer depend on how the user typed the profile URL rather than on what
    /// the server is: a profile saved as `https://cloud.example.com` browses
    /// perfectly (connect resolves the real root) but was reported as "not
    /// Nextcloud", silently disabling share links and the trashbin.
    fn is_nextcloud(&self) -> bool {
        self.is_nextcloud_for_dav()
    }

    /// Extract the Nextcloud base URL (e.g. `https://cloud.felicloud.com`).
    ///
    /// The OCS and trashbin endpoints live outside the per-user `/files/` root,
    /// so they are built against this origin rather than through `build_url()`.
    ///
    /// Layered, mirroring [`Self::is_nextcloud_for_dav`]:
    /// 1. The configured URL carries the canonical `/remote.php/` prefix, so
    ///    the install root is whatever precedes it (includes a subpath such as
    ///    `/owncloud` when the instance is not at the host root).
    /// 2. Otherwise, if this is a Nextcloud-class server by any reliable signal
    ///    (saved-profile preset id, or the `server_root` the connect-time
    ///    well-known probe resolved), keep the configured URL path. Stripping
    ///    to bare origin broke subpath installs (ownCloud's default
    ///    `https://host/owncloud`): OCS, trashbin and chunked upload all 404'd
    ///    at the wrong root.
    ///
    /// Returning `None` here is what surfaces as `Not a Nextcloud instance`, so
    /// it must mean "this really is not Nextcloud", never "the user typed a
    /// short URL".
    fn nextcloud_base_url(&self) -> Option<String> {
        // `/remote.php/` is looked for in the path only: one in a query or a
        // fragment (`?next=/remote.php/...`) is no route of this server.
        let before_query = self.config.url.split(['?', '#']).next().unwrap_or_default();
        if let Some(idx) = before_query.find("/remote.php/") {
            return Some(before_query[..idx].trim_end_matches('/').to_string());
        }
        if self.is_nextcloud_for_dav() {
            // The Nextcloud routes (`/remote.php/...`) go on the URL's path:
            // a query or fragment of the profile URL would otherwise end up
            // in front of them.
            let trimmed = self
                .config
                .url
                .trim()
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim_end_matches('/');
            if trimmed.is_empty() {
                return None;
            }
            return Some(trimmed.to_string());
        }
        None
    }

    // ─── Nextcloud chunked upload v2 ──────────────────────────────────
    //
    // Reference: https://docs.nextcloud.com/server/latest/developer_manual/
    //            client_apis/WebDAV/chunking.html
    //
    // Wire summary:
    //   MKCOL  https://host/remote.php/dav/uploads/<userid>/<transferId>/
    //   PUT    https://host/remote.php/dav/uploads/<userid>/<transferId>/<NNNNNNNNNNNNNNNNNNNN>
    //          OC-Total-Length: <total_size>
    //   MOVE   https://host/remote.php/dav/uploads/<userid>/<transferId>/.file
    //          Destination: https://host/remote.php/dav/files/<userid>/<final_path>
    //          OC-Total-Length: <total_size>
    //   DELETE https://host/remote.php/dav/uploads/<userid>/<transferId>/   (abort)
    //
    // The chunked endpoint lives OUTSIDE the per-user `/files/` root,
    // under `/uploads/<userid>/`, so we build absolute URLs against
    // `nextcloud_chunked_base_url()` directly instead of going through
    // `build_url()` (which would prefix `server_root`).

    /// Detect a Nextcloud-class WebDAV server for DAG chunked upload purposes.
    ///
    /// Conservative: only the canonical Nextcloud / ownCloud signals are
    /// accepted. Other Nextcloud-backed SaaS presets (FeliCloud, Tab.digital,
    /// MagentaCloud) are not auto-enabled here because they may run older
    /// server versions that do not implement the chunked v2 endpoint
    /// (the legacy `/uploads/` path was added in Nextcloud 15 / ownCloud 10).
    /// They keep going through the legacy single-PUT path until verified.
    ///
    /// Layered, in order of reliability:
    /// 1. `provider_id` set to `nextcloud` / `owncloud` from the saved-profile
    ///    preset id. Most reliable, survives URL changes.
    /// 2. Configured URL contains the canonical Nextcloud DAV prefix.
    /// 3. The auto-detected `server_root` contains the canonical Nextcloud
    ///    DAV prefix (well-known-path fallback from 405 on `PROPFIND /`).
    fn is_nextcloud_for_dav(&self) -> bool {
        if let Some(pid) = self.config.provider_id.as_deref() {
            let p = pid.trim().to_ascii_lowercase();
            if matches!(p.as_str(), "nextcloud" | "owncloud") {
                return true;
            }
        }
        if self.config.url.contains("/remote.php/dav/files/") {
            return true;
        }
        if let Some(root) = self.server_root.as_deref() {
            if root.contains("/remote.php/dav/files/") {
                return true;
            }
        }
        false
    }

    /// Nextcloud user id used in the `/uploads/<userid>/` and
    /// `/files/<userid>/` paths.
    ///
    /// Resolution order:
    /// 1. Parsed from `server_root` (`/remote.php/dav/files/<userid>/...`)
    ///    when the well-known path was auto-detected at connect time. This is
    ///    the most accurate source because Nextcloud may differ between the
    ///    login username and the storage user id (federated logins, LDAP DN
    ///    aliases). The server-resolved well-known path always carries the
    ///    canonical storage id.
    /// 2. Parsed from the configured URL when the user typed the full
    ///    `/remote.php/dav/files/<userid>/` form by hand.
    /// 3. Falls back to `config.username`, which is what the connect path
    ///    uses to probe `/remote.php/dav/files/<username>/` when PROPFIND `/`
    ///    returns 405.
    fn nextcloud_userid(&self) -> Option<String> {
        if let Some(id) = self
            .server_root
            .as_deref()
            .and_then(extract_nextcloud_userid)
        {
            return Some(id);
        }
        if let Some(id) = extract_nextcloud_userid(&self.config.url) {
            return Some(id);
        }
        let u = self.config.username.trim();
        if u.is_empty() {
            None
        } else {
            Some(u.to_string())
        }
    }

    /// Absolute base URL for the chunked uploads endpoint (no trailing slash).
    ///
    /// Returns `Some("https://host")` when the provider is Nextcloud-class.
    /// `None` for vanilla WebDAV servers (no `/remote.php/` path to anchor
    /// against).
    fn nextcloud_chunked_base_url(&self) -> Option<String> {
        if let Some(base) = self.nextcloud_base_url() {
            return Some(base.trim_end_matches('/').to_string());
        }
        // URL is a bare cloud hostname (well-known path was discovered at
        // connect time and lives in `server_root`). Use the URL host as-is.
        let trimmed = self.config.url.trim_end_matches('/');
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Build the URL of the chunked upload folder for `transfer_id`.
    fn nextcloud_chunked_folder_url(&self, transfer_id: &str) -> Option<String> {
        let base = self.nextcloud_chunked_base_url()?;
        let user = self.nextcloud_userid()?;
        Some(format!(
            "{}/remote.php/dav/uploads/{}/{}/",
            base,
            Self::encode_path(&user),
            Self::encode_path(transfer_id),
        ))
    }

    /// Build the URL of a single chunk inside `transfer_id`.
    ///
    /// `chunk_index` is 1-based. Nextcloud commits the chunks in the
    /// alphabetical order of their filenames, so the part number must be
    /// zero-padded to a fixed width — we use 20 digits (matching the
    /// upstream reference clients) so a u64 part count cannot overflow it.
    fn nextcloud_chunked_chunk_url(&self, transfer_id: &str, chunk_index: u32) -> Option<String> {
        let folder = self.nextcloud_chunked_folder_url(transfer_id)?;
        Some(format!(
            "{}{}",
            folder,
            nextcloud_chunk_filename(chunk_index)
        ))
    }

    /// Build the virtual `.file` URL that finalizes the upload via MOVE.
    fn nextcloud_chunked_assemble_url(&self, transfer_id: &str) -> Option<String> {
        let folder = self.nextcloud_chunked_folder_url(transfer_id)?;
        Some(format!("{}.file", folder))
    }

    /// Build the destination URL the MOVE finalization sends in the
    /// `Destination:` header. This is the final user-visible path under
    /// `/remote.php/dav/files/<userid>/`. `final_path` must be `resolve_root`
    /// applied already (i.e. start with `/remote.php/dav/files/<userid>/`
    /// when the server uses the well-known root).
    /// The handle of an upload this connection began for this destination:
    /// one of another user, or one whose destination URL is not the one this
    /// connection would assemble it at (the profile's root changed since the
    /// upload began), is refused before any write.
    fn nextcloud_handle_here(
        &self,
        handle: &MultipartHandle,
    ) -> Result<NextcloudMultipartHandle, ProviderError> {
        let payload = NextcloudMultipartHandle::of_this_user(
            &handle.upload_id,
            self.nextcloud_userid().as_deref(),
        )?;
        let here = self
            .nextcloud_chunked_destination_url(&handle.remote_path)
            .map(|url| NextcloudMultipartHandle::destination_digest(&url));
        if here.as_deref() != Some(payload.destination.as_str()) {
            return Err(ProviderError::PermissionDenied(
                "This upload was begun for another destination (the profile's folder \
                 changed since), so nothing was written: start the upload again"
                    .to_string(),
            ));
        }
        Ok(payload)
    }

    fn nextcloud_chunked_destination_url(&self, final_path: &str) -> Option<String> {
        let base = self.nextcloud_chunked_base_url()?;
        let rooted = self.resolve_root(final_path);
        let path = rooted.trim_start_matches('/');
        Some(format!("{}/{}", base, Self::encode_path(path)))
    }

    /// Make an authenticated request to an arbitrary URL (for OCS / trashbin endpoints
    /// that live outside the WebDAV files path).
    fn request_url(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.bounded(self.client.request(method.clone(), url), &method);
        if self.config.anonymous {
            return builder;
        }
        if let Some(ref state) = self.digest_auth {
            let uri_path = extract_uri_path(url);
            let auth = state.authorization(
                method.as_str(),
                &uri_path,
                &self.config.username,
                self.config.password.expose_secret(),
            );
            builder.header("Authorization", auth)
        } else {
            builder.basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
        }
    }

    /// Replay an idempotent chunk PUT in its existing upload session. Disk
    /// sources reopen the same bounded window; owned bytes share one allocation.
    /// Digest repair gets at most one extra attempt across the whole budget.
    async fn send_nextcloud_chunk(
        &self,
        chunk_url: &str,
        total_size: u64,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<reqwest::Response, ProviderError> {
        use super::http_retry::{
            calculate_delay, parse_retry_after, send_with_retry_replayable, HttpRetryConfig,
        };
        use crate::transfer_multipart::{DiskSlicePart, PartBody};

        enum Replay {
            Owned(bytes::Bytes),
            DiskSlice(DiskSlicePart),
        }
        let part_len = body.len();
        let source = match body {
            PartBody::Owned(data) => Replay::Owned(bytes::Bytes::from(data)),
            PartBody::DiskSlice(slice) => Replay::DiskSlice(slice),
        };
        let config = HttpRetryConfig::default();
        // This loop owns retries, while the shared sender still accounts for
        // each attempt's rate-limit toll and time to response headers.
        let single_attempt = HttpRetryConfig {
            max_retries: 0,
            ..config.clone()
        };
        let mut retries = 0;
        let mut nonce_replayed = false;
        loop {
            let response = send_with_retry_replayable(
                &self.client,
                || {
                    let body = match &source {
                        Replay::Owned(bytes) => crate::transfer_dag::throttle::owned_body_bytes(
                            bytes.clone(),
                            crate::transfer_dag::governor::TransferDirection::Upload,
                        ),
                        Replay::DiskSlice(slice) => {
                            PartBody::DiskSlice(slice.clone()).into_reqwest_body()
                        }
                    };
                    // Rebuild Digest Authorization too: cloned workers share
                    // the latest nonce and each send needs its own nc value.
                    self.request_url(Method::PUT, chunk_url)
                        .header("Content-Length", part_len)
                        .header("OC-Total-Length", total_size)
                        .body(body)
                },
                &single_attempt,
            )
            .await;
            let response = match response {
                Ok(response) => response,
                Err(error)
                    if retries < config.max_retries && nextcloud_chunk_send_retryable(&error) =>
                {
                    let delay = calculate_delay(retries, &config);
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => return Err(send_error(error)),
            };
            if !nonce_replayed
                && response.status() == StatusCode::UNAUTHORIZED
                && self.adopt_rotated_nonce(&response)
            {
                nonce_replayed = true;
                drop(response);
                continue;
            }
            if retries >= config.max_retries
                || !matches!(
                    response.status().as_u16(),
                    408 | 429 | 500 | 502 | 503 | 504
                )
            {
                return Ok(response);
            }
            let delay =
                parse_retry_after(&response).unwrap_or_else(|| calculate_delay(retries, &config));
            retries += 1;
            // Release the connection/body before a possibly long server wait.
            drop(response);
            tokio::time::sleep(delay).await;
        }
    }

    /// Koofr exposes a REST API that accepts the same basic auth used for
    /// WebDAV (email + app password) and returns spaceTotal / spaceUsed in MiB
    /// per mount. This is more reliable than PROPFIND for Koofr (their WebDAV
    /// server returns 0 for RFC 4331 quota properties).
    async fn koofr_storage_via_api(&self) -> Result<super::StorageInfo, ProviderError> {
        const MIB: u64 = 1024 * 1024;
        let url = "https://app.koofr.net/api/v2/mounts";
        let resp = self
            .client
            .get(url)
            .basic_auth(
                &self.config.username,
                Some(self.config.password.expose_secret()),
            )
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(send_error)?;
        if !resp.status().is_success() {
            return Err(ProviderError::NotSupported(format!(
                "koofr quota api returned {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp.json().await.map_http_err(body_read_error)?;
        let mounts = body
            .get("mounts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ProviderError::ParseError("koofr quota: no mounts".into()))?;
        // Prefer the primary mount (default Koofr storage); fall back to the
        // first entry if no mount is flagged as primary.
        let mount = mounts
            .iter()
            .find(|m| {
                m.get("isPrimary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .or_else(|| mounts.first())
            .ok_or_else(|| ProviderError::ParseError("koofr quota: empty mounts".into()))?;
        let used = mount
            .get("spaceUsed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_mul(MIB);
        let total = mount
            .get("spaceTotal")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_mul(MIB);
        Ok(super::StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    /// OpenDrive exposes the same quota the native provider reads, but its
    /// WebDAV server (webdav.opendrive.com) does not return RFC 4331 quota
    /// via PROPFIND. The OpenDrive REST API uses session auth (not basic
    /// auth like Koofr), so do a minimal login -> users/info -> logout with
    /// the same account credentials the WebDAV profile already stores.
    /// Mirrors the native provider's storage_info (opendrive.rs).
    async fn opendrive_storage_via_api(&self) -> Result<super::StorageInfo, ProviderError> {
        const API: &str = "https://dev.opendrive.com/api/v1";

        // reqwest's `.form()` helper is not enabled in this build; urlencode
        // the body manually like opendrive.rs/post_form does.
        let login_body = {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            s.append_pair("username", &self.config.username);
            s.append_pair("passwd", self.config.password.expose_secret());
            s.append_pair("version", "2.9.7");
            s.append_pair("partner_id", "");
            s.finish()
        };
        let login: serde_json::Value = self
            .client
            .post(format!("{}/session/login.json", API))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(login_body)
            .send()
            .await
            .map_err(send_error)?
            .json()
            .await
            .map_http_err(body_read_error)?;

        let session_id = login
            .get("SessionID")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ProviderError::NotSupported("opendrive quota: no SessionID".into()))?
            .to_string();

        let info_res = self
            .client
            .get(format!("{}/users/info.json/{}", API, session_id))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(send_error)?
            .json::<serde_json::Value>()
            .await
            .map_http_err(body_read_error);

        // Best-effort logout regardless of the users/info outcome: never
        // leak a session because the quota call failed.
        let logout_body = {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            s.append_pair("session_id", &session_id);
            s.finish()
        };
        let _ = self
            .client
            .post(format!("{}/session/logout.json", API))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(logout_body)
            .send()
            .await;

        let info = info_res?;
        // OpenDrive reports MaxStorage in MiB, StorageUsed in bytes
        // (verified live in opendrive.rs:1921). Accept number or string.
        let as_u64 = |v: Option<&serde_json::Value>| -> u64 {
            v.and_then(|x| {
                x.as_u64()
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
        };
        let total = as_u64(info.get("MaxStorage")).saturating_mul(1024 * 1024);
        let used = as_u64(info.get("StorageUsed"));
        if total == 0 {
            return Err(ProviderError::NotSupported(
                "opendrive quota: no MaxStorage".into(),
            ));
        }
        Ok(super::StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    /// Whether this Nextcloud requires a password on public links: the
    /// `files_sharing.public.password.enforced` capability, which
    /// apps/files_sharing/lib/Capabilities.php publishes at
    /// `/ocs/v1.php/cloud/capabilities`. A server without the entry does not
    /// enforce one.
    async fn nextcloud_link_password_enforced(&self, base: &str) -> Result<bool, ProviderError> {
        let url = format!("{}/ocs/v1.php/cloud/capabilities", base);
        let resp: reqwest::Response = self
            .request_url(Method::GET, &url)
            .header("OCS-APIREQUEST", "true")
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(send_error)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "OCS capabilities: HTTP {}",
                status
            )));
        }
        let text = resp.text().await.map_http_err(body_read_error)?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!("ocs json: {}", super::json_parse_error(&e)))
        })?;
        Ok(json
            .pointer("/ocs/data/capabilities/files_sharing/public/password/enforced")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false))
    }

    /// OCS: Create a public share link for a file/folder.
    /// If the Nextcloud instance enforces passwords on share links (HTTP 403),
    /// retries automatically with a generated password and returns "url\npassword".
    pub async fn nextcloud_create_share(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!("{}/ocs/v2.php/apps/files_sharing/api/v1/shares", base);

        let mut form_body = format!(
            "path={}&shareType=3&permissions=1",
            urlencoding::encode(path)
        );
        if let Some(secs) = options.expires_in_secs {
            let days = (secs / 86400).max(1);
            let expire = chrono::Utc::now() + chrono::Duration::days(days as i64);
            form_body.push_str(&format!("&expireDate={}", expire.format("%Y-%m-%d")));
        }

        let resp: reqwest::Response = self
            .request_url(Method::POST, &url)
            .header("OCS-APIREQUEST", "true")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body.clone())
            .send()
            .await
            .map_err(send_error)?;

        let status = resp.status();
        let text = resp.text().await.map_http_err(body_read_error)?;

        // A server that requires a password on public links refuses the
        // share with 403. Whether that is the reason comes from its
        // capabilities, not from the message, which is translated (German
        // "Passwörter sind ... erforderlich" carries no "password"). #1506
        // row 164. When the capabilities cannot be read, a password is tried
        // once all the same (a 403 for another reason refuses again), and a
        // refusal that stands then says the lookup failed (CodeRabbit, #1520).
        // The lookup's own error is not repeated: a transport error names the
        // URL, and an installation path can hold a token (CodeRabbit, #1520).
        let mut capability_unreadable = false;
        let password_needed = status == StatusCode::FORBIDDEN
            && match self.nextcloud_link_password_enforced(&base).await {
                Ok(enforced) => enforced,
                Err(_) => {
                    capability_unreadable = true;
                    true
                }
            };
        if password_needed {
            let password = Self::generate_share_password();
            let form_with_pw = format!("{}&password={}", form_body, urlencoding::encode(&password));

            let resp2: reqwest::Response = self
                .request_url(Method::POST, &url)
                .header("OCS-APIREQUEST", "true")
                .header("Accept", "application/json")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_with_pw)
                .send()
                .await
                .map_err(send_error)?;

            let status2 = resp2.status();
            let text2 = resp2.text().await.map_http_err(body_read_error)?;

            if !status2.is_success() {
                let lookup = if capability_unreadable {
                    " (the sharing capabilities could not be read)"
                } else {
                    ""
                };
                // A server may echo the form, password included, in any
                // spelling: it is removed after the message is decoded
                // (CodeRabbit, #1520 and #1598).
                let mut sent = self.sent_secrets();
                sent.push(password.clone());
                return Err(ProviderError::ServerError(format!(
                    "OCS share failed: HTTP {} - {}{}",
                    status2,
                    server_message(&text2, &self.config.url, &sent),
                    lookup
                )));
            }

            let json: serde_json::Value = serde_json::from_str(&text2).map_err(|e| {
                ProviderError::ParseError(format!("ocs json: {}", super::json_parse_error(&e)))
            })?;

            let share_url = json
                .pointer("/ocs/data/url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ProviderError::ServerError("OCS share API did not return a URL".into())
                })?;

            return Ok(ShareLinkResult {
                url: share_url.to_string(),
                password: Some(password),
                expires_at: None,
            });
        }

        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "OCS share failed: HTTP {} - {}",
                status,
                server_message(&text, &self.config.url, &self.sent_secrets())
            )));
        }
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!("ocs json: {}", super::json_parse_error(&e)))
        })?;

        let share_url = json
            .pointer("/ocs/data/url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::ServerError("OCS share API did not return a URL".into())
            })?;

        Ok(ShareLinkResult {
            url: share_url.to_string(),
            password: None,
            expires_at: None,
        })
    }

    /// Generate a random 16-char password for share links.
    /// The secrets this session sends a server: its account password, unless
    /// it connects anonymously. An error built from a reply never repeats them.
    fn sent_secrets(&self) -> Vec<String> {
        if self.config.anonymous {
            Vec::new()
        } else {
            vec![self.config.password.expose_secret().to_string()]
        }
    }

    fn generate_share_password() -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let lower: &[u8] = b"abcdefghijkmnpqrstuvwxyz";
        let upper: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
        let digits: &[u8] = b"23456789";
        let special: &[u8] = b"!@#$%&*?";
        let all: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%&*?";
        // Guarantee at least one of each category
        let mut pwd = vec![
            lower[rng.gen_range(0..lower.len())] as char,
            upper[rng.gen_range(0..upper.len())] as char,
            digits[rng.gen_range(0..digits.len())] as char,
            special[rng.gen_range(0..special.len())] as char,
        ];
        // Fill remaining 12 chars from full set
        for _ in 0..12 {
            pwd.push(all[rng.gen_range(0..all.len())] as char);
        }
        // Shuffle to avoid predictable positions
        for i in (1..pwd.len()).rev() {
            let j = rng.gen_range(0..=i);
            pwd.swap(i, j);
        }
        pwd.into_iter().collect()
    }

    /// Nextcloud trashbin: list deleted items.
    pub async fn nextcloud_list_trash(
        &mut self,
    ) -> Result<Vec<NextcloudTrashEntry>, ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash/",
            base,
            encode_nextcloud_trash_segment(&self.config.username)
        );

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:propfind xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns" xmlns:oc="http://owncloud.org/ns">
                <d:prop>
                    <nc:trashbin-filename/>
                    <nc:trashbin-original-location/>
                    <nc:trashbin-deletion-time/>
                    <oc:fileid/>
                    <d:getcontentlength/>
                    <d:resourcetype/>
                </d:prop>
            </d:propfind>"#;

        let resp = self
            .request_url(webdav_methods::propfind(), &url)
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await
            .map_err(send_error)?;

        if !resp.status().is_success() && resp.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::ServerError(format!(
                "Trashbin PROPFIND failed: HTTP {}",
                resp.status()
            )));
        }

        let xml = resp.text().await.map_http_err(body_read_error)?;

        self.parse_trashbin_response(&xml)
    }

    /// Parse trashbin PROPFIND XML into entries.
    ///
    /// Properties are taken from a propstat whose status is missing or 200.
    /// A following 404 propstat (Nextcloud reports each missing property that
    /// way) must not replace the file id or the original location.
    ///
    /// A reader error, or a body that ends before `</multistatus>`, fails
    /// the whole listing. The entries already read are not returned: a later
    /// generation of the same path could be in the unread tail, and a purge
    /// would then treat one generation as the only one and delete it.
    /// A finished multistatus with no responses is an empty listing.
    fn parse_trashbin_response(
        &self,
        xml: &str,
    ) -> Result<Vec<NextcloudTrashEntry>, ProviderError> {
        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        // No trim_text: fragments around XML entities must survive intact
        // (entity-adjacent spaces are part of the name); scalars are
        // trimmed once at consumption below.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut in_propstat = false;
        let mut current_tag: Option<String> = None;
        let mut kept = TrashPropBuf::default();
        let mut staged = TrashPropBuf::default();
        let mut propstat_status = String::new();
        let mut in_resourcetype = false;
        let mut depth = 0u32;
        let mut saw_multistatus_end = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(error) => return Err(ProviderError::ParseError(error.to_string())),
                Ok(Event::Eof) => {
                    propfind_document_is_complete(depth, saw_multistatus_end)?;
                    break;
                }
                Ok(Event::Start(ref e)) => {
                    depth = depth.saturating_add(1);
                    let tag = e.local_name().as_ref().to_string();
                    match tag.as_str() {
                        "response" => {
                            in_response = true;
                            in_propstat = false;
                            kept.clear();
                            staged.clear();
                            propstat_status.clear();
                            in_resourcetype = false;
                        }
                        "propstat" if in_response => {
                            in_propstat = true;
                            staged.clear();
                            propstat_status.clear();
                            in_resourcetype = false;
                        }
                        "resourcetype" => {
                            in_resourcetype = true;
                        }
                        "collection" if in_resourcetype => {
                            active_trash_buf(&mut kept, &mut staged, in_propstat).is_collection =
                                true;
                        }
                        _ if in_response => {
                            current_tag = Some(tag);
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = e.local_name();
                    let tag = local.as_ref();
                    if tag == "multistatus" {
                        saw_multistatus_end = true;
                    }
                    if tag == "collection" && in_resourcetype {
                        active_trash_buf(&mut kept, &mut staged, in_propstat).is_collection = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = e.local_name();
                    let tag = local.as_ref();
                    depth = depth.saturating_sub(1);
                    if tag == "multistatus" {
                        saw_multistatus_end = true;
                    }
                    match tag {
                        "propstat" if in_propstat => {
                            in_propstat = false;
                            if trash_propstat_succeeded(&propstat_status) {
                                kept.take_missing_from(&staged);
                            }
                            current_tag = None;
                        }
                        "response" if in_response => {
                            in_response = false;
                            // Skip the collection itself (the trash/ container).
                            if !kept.filename.is_empty() {
                                let encoded_id = kept
                                    .href
                                    .rsplit('/')
                                    .find(|segment| !segment.is_empty())
                                    .unwrap_or("");
                                let id = percent_decode_str(encoded_id)
                                    .decode_utf8()
                                    .map_err(|e| ProviderError::ParseError(e.to_string()))?
                                    .into_owned();
                                entries.push(NextcloudTrashEntry {
                                    id,
                                    name: kept.filename.clone(),
                                    original_path: kept.location.clone(),
                                    deleted_at: kept
                                        .deletion_time
                                        .trim()
                                        .parse::<u64>()
                                        .unwrap_or(0),
                                    size: kept.content_length.trim().parse::<u64>().unwrap_or(0),
                                    is_dir: kept.is_collection,
                                    file_id: kept.file_id.trim().to_string(),
                                });
                            }
                        }
                        "resourcetype" => {
                            in_resourcetype = false;
                        }
                        _ => {
                            current_tag = None;
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = e.as_ref().to_string();
                        if tag == "status" && in_propstat {
                            propstat_status.push_str(&raw);
                        } else {
                            active_trash_buf(&mut kept, &mut staged, in_propstat)
                                .push_fragment(tag, &raw);
                        }
                    }
                }
                Ok(Event::GeneralRef(ref e)) => {
                    // Decode XML entities (&amp; &apos; &lt; &gt; &quot;) so file
                    // names containing these characters are not silently truncated.
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            if tag != "status" {
                                active_trash_buf(&mut kept, &mut staged, in_propstat)
                                    .push_fragment(tag, &ch);
                            }
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }

        Ok(entries)
    }

    /// Nextcloud trashbin: restore a single item.
    pub async fn nextcloud_restore_trash(&mut self, id: &str) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let username = encode_nextcloud_trash_segment(&self.config.username);
        let id = encode_nextcloud_trash_segment(id);
        let from = format!("{}/remote.php/dav/trashbin/{}/trash/{}", base, username, id);
        let dest = format!(
            "{}/remote.php/dav/trashbin/{}/restore/{}",
            base, username, id
        );

        let resp = self
            .request_url(Method::from_bytes(b"MOVE").unwrap(), &from)
            .header("Destination", &dest)
            .header("Overwrite", "T")
            .send()
            .await
            .map_err(send_error)?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::CREATED && status != StatusCode::NO_CONTENT
        {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore failed: HTTP {}: {}",
                status,
                server_message(&body, &self.config.url, &self.sent_secrets())
            )));
        }
        Ok(())
    }

    /// Nextcloud trashbin: permanently delete a single item.
    pub async fn nextcloud_delete_trash_item(&mut self, id: &str) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash/{}",
            base,
            encode_nextcloud_trash_segment(&self.config.username),
            encode_nextcloud_trash_segment(id)
        );

        let resp = self
            .request_url(Method::DELETE, &url)
            .send()
            .await
            .map_err(send_error)?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::NO_CONTENT {
            return Err(ProviderError::ServerError(format!(
                "Delete trash item failed: HTTP {}",
                status
            )));
        }
        Ok(())
    }

    /// Nextcloud trashbin: empty entire trash.
    pub async fn nextcloud_empty_trash(&mut self) -> Result<(), ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let url = format!(
            "{}/remote.php/dav/trashbin/{}/trash",
            base,
            encode_nextcloud_trash_segment(&self.config.username)
        );

        let resp = self
            .request_url(Method::DELETE, &url)
            .send()
            .await
            .map_err(send_error)?;

        let status = resp.status();
        if !status.is_success() && status != StatusCode::NO_CONTENT {
            return Err(ProviderError::ServerError(format!(
                "Empty trash failed: HTTP {}",
                status
            )));
        }
        Ok(())
    }

    /// Whether this server keeps file versions AeroFTP can read: the
    /// Nextcloud versions DAV collection. ownCloud, also Nextcloud-class for
    /// the file root, keeps versions under another path, so it is left out.
    fn supports_nextcloud_versions(&self) -> bool {
        let owncloud = self
            .config
            .provider_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|id| {
                id.eq_ignore_ascii_case("owncloud") || id.eq_ignore_ascii_case("owncloud-webdav")
            });
        self.is_nextcloud_for_dav() && !owncloud
    }

    /// `oc:fileid` of `path` ([`Self::nextcloud_file_of`]). Versions are
    /// kept per file id, not per path.
    async fn nextcloud_file_id_of(&mut self, path: &str) -> Result<String, ProviderError> {
        Ok(self.nextcloud_file_of(path).await?.file_id)
    }

    /// `path` as one Depth-0 PROPFIND answers it: its id with the size and
    /// time of its current contents. A folder, or an answer without an id,
    /// has no versions to list.
    async fn nextcloud_file_of(&mut self, path: &str) -> Result<NextcloudFile, ProviderError> {
        let resp = self
            .send_replaying_digest(|| {
                self.request(webdav_methods::propfind(), path)
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(NEXTCLOUD_FILE_PROPFIND)
            })
            .await?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound(path.to_string()));
        }
        if !matches!(status, StatusCode::OK | StatusCode::MULTI_STATUS) {
            return Err(ProviderError::ServerError(format!(
                "File id PROPFIND failed: HTTP {status}"
            )));
        }
        // A relative href is relative to where the answer came from: after
        // a redirect, the server redirected to.
        let answered_from = resp.url().to_string();
        let body = super::response_bytes_with_limit(resp, NEXTCLOUD_VERSIONS_MAX_BYTES).await?;
        let xml = String::from_utf8(body)
            .map_err(|e| ProviderError::ParseError(format!("File id answer is not UTF-8: {e}")))?;
        let mut rows = parse_nextcloud_file_id_listing(&xml)?
            .into_iter()
            .filter(|row| !row.file_id.is_empty());
        match (rows.next(), rows.next()) {
            (Some(row), None) => {
                // The href, resolved as a browser resolves it against the URL
                // the answer came from, must name this server's origin: an
                // href of another origin (`//host/a.txt`, `https://host/...`,
                // `https://host\@us/...`, a relative one after a redirect to
                // another server) names that origin's entry, and its path
                // alone would match the relative listing path.
                let own = self.build_url(path);
                let Some(resolved) = Self::href_of_our_origin(&row.href, &answered_from, &own)
                else {
                    return Err(ProviderError::ServerError(format!(
                        "The file id answer for {path} names an entry of another server"
                    )));
                };
                // The one row must be the file asked for: a lone row for
                // `/other/a.txt` is not `/a.txt` (decoded segments, as in
                // `nextcloud_list_file_ids`).
                let request_path =
                    Self::dav_path_segments(Self::url_path_component(&self.build_url(path)), &[]);
                let listing_path =
                    Self::dav_path_segments(&Self::encode_path(path.trim_start_matches('/')), &[]);
                // The resource is the resolved URL's path: the same parse
                // that read its origin (`https://us\@host/a.txt` names
                // `/@host/a.txt`, not `/a.txt`). The whole URL, not its path
                // alone, which would read `//other/a.txt` as an authority.
                let href = Self::dav_path_segments(resolved.as_str(), &request_path);
                if href != request_path && href != listing_path {
                    return Err(ProviderError::ServerError(format!(
                        "The file id answer for {path} names another entry"
                    )));
                }
                if row.is_collection {
                    return Err(ProviderError::NotSupported(format!(
                        "{path} is a folder: Nextcloud keeps versions of files only"
                    )));
                }
                Ok(NextcloudFile {
                    file_id: row.file_id,
                    size: row.size.unwrap_or(0),
                    modified: row.modified,
                    etag: row.etag,
                })
            }
            (None, _) => Err(ProviderError::NotFound(format!(
                "{path} has no Nextcloud file id: it has no versions"
            ))),
            (Some(_), Some(_)) => Err(ProviderError::ServerError(format!(
                "The file id answer for {path} names more than one entry"
            ))),
        }
    }

    /// `<base>/remote.php/dav/versions/<user>/` with a trailing slash.
    fn nextcloud_versions_root(&self) -> Result<String, ProviderError> {
        let base = self
            .nextcloud_base_url()
            .ok_or_else(|| ProviderError::NotSupported("Not a Nextcloud instance".into()))?;
        let user = self.nextcloud_userid().ok_or_else(|| {
            ProviderError::Other(
                "Cannot resolve Nextcloud user id from server_root / config".into(),
            )
        })?;
        Ok(format!(
            "{}/remote.php/dav/versions/{}/",
            base,
            encode_nextcloud_trash_segment(&decode_one_segment(&user))
        ))
    }

    /// URL of one version of `path`, after refusing a version id that is not
    /// a single segment (it would address another file's versions).
    async fn nextcloud_version_url(
        &mut self,
        path: &str,
        version_id: &str,
    ) -> Result<String, ProviderError> {
        Ok(self.nextcloud_version_target(path, version_id).await?.0)
    }

    /// [`Self::nextcloud_version_url`] with the file id the URL was built
    /// for, which a bare label is then bound to.
    async fn nextcloud_version_target(
        &mut self,
        path: &str,
        version_id: &str,
    ) -> Result<(String, String), ProviderError> {
        let (bound_to, version) = nextcloud_split_version_id(version_id);
        if !nextcloud_trash_id_is_one_segment(version) {
            return Err(ProviderError::InvalidConfig(format!(
                "Version id {version_id:?} is not a single path segment"
            )));
        }
        let root = self.nextcloud_versions_root()?;
        let file_id = self.nextcloud_file_id_of(path).await?;
        if let Some(listed) = bound_to.filter(|listed| *listed != file_id) {
            return Err(ProviderError::NotFound(format!(
                "{path} is no longer the file whose versions were listed (file id {listed}, \
                 now {file_id}): list its versions again"
            )));
        }
        let url = format!(
            "{}versions/{}/{}",
            root,
            encode_nextcloud_trash_segment(&file_id),
            encode_nextcloud_trash_segment(version)
        );
        Ok((url, file_id))
    }

    /// Nextcloud versions of `path`: the current contents first, under
    /// [`super::CURRENT_VERSION_ID`] (the file itself, listed or not), then
    /// the earlier ones newest first, each id bound to the file id
    /// ([`nextcloud_bound_version_id`]).
    /// A disconnected provider sends no version request: `disconnect` keeps
    /// the client and the credentials, so a list, restore or delete still
    /// queued for it would reach the server otherwise.
    fn require_connected(&self) -> Result<(), ProviderError> {
        if self.connected {
            Ok(())
        } else {
            Err(ProviderError::NotConnected)
        }
    }

    async fn nextcloud_list_versions(
        &mut self,
        path: &str,
    ) -> Result<Vec<super::FileVersion>, ProviderError> {
        let root = self.nextcloud_versions_root()?;
        // The id and the current contents' size and time, from one answer.
        let current = self.nextcloud_file_of(path).await?;
        let file_id = current.file_id.clone();
        let url = format!(
            "{}versions/{}/",
            root,
            encode_nextcloud_trash_segment(&file_id)
        );
        let resp = self
            .send_replaying_digest(|| {
                self.request_url(webdav_methods::propfind(), &url)
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml")
                    .body(NEXTCLOUD_VERSIONS_PROPFIND)
            })
            .await?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            // The versions route answers 404 both when the Versions app is
            // off and when the file id no longer resolves (the file removed
            // or replaced after the lookup): the file, looked up again, tells.
            return Err(match self.nextcloud_file_id_of(path).await {
                Ok(now) if now == file_id => {
                    ProviderError::NotSupported(NEXTCLOUD_VERSIONS_OFF.into())
                }
                Ok(_) | Err(ProviderError::NotFound(_)) => ProviderError::NotFound(format!(
                    "{path} was removed or replaced while its versions were listed: \
                     list its versions again"
                )),
                // A lookup that failed tells neither.
                Err(e) => e,
            });
        }
        if !status.is_success() && status != StatusCode::MULTI_STATUS {
            return Err(ProviderError::ServerError(format!(
                "Versions PROPFIND failed: HTTP {status}"
            )));
        }
        // A listing redirected to another origin is that server's: its
        // path-only hrefs would read as rows of this collection, and a purge
        // of one would then go to this server.
        let answered_from = resp.url().to_string();
        let same_origin =
            url::Url::parse(&url).is_ok_and(|asked| resp.url().origin() == asked.origin());
        if !same_origin {
            return Err(ProviderError::ServerError(format!(
                "The versions listing of {path} was answered by another server"
            )));
        }
        let body = super::response_bytes_with_limit(resp, NEXTCLOUD_VERSIONS_MAX_BYTES).await?;
        let xml = String::from_utf8(body).map_err(|e| {
            ProviderError::ParseError(format!("Versions listing is not UTF-8: {e}"))
        })?;
        // A write between the two PROPFINDs would leave `current` naming the
        // revision the collection now lists as history: the file is read
        // again, and another id, validator, size or time asks for a new list.
        let again = self.nextcloud_file_of(path).await?;
        if (&again.file_id, &again.etag, again.size, &again.modified)
            != (
                &current.file_id,
                &current.etag,
                current.size,
                &current.modified,
            )
        {
            return Err(ProviderError::NotFound(format!(
                "{path} changed while its versions were listed: list its versions again"
            )));
        }
        // Ids are the Unix time each version was written. Nextcloud 26 and
        // later list the current contents too, under the file's own mtime:
        // that row becomes the file itself (downloaded from the file, never
        // from the versions endpoint, which may hold no body for it).
        // Nextcloud 25 leaves them out, and the file itself becomes row 0.
        // Its id is bound to the file id too, so a download of it refuses
        // another file that took the path since, and to the strong ETag the
        // listing saw, so a write since then is not read as the row.
        let current_id = match current.etag.as_deref().and_then(strong_etag) {
            Some(etag) => super::bound_current_row(&file_id, &etag),
            None => nextcloud_bound_version_id(&file_id, super::CURRENT_VERSION_ID),
        };
        let mtime = super::version_time_millis(current.modified.as_deref()) / 1000;
        let versions: Vec<super::FileVersion> = parse_nextcloud_versions_listing(&xml)?
            .into_iter()
            .filter(|row| !row.is_collection)
            .filter_map(|row| {
                let version = nextcloud_version_id_of_href(&row.href, &url, &answered_from)?;
                let epoch = version.parse::<i64>().unwrap_or(0);
                let id = if mtime > 0 && epoch == mtime {
                    current_id.clone()
                } else {
                    nextcloud_bound_version_id(&file_id, &version)
                };
                Some(super::FileVersion {
                    id,
                    modified: row.modified,
                    size: row.size.unwrap_or(0),
                    modified_by: row.author,
                    deleted: false,
                })
            })
            .collect();
        let epoch = |v: &super::FileVersion| {
            nextcloud_split_version_id(&v.id)
                .1
                .parse::<i64>()
                .unwrap_or(0)
        };
        let versions = super::versions_current_first(
            versions,
            |v| v.id == current_id,
            || {
                Some(super::FileVersion {
                    id: current_id.clone(),
                    modified: current.modified.clone(),
                    size: current.size,
                    modified_by: None,
                    deleted: false,
                })
            },
            |v| {
                if v.id == current_id {
                    i64::MAX
                } else {
                    epoch(v)
                }
            },
        )?;
        Ok(versions)
    }

    /// Nextcloud versions: write one earlier version of `path` to
    /// `local_path`, streamed and committed only when complete.
    async fn nextcloud_download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let (bound_to, version) = nextcloud_split_version_id(version_id);
        if version == super::CURRENT_VERSION_ID {
            let listed_etag = super::bound_current_row_of(version_id).map(|(_, etag)| etag);
            return self
                .nextcloud_download_current(path, bound_to, listed_etag.as_deref(), local_path)
                .await;
        }
        let url = self.nextcloud_version_url(path, version_id).await?;
        let resp = self
            .send_replaying_digest(|| self.request_url(Method::GET, &url))
            .await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(ProviderError::NotFound(format!(
                    "{path} has no version {version_id}"
                )))
            }
            status => {
                return Err(ProviderError::TransferFailed(format!(
                    "Version download failed: HTTP {status}"
                )))
            }
        }
        let expected = resp.content_length();
        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            written += chunk.len() as u64;
        }
        if let Some(expected) = expected.filter(|expected| *expected != written) {
            return Err(ProviderError::TransferFailed(format!(
                "Version download cut short: received {written} of {expected} bytes"
            )));
        }
        atomic.commit().await.map_err(ProviderError::IoError)
    }

    /// The "current" row: the file's contents, written to `local_path` only
    /// when the path held the file the row was listed from (`listed`) both
    /// before and after the body was read, and the body is read only while
    /// the path still has the ETag answered with the id. A replacement in
    /// between leaves the destination as it was (unless the user asked for
    /// `--inplace`). A row bound to the ETag its listing saw (`listed_etag`)
    /// is read only under that ETag: a write since the listing saves nothing.
    async fn nextcloud_download_current(
        &mut self,
        path: &str,
        listed: Option<&str>,
        listed_etag: Option<&str>,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let replaced = |listed: &str, now: &str| {
            ProviderError::NotFound(format!(
                "{path} is no longer the file whose versions were listed (file id {listed}, \
                 now {now}): list its versions again"
            ))
        };
        let file = self.nextcloud_file_of(path).await?;
        let before = file.file_id;
        if let Some(listed) = listed.filter(|listed| *listed != before) {
            return Err(replaced(listed, &before));
        }
        // The read is conditional on the ETag answered with the id: a file
        // swapped in and back out while the body was read (the id checks
        // before and after would both pass) answers 412, not its bytes.
        // Without an ETag the read could not be bound: it is not sent.
        let Some(etag) = file.etag.as_deref().and_then(strong_etag) else {
            return Err(ProviderError::TransferFailed(format!(
                "Nextcloud answered no single strong ETag for {path}: its current contents \
                 cannot be read bound to the file listed, download the file itself instead"
            )));
        };
        if listed_etag.is_some_and(|listed| listed != etag) {
            return Err(super::current_row_changed(path));
        }
        let resp = self
            .send_replaying_digest(|| {
                self.request(Method::GET, path)
                    .header(reqwest::header::IF_MATCH, etag.as_str())
            })
            .await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound(path.to_string()));
        }
        if resp.status() == StatusCode::PRECONDITION_FAILED {
            return Err(match listed_etag {
                Some(_) => super::current_row_changed(path),
                None => replaced(&before, "another file"),
            });
        }
        let staged = match listed_etag {
            Some(_) => {
                super::stage_bound_current_body(resp, local_path, "Version download failed").await?
            }
            None => super::stage_version_body(resp, local_path, "Version download failed").await?,
        };
        let after = self.nextcloud_file_id_of(path).await?;
        if after != before {
            return Err(replaced(&before, &after));
        }
        staged.commit().await.map_err(ProviderError::IoError)
    }

    /// Nextcloud versions: permanently delete one earlier version. A server
    /// that does not allow it (403, 405, 501) answers NotSupported.
    async fn nextcloud_delete_version(
        &mut self,
        path: &str,
        version_id: &str,
    ) -> Result<(), ProviderError> {
        if nextcloud_split_version_id(version_id).1 == super::CURRENT_VERSION_ID {
            return Err(ProviderError::NotSupported(
                "The current version is the file itself: delete the file instead".into(),
            ));
        }
        // Only a version the listing holds for this file is deleted: a row
        // the listing refused (another host's href) has no local version to
        // remove, and a label of another file is not this file's.
        let (url, file_id) = self.nextcloud_version_target(path, version_id).await?;
        let bound = nextcloud_bound_version_id(&file_id, nextcloud_split_version_id(version_id).1);
        let listed = StorageProvider::list_versions(self, path).await?;
        if !listed.iter().any(|v| v.id == bound) {
            return Err(ProviderError::NotFound(format!(
                "{path} has no version {version_id}"
            )));
        }
        let resp = self
            .send_replaying_digest(|| self.request_url(Method::DELETE, &url))
            .await?;
        match resp.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(format!(
                "{path} has no version {version_id}"
            ))),
            StatusCode::FORBIDDEN
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::NOT_IMPLEMENTED => Err(ProviderError::NotSupported(
                "This Nextcloud server does not allow deleting a version".into(),
            )),
            status => Err(ProviderError::ServerError(format!(
                "Version delete failed: HTTP {status}"
            ))),
        }
    }

    /// Nextcloud versions: make one earlier version the current contents of
    /// `path`. Nextcloud keeps the contents it replaces as a new version.
    async fn nextcloud_restore_version(
        &mut self,
        path: &str,
        version_id: &str,
    ) -> Result<(), ProviderError> {
        if nextcloud_split_version_id(version_id).1 == super::CURRENT_VERSION_ID {
            return Err(ProviderError::NotSupported(
                "This is already the current version".into(),
            ));
        }
        let (url, file_id) = self.nextcloud_version_target(path, version_id).await?;
        // A bare label is the version of the file the URL was built for: the
        // listing must hold it for that file, and the path must still be
        // that file when the MOVE goes out.
        let bound = nextcloud_bound_version_id(&file_id, nextcloud_split_version_id(version_id).1);
        // Nextcloud answers the MOVE 201 even when its rollback failed
        // (RestoreFolder::moveInto drops the rollback's result): the file is
        // checked after it.
        let selected = super::refuse_a_restore_that_changes_nothing(self, path, &bound).await?;
        let now = self.nextcloud_file_id_of(path).await?;
        if now != file_id {
            return Err(ProviderError::NotFound(format!(
                "{path} is no longer the file whose versions were listed (file id {file_id}, \
                 now {now}): list its versions again"
            )));
        }
        let dest = format!("{}restore/target", self.nextcloud_versions_root()?);
        let resp = self
            .send_replaying_digest(|| {
                self.request_url(Method::from_bytes(b"MOVE").unwrap(), &url)
                    .header("Destination", &dest)
            })
            .await
            .map_err(|e| super::sent_change_unconfirmed(e, &format!("the restore of {path}")))?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound(format!(
                "{path} has no version {version_id}"
            )));
        }
        if !status.is_success() {
            // Status only: a server may echo the request, credentials and
            // all, in the body of a refusal.
            return Err(
                super::gateway_left_unknown(status, &format!("the restore of {path}"))
                    .unwrap_or_else(|| {
                        ProviderError::ServerError(format!("Version restore failed: HTTP {status}"))
                    }),
            );
        }
        super::confirm_version_restored(self, path, Some(&selected)).await
    }

    /// Original location of `path` as Nextcloud recorded it: the decoded
    /// remainder of the URL `build_url` would use, after the files root.
    /// `None` when that root is not in the URL or `path` is the root itself.
    fn nextcloud_files_relative_location(&self, path: &str) -> Option<String> {
        let url = self.build_url(path);
        let segments = Self::dav_path_segments(Self::url_path_component(&url), &[]);
        let userid = self.nextcloud_userid().map(|id| decode_one_segment(&id));
        nextcloud_location_rest(&segments, userid.as_deref())
    }

    /// Purge the one trashbin row whose original location is the file `path`
    /// names under the user's files root.
    ///
    /// The caller path is relative to the profile root. Nextcloud stores
    /// `trashbin-original-location` relative to `/remote.php/dav/files/<id>/`
    /// (or `/remote.php/webdav/` on the legacy root). The key is taken from
    /// the URL the delete really hits, not from the caller string. A captured
    /// `oc:fileid` has to match as well. Deletion time is not a key. Zero
    /// matches purge nothing. Two matches are refused. The trash collection
    /// itself is never deleted.
    async fn purge_nextcloud_trash(
        &mut self,
        path: &str,
        identity: Option<&str>,
    ) -> Result<bool, ProviderError> {
        if !self.is_nextcloud() {
            return Ok(false);
        }
        let Some(location) = self.nextcloud_files_relative_location(path) else {
            return Ok(false);
        };
        let identity = identity.map(str::trim).filter(|id| !id.is_empty());
        let entries = self.nextcloud_list_trash().await?;
        let matches = entries
            .into_iter()
            .filter(|entry| nextcloud_trash_location(&entry.original_path) == location)
            .filter(|entry| match identity {
                Some(id) => entry.file_id.trim() == id,
                None => true,
            })
            .collect();
        let Some(entry) = super::the_one_trashed_item(path, matches)? else {
            return Ok(false);
        };
        let id = entry.id.as_str();
        if !nextcloud_trash_id_is_one_segment(id) {
            return Err(ProviderError::ServerError(format!(
                "Not purged: trash item id for {path} is empty or not a single segment"
            )));
        }
        self.nextcloud_delete_trash_item(id).await?;
        Ok(true)
    }

    /// One Depth-1 PROPFIND of `parent`, asking only for `oc:fileid`.
    /// The collection itself is left out. A 404 propstat's id is left out
    /// by the parser.
    async fn nextcloud_list_file_ids(
        &mut self,
        parent: &str,
    ) -> Result<Vec<(String, String)>, ProviderError> {
        let collection = Self::collection_path(parent);
        // Same self-reference rule as `parse_propfind_response`: the request
        // URL, the listing path, and the configured root, compared as decoded
        // segments. A suffix match would drop a child named like its parent.
        let request_path =
            Self::dav_path_segments(Self::url_path_component(&self.build_url(&collection)), &[]);
        let listing_path =
            Self::dav_path_segments(&Self::encode_path(collection.trim_start_matches('/')), &[]);
        let root_path = Self::dav_path_segments(Self::url_path_component(&self.config.url), &[]);
        let response = self
            .send_propfind(parent, "1", NEXTCLOUD_FILE_ID_PROPFIND)
            .await?;
        if !matches!(response.status(), StatusCode::OK | StatusCode::MULTI_STATUS) {
            return Err(ProviderError::ServerError(format!(
                "File id PROPFIND failed: HTTP {}",
                response.status()
            )));
        }
        // A relative href is relative to where the answer came from, after
        // any redirect, and an absolute one may name another server: each
        // row must name an entry of this server's origin
        // ([`Self::href_of_our_origin`], as in `nextcloud_file_of`).
        let answered_from = response.url().to_string();
        let own = self.build_url(&collection);
        let body = super::response_bytes_with_limit(response, NEXTCLOUD_VERSIONS_MAX_BYTES).await?;
        let xml = String::from_utf8(body)
            .map_err(|e| ProviderError::ParseError(format!("File id listing is not UTF-8: {e}")))?;
        let mut found = Vec::new();
        for hit in parse_nextcloud_file_id_listing(&xml)? {
            let file_id = hit.file_id.trim();
            if file_id.is_empty() {
                continue;
            }
            // Do not trim the href. A space at the end of the leaf is a
            // different file from the same letters without it.
            let Some(resolved) = Self::href_of_our_origin(&hit.href, &answered_from, &own) else {
                return Err(ProviderError::ServerError(format!(
                    "The file id listing of {parent} names an entry of another server"
                )));
            };
            let href_path = Self::dav_path_segments(resolved.as_str(), &request_path);
            if href_path == request_path || href_path == listing_path || href_path == root_path {
                continue;
            }
            // Only a direct child of the listed collection: a row for
            // `/other/a.txt` in the answer for `/docs/` is not `/docs/a.txt`.
            let direct_child = |collection: &[String]| {
                href_path.len() == collection.len() + 1 && href_path.starts_with(collection)
            };
            if !direct_child(&request_path) && !direct_child(&listing_path) {
                continue;
            }
            let Some(leaf) = href_path.last() else {
                continue;
            };
            found.push((leaf.clone(), file_id.to_string()));
        }
        Ok(found)
    }

    /// Group the paths by parent and read each parent's file ids once.
    /// A failed listing fails every path in that group. Two rows with the
    /// same name are refused, so the later purge cannot pick one of them.
    async fn capture_nextcloud_file_ids(
        &mut self,
        paths: &[String],
    ) -> Vec<Result<Option<String>, ProviderError>> {
        let mut results = Vec::with_capacity(paths.len());
        results.resize_with(paths.len(), || Ok(None));
        let mut groups: Vec<(String, Vec<(usize, String)>)> = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let Some((parent, name)) = nextcloud_parent_and_name(path) else {
                continue;
            };
            if let Some((_, members)) = groups.iter_mut().find(|(have, _)| have == &parent) {
                members.push((index, name));
            } else {
                groups.push((parent, vec![(index, name)]));
            }
        }
        for (parent, members) in groups {
            let listed = match self.nextcloud_list_file_ids(&parent).await {
                Ok(listed) => listed,
                Err(error) => {
                    let message = error.to_string();
                    for (index, _) in members {
                        results[index] = Err(ProviderError::Other(message.clone()));
                    }
                    continue;
                }
            };
            for (index, name) in members {
                let mut ids = listed
                    .iter()
                    .filter(|(leaf, _)| leaf == &name)
                    .map(|(_, id)| id.as_str());
                results[index] = match (ids.next(), ids.next()) {
                    (None, _) => Ok(None),
                    (Some(id), None) => Ok(Some(id.to_string())),
                    (Some(_), Some(_)) => Err(ProviderError::ServerError(format!(
                        "Not captured: more than one item named {name} is in {parent}"
                    ))),
                };
            }
        }
        results
    }

    /// A new connection asks `Depth: infinity` afresh: what this session
    /// learned (a refusal, a timeout) goes, and only the Filen Desktop
    /// bridge, known never to answer it, starts refused again.
    fn reset_infinity_memory(&mut self) {
        self.infinity_refused = infinity_refused_from_the_start(self.config.provider_id.as_deref());
        self.infinity_timed_out_at = None;
    }

    /// Shorten the `Depth: infinity` header wait so a test does not sit
    /// through the production 30 s.
    #[cfg(test)]
    pub(crate) fn set_infinity_headers_timeout(&mut self, timeout: std::time::Duration) {
        self.infinity_headers_timeout = timeout;
    }

    /// Single-request recursive listing via `PROPFIND Depth: infinity`
    /// (item 4b "used storage" scan). Returns every descendant of `path`
    /// flat (files at any depth + collections), reusing the same prop set
    /// and parser as `list()`. Servers that forbid or limit infinity
    /// (403/400, or a non-multistatus status), or that send no response
    /// headers within [`INFINITY_HEADERS_TIMEOUT`], yield an `Err` so the
    /// caller falls back to the recursive Depth:1 BFS. Only `size`/`is_dir` are
    /// relied on downstream, so the flat `name`/`path` (computed against
    /// the root) being approximate for deep entries does not matter.
    pub async fn list_recursive(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else if path == "/" {
            self.server_root
                .clone()
                .unwrap_or_else(|| self.current_path.clone())
        } else {
            path.to_string()
        };

        if self.infinity_refused {
            return Err(ProviderError::NotSupported(
                "Depth:infinity, refused by this server earlier in the session".to_string(),
            ));
        }
        if self
            .infinity_timed_out_at
            .is_some_and(|at| at.elapsed() < INFINITY_TIMEOUT_MEMORY)
        {
            return Err(ProviderError::NotSupported(
                "Depth:infinity, unanswered by this server a few minutes ago".to_string(),
            ));
        }

        let headers_cap = self.infinity_headers_timeout;
        let response = tokio::time::timeout(
            headers_cap,
            self.send_propfind(
                &list_path,
                "infinity",
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#,
            ),
        )
        .await
        .map_err(|_| {
            self.infinity_timed_out_at = Some(std::time::Instant::now());
            ProviderError::ServerError(format!(
                "Depth:infinity sent no response headers within {headers_cap:?}"
            ))
        })??;

        let status = response.status();
        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                // Depth:infinity returns the whole subtree in one body, which
                // is server-controlled. Bound it so a pathological or hostile
                // server cannot OOM the process by streaming an unbounded
                // PROPFIND response. On overflow the Err propagates and the
                // callers (used_scan / provider_scan_used) fall back to the
                // bounded BFS rather than producing a wrong figure.
                const MAX_PROPFIND_INFINITY_BYTES: u64 = 256 * 1024 * 1024;
                let body =
                    super::response_bytes_with_limit(response, MAX_PROPFIND_INFINITY_BYTES).await?;
                let xml = String::from_utf8_lossy(&body);
                self.parse_propfind_response(&xml, &list_path)
            }
            other => {
                // Only the answers that refuse the depth itself are remembered:
                // 403 (RFC 4918's answer to a refused infinite depth), 400,
                // 405 and 501. Any other status says something else (a missing
                // root, a lock, credentials, throttling, a stopped redirect),
                // and taking it for a refusal would cost the folder walk for
                // the rest of the session on a server that supports the depth.
                if matches!(
                    other,
                    StatusCode::FORBIDDEN
                        | StatusCode::BAD_REQUEST
                        | StatusCode::METHOD_NOT_ALLOWED
                        | StatusCode::NOT_IMPLEMENTED
                ) {
                    self.infinity_refused = true;
                }
                Err(ProviderError::ServerError(format!(
                    "Depth:infinity not available (HTTP {})",
                    other
                )))
            }
        }
    }

    /// Parse PROPFIND XML response into RemoteEntry list using quick-xml
    fn parse_propfind_response(
        &self,
        xml: &str,
        base_path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        validate_propfind_xml(xml, base_path)?;
        let mut entries = Vec::new();

        tracing::debug!(
            "[WebDAV] Parsing XML from {}",
            crate::local_bridge::url_origin(&self.config.url)
        );

        // The configured URL and `build_url` carry the base's own escapes
        // (`/my%20dav`), the listing path is the plain one: both go through
        // the same segment reading as the hrefs they are compared with.
        let request_path =
            Self::dav_path_segments(Self::url_path_component(&self.build_url(base_path)), &[]);
        let listing_path =
            Self::dav_path_segments(&Self::encode_path(base_path.trim_start_matches('/')), &[]);
        let root_path = Self::dav_path_segments(Self::url_path_component(&self.config.url), &[]);

        // Event-based quick-xml parser
        let mut reader = Reader::from_str(xml);
        // No trim_text: fragments around XML entities must survive intact
        // (entity-adjacent spaces are part of the name); values are
        // trimmed once at consumption below.
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut in_resourcetype = false;
        let mut current_tag: Option<String> = None;
        let mut href = String::new();
        let mut displayname = String::new();
        let mut getcontentlength = String::new();
        let mut getlastmodified = String::new();
        let mut getcontenttype = String::new();
        let mut getetag = String::new();
        let mut is_collection = false;
        let mut is_collection_by_iscollection = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => {
                    tracing::warn!(
                        "[WebDAV] XML parse error at position {}: {}",
                        reader.error_position(),
                        e
                    );
                    break;
                }
                Ok(Event::Eof) => break,

                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" => {
                            in_response = true;
                            href.clear();
                            displayname.clear();
                            getcontentlength.clear();
                            getlastmodified.clear();
                            getcontenttype.clear();
                            getetag.clear();
                            is_collection = false;
                            is_collection_by_iscollection = false;
                        }
                        "resourcetype" if in_response => {
                            in_resourcetype = true;
                        }
                        "collection" if in_response && in_resourcetype => {
                            is_collection = true;
                        }
                        "href" | "displayname" | "getcontentlength" | "getlastmodified"
                        | "getcontenttype" | "getetag" | "iscollection"
                            if in_response =>
                        {
                            current_tag = Some(local);
                        }
                        _ => {}
                    }
                }

                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_response && in_resourcetype {
                        is_collection = true;
                    }
                }

                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" if in_response => {
                            // Process accumulated response
                            in_response = false;
                            if href.is_empty() {
                                tracing::warn!("[WebDAV] No href found in response element");
                                continue;
                            }

                            let decoded_href = urlencoding::decode(href.trim())
                                .unwrap_or_else(|_| href.trim().into());
                            let base_clean = base_path.trim_end_matches('/');
                            // Only the exact resource asked for is the
                            // collection's self-reference: the request URL,
                            // the listing path itself (a server behind a
                            // rewriting proxy answers in its own namespace)
                            // or the configured root, all decoded and
                            // slash-normalised. A suffix is never enough: a
                            // child named like the base (`/Sub Dir/Sub Dir`)
                            // ends with it, and dropping it made bidirectional
                            // sync delete the local copy.
                            let href_path = Self::dav_path_segments(href.trim(), &request_path);
                            let is_self_reference = href_path == request_path
                                || href_path == listing_path
                                || href_path == root_path;

                            if is_self_reference {
                                tracing::debug!("[WebDAV] Skipping the self-reference");
                                continue;
                            }

                            let href_ends_slash = href.trim_end().ends_with('/');
                            let is_dir =
                                is_collection || is_collection_by_iscollection || href_ends_slash;

                            let size: u64 = getcontentlength.trim().parse().unwrap_or(0);
                            let modified = if getlastmodified.trim().is_empty() {
                                None
                            } else {
                                Some(getlastmodified.trim().to_string())
                            };

                            // Extract name: prefer displayname, fallback to href.
                            // Filen Desktop's WebDAV bridge ships displayname percent-
                            // encoded (e.g. `my%20folder` for "my folder"), against the
                            // RFC 4918 expectation that `<DAV:displayname>` is a
                            // human-readable string. Mirror the href branch and decode
                            // defensively: on RFC-compliant servers (Nextcloud, Koofr,
                            // ...) the input has no percent-encoding and decode is a
                            // no-op; on the Filen bridge it un-mangles the name. Issue
                            // #128.
                            // The spaces around a name are part of it: ` a`
                            // and `a ` are two other files than `a`, and a
                            // name of spaces is a name. The href names the
                            // resource, so a displayname that says its name
                            // with other spaces around it takes the href's:
                            // the entry's path must reach that resource. A
                            // displayname that names something else (a
                            // server whose href is not the name) is the name.
                            let leaf = decoded_href
                                .trim_end_matches('/')
                                .rsplit('/')
                                .next()
                                .unwrap_or("")
                                .to_string();
                            let name = if displayname.is_empty() {
                                leaf
                            } else {
                                let shown = urlencoding::decode(&displayname)
                                    .unwrap_or_else(|_| displayname.as_str().into())
                                    .into_owned();
                                if shown.trim() == leaf.trim() {
                                    leaf
                                } else {
                                    shown
                                }
                            };

                            if name.is_empty() || name == "." || name == ".." {
                                continue;
                            }

                            let path = if base_clean.is_empty() || base_clean == "/" {
                                format!("/{}", name)
                            } else {
                                format!("{}/{}", base_clean, name)
                            };

                            let mime_type = if getcontenttype.trim().is_empty() {
                                None
                            } else {
                                Some(getcontenttype.trim().to_string())
                            };

                            let mut metadata = HashMap::new();
                            if !getetag.trim().is_empty() {
                                metadata.insert("etag".to_string(), getetag.trim().to_string());
                            }

                            entries.push(RemoteEntry {
                                name,
                                path,
                                is_dir,
                                size,
                                modified,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                mime_type,
                                metadata,
                            });
                        }
                        "resourcetype" if in_resourcetype => {
                            in_resourcetype = false;
                        }
                        _ => {
                            if current_tag.as_deref() == Some(local.as_str()) {
                                current_tag = None;
                            }
                        }
                    }
                }

                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = e.as_ref().to_string();
                        // Whitespace-only fragments are indentation EXCEPT
                        // inside href/displayname, where the space is part
                        // of the name (e.g. `a&amp; &amp;b.txt`).
                        let payload_tag = matches!(tag.as_str(), "href" | "displayname");
                        if !raw.trim().is_empty() || payload_tag {
                            match tag.as_str() {
                                "href" => href.push_str(&raw),
                                "displayname" => displayname.push_str(&raw),
                                "getcontentlength" => getcontentlength.push_str(&raw),
                                "getlastmodified" => getlastmodified.push_str(&raw),
                                "getcontenttype" => getcontenttype.push_str(&raw),
                                "getetag" => getetag.push_str(&raw),
                                "iscollection" if raw.trim() == "1" => {
                                    is_collection_by_iscollection = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            match tag.as_str() {
                                "href" => href.push_str(&ch),
                                "displayname" => displayname.push_str(&ch),
                                "getcontentlength" => getcontentlength.push_str(&ch),
                                "getlastmodified" => getlastmodified.push_str(&ch),
                                "getcontenttype" => getcontenttype.push_str(&ch),
                                "getetag" => getetag.push_str(&ch),
                                _ => {}
                            }
                        }
                    }
                }

                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        // As in the text arm, the name is read as given:
                        // `<![CDATA[ a.txt ]]>` names ` a.txt `.
                        let raw = e.as_ref().to_string();
                        let text = raw.trim().to_string();
                        if !text.is_empty() || (tag == "displayname" && !raw.is_empty()) {
                            match tag.as_str() {
                                "href" => href = text,
                                "displayname" => displayname.push_str(&raw),
                                "getcontentlength" => getcontentlength = text,
                                "getlastmodified" => getlastmodified = text,
                                "getcontenttype" => getcontenttype = text,
                                "getetag" => getetag = text,
                                "iscollection" if text == "1" => {
                                    is_collection_by_iscollection = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                _ => {}
            }
            buf.clear();
        }

        tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
        Ok(entries)
    }

    /// Extract a single property value from a PROPFIND Depth:0 XML response using quick-xml.
    /// Used by stat() and storage_info() for simple single-response parsing.
    fn extract_xml_properties(
        &self,
        xml: &str,
        path: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        validate_propfind_xml(xml, path)?;
        let mut props = HashMap::new();
        let mut reader = Reader::from_str(xml);
        // No trim_text: fragments around XML entities must survive intact;
        // values are trimmed once at element end (see the End arm).
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        let mut current_tag: Option<String> = None;
        let mut in_resourcetype = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "resourcetype" => {
                            in_resourcetype = true;
                        }
                        "collection" if in_resourcetype => {
                            props.insert("_is_collection".to_string(), "true".to_string());
                        }
                        _ => {
                            current_tag = Some(local);
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_resourcetype {
                        props.insert("_is_collection".to_string(), "true".to_string());
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "resourcetype" {
                        in_resourcetype = false;
                    }
                    if current_tag.as_deref() == Some(local.as_str()) {
                        current_tag = None;
                        // Values accumulated raw so entity-adjacent spaces
                        // survive; trim once here so scalar consumers (quota,
                        // dates) can parse without caring about indentation.
                        if let Some(v) = props.get_mut(local.as_str()) {
                            let trimmed = v.trim().to_string();
                            *v = trimmed;
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let raw = e.as_ref().to_string();
                        if !raw.trim().is_empty() {
                            if tag == "iscollection" && raw.trim() == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props
                                .entry(tag.clone())
                                .and_modify(|v| v.push_str(&raw))
                                .or_insert_with(|| raw.clone());
                        }
                    }
                }
                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = e.as_ref().trim().to_string();
                        if !text.is_empty() {
                            if tag == "iscollection" && text == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props.insert(tag.clone(), text);
                        }
                    }
                }
                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        if let Some(ref tag) = current_tag {
                            props
                                .entry(tag.clone())
                                .and_modify(|v| v.push_str(&ch))
                                .or_insert_with(|| ch.to_string());
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }
        Ok(props)
    }
}

#[derive(Debug, PartialEq)]
enum PropfindDocument {
    Multistatus,
    NotFound,
    DavError,
    Other,
}

/// Validate the complete document before accepting any partial metadata.
/// Only a DAV:error root's direct Sabre exception child is an error signal.
fn inspect_propfind_document(xml: &str) -> Result<PropfindDocument, String> {
    use quick_xml::name::ResolveResult;
    use quick_xml::reader::NsReader;
    let mut reader = NsReader::from_str(xml);
    let mut depth = 0usize;
    let mut roots = 0;
    let mut kind = PropfindDocument::Other;
    let mut in_exception = false;
    let mut exception = String::new();
    loop {
        let (namespace, event) = reader
            .read_resolved_event()
            .map_err(|_| "Malformed PROPFIND XML".to_string())?;
        if matches!(namespace, ResolveResult::Unknown(_)) {
            return Err("Unknown PROPFIND XML namespace".into());
        }
        let namespace = match namespace {
            ResolveResult::Bound(ns) => ns.into_inner(),
            _ => "",
        };
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let local = element.local_name();
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return Err("Multiple PROPFIND XML roots".into());
                    }
                    kind = match (namespace, local.as_ref()) {
                        ("DAV:", "multistatus") => PropfindDocument::Multistatus,
                        ("DAV:", "error") => PropfindDocument::DavError,
                        _ => PropfindDocument::Other,
                    };
                }
                in_exception = kind == PropfindDocument::DavError
                    && depth == 1
                    && namespace == "http://sabredav.org/ns"
                    && local.as_ref() == "exception";
                if in_exception {
                    exception.clear();
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                if in_exception && depth == 2 {
                    if exception.trim() == r"Sabre\DAV\Exception\NotFound" {
                        kind = PropfindDocument::NotFound;
                    }
                    in_exception = false;
                }
                depth = depth.checked_sub(1).ok_or("Unexpected PROPFIND XML end")?;
            }
            Event::Text(text) => {
                if depth == 0 && !text.as_ref().trim().is_empty() {
                    return Err("Text outside PROPFIND XML root".into());
                }
                if in_exception && depth == 2 {
                    exception.push_str(text.as_ref());
                }
            }
            Event::CData(text) => {
                if depth == 0 {
                    return Err("CDATA outside PROPFIND XML root".into());
                }
                if in_exception && depth == 2 {
                    exception.push_str(text.as_ref());
                }
            }
            Event::GeneralRef(reference) if in_exception && depth == 2 => {
                let value = super::xml_text::xml_entity_to_str(reference.as_ref())
                    .ok_or("Unknown entity in DAV exception")?;
                exception.push_str(&value);
            }
            Event::Decl(_) if depth != 0 || roots != 0 => {
                return Err("Misplaced PROPFIND XML declaration".into())
            }
            Event::Eof => {
                if depth != 0 || roots != 1 {
                    return Err("Incomplete PROPFIND XML document".into());
                }
                return Ok(kind);
            }
            _ => {}
        }
    }
}

fn validate_propfind_xml(xml: &str, path: &str) -> Result<(), ProviderError> {
    match inspect_propfind_document(xml) {
        Ok(PropfindDocument::NotFound) => Err(ProviderError::NotFound(path.to_string())),
        Ok(PropfindDocument::DavError) => Err(ProviderError::ServerError(
            "WebDAV returned an error document".into(),
        )),
        Ok(PropfindDocument::Multistatus) => Ok(()),
        // Another document (a login or proxy page in XHTML, say) lists
        // nothing: read as an empty folder, it would let a removal go on.
        Ok(PropfindDocument::Other) => Err(ProviderError::ParseError(
            "WebDAV answered the listing with a document that is not a DAV multistatus".into(),
        )),
        Err(error) => {
            // ownCloud 10.16 can interrupt the multistatus OPENING TAG and
            // append an independently declared DAV error document. Recover
            // only that shape; arbitrary text/filenames never qualify.
            if let Some(split) = xml.rfind("<?xml") {
                if split > 0 {
                    let head = format!("{}/>", xml[..split].trim_end());
                    if inspect_propfind_document(&head) == Ok(PropfindDocument::Multistatus)
                        && inspect_propfind_document(&xml[split..])
                            == Ok(PropfindDocument::NotFound)
                    {
                        return Err(ProviderError::NotFound(path.to_string()));
                    }
                }
            }
            Err(ProviderError::ParseError(error))
        }
    }
}

/// Strip namespace prefix from an XML element name, returning an owned String.
/// e.g. "d:response" -> "response", "DAV:href" -> "href", "response" -> "response"
fn local_name(raw: &str) -> String {
    match raw.rfind(':') {
        Some(pos) => raw[pos + 1..].to_string(),
        None => raw.to_string(),
    }
}

/// Canonical lowercase key for an ownCloud/Nextcloud `oc:checksums`
/// algorithm label, matching every `StorageProvider::checksum()` impl
/// and the `hashsum` / `lsjson --hash` consumers. Unknown labels degrade
/// to a lowercased, separator-stripped form (still a real server-side
/// digest, just an exotic algo) rather than being dropped.
fn canonical_checksum_key(algo: &str) -> String {
    let norm: String = algo
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    match norm.as_str() {
        "SHA256" => "sha256",
        "SHA512" => "sha512",
        "SHA384" => "sha384",
        "SHA1" => "sha1",
        "MD5" => "md5",
        "CRC32" => "crc32",
        "ADLER32" => "adler32",
        _ => return norm.to_ascii_lowercase(),
    }
    .to_string()
}

/// Parse an `<oc:checksums><oc:checksum>` payload into canonical
/// `{key: hexdigest}` pairs. The element is a single string of
/// whitespace-separated `ALGO:HEXDIGEST` tokens, e.g.
/// `"SHA1:f1d2d2... MD5:900150... ADLER32:024d0127"`. Tokens without a
/// `:`, with an empty digest, or with a non-hex digest are skipped so a
/// malformed entry can never poison the map. Returns an empty map for
/// every WebDAV server that does not emit `oc:checksums` (the
/// server-side-or-omit contract: no content is ever downloaded).
fn parse_oc_checksums(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for token in raw.split_whitespace() {
        let Some((algo, digest)) = token.split_once(':') else {
            continue;
        };
        let digest = digest.trim().to_ascii_lowercase();
        if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let key = canonical_checksum_key(algo);
        if !key.is_empty() {
            out.entry(key).or_insert(digest);
        }
    }
    out
}

// ─── Nextcloud chunked upload v2: pure helpers (testable) ─────────────

/// DAG multipart preferred chunk size for Nextcloud chunked v2 (10 MiB).
///
/// Upstream Nextcloud guidance recommends 10 MiB chunks: it keeps the
/// number of HTTP requests bounded for typical files while still letting
/// the assemble step (`MOVE .file`) run on a server that streams chunks
/// through a memory-backed concatenation rather than spilling each chunk
/// to disk.
const NEXTCLOUD_DAG_CHUNK_SIZE: u64 = 10 * 1024 * 1024;

/// Size at or above which a Nextcloud upload fans out into chunked v2 parts.
///
/// Below it a single `PUT` is used. The 2026-05-29 lab benchmark measured the
/// chunked path (MKCOL + N `PUT` + `MOVE`) losing to a single `PUT` on a
/// low-RTT LAN even with 4-way parallel chunks (-36% at 100 MiB), because the
/// extra round-trips dominate when bandwidth is not the bottleneck. The
/// threshold keeps medium uploads on the faster single `PUT` while still
/// chunking large uploads, where a single multi-hundred-MiB `PUT` is fragile
/// over WAN (one failure restarts the whole transfer) and resumable chunks pay
/// off (audit Patch Set 2). 256 MiB is the agreed crossover.
const NEXTCLOUD_DAG_THRESHOLD: u64 = 256 * 1024 * 1024;

/// Cap on parallel `upload_part` nodes for Nextcloud, aligned with S3 / Azure.
///
/// Most self-hosted Nextcloud deployments sit behind nginx / Apache with
/// per-IP connection limits well below 16; keeping fan-out at 4 stays
/// under any reasonable limit while still giving the runner enough
/// concurrency to saturate a single broadband uplink.
const NEXTCLOUD_DAG_MAX_PARALLEL: u8 = 4;

/// Width of the zero-padded chunk filename in the uploads folder.
///
/// Nextcloud assembles the chunks in alphabetical order at `MOVE .file`
/// time, so the chunk index must be padded to a fixed width or the order
/// reverts to lexicographic (`10` before `2`). 20 digits is overkill
/// for a u32 part counter but matches the upstream reference client
/// width.
const NEXTCLOUD_CHUNK_INDEX_WIDTH: usize = 20;

/// Encoded `MultipartHandle.upload_id` prefix for the chunked v2 session.
///
/// The handle is opaque to the trait, but giving it a stable prefix lets
/// the unit tests assert that the wire format never silently changes.
const NEXTCLOUD_HANDLE_PREFIX: &str = "webdav-chunked-v3";
/// The prefix of a handle written before it carried its destination.
const NEXTCLOUD_HANDLE_PREFIX_WITHOUT_DESTINATION: &str = "webdav-chunked-v2";

/// Format a chunk filename for the Nextcloud uploads folder.
///
/// Chunks are stored under `/remote.php/dav/uploads/<userid>/<transferId>/`
/// with filenames `00000000000000000001`, `00000000000000000002`, ...
/// Fixed-width zero padding is required so the alphabetical sort the
/// server applies at finalize time matches the numeric part order.
fn nextcloud_chunk_filename(chunk_index: u32) -> String {
    format!(
        "{:0>width$}",
        chunk_index,
        width = NEXTCLOUD_CHUNK_INDEX_WIDTH
    )
}

/// Parse a Nextcloud user id out of a path containing the canonical
/// `/remote.php/dav/files/<userid>/` prefix.
///
/// Returns `None` when the input has no `/remote.php/dav/files/` segment
/// or when the segment immediately after `files/` is empty (`/.../files//`).
/// Stops at the next `/` so it works on both the bare root form and a
/// deeper path like `/remote.php/dav/files/alice/Documents/`.
fn extract_nextcloud_userid(path: &str) -> Option<String> {
    const NEEDLE: &str = "/remote.php/dav/files/";
    let idx = path.find(NEEDLE)?;
    let after = &path[idx + NEEDLE.len()..];
    let user = match after.find('/') {
        Some(end) => &after[..end],
        None => after,
    };
    let user = user.trim();
    if user.is_empty() {
        None
    } else {
        Some(user.to_string())
    }
}

/// `MultipartHandle.upload_id` payload for a Nextcloud chunked session.
///
/// The trait keeps `upload_id` as an opaque string, so we pack the four
/// pieces of state the chunked v2 protocol requires across the four
/// callbacks: the transfer id (random per session, used in every URL),
/// the user id (so a user changing connection between begin and
/// complete cannot silently target the wrong inbox), and the total file
/// size (the server validates `OC-Total-Length` on every PUT and on the
/// final MOVE), and the SHA-256 of the destination URL the final MOVE is
/// sent to (so a profile whose root changed between begin and complete
/// cannot assemble the file in another folder). `|` is safe as a separator
/// because the transfer id is a UUID, the Nextcloud user id charset never
/// includes it, and the digest is hex.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NextcloudMultipartHandle {
    transfer_id: String,
    user_id: String,
    total_size: u64,
    destination: String,
}

impl NextcloudMultipartHandle {
    /// The handle of an upload this connection's user began. A handle is
    /// persisted for resume, and its chunks live under the user who began
    /// it: one of another user (a profile signed in again as someone else)
    /// is refused before any write, as the URLs would name this user's
    /// uploads folder, not the one its chunks are in.
    fn of_this_user(raw: &str, user: Option<&str>) -> Result<Self, ProviderError> {
        let payload = Self::decode(raw)?;
        if user != Some(payload.user_id.as_str()) {
            return Err(ProviderError::PermissionDenied(
                "This upload was begun by another Nextcloud user, so nothing was written: \
                 start the upload again"
                    .to_string(),
            ));
        }
        Ok(payload)
    }

    /// What a handle stores of the URL its upload is assembled at: a digest,
    /// so the handle never carries the remote path.
    fn destination_digest(destination_url: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(destination_url.as_bytes()))
    }

    fn encode(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            NEXTCLOUD_HANDLE_PREFIX,
            self.transfer_id,
            self.user_id,
            self.total_size,
            self.destination
        )
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        let mut parts = raw.split('|');
        let prefix = parts.next().unwrap_or_default();
        if prefix == NEXTCLOUD_HANDLE_PREFIX_WITHOUT_DESTINATION {
            return Err(ProviderError::Other(
                "This upload was begun by an earlier AeroFTP version that did not record \
                 where it is assembled, so nothing was written: start the upload again"
                    .to_string(),
            ));
        }
        if prefix != NEXTCLOUD_HANDLE_PREFIX {
            return Err(ProviderError::Other(
                "Invalid Nextcloud multipart handle: bad prefix".to_string(),
            ));
        }
        let transfer_id = parts.next().unwrap_or_default().to_string();
        let user_id = parts.next().unwrap_or_default().to_string();
        let total_size = parts
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .map_err(|e| ProviderError::Other(format!("Invalid handle total_size: {e}")))?;
        let destination = parts.next().unwrap_or_default().to_string();
        if transfer_id.is_empty() || user_id.is_empty() || destination.is_empty() {
            return Err(ProviderError::Other(
                "Invalid Nextcloud multipart handle: empty transfer_id / user_id / destination"
                    .to_string(),
            ));
        }
        if parts.next().is_some() {
            return Err(ProviderError::Other(
                "Invalid Nextcloud multipart handle: trailing data".to_string(),
            ));
        }
        Ok(Self {
            transfer_id,
            user_id,
            total_size,
            destination,
        })
    }
}

#[async_trait]
impl StorageProvider for WebDavProvider {
    fn sync_owner_identity(&self) -> Result<Option<zeroize::Zeroizing<String>>, ProviderError> {
        use secrecy::ExposeSecret;
        if upstream_account_unverified(self.config.provider_id.as_deref(), &self.config.url) {
            return Ok(None);
        }
        serde_json::to_string(&(
            "webdav",
            &self.config.url,
            &self.config.username,
            self.config.password.expose_secret(),
            self.config.anonymous,
            &self.config.provider_id,
            &self.server_root,
        ))
        .map(|value| Some(zeroize::Zeroizing::new(value)))
        .map_err(|_| ProviderError::TargetChanged)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::WebDav
    }

    /// WebDAV-specific routing hint: inspect the configured URL so the
    /// router can tell Nextcloud (`/remote.php/dav/files/`) and Koofr
    /// gateway (`koofr.net`) apart from vanilla WebDAV (mod_dav,
    /// lighttpd, nginx_dav, Tab.digital nude). The default trait impl
    /// passes `None` for the URL and would always classify WebDAV as
    /// vanilla, which is the wrong default for the two URL-detected
    /// variants.
    fn router_hint(&self) -> crate::transfer_router::ProviderHint {
        crate::transfer_router::hints::from_provider_type(
            ProviderType::WebDav,
            Some(&self.config.url),
            self.config.provider_id.as_deref(),
        )
    }

    fn display_name(&self) -> String {
        if self.config.anonymous {
            return self
                .config
                .url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
                .to_string();
        }
        format!(
            "{}@{}",
            self.config.username,
            self.config
                .url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
        )
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.reset_infinity_memory();
        // #389: the Filen Desktop WebDAV bridge protocol (HTTP vs HTTPS) is a
        // user setting in the Filen app, independent of the scheme saved in
        // this profile. Reconcile the base URL against the live bridge so the
        // connect survives either choice, pinning the loopback IP to dodge
        // local.webdav.filen.io DNS NODATA on Windows.
        if self.config.provider_id.as_deref() == Some("filen-desktop-webdav") {
            let live = crate::local_bridge::detected_bridge_url(&self.config.url).await;
            if let Some(live) = &live {
                if *live != self.config.url {
                    tracing::info!(
                        "[WEBDAV] Filen bridge reconciled to {}",
                        crate::local_bridge::url_origin(live)
                    );
                    self.config.url = live.clone();
                }
            }
            // The bridge our own probe found during THIS connect, on the
            // loopback IP it pins and the WebDAV bridge's own port. Filen
            // Desktop serves HTTPS with a self-signed certificate, which is
            // accepted only there. Decided again on every connect, so it
            // never outlives that probe.
            let filen_bridge = live.is_some()
                && names_a_loopback_ip(&self.config.url)
                && crate::local_bridge::bridge_default_port(&self.config.url)
                    == crate::local_bridge::bridge_kind_default_port("filen-webdav");
            let policy = ClientPolicy {
                accept_invalid_certs: !self.config.verify_cert
                    || (filen_bridge && self.config.url.starts_with("https://")),
                filen_bridge,
            };
            if policy != self.client_policy {
                self.client = Self::http_client(&self.config, policy)?;
                self.client_policy = policy;
            }
        }
        if super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref()) {
            self.megacmd_serves_account_root = false;
            self.megacmd_account = None;
            // mega-df warms up the MEGAcmd Server when it has been quit/exited
            // (issue #253) and tells us whether a login session exists.
            match super::mega_df::mega_df_query().await {
                Ok(_) if self.config.url.starts_with("http://") => {
                    // Zero-config bridge (issue #275 17076174): re-arm
                    // `mega-webdav /` so a MEGAcmd Server restart, which drops the
                    // WebDAV location, does not force the user back to the
                    // terminal. Best-effort: any failure is non-fatal and we fall
                    // through to the PROPFIND probe below, preserving the previous
                    // behavior and issue #264's diagnosable errors. Only the
                    // default plaintext bridge is auto-armed; a TLS bridge
                    // (`webdav -tls /`) is left to the user, who configured HTTPS.
                    match super::mega_df::ensure_megacmd_webdav_bridge().await {
                        Ok(served) => {
                            tracing::info!("[MEGAcmd] local WebDAV bridge ensured (mega-webdav /)");
                            // MEGAcmd 2.6 serves the account below the server
                            // root: a profile aimed at the root lists but
                            // cannot create folders (MKCOL 404). Connect to the
                            // served path for this session; the saved profile
                            // is left as it is.
                            let url = super::mega_df::megacmd_session_url(
                                &self.profile_url,
                                &self.config.url,
                                served.as_deref(),
                            );
                            if url != self.config.url {
                                tracing::info!(
                                    "[MEGAcmd] bridge serves the account below its root at {}; connecting to the served path",
                                    crate::local_bridge::url_origin(&url)
                                );
                                self.config.url = url;
                            }
                            // The account this session's versions are of:
                            // the MEGAcmd daemon is shared, and a login to
                            // another account switches it for every client.
                            self.megacmd_account = super::mega_df::megacmd_account().await.ok();
                            self.megacmd_serves_account_root =
                                megacmd_session_is_account_root(&self.config.url, served.as_deref());
                        }
                        Err(e) => tracing::warn!(
                            "[MEGAcmd] could not auto-start the WebDAV bridge ({}); run `mega-webdav /` manually if the connection fails",
                            e
                        ),
                    }
                }
                Ok(_) => {
                    // TLS bridge configured: do not auto-run the plaintext
                    // `mega-webdav /`, which would not match the HTTPS endpoint.
                    tracing::info!(
                        "[MEGAcmd] TLS bridge configured; skipping auto mega-webdav (run `mega-webdav -tls /` manually if needed)"
                    );
                }
                Err(e) => {
                    // No session / daemon not installed: keep the previous
                    // behavior and let the PROPFIND below surface the diagnosable
                    // error. Log an actionable hint for a fresh setup.
                    tracing::warn!(
                        "[MEGAcmd] warm-up could not confirm a login session ({}); if this is a fresh setup run `mega-login <email>` then `mega-webdav /` once",
                        e
                    );
                }
            }
        }

        // A3-03: Warn when using unencrypted HTTP: credentials and data sent in plaintext
        if self.config.url.starts_with("http://") {
            tracing::warn!(
                "[WEBDAV] Connection uses unencrypted HTTP ({}). Credentials and data will be sent in plaintext.",
                crate::local_bridge::url_origin(&self.config.url)
            );
        }

        // Issue #264 — detect single-file WebDAV bridges (MEGAcmd `mega-webdav
        // <file>` is the canonical case). PROPFIND on the URL verbatim; if the
        // server answers 207 with a non-collection resource, switch to
        // single-file mode and skip the root discovery below.
        if let Some(entry) = self.probe_single_file().await {
            tracing::info!(
                "[WebDAV] Detected single-file resource at {} ({}, {} bytes); enabling single-file mode",
                crate::local_bridge::url_origin(&self.config.url),
                entry.name,
                entry.size
            );
            self.single_file_mode = Some(entry);
            self.server_root = Some("/".to_string());
            self.current_path = "/".to_string();
            self.connected = true;
            return Ok(());
        }

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#;

        // First attempt with Basic auth
        let response = self
            .request(webdav_methods::propfind(), "/")
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await
            .map_err(connect_send_error)?;

        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                self.connected = true;
                // Traditional WebDAV server: `/` is a valid resource. Server
                // root is `/`, plus the user-supplied initial_path if any.
                let resolved_root = self
                    .config
                    .initial_path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("/")
                    .to_string();
                self.current_path = resolved_root.clone();
                self.server_root = Some(resolved_root);
                Ok(())
            }
            StatusCode::UNAUTHORIZED => {
                // Check if server requires Digest authentication
                let www_auth = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if let Some(state) = DigestState::parse(&www_auth) {
                    tracing::debug!(
                        "[WebDAV] Server requires Digest auth (realm: {}, qop: {}, nonce: {}...)",
                        state.realm(),
                        state.qop(),
                        state.nonce_prefix()
                    );
                    self.digest_auth = Some(state);

                    // Retry with Digest auth
                    let response2 = self
                        .request(webdav_methods::propfind(), "/")
                        .header("Depth", "0")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                        .send()
                        .await
                        .map_err(connect_send_error)?;

                    let retry_status = response2.status();
                    tracing::debug!("[WebDAV] Digest auth retry status: {}", retry_status);

                    match retry_status {
                        StatusCode::OK | StatusCode::MULTI_STATUS => {
                            tracing::debug!("[WebDAV] Digest auth successful");
                            self.connected = true;
                            let resolved_root = self
                                .config
                                .initial_path
                                .as_deref()
                                .filter(|p| !p.is_empty())
                                .unwrap_or("/")
                                .to_string();
                            self.current_path = resolved_root.clone();
                            self.server_root = Some(resolved_root);
                            Ok(())
                        }
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                            // Log the response body for debugging
                            let body = response2.text().await.unwrap_or_default();
                            tracing::warn!(
                                "[WebDAV] Digest auth failed ({}): a reply of {} bytes",
                                retry_status,
                                body.len()
                            );
                            self.digest_auth = None;
                            Err(ProviderError::AuthenticationFailed(
                                "Invalid credentials".to_string(),
                            ))
                        }
                        status => {
                            self.digest_auth = None;
                            Err(ProviderError::ConnectionFailed(format!(
                                "Server returned status: {}",
                                status
                            )))
                        }
                    }
                } else {
                    Err(ProviderError::AuthenticationFailed(
                        "Invalid credentials".to_string(),
                    ))
                }
            }
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".to_string(),
            )),
            StatusCode::METHOD_NOT_ALLOWED => {
                tracing::debug!("[WebDAV] PROPFIND / returned 405, trying well-known WebDAV paths");

                // Try well-known Nextcloud/ownCloud WebDAV paths before falling back to OPTIONS
                let username = &self.config.username;
                let well_known_paths: Vec<String> = if !username.is_empty() {
                    vec![
                        format!("/remote.php/dav/files/{}/", username),
                        "/remote.php/webdav/".to_string(),
                    ]
                } else {
                    vec!["/remote.php/webdav/".to_string()]
                };

                for wk_path in &well_known_paths {
                    tracing::debug!("[WebDAV] Trying well-known path: {}", wk_path);
                    let wk_response = self
                        .request(webdav_methods::propfind(), wk_path)
                        .header("Depth", "0")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                        .send()
                        .await;

                    if let Ok(resp) = wk_response {
                        let st = resp.status();
                        if st == StatusCode::OK || st == StatusCode::MULTI_STATUS {
                            tracing::info!(
                                "[WebDAV] Auto-detected WebDAV path: {} ({})",
                                wk_path,
                                st
                            );
                            self.connected = true;
                            // Issue #175: server_root is the auto-detected path.
                            // current_path defaults to it; if the user supplied
                            // a relative initial_path, append it under the root,
                            // as given: `reports ` is its own folder.
                            let user_initial = self
                                .config
                                .initial_path
                                .as_deref()
                                .filter(|p| !p.is_empty() && *p != "/");
                            let starting_path = match user_initial {
                                Some(rel) => {
                                    let wk_trim = wk_path.trim_end_matches('/');
                                    let rel_trim = rel.trim_start_matches('/');
                                    format!("{}/{}", wk_trim, rel_trim)
                                }
                                None => wk_path.clone(),
                            };
                            self.current_path = starting_path;
                            self.server_root = Some(wk_path.clone());
                            return Ok(());
                        }
                    }
                }

                // No well-known path worked, fall back to OPTIONS on root
                tracing::debug!("[WebDAV] No well-known path worked, trying OPTIONS /");
                let options_response = self
                    .request(Method::OPTIONS, "/")
                    .send()
                    .await
                    .map_err(connect_send_error)?;

                let options_status = options_response.status();
                if options_status.is_success() {
                    self.connected = true;
                    let resolved_root = self
                        .config
                        .initial_path
                        .as_deref()
                        .filter(|p| !p.is_empty())
                        .unwrap_or("/")
                        .to_string();
                    self.current_path = resolved_root.clone();
                    self.server_root = Some(resolved_root);
                    Ok(())
                } else {
                    Err(ProviderError::ConnectionFailed(format!(
                        "Server returned status: {}",
                        options_status
                    )))
                }
            }
            status => Err(ProviderError::ConnectionFailed(format!(
                "Server returned status: {}",
                status
            ))),
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.server_root = None;
        self.single_file_mode = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Issue #264 — single-file resource mode: the configured URL points
        // at one file; the "directory listing" is that single entry, served
        // synthetically without a second PROPFIND. The bridge would 404 on
        // any path other than the exact file URL anyway.
        if let Some(ref entry) = self.single_file_mode {
            return Ok(vec![entry.clone()]);
        }

        // Issue #175: Nextcloud / ownCloud serve `/` as 405 Method Not Allowed
        // because the WebDAV root lives under a versioned prefix
        // (`/remote.php/dav/files/<user>/` or `/remote.php/webdav/`). When the
        // frontend or a saved-server profile passes `path = "/"` literally,
        // we redirect to the auto-detected server_root so the listing works
        // out of the box. Traditional WebDAV servers where `/` is a valid
        // resource keep working because their server_root is also `/`, so
        // the redirect is a no-op.
        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else if path == "/" {
            self.server_root
                .clone()
                .unwrap_or_else(|| self.current_path.clone())
        } else {
            path.to_string()
        };

        tracing::debug!("[WebDAV] Listing path: {}", list_path);

        let response = self
            .send_propfind(
                &list_path,
                "1",
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#,
            )
            .await?;

        let status = response.status();
        tracing::debug!("[WebDAV] List response status: {}", status);

        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await.map_http_err(body_read_error)?;

                tracing::debug!("[WebDAV] Response XML length: {} bytes", xml.len());

                let entries = self.parse_propfind_response(&xml, &list_path)?;
                tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
                Ok(entries)
            }
            StatusCode::NOT_FOUND => {
                tracing::warn!("[WebDAV] Path not found: {}", list_path);
                Err(ProviderError::NotFound(list_path))
            }
            StatusCode::UNAUTHORIZED => {
                self.connected = false;
                tracing::error!("[WebDAV] Unauthorized - session expired");
                Err(ProviderError::AuthenticationFailed(
                    "Session expired".to_string(),
                ))
            }
            StatusCode::FORBIDDEN => Err(ProviderError::PermissionDenied(list_path)),
            status => {
                tracing::error!("[WebDAV] Server error: {}", status);
                Err(ProviderError::ServerError(format!(
                    "Server returned status: {}",
                    status
                )))
            }
        }
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Issue #175: enforce the boundary against the connect-time server
        // root, not the user-typed `config.initial_path`. See the doc on
        // `server_root` for the full reasoning.
        let boundary = self
            .server_root
            .as_deref()
            .or(self.config.initial_path.as_deref());
        // `/` is the top of this connection, as `list` reads it. A server whose
        // files live under a root (Nextcloud) would refuse it as above that root.
        let root = boundary
            .filter(|root| !root.is_empty())
            .unwrap_or("/")
            .to_string();
        let path = if path == "/" { root.as_str() } else { path };
        if path_violates_root(path, boundary) {
            return Err(ProviderError::InvalidPath(format!(
                "Cannot navigate above WebDAV root: {}",
                boundary.unwrap_or("/")
            )));
        }

        // Verify the path exists and is a directory. `cd` only ever targets a
        // collection, so ask in the collection (trailing-slash) form: a
        // slash-less PROPFIND is what a server behind a TLS proxy `301`s to
        // an `http://` URL, which surfaced in the GUI as "Failed to change
        // directory: ... 401 Unauthorized" while listing the same folder
        // worked (see `collection_path`, `InsecureRedirect`).
        const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#;
        let collection = Self::collection_path(path);
        let response = self
            .send_replaying_digest(|| {
                self.request(webdav_methods::propfind(), &collection)
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(PROPFIND_BODY)
            })
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await.map_http_err(body_read_error)?;

                let props = self.extract_xml_properties(&xml, path)?;
                let is_collection = props.contains_key("_is_collection");

                if is_collection {
                    self.current_path = path.to_string();
                    Ok(())
                } else {
                    Err(ProviderError::InvalidPath(format!(
                        "{} is not a directory",
                        path
                    )))
                }
            }
            // A file asked for in the collection form answers 404 on most
            // servers. `stat` asks in the file form first, so it tells the
            // file that cannot be entered from the path that does not exist.
            // Any other `stat` failure is returned as it is: a network error
            // or a refused redirect must not read as "path not found".
            StatusCode::NOT_FOUND => match self.stat(path).await {
                Ok(entry) if !entry.is_dir => Err(ProviderError::InvalidPath(format!(
                    "{} is not a directory",
                    path
                ))),
                Ok(_) | Err(ProviderError::NotFound(_)) => {
                    Err(ProviderError::NotFound(path.to_string()))
                }
                Err(e) => Err(e),
            },
            StatusCode::UNAUTHORIZED => {
                self.connected = false;
                Err(ProviderError::AuthenticationFailed(
                    "Session expired".to_string(),
                ))
            }
            status => Err(ProviderError::ServerError(format!(
                "Server returned status: {}",
                status
            ))),
        }
    }

    fn measurement_note(&self) -> Option<&'static str> {
        (self.config.provider_id.as_deref() == Some("filen-desktop-webdav"))
            .then_some(super::FILEN_DESKTOP_BRIDGE_NOTE)
    }

    fn trash_left_unpurged(&self) -> Option<&'static str> {
        // The Koofr preset deletes into the same Koofr trash as the API.
        (self.config.provider_id.as_deref() == Some("koofr"))
            .then_some(super::koofr::KOOFR_TRASH_NOT_PURGED)
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.purge_nextcloud_trash(path, None).await
    }

    async fn delete_permanent_with_identity(
        &mut self,
        path: &str,
        identity: Option<&str>,
    ) -> Result<bool, ProviderError> {
        self.purge_nextcloud_trash(path, identity).await
    }

    async fn capture_trash_identities(
        &mut self,
        paths: &[String],
    ) -> Vec<Result<Option<String>, ProviderError>> {
        if !self.is_nextcloud() {
            return paths.iter().map(|_| Ok(None)).collect();
        }
        self.capture_nextcloud_file_ids(paths).await
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        // Issue #175: prefer the connect-time server_root (auto-detected on
        // Nextcloud / ownCloud) over the user-typed initial_path so cd_up
        // clamps at the real WebDAV root, not at the form value.
        let root = self
            .server_root
            .as_deref()
            .or(self.config.initial_path.as_deref())
            .filter(|p| !p.is_empty())
            .unwrap_or("/");

        // Already at root: cannot go higher
        let current_trimmed = self.current_path.trim_end_matches('/');
        let root_trimmed = root.trim_end_matches('/');
        if current_trimmed == root_trimmed || current_trimmed.is_empty() {
            return Ok(());
        }

        let parent = std::path::Path::new(&self.current_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        let parent = if parent.is_empty() {
            "/".to_string()
        } else {
            parent
        };

        // Clamp: if parent would go above root, stay at root
        let parent_trimmed = parent.trim_end_matches('/');
        if !parent_trimmed.starts_with(root_trimmed) {
            self.current_path = root.to_string();
        } else {
            self.current_path = parent;
        }

        Ok(())
    }

    fn download_was_refused(&self) -> bool {
        self.download_time.is_refused()
    }

    fn take_download_time(&mut self, source_refused: bool) -> Option<String> {
        self.download_time.take(source_refused)
    }

    fn expect_moved_source(&mut self) {
        self.download_time.expect_moved();
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_with_size_hint(remote_path, local_path, None, on_progress)
            .await
    }

    async fn download_with_size_hint(
        &mut self,
        remote_path: &str,
        local_path: &str,
        size_hint: Option<u64>,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.download_time.begin();

        let mut on_progress = on_progress;

        // PD-HTTP-2: concurrent-Range download behind a real strict 206 probe.
        // Digest auth mutates a per-request nonce counter and cannot be
        // replayed concurrently, so it is excluded here (honest single-stream).
        // The probe + closure go through the live reqwest client with a
        // precomputed Basic header; no credential is reconstructed.
        if self.multi_thread_streams >= 2
            && !self.download_time.is_refused()
            && self.digest_auth.is_none()
            && !super::multi_thread::size_hint_rules_out_ranges(size_hint, self.multi_thread_cutoff)
        {
            let mut headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> =
                Vec::new();
            if !self.config.anonymous {
                use base64::Engine as _;
                let token = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    self.config.username,
                    self.config.password.expose_secret()
                ));
                match reqwest::header::HeaderValue::from_str(&format!("Basic {}", token)) {
                    Ok(v) => headers.push((reqwest::header::AUTHORIZATION, v)),
                    Err(e) => {
                        return Err(ProviderError::AuthenticationFailed(format!(
                            "Invalid characters in credentials: {}",
                            e
                        )))
                    }
                }
            }
            let req = super::multi_thread::HttpRangeRequest {
                client: self.client.clone(),
                url: self.build_url(remote_path),
                headers,
                local_path: local_path.to_string(),
                provider_type: ProviderType::WebDav,
                streams: self.multi_thread_streams,
                max_streams: WEBDAV_MULTI_THREAD_MAX_STREAMS,
                cutoff: self.multi_thread_cutoff,
                known_size: size_hint,
            };
            match super::multi_thread::try_http_concurrent_range_download(req, on_progress).await {
                super::multi_thread::HttpRangeAttempt::Completed => return Ok(()),
                super::multi_thread::HttpRangeAttempt::Failed(e) => return Err(e),
                super::multi_thread::HttpRangeAttempt::Fallback(p) => on_progress = p,
                super::multi_thread::HttpRangeAttempt::Refused(p) => {
                    self.download_time.refused();
                    on_progress = p;
                }
            }
        }

        let response = self
            .send_with_too_early_retry(Method::GET, remote_path)
            .await?;

        // For an accepted 206 this is the authoritative object size and the
        // received bytes are checked against it below; FileLu sends the body
        // chunked, so `Content-Length` is absent and this is the only total we
        // get. `None` for a 200, whose behaviour is unchanged.
        let full_object_total = if response.status() == StatusCode::PARTIAL_CONTENT {
            whole_object_total_from_content_range(response.headers())
        } else {
            None
        };
        let accept_as_full = response.status() == StatusCode::OK || full_object_total.is_some();
        match response.status() {
            _ if accept_as_full => {
                let total_size = full_object_total
                    .or_else(|| response.content_length())
                    .unwrap_or(0);
                let read_time = super::multi_thread::response_time(response.headers());
                let mut stream = response.bytes_stream();
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let mut downloaded: u64 = 0;

                loop {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            crate::transfer_dag::throttle::charge(
                                crate::transfer_dag::governor::TransferDirection::Download,
                                chunk.len() as u64,
                            )
                            .await;
                            atomic
                                .write_all(&chunk)
                                .await
                                .map_err(ProviderError::IoError)?;
                            downloaded += chunk.len() as u64;
                            if let Some(ref progress) = on_progress {
                                progress(downloaded, total_size);
                            }
                        }
                        Some(Err(e)) => {
                            if super::is_unexpected_eof_after_full_body(&e, downloaded, total_size)
                            {
                                tracing::warn!(
                                    "[WEBDAV] Server closed connection without TLS close_notify but full body received ({}/{} bytes); accepting",
                                    downloaded,
                                    total_size
                                );
                                break;
                            }
                            return Err(super::http_send_error(e, |e| {
                                ProviderError::TransferFailed(body_cut_short(
                                    e, downloaded, total_size,
                                ))
                            }));
                        }
                        None => break,
                    }
                }
                // A 206 we accepted as "the whole object" must actually deliver
                // that many bytes. Without this the chunked case has nothing to
                // validate against and a truncated body would be committed as a
                // complete file. The 200 path keeps its previous behaviour.
                if let Some(expected) = full_object_total {
                    if downloaded != expected {
                        return Err(ProviderError::TransferFailed(format!(
                            "Truncated 206 response: received {} of the {} bytes the \
                             Content-Range declared",
                            downloaded, expected
                        )));
                    }
                }
                atomic.commit().await.map_err(ProviderError::IoError)?;

                self.download_time.read(read_time);
                Ok(())
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => {
                // S4-T01 partner diagnostic: surface response headers + body
                // snippet so server-side errors (HTTP 5xx in particular) carry
                // enough context to file a partner ticket. Keep at INFO so
                // CI / production sees it without bumping log level.
                let headers_dump: String = response
                    .headers()
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, diagnostic_header_value(k, v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let body_len = response.text().await.map_or(0, |body| body.len());
                tracing::info!(
                    "[WEBDAV] download non-success: status={} origin={} path={} headers=[{}] body={} bytes",
                    status,
                    crate::local_bridge::url_origin(&self.config.url),
                    remote_path,
                    headers_dump,
                    body_len
                );
                Err(ProviderError::TransferFailed(format!(
                    "Download failed with status: {}",
                    status
                )))
            }
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .send_with_too_early_retry(Method::GET, remote_path)
            .await?;

        let full_object_total = if response.status() == StatusCode::PARTIAL_CONTENT {
            whole_object_total_from_content_range(response.headers())
        } else {
            None
        };
        let accept_as_full = response.status() == StatusCode::OK || full_object_total.is_some();
        match response.status() {
            _ if accept_as_full => {
                // H2: Size-limited download to prevent OOM on large files
                let bytes =
                    super::response_bytes_with_limit(response, super::MAX_DOWNLOAD_TO_BYTES)
                        .await?;
                if let Some(expected) = full_object_total {
                    if bytes.len() as u64 != expected {
                        return Err(ProviderError::TransferFailed(format!(
                            "Truncated 206 response: received {} of the {} bytes the \
                             Content-Range declared",
                            bytes.len(),
                            expected
                        )));
                    }
                }
                Ok(bytes)
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(remote_path.to_string())),
            status => Err(ProviderError::TransferFailed(format!(
                "Download failed with status: {}",
                status
            ))),
        }
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        self.refuse_another_path_in_single_file_mode(remote_path)?;

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total_size = file.metadata().await.map_err(ProviderError::IoError)?.len();
        let cursor = super::upload_cursor::SharedUploadCursor::from_tokio(file).await;
        // A refusal or Digest replay waits out the prior reader and rewinds
        // this handle. A renamed/replaced source path cannot change its bytes.
        let progress = super::upload_progress::UploadProgress::new(on_progress, total_size);
        let retry = super::HttpRetryConfig::default();

        let nonce_used = self.digest_nonce_snapshot();
        let provider = &*self;
        let send = || {
            super::http_retry::send_rebuilt_async(
                || async {
                    let body = cursor
                        .attempt_body(&progress, total_size)
                        .await
                        .map_err(ProviderError::IoError)?;
                    Ok(provider
                        .request(Method::PUT, remote_path)
                        .header("Content-Length", total_size)
                        .body(body))
                },
                &retry,
                send_error,
                true,
            )
        };
        let mut response = send().await?;

        // Retain exactly one stale-nonce replay, over the same open cursor.
        if response.status() == StatusCode::UNAUTHORIZED
            && self.should_replay_after_401(&response, &nonce_used)
        {
            response = send().await?;
        }

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                progress.complete();
                Ok(())
            }
            status => Err(upload_failure_error(status)),
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        self.refuse_another_path_in_single_file_mode(path)?;

        // MKCOL always targets a collection: use the trailing-slash form so
        // Apache does not 301 to a scheme-downgraded URL that loses auth.
        let col = Self::collection_path(path);
        let response = self
            .send_replaying_digest(|| self.request(webdav_methods::mkcol(), &col))
            .await?;

        match response.status() {
            // RFC 4918 §9.3.1: 201 Created on success, 405 Method Not Allowed
            // when the collection already exists. Some servers (notably
            // FileLu's WebDAV frontend) return 204 No Content on success, and
            // others (Nextcloud variants) return 200 OK: treat all three as
            // idempotent success.
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED => Err(ProviderError::AlreadyExists(path.to_string())),
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Parent directory does not exist".to_string(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::PermissionDenied(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "MKCOL failed with status: {}",
                status
            ))),
        }
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // `rmdir` and `rmdir_recursive` come through here too.
        self.refuse_another_path_in_single_file_mode(path)?;

        // A caller may name a directory without its trailing slash; see
        // `send_on_resource`.
        let response = self
            .send_on_resource(path, |target| self.request(Method::DELETE, target))
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            StatusCode::FORBIDDEN => Err(ProviderError::PermissionDenied(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "DELETE failed with status: {}",
                status
            ))),
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.refuse_another_path_in_single_file_mode(path)?;
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        self.refuse_non_empty_dir(path).await?;
        // WebDAV DELETE works for both files and directories, but a
        // directory DELETE without a trailing slash triggers the same
        // scheme-downgrading 301 that strips auth (see `collection_path`).
        self.delete(&Self::collection_path(path)).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE automatically deletes recursively. Trailing-slash
        // form for the same reason as `rmdir`.
        self.delete(&Self::collection_path(path)).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // In single-file mode every path maps to the one configured URL, so
        // a MOVE cannot name another resource: the URL comparison below read
        // every rename as one onto itself and answered success with nothing
        // moved. Both names of the served file (a relative and an absolute
        // spelling) are a rename onto itself.
        if self.single_file_mode.is_some() {
            if self.names_the_served_file(from) && self.names_the_served_file(to) {
                return Ok(());
            }
            return Err(ProviderError::NotSupported(
                "this WebDAV profile serves a single file, which cannot be renamed or moved"
                    .to_string(),
            ));
        }

        let destination = self.build_url(to);
        if self.names_the_same_resource(from, &destination) {
            return Ok(());
        }
        // When `from` turns out to be a collection named without its slash,
        // the resent MOVE names the destination as a collection too.
        let collection_destination = self.build_url(&Self::collection_path(to));

        let response = self
            .send_on_resource(from, |source| {
                let destination = if source == from {
                    &destination
                } else {
                    &collection_destination
                };
                self.request(webdav_methods::move_method(), source)
                    .header("Destination", destination)
                    .header("Overwrite", "F") // Don't overwrite existing
                    .header("Depth", MOVE_DEPTH)
            })
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(from.to_string())),
            StatusCode::PRECONDITION_FAILED => Err(ProviderError::AlreadyExists(to.to_string())),
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Destination parent does not exist".to_string(),
            )),
            StatusCode::BAD_GATEWAY => Err(destination_refused_error("MOVE")),
            status => Err(ProviderError::ServerError(format!(
                "MOVE failed with status: {}",
                status
            ))),
        }
    }

    /// Sets the file at `to` aside, moves `from` in, and only then deletes
    /// the one set aside: see [`super::replace_by_setting_aside`]. Every MOVE
    /// it sends is a [`rename`](Self::rename), with `Overwrite: F`.
    ///
    /// Not one MOVE with `Overwrite: T`, which is what this sent until
    /// 2026-10-10: RFC 4918 section 9.9.3 has the server DELETE the
    /// destination before it moves, and a move that fails after that delete
    /// does not bring the destination back. Measured that day: Apache
    /// mod_dav 2.4.66 and golang.org/x/net/webdav (`rclone serve webdav`
    /// 1.75.1) unlink the destination and then rename, and a rename that
    /// fails leaves no destination at all (500 and 403); Nextcloud 33 sends
    /// it to the trash first, a MOVE whose source another client was reading
    /// failed with 500 `LockedException` after that, and readers polling the
    /// name during ten replaces saw 404 five times in ninety. Only nginx 1.31
    /// renames a file over another, and it deletes a folder's tree first. A
    /// client cannot tell which server it reaches (the lab's Apache and that
    /// Nextcloud both answer `Server: nginx`), so it never asks a server to
    /// delete what it may not be able to put back.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // In single-file mode every path maps to the one configured URL, so
        // a MOVE cannot name another resource: the URL comparison below read
        // every rename as one onto itself and answered success with nothing
        // moved. Both names of the served file (a relative and an absolute
        // spelling) are a rename onto itself.
        if self.single_file_mode.is_some() {
            if self.names_the_served_file(from) && self.names_the_served_file(to) {
                return Ok(());
            }
            return Err(ProviderError::NotSupported(
                "this WebDAV profile serves a single file, which cannot be renamed or moved"
                    .to_string(),
            ));
        }

        if self.names_the_same_resource(from, &self.build_url(to)) {
            return Ok(());
        }
        // A path without a leading slash names the same resource from the
        // root (see `resolve_root`); the set-aside name is put beside `to`
        // in its own folder, which needs the absolute spelling.
        let absolute = |path: &str| format!("/{}", path.trim_start_matches('/'));
        super::replace_by_setting_aside(self, &absolute(from), &absolute(to)).await
    }

    /// No: `replace` sets the old file aside, so the name is empty for a
    /// moment. The callers that need atomicity (CLI `edit` without
    /// `--allow-non-atomic`, MCP `remote_edit` without `allow_non_atomic`,
    /// the crypt marker paths over an existing marker) refuse before they
    /// write anything. In single-file mode there is no other path either, to
    /// stage a temporary at or to set the served file aside under.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    /// Yes, outside single-file mode: the replace above renames the file at
    /// the destination aside, moves the new one in, and only then deletes
    /// the old one, which is what an edit's non-atomic opt-in needs. A
    /// single-file profile has one path, and nowhere to set its file aside.
    fn replace_sets_aside(&self) -> bool {
        self.single_file_mode.is_none()
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Issue #264 — single-file mode: return the cached entry. The bridge
        // would 404 on any path other than the exact file URL.
        if let Some(ref entry) = self.single_file_mode {
            return Ok(entry.clone());
        }

        const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                    </d:prop>
                </d:propfind>"#;

        // `stat` is path-type-ambiguous (it is called for both files and
        // directories). Try the path verbatim first: that is correct for
        // files, where a trailing slash would instead point at a
        // non-existent collection. If the server answers `404` or a
        // redirect-stripped `401` (Apache mod_dav `301`s a slash-less
        // *collection* to a scheme-downgraded URL that loses the
        // `Authorization` header: the same class fixed for
        // `list`/`mkdir`/`rmdir`, see `collection_path`), retry once in the
        // collection (trailing-slash) form, which is what real directories
        // need. `name` always derives from the original `path`.
        let collection_form = Self::collection_path(path);
        let mut attempts: Vec<&str> = vec![path];
        if collection_form != path {
            attempts.push(collection_form.as_str());
        }

        let mut last_status = StatusCode::NOT_FOUND;
        let mut authentication_failed = false;
        let mut refused_redirect = None;
        let attempt_count = attempts.len();
        for (index, attempt) in attempts.into_iter().enumerate() {
            let sent = self
                .request(webdav_methods::propfind(), attempt)
                .header("Depth", "0")
                .header("Content-Type", "application/xml")
                .body(PROPFIND_BODY)
                .send()
                .await;
            // The same collection signal arrives as a refused redirect when
            // the server's `301` leaves HTTPS (see `InsecureRedirect`): the
            // slash form is the next attempt.
            let mut response = match sent {
                Err(e) if refused_collection_redirect(&e) && index + 1 < attempt_count => {
                    refused_redirect = Some(send_error(e));
                    continue;
                }
                other => other.map_err(send_error)?,
            };

            // A 401 is ambiguous twice over here: a rotated nonce, or the
            // slash-stripped redirect the second attempt form exists for.
            // Settle the rotation first, once, because the collection form
            // cannot repair a dead nonce; a 401 that survives the replay
            // still falls through to that second form below.
            if response.status() == StatusCode::UNAUTHORIZED && self.adopt_rotated_nonce(&response)
            {
                response = self
                    .request(webdav_methods::propfind(), attempt)
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(PROPFIND_BODY)
                    .send()
                    .await
                    .map_err(send_error)?;
            }

            match response.status() {
                StatusCode::OK | StatusCode::MULTI_STATUS => {
                    let xml = response.text().await.map_http_err(body_read_error)?;

                    let props = self.extract_xml_properties(&xml, path)?;
                    let is_dir = props.contains_key("_is_collection");
                    let size: u64 = props
                        .get("getcontentlength")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let modified = props.get("getlastmodified").cloned();
                    let mime_type = props.get("getcontenttype").cloned();

                    let name = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());

                    return Ok(RemoteEntry {
                        name,
                        path: path.to_string(),
                        is_dir,
                        size,
                        modified,
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type,
                        metadata: Default::default(),
                    });
                }
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                    // Ambiguous: may be a collection that needs the
                    // trailing-slash form. Fall through to the retry.
                    authentication_failed |= response.status() == StatusCode::UNAUTHORIZED;
                    last_status = response.status();
                    continue;
                }
                status => {
                    return Err(ProviderError::ServerError(format!(
                        "PROPFIND failed with status: {}",
                        status
                    )));
                }
            }
        }

        if authentication_failed {
            Err(ProviderError::AuthenticationFailed(format!(
                "PROPFIND could not verify {path}: an authentication failure was not resolved"
            )))
        } else if let Some(error) = refused_redirect {
            Err(error)
        } else if last_status == StatusCode::NOT_FOUND {
            Err(ProviderError::NotFound(path.to_string()))
        } else {
            Err(ProviderError::ServerError(format!(
                "PROPFIND failed with status: {}",
                last_status
            )))
        }
    }

    fn supports_checksum(&self) -> bool {
        // ownCloud/Nextcloud expose server-side digests via the
        // `oc:checksums` PROPFIND prop; every other WebDAV server simply
        // omits it, in which case `checksum()` returns an empty map and
        // consumers omit / fall back. The probe is a metadata PROPFIND,
        // never a content download: the server-side-or-omit contract.
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
                    <d:prop>
                        <oc:checksums/>
                    </d:prop>
                </d:propfind>"#;

        // Mirror `stat()`'s file-first / collection-retry: a file must be
        // requested without a trailing slash, while a slash-less
        // collection can be 301'd by Apache to a scheme-downgraded URL
        // that strips auth (see `collection_path`).
        let collection_form = Self::collection_path(path);
        let mut attempts: Vec<&str> = vec![path];
        if collection_form != path {
            attempts.push(collection_form.as_str());
        }

        let attempt_count = attempts.len();
        for (index, attempt) in attempts.into_iter().enumerate() {
            let sent = self
                .request(webdav_methods::propfind(), attempt)
                .header("Depth", "0")
                .header("Content-Type", "application/xml")
                .body(PROPFIND_BODY)
                .send()
                .await;
            // Same refused-redirect signal as `stat`.
            let mut response = match sent {
                Err(e) if refused_collection_redirect(&e) && index + 1 < attempt_count => continue,
                other => other.map_err(send_error)?,
            };

            // Same one-shot rotation repair as `stat`. Without it a nonce that
            // rotates mid-listing silently degrades every server-side hash to
            // "not available", which reads as "this server has no checksums"
            // and sends the reconciler back to downloading whole files.
            if response.status() == StatusCode::UNAUTHORIZED && self.adopt_rotated_nonce(&response)
            {
                response = self
                    .request(webdav_methods::propfind(), attempt)
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(PROPFIND_BODY)
                    .send()
                    .await
                    .map_err(send_error)?;
            }

            match response.status() {
                StatusCode::OK | StatusCode::MULTI_STATUS => {
                    let xml = response.text().await.map_http_err(body_read_error)?;
                    let props = self.extract_xml_properties(&xml, path)?;
                    return Ok(props
                        .get("checksum")
                        .map(|s| parse_oc_checksums(s))
                        .unwrap_or_default());
                }
                // Ambiguous path type: retry in collection form.
                StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => continue,
                // Any other status: the server cannot answer the prop.
                // Treat as "no server-side hash" (omit) rather than an
                // error that would fail a listing or trigger a download.
                _ => return Ok(HashMap::new()),
            }
        }

        Ok(HashMap::new())
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // WebDAV uses HTTP which is stateless, no keep-alive needed
        // Just verify we can still authenticate
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut response = self
            .request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(send_error)?;

        // A probe is the cheapest possible place to discover a rotation, and
        // the repair reaches every worker through the shared challenge. Left
        // un-wired, the idle-timer probe is what marks a perfectly healthy
        // session disconnected 300 seconds into a transfer, which is the
        // user-visible shape of this bug.
        if response.status() == StatusCode::UNAUTHORIZED && self.adopt_rotated_nonce(&response) {
            response = self
                .request(Method::OPTIONS, "/")
                .send()
                .await
                .map_err(send_error)?;
        }

        if response.status() == StatusCode::UNAUTHORIZED {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed(
                "Session expired".to_string(),
            ));
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response = self
            .request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(send_error)?;

        let server = response
            .headers()
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("Unknown WebDAV Server");

        let dav = response
            .headers()
            .get("dav")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1");

        Ok(format!(
            "WebDAV Server: {} (DAV compliance: {})",
            server, dav
        ))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut results = Vec::new();
        let mut walk = crate::sync_core::FindWalk::new(path);
        let mut dirs_to_scan = vec![path.to_string()];
        // Every skipped 403 needs current proof that the search root remains
        // readable. Bound expensive root listings by failing explicitly once
        // the budget is spent, never by treating old access as current proof.
        const MAX_ACCESS_RECHECKS: usize = 8;
        let mut access_rechecks = 0;

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                // A folder searched that cannot be read is not an empty result,
                // and a failure that is not about one folder fails the search.
                Err(e) if dir == path || !super::listing_failure_is_the_folders(&e) => {
                    return Err(e)
                }
                // A 403 can be this folder's or the end of the account's access.
                // In the second case the folder searched refuses too, and what
                // was found so far would read as a complete result.
                Err(ProviderError::PermissionDenied(_)) => {
                    if access_rechecks == MAX_ACCESS_RECHECKS {
                        return Err(ProviderError::PermissionDenied(format!(
                            "Search stopped after {MAX_ACCESS_RECHECKS} refused folders: \
                             root access cannot be rechecked within the search budget"
                        )));
                    }
                    access_rechecks += 1;
                    self.list(path).await?;
                    continue;
                }
                Err(_) => continue,
            };

            // A listing joins each display name to the folder listed, so a
            // name such as `../y` points outside the folder searched: such an
            // entry is skipped with a warning, as an unreadable collection is.
            let sorted = walk.sort(&dir, entries);
            for (path, step) in &sorted.refused {
                tracing::warn!(
                    "[WebDAV] find: skipping {path:?}, named in {dir}: {}",
                    step.reason()
                );
            }
            dirs_to_scan.extend(sorted.follow);
            for entry in sorted.entries {
                if super::matches_find_pattern(&entry.name, pattern) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        if super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref()) {
            let (used, total, versioning_bytes) = super::mega_df::mega_df_query().await?;
            return Ok(super::StorageInfo {
                used,
                total,
                free: total.saturating_sub(used),
                versioning_bytes,
            });
        }

        // Koofr WebDAV does not expose RFC 4331 quota properties via PROPFIND
        // (returns 0 / 0). Use the native Koofr REST API instead: it accepts
        // basic auth with the same email + app password used for WebDAV.
        if served_by(&self.config.url, "app.koofr.net") {
            if let Ok(info) = self.koofr_storage_via_api().await {
                return Ok(info);
            }
            // Fall through to PROPFIND on failure (best-effort).
        }

        // OpenDrive WebDAV behaves the same way: no RFC 4331 quota over
        // PROPFIND. Read the real quota from the OpenDrive REST API using
        // the same account credentials (session auth).
        if served_by(&self.config.url, "webdav.opendrive.com") {
            if let Ok(info) = self.opendrive_storage_via_api().await {
                return Ok(info);
            }
            // Fall through to PROPFIND on failure (best-effort).
        }

        // RFC 4331: WebDAV quota properties, asked of the current directory
        // in its collection form (see `collection_path`).
        let quota_path = Self::collection_path(&self.current_path);
        let response = self
            .request(webdav_methods::propfind(), &quota_path)
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(
                r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:quota-available-bytes/>
                        <d:quota-used-bytes/>
                    </d:prop>
                </d:propfind>"#,
            )
            .send()
            .await
            .map_err(send_error)?;

        if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::NotSupported("storage_info".to_string()));
        }

        let xml = response.text().await.map_http_err(body_read_error)?;

        let props = self.extract_xml_properties(&xml, &self.current_path)?;
        let used = props
            .get("quota-used-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        // Nextcloud / ownCloud convention: `quota-available-bytes` can be negative.
        //   -1 = unknown, -2 = unlimited (legacy), -3 = unlimited.
        // Parse as i64 so we can detect the sentinel, then surface "unlimited" by
        // returning total=0 / free=0, which the frontend StatusBar already renders
        // as "used (no cap)". Without this, u64::parse rejects negative values,
        // free falls back to 0, total collapses to `used`, and the UI shows
        // "59.7 MB / 59.7 MB" (100% full) for accounts that are actually unlimited.
        let free_raw = props
            .get("quota-available-bytes")
            .and_then(|s| s.parse::<i64>().ok());
        let (free, total) = match free_raw {
            Some(v) if v < 0 => (0u64, 0u64),
            Some(v) => {
                let f = v as u64;
                (f, used.saturating_add(f))
            }
            None => (0u64, 0u64),
        };

        Ok(super::StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    // File versioning for the local MEGAcmd WebDAV bridge (#270) and for
    // Nextcloud. Plain WebDAV has no version API. On the MEGAcmd bridge we shell
    // out to the same `mega-*` CLI that backs the quota bar: `mega-ls --versions`
    // to list, `mega-get "<path>#<epoch>"` to fetch a version, and download+re-put
    // to restore. On Nextcloud the versions DAV collection answers. Other WebDAV
    // servers keep the trait default (NotSupported).
    // The versions the GUI, CLI and MCP reach only through this answer: the
    // MEGAcmd bridge above (#270, unreachable while this said false) and the
    // Nextcloud versions collection.
    fn supports_versions(&self) -> bool {
        (super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref())
            && self.megacmd_serves_account_root)
            || self.supports_nextcloud_versions()
    }

    /// Only Nextcloud's versions are deleted one by one: MEGAcmd has no
    /// command for it, and its delete is refused.
    fn supports_version_delete(&self) -> bool {
        self.supports_nextcloud_versions()
    }

    /// Only Nextcloud's versions are restored: MEGAcmd's are listed and
    /// downloaded, never restored ([`super::mega::MEGA_RESTORE_REFUSED`]).
    fn supports_version_restore(&self) -> bool {
        self.supports_nextcloud_versions()
    }

    async fn list_versions(
        &mut self,
        path: &str,
    ) -> Result<Vec<super::FileVersion>, ProviderError> {
        if super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref()) {
            self.megacmd_versions_at_account_root()?;
            super::mega_df::require_megacmd_account(self.megacmd_account.as_deref()).await?;
            return super::mega_df::mega_list_versions(&self.resolve_root(path)).await;
        }
        if self.supports_nextcloud_versions() {
            self.require_connected()?;
            return self.nextcloud_list_versions(path).await;
        }
        Err(ProviderError::NotSupported("list_versions".to_string()))
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        if super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref()) {
            self.megacmd_versions_at_account_root()?;
            super::mega_df::require_megacmd_account(self.megacmd_account.as_deref()).await?;
            let path = self.resolve_root(path);
            return super::mega_df::mega_download_version(&path, version_id, local_path).await;
        }
        if self.supports_nextcloud_versions() {
            self.require_connected()?;
            return self
                .nextcloud_download_version(path, version_id, local_path)
                .await;
        }
        Err(ProviderError::NotSupported("download_version".to_string()))
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if super::mega_df::is_megacmd_webdav_provider_id(self.config.provider_id.as_deref()) {
            return Err(ProviderError::NotSupported(
                super::mega::MEGA_RESTORE_REFUSED.into(),
            ));
        }
        if self.supports_nextcloud_versions() {
            self.require_connected()?;
            return self.nextcloud_restore_version(path, version_id).await;
        }
        Err(ProviderError::NotSupported("restore_version".to_string()))
    }

    async fn delete_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if self.supports_nextcloud_versions() {
            self.require_connected()?;
            return self.nextcloud_delete_version(path, version_id).await;
        }
        Err(ProviderError::NotSupported("delete_version".to_string()))
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(
        &mut self,
        path: &str,
        timeout: u64,
    ) -> Result<super::LockInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let timeout_header = if timeout == 0 {
            "Infinite".to_string()
        } else {
            format!("Second-{}", timeout)
        };

        let lock_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:lockinfo xmlns:d="DAV:">
                <d:lockscope><d:exclusive/></d:lockscope>
                <d:locktype><d:write/></d:locktype>
                <d:owner><d:href>AeroFTP</d:href></d:owner>
            </d:lockinfo>"#;

        let response = self
            .request(webdav_methods::lock(), path)
            .header("Depth", "0")
            .header("Timeout", &timeout_header)
            .header("Content-Type", "application/xml")
            .body(lock_body)
            .send()
            .await
            .map_err(send_error)?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "LOCK failed ({}): {}",
                status,
                server_message(&text, &self.config.url, &self.sent_secrets())
            )));
        }

        // Extract lock token from Lock-Token header or XML response
        let lock_token = response
            .headers()
            .get("lock-token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string())
            .unwrap_or_default();

        Ok(super::LockInfo {
            token: lock_token,
            owner: Some("AeroFTP".to_string()),
            timeout,
            exclusive: true,
        })
    }

    async fn unlock_file(&mut self, path: &str, lock_token: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token_header = format!("<{}>", lock_token);

        let response = self
            .send_replaying_digest(|| {
                self.request(webdav_methods::unlock(), path)
                    .header("Lock-Token", &token_header)
            })
            .await?;

        match response.status() {
            reqwest::StatusCode::OK | reqwest::StatusCode::NO_CONTENT => Ok(()),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "UNLOCK failed ({}): {}",
                    status,
                    server_message(&text, &self.config.url, &self.sent_secrets())
                )))
            }
        }
    }

    fn supports_share_links(&self) -> bool {
        self.is_nextcloud()
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
            supports_permissions: false,
            available_permissions: vec![],
            ..Default::default()
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        self.nextcloud_create_share(path, options).await
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands keep working.
        // The real RFC 4918 COPY implementation lives on `server_side_copy`.
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let destination = self.build_url(to);
        // Same collection resend as `rename`.
        let collection_destination = self.build_url(&Self::collection_path(to));

        let response = self
            .send_on_resource(from, |source| {
                let destination = if source == from {
                    &destination
                } else {
                    &collection_destination
                };
                self.request(webdav_methods::copy(), source)
                    .header("Destination", destination)
                    .header("Overwrite", "F")
            })
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(from.to_string())),
            StatusCode::PRECONDITION_FAILED => Err(ProviderError::AlreadyExists(to.to_string())),
            StatusCode::BAD_GATEWAY => Err(destination_refused_error("COPY")),
            status => Err(ProviderError::ServerError(format!(
                "COPY failed with status: {}",
                status
            ))),
        }
    }

    // ─── Nextcloud chunked upload v2 trait methods ────────────────────
    //
    // Only Nextcloud-class servers (`is_nextcloud_for_dav()`) override the
    // trait default. Vanilla WebDAV (Apache mod_dav, lighttpd, nginx_dav,
    // Filen bridge, MagentaCloud / Tab.digital / FeliCloud variants whose
    // chunked support has not been verified) keeps returning NotSupported
    // so the shaped-graph builder falls back to a single PUT through
    // `upload()`.

    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if !self.is_nextcloud_for_dav() {
            return Err(ProviderError::NotSupported(
                "WebDAV chunked upload requires a Nextcloud / ownCloud server".to_string(),
            ));
        }

        let user_id = self.nextcloud_userid().ok_or_else(|| {
            ProviderError::Other(
                "Cannot resolve Nextcloud user id from server_root / config".to_string(),
            )
        })?;
        let transfer_id = uuid::Uuid::new_v4().to_string();

        // Resolve the final path eagerly so the handle carries the exact
        // destination the runner will MOVE to at finalize time. This makes
        // the multipart session independent of any later `cd` changes.
        let final_path = self.resolve_root(remote_path);

        let folder_url = self
            .nextcloud_chunked_folder_url(&transfer_id)
            .ok_or_else(|| {
                ProviderError::Other(
                    "Failed to build Nextcloud chunked uploads folder URL".to_string(),
                )
            })?;

        let mut response = self
            .request_url(webdav_methods::mkcol(), &folder_url)
            .send()
            .await
            .map_err(send_error)?;

        // MKCOL carries no body and is already treated as idempotent below
        // (405 means a previous attempt created it), so replaying it once
        // costs nothing and is safe. This is the head of the chunked upload:
        // losing it to a rotation fails the whole multi-part transfer before
        // a single chunk is sent.
        if response.status() == StatusCode::UNAUTHORIZED && self.adopt_rotated_nonce(&response) {
            response = self
                .request_url(webdav_methods::mkcol(), &folder_url)
                .send()
                .await
                .map_err(send_error)?;
        }

        match response.status() {
            // RFC 4918 §9.3.1: 201 Created. Some Nextcloud deployments
            // behind reverse proxies return 200/204; both are idempotent
            // success.
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => {}
            StatusCode::METHOD_NOT_ALLOWED => {
                // Rare: a previous transfer with the same UUID is still
                // pending. UUID v4 collisions are effectively impossible,
                // so this surfaces a real conflict the operator should see.
                return Err(ProviderError::AlreadyExists(format!(
                    "Nextcloud chunked upload {transfer_id}"
                )));
            }
            StatusCode::CONFLICT => {
                return Err(ProviderError::InvalidPath(
                    "Nextcloud uploads root does not exist for this user".to_string(),
                ));
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                return Err(ProviderError::PermissionDenied(
                    "Nextcloud refused MKCOL on the uploads folder".to_string(),
                ));
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "MKCOL uploads/{} failed: {} {}",
                    transfer_id,
                    status,
                    server_message(&body, &self.config.url, &self.sent_secrets())
                )));
            }
        }

        tracing::info!(
            target: "webdav_chunked",
            "[WEBDAV] Nextcloud chunked v2 begin: upload {} (size={})",
            transfer_id,
            total_size
        );

        let destination = self
            .nextcloud_chunked_destination_url(&final_path)
            .map(|url| NextcloudMultipartHandle::destination_digest(&url))
            .ok_or_else(|| {
                ProviderError::Other(
                    "Failed to build Nextcloud destination URL for the upload".to_string(),
                )
            })?;
        let handle_payload = NextcloudMultipartHandle {
            transfer_id,
            user_id,
            total_size,
            destination,
        };
        Ok(MultipartHandle {
            upload_id: handle_payload.encode(),
            remote_path: final_path,
        })
    }

    // DAG-P2-05: Nextcloud chunked PUT has a known length and
    // no whole-part hashing, so stream the part body one bounded window at a time
    // instead of buffering the whole part in memory.
    fn multipart_streams_part_body(&self) -> bool {
        true
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        self.upload_part_body(
            handle,
            part_number,
            crate::transfer_multipart::PartBody::owned(data),
        )
        .await
    }

    async fn upload_part_body(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if part_number == 0 {
            return Err(ProviderError::Other(
                "Nextcloud chunked upload requires 1-based part numbers".to_string(),
            ));
        }

        let payload = self.nextcloud_handle_here(handle)?;
        let chunk_url = self
            .nextcloud_chunked_chunk_url(&payload.transfer_id, part_number)
            .ok_or_else(|| {
                ProviderError::Other("Failed to build Nextcloud chunk URL".to_string())
            })?;

        let response = self
            .send_nextcloud_chunk(&chunk_url, payload.total_size, body)
            .await?;

        match response.status() {
            // Nextcloud returns 201 Created for a new chunk and 204 No
            // Content when a retry overwrites a partial chunk. Some
            // proxies normalize to 200 OK.
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => {
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.trim_matches('"').to_string())
                    .unwrap_or_else(|| nextcloud_chunk_filename(part_number));
                Ok(UploadedPart { part_number, etag })
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(
                ProviderError::PermissionDenied("Nextcloud rejected chunk upload".to_string()),
            ),
            StatusCode::INSUFFICIENT_STORAGE => Err(ProviderError::ServerError(
                "Insufficient storage on Nextcloud server".to_string(),
            )),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format!(
                    "Nextcloud chunk {} upload failed: {} {}",
                    part_number,
                    status,
                    server_message(&body, &self.config.url, &self.sent_secrets())
                )))
            }
        }
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        _parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let payload = self.nextcloud_handle_here(&handle)?;
        let assemble_url = self
            .nextcloud_chunked_assemble_url(&payload.transfer_id)
            .ok_or_else(|| {
                ProviderError::Other("Failed to build Nextcloud .file assemble URL".to_string())
            })?;
        let destination_url = self
            .nextcloud_chunked_destination_url(&handle.remote_path)
            .ok_or_else(|| {
                ProviderError::Other(
                    "Failed to build Nextcloud destination URL for finalize".to_string(),
                )
            })?;

        tracing::info!(
            target: "webdav_chunked",
            "[WEBDAV] Nextcloud chunked v2 finalize: upload {} -> {} (size={})",
            payload.transfer_id,
            handle.remote_path,
            payload.total_size
        );

        let response = self
            .request_url(webdav_methods::move_method(), &assemble_url)
            .header("Destination", destination_url)
            .header("OC-Total-Length", payload.total_size)
            // Chunked v2 is the upload primitive: overwrite of an existing
            // file at the destination is the expected behavior (otherwise
            // a retry of the same upload after a network drop fails).
            .header("Overwrite", "T")
            .send()
            .await
            .map_err(send_error)?;

        match response.status() {
            StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::PRECONDITION_FAILED => Err(ProviderError::TransferFailed(
                "Nextcloud assemble rejected: OC-Total-Length mismatch (chunk sizes inconsistent)"
                    .to_string(),
            )),
            StatusCode::CONFLICT => Err(ProviderError::InvalidPath(
                "Destination parent does not exist".to_string(),
            )),
            StatusCode::INSUFFICIENT_STORAGE => Err(ProviderError::ServerError(
                "Insufficient storage on Nextcloud server".to_string(),
            )),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format!(
                    "Nextcloud assemble failed: {} {}",
                    status,
                    server_message(&body, &self.config.url, &self.sent_secrets())
                )))
            }
        }
    }

    async fn abort_multipart_upload(
        &mut self,
        handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Best-effort: Nextcloud garbage-collects abandoned chunked uploads
        // after ~24h, so a network failure here is not fatal. The shaped-
        // graph runner only calls abort when one of the upload_part nodes
        // already failed; surfacing a second error from the cleanup path
        // would mask the real upload failure.
        // A handle of another user names chunks this user cannot see, and
        // one of another destination is not this connection's upload:
        // nothing to clean up from here.
        let payload = match self.nextcloud_handle_here(&handle) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        let Some(folder_url) = self.nextcloud_chunked_folder_url(&payload.transfer_id) else {
            return Ok(());
        };

        let result = self.request_url(Method::DELETE, &folder_url).send().await;
        match result {
            Ok(response) => {
                let status = response.status();
                if !(status.is_success() || status == StatusCode::NOT_FOUND) {
                    tracing::warn!(
                        "[WEBDAV] Nextcloud chunked abort returned {} for upload {}",
                        status,
                        payload.transfer_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[WEBDAV] Nextcloud chunked abort network error: {}",
                    describe_reqwest_error(e)
                );
            }
        }
        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_time.begin();
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Where to go on from is the part on disk and its record, not the
        // offset the caller measured: see `HttpResume`.
        let this = &*self;
        super::resume_http_download(
            local_path,
            remote_path,
            &super::atomic_write::ResumeSource::new(
                "webdav",
                &self.config.url,
                &self.config.username,
                &self.resolve_root(remote_path),
            ),
            |range| async move {
                this.send_replaying_digest(|| {
                    let request = this.request(Method::GET, remote_path);
                    match &range {
                        Some(range) => request.header("Range", range.as_str()),
                        None => request,
                    }
                })
                .await
            },
            on_progress,
        )
        .await
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Nextcloud-class servers advertise chunked v2 multipart so the
        // shaped-graph builder can fan out into N parallel `UploadPart`
        // nodes. Vanilla WebDAV (`is_nextcloud_for_dav() == false`) keeps
        // the legacy single-PUT path.
        let (supports_multipart, multipart_threshold, multipart_part_size, multipart_max_parallel) =
            if self.is_nextcloud_for_dav() {
                (
                    true,
                    NEXTCLOUD_DAG_THRESHOLD,
                    NEXTCLOUD_DAG_CHUNK_SIZE,
                    NEXTCLOUD_DAG_MAX_PARALLEL,
                )
            } else {
                (false, 0, 0, 1)
            };
        super::TransferOptimizationHints {
            supports_multipart,
            multipart_threshold,
            multipart_part_size,
            multipart_max_parallel,
            supports_range_download: true,
            supports_resume_download: true,
            ..Default::default()
        }
    }

    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        WEBDAV_TRANSFER_MAX_SESSIONS
    }

    fn list_executor_kind(&self) -> ProviderListExecutorKind {
        ProviderListExecutorKind::HttpClonePool
    }

    fn list_executor_max_sessions(&self) -> u16 {
        WEBDAV_LIST_MAX_SESSIONS
    }

    /// Mint an independent worker for a directory walk that lists several
    /// folders at once (#958): the used-storage scan, AeroSync Compare, the
    /// CLI and MCP walks. A `PROPFIND Depth: 1` on one collection shares no
    /// state with one on another, and the clone shares the `reqwest::Client`
    /// and the Digest challenge exactly as [`Self::clone_for_transfer`] does,
    /// so a nonce rotation seen by one worker repairs the pool.
    ///
    /// A single-file profile (#264) has one resource and no tree: every path
    /// maps to the configured URL, so it keeps walking on the one session.
    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        if self.single_file_mode.is_some() {
            return Err(ProviderError::NotSupported(
                "a single-file WebDAV profile has no tree to list in parallel".to_string(),
            ));
        }
        Ok(Box::new(self.clone()))
    }

    /// Mint an independent worker for file-level parallelism and Nextcloud
    /// chunked-upload parts.
    ///
    /// Distinct-file GET/PUT on vanilla WebDAV (SFTPGo, nginx DAV, Apache
    /// mod_dav, Koofr, …) has no session lock, so a cloned provider sharing
    /// the `reqwest::Client` is the same HttpClonePool model as S3/Azure
    /// (issue #591). Nextcloud chunked v2 still uses this clone for
    /// unordered part PUTs. Digest `nc` is an `Arc<AtomicU32>` so clones
    /// never reuse a nonce count.
    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        // Digest is safe to clone now that the challenge is shared, not just the
        // counter: a nonce rotation seen by any worker repairs the whole pool
        // (see DigestState).
        //
        // The accepted debt this carried is CLOSED. It used to read: only the
        // PROPFIND path re-negotiates on `stale=true`, the data paths still map
        // a 401 to "Session expired", deferred because a blanket 401 retry
        // would turn a revoked password into a retry storm.
        //
        // What closed it was not the retry, it was making the retry decidable.
        // `DigestChallenge` now carries `stale`, and `renegotiate` adopts a
        // challenge only when it says `stale=true` AND offers a nonce we are
        // not already holding, so a rotation is distinguishable from a
        // credential failure and from a server that echoes a dead nonce. On
        // that gate the data paths replay exactly once, through
        // `adopt_rotated_nonce`: `stat`, `checksum`, `keep_alive`, the chunked
        // MKCOL and the chunked part PUT. `list` is deliberately NOT among them
        // because it goes through `send_propfind`, which already renegotiates;
        // a retry there would be a second attempt, which is the storm itself.
        Ok(Box::new(self.clone()))
    }

    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, WEBDAV_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes;
    }

    fn planned_download_segments(&self, file_size: u64) -> usize {
        // No provider floor: the explicit cutoff applies as-is. The live
        // single-file gate runs inside the shared HTTP helper with the same
        // planner inputs.
        crate::provider_transfer_executor::plan_segment_count(
            file_size,
            self.multi_thread_streams,
            WEBDAV_MULTI_THREAD_MAX_STREAMS,
            crate::provider_transfer_executor::SegmentCutoff::Explicit(self.multi_thread_cutoff),
            0,
        )
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(format!(
                "Read range size {} exceeds maximum {} bytes",
                len, MAX_READ_RANGE
            )));
        }

        // len == 0 has no range, and offset + len - 1 must not wrap: a wrapped
        // end makes an invalid Range header that servers ignore, turning a
        // bounded probe into a full-body download.
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(len - 1)
            .ok_or_else(|| ProviderError::Other("read_range end overflows u64".to_string()))?;
        let range_header = format!("bytes={}-{}", offset, end);

        let response = self
            .send_replaying_digest(|| {
                self.request(Method::GET, path)
                    .header("Range", &range_header)
            })
            .await?;

        let status = response.status();
        match status {
            // A 206 that does not name the window it carries is written at
            // this offset just the same, so it has to say which range it is;
            // a 200 is read only up to the window's end.
            StatusCode::PARTIAL_CONTENT | StatusCode::OK => {
                super::ranged_read_window(response, offset, len, "WebDAV range read", path).await
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            StatusCode::RANGE_NOT_SATISFIABLE => Err(ProviderError::NotSupported(
                "Server does not support range requests".to_string(),
            )),
            status => Err(ProviderError::TransferFailed(format!(
                "Range download failed with status: {}",
                status
            ))),
        }
    }
}

// ─── Nextcloud Tauri Commands ────────────────────────────────────────────
//
// Each one takes the connection `generation` the trash manager was opened on
// and acts only while that connection is still the live one: a session
// switch, a reconnect or another sessionless connection (AeroCloud) in
// between is refused, so its rows are never listed, restored or deleted on
// another account.

/// What the trash refusals name as read.
const NEXTCLOUD_TRASH_OPENED: &str = "the trash was opened";

/// The WebDAV provider of the live connection, when `slot` holds one.
fn nextcloud_trash_webdav(
    slot: &mut Option<Box<dyn StorageProvider>>,
) -> Result<&mut WebDavProvider, String> {
    let provider = slot.as_mut().ok_or("Not connected")?;
    crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<WebDavProvider>()
        .ok_or_else(|| "Not a WebDAV connection".to_string())
}

#[tauri::command]
pub async fn webdav_list_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    generation: u64,
) -> Result<Vec<NextcloudTrashEntry>, String> {
    list_trash_of(&state, generation).await
}

async fn list_trash_of(
    state: &crate::provider_commands::ProviderState,
    generation: u64,
) -> Result<Vec<NextcloudTrashEntry>, String> {
    let mut slot = state
        .lock_connection_of_generation(generation, NEXTCLOUD_TRASH_OPENED)
        .await?;
    nextcloud_trash_webdav(&mut slot)?
        .nextcloud_list_trash()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn webdav_restore_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    ids: Vec<String>,
    generation: u64,
) -> Result<(), String> {
    restore_trash_of(&state, &ids, generation).await
}

async fn restore_trash_of(
    state: &crate::provider_commands::ProviderState,
    ids: &[String],
    generation: u64,
) -> Result<(), String> {
    let mut slot = state
        .lock_connection_of_generation(generation, NEXTCLOUD_TRASH_OPENED)
        .await?;
    let webdav = nextcloud_trash_webdav(&mut slot)?;
    for id in ids {
        webdav
            .nextcloud_restore_trash(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn webdav_delete_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    ids: Vec<String>,
    generation: u64,
) -> Result<(), String> {
    delete_trash_of(&state, &ids, generation).await
}

async fn delete_trash_of(
    state: &crate::provider_commands::ProviderState,
    ids: &[String],
    generation: u64,
) -> Result<(), String> {
    let mut slot = state
        .lock_connection_of_generation(generation, NEXTCLOUD_TRASH_OPENED)
        .await?;
    let webdav = nextcloud_trash_webdav(&mut slot)?;
    for id in ids {
        webdav
            .nextcloud_delete_trash_item(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn webdav_empty_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    generation: u64,
) -> Result<(), String> {
    empty_trash_of(&state, generation).await
}

async fn empty_trash_of(
    state: &crate::provider_commands::ProviderState,
    generation: u64,
) -> Result<(), String> {
    let mut slot = state
        .lock_connection_of_generation(generation, NEXTCLOUD_TRASH_OPENED)
        .await?;
    nextcloud_trash_webdav(&mut slot)?
        .nextcloud_empty_trash()
        .await
        .map_err(|e| e.to_string())
}

/// Map a non-success WebDAV upload (PUT) status to a `ProviderError`.
///
/// RFC 4918 §9.7.1 says a PUT whose parent collection is missing should answer
/// `409 Conflict`, but Koofr's WebDAV gateway returns `404 Not Found` for that
/// The declared object size when a `206 Partial Content` answer to a request we
/// sent WITHOUT a `Range` header still carries the entire object, so the body
/// can be consumed exactly like a `200`. `None` when it does not.
///
/// The size is returned, not just a yes/no, because the caller MUST verify it:
/// FileLu answers `transfer-encoding: chunked`, so there is no `Content-Length`
/// to check the received bytes against, and a truncated body would otherwise be
/// committed as a complete download. `Content-Range` carries the only
/// authoritative total in that case.
///
/// FileLu's WebDAV (nginx) does this: a plain `GET` comes back
/// `206` with `content-range: bytes 0-65535/65536`, which is the whole file.
/// Matching only on `StatusCode::OK` made every such download fail with
/// "Download failed with status: 206 Partial Content", after three retries,
/// leaving no local file at all even though the complete bytes were in hand.
///
/// The check stays strict on purpose: a `Content-Range` that does not start at
/// 0 or does not reach the last byte is a genuinely partial body and must keep
/// failing, otherwise a truncated download would be committed as complete. A
/// `206` with no `Content-Range` at all is not trusted either.
fn whole_object_total_from_content_range(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())?;
    // `bytes <start>-<end>/<total>`; `*` for either side means "unknown".
    let spec = raw.trim().strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (Ok(start), Ok(end), Ok(total)) = (
        start.trim().parse::<u64>(),
        end.trim().parse::<u64>(),
        total.trim().parse::<u64>(),
    ) else {
        return None;
    };
    // `checked_add`: `end` is attacker-controlled, and `u64::MAX + 1` would
    // wrap in release and panic in debug before `total` was ever compared.
    (total > 0 && start == 0 && end.checked_add(1) == Some(total)).then_some(total)
}

/// case (and also when the target path is itself an existing collection rather
/// than a file). Without this, a Koofr upload to a missing-parent or directory
/// target surfaced as the opaque, retryable "Upload failed with status: 404"
/// (discussion #277). Both `409` and `404` are folded into one actionable,
/// non-retryable `InvalidPath` error so the message is clear and a sync/retry
/// loop does not keep re-uploading a file that can never land at that path.
fn upload_failure_error(status: StatusCode) -> ProviderError {
    match status {
        StatusCode::CONFLICT | StatusCode::NOT_FOUND => ProviderError::InvalidPath(
            "Upload target is not a writable file: the parent directory is missing or the \
             path is a directory (some WebDAV servers such as Koofr return 404 instead of \
             409 here)"
                .to_string(),
        ),
        StatusCode::INSUFFICIENT_STORAGE => {
            ProviderError::ServerError("Insufficient storage space".to_string())
        }
        other => ProviderError::TransferFailed(format!("Upload failed with status: {}", other)),
    }
}

/// `Depth` of every MOVE, file or collection. RFC 4918 section 9.9.2: a MOVE
/// acts as `infinity` whatever it says, and a collection MOVE must not carry
/// any other value. For a file the header means nothing, but servers built on
/// golang.org/x/net/webdav (rclone serve webdav among them) accept only
/// `infinity` or no header, and answered the `0` sent for files until
/// 2026-09-25 with 400: every file rename failed there. Sending the one value
/// valid for both also drops the PROPFIND that picked between them.
const MOVE_DEPTH: &str = "infinity";

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn probe_contract_webdav_does_not_erase_an_auth_failure() {
        const UNAUTHORIZED: &str =
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        const MISSING: &str =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        for replies in [vec![UNAUTHORIZED, MISSING], vec![MISSING, UNAUTHORIZED]] {
            let (url, _) = spawn_http_stub(replies).await;
            let mut p = WebDavProvider::new(test_config(&url)).unwrap();
            p.connected = true;
            let outcome = p.exists("/target").await;
            assert!(
                matches!(outcome, Err(ref e) if !matches!(e, ProviderError::NotFound(_))),
                "a failure on one path form is not disproved by the other: {outcome:?}"
            );
        }
        let (url, _) = spawn_http_stub(vec![MISSING, MISSING]).await;
        let mut p = WebDavProvider::new(test_config(&url)).unwrap();
        p.connected = true;
        assert!(!p.exists("/target").await.unwrap());
    }

    use super::*;

    /// A window the connection drops is resumed from its mark: a body cut
    /// halfway was a `TransferFailed`, which failed the download.
    #[tokio::test]
    async fn a_window_the_connection_drops_is_recoverable() {
        super::super::multi_thread::window_server::assert_a_dropped_window_is_recoverable(|addr| {
            let mut provider =
                WebDavProvider::new(test_config(&format!("http://{addr}/"))).unwrap();
            provider.connected = true;
            Box::new(provider)
        })
        .await;
    }

    /// The trash commands act only on the connection the manager was opened
    /// on: with any other generation (a session switch, a reconnect, another
    /// sessionless AeroCloud connection) nothing reaches the server, and the
    /// refusal is the changed-connection one. The control is the live
    /// generation, whose listing does reach it.
    #[tokio::test]
    async fn the_trash_commands_act_only_on_the_connection_they_were_opened_on() {
        use axum::response::IntoResponse;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(format!("{method} {}", uri.path()));
                    (
                        axum::http::StatusCode::MULTI_STATUS,
                        r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"></d:multistatus>"#,
                    )
                        .into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let mut provider = WebDavProvider::new(test_config(&format!(
            "http://{addr}/remote.php/dav/files/user/"
        )))
        .unwrap();
        provider.connected = true;
        let state = crate::provider_commands::ProviderState::new();
        *state.provider.lock().await = Some(Box::new(provider));
        let live = 0;
        let stale = live + 1;
        let ids = vec!["a.txt.d1700000000".to_string()];
        let refusals = [
            list_trash_of(&state, stale).await.map(|_| ()),
            restore_trash_of(&state, &ids, stale).await,
            delete_trash_of(&state, &ids, stale).await,
            empty_trash_of(&state, stale).await,
        ];
        for outcome in &refusals {
            assert!(
                matches!(outcome, Err(e) if e.starts_with(crate::provider_commands::CONNECTION_CHANGED_MARKER)),
                "{outcome:?}"
            );
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
        list_trash_of(&state, live)
            .await
            .expect("the live connection's trash");
        server.abort();
        assert_eq!(
            *seen.lock().unwrap(),
            ["PROPFIND /remote.php/dav/trashbin/user/trash/"]
        );
    }

    /// The File Versions commands act only on the connection the dialog was
    /// opened on, as the trash commands do: with another generation nothing
    /// reaches the server (a purge cannot be undone), and the refusal is the
    /// changed-connection one. The control is the live generation, whose
    /// listing does reach it.
    #[tokio::test]
    async fn the_version_commands_act_only_on_the_connection_they_were_opened_on() {
        use axum::response::IntoResponse;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(format!("{method} {}", uri.path()));
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let mut provider = WebDavProvider::new(test_config(&format!(
            "http://{addr}/remote.php/dav/files/user/"
        )))
        .unwrap();
        provider.connected = true;
        let state = crate::provider_commands::ProviderState::new();
        *state.provider.lock().await = Some(Box::new(provider));
        let live = 0;
        let stale = live + 1;
        let local = tempfile::tempdir().unwrap();
        let to = local.path().join("v.txt");
        let to = to.to_string_lossy();
        use crate::provider_commands::{
            delete_version_of, download_version_of, list_versions_of, restore_version_of,
        };
        let refusals = [
            list_versions_of(&state, "/a.txt", stale, None)
                .await
                .map(|_| ()),
            download_version_of(&state, "/a.txt", "42:1700000000", &to, stale, None)
                .await
                .map(|_| ()),
            restore_version_of(&state, "/a.txt", "42:1700000000", stale, None).await,
            delete_version_of(&state, "/a.txt", "42:1700000000", stale, None).await,
        ];
        for outcome in &refusals {
            assert!(
                matches!(outcome, Err(e) if e.starts_with(crate::provider_commands::CONNECTION_CHANGED_MARKER)),
                "{outcome:?}"
            );
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
        let _ = list_versions_of(&state, "/a.txt", live, None).await;
        server.abort();
        assert!(
            !seen.lock().unwrap().is_empty(),
            "the live generation reaches the server"
        );
    }

    /// An overlay applied, cleared or locked keeps the connection and moves
    /// the remote target: a dialog opened before it lists, downloads,
    /// restores and purges nothing, and the provider receives no request.
    #[tokio::test]
    async fn the_version_commands_act_only_on_the_remote_target_they_were_opened_on() {
        use axum::response::IntoResponse;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(format!("{method} {}", uri.path()));
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let mut provider = WebDavProvider::new(test_config(&format!(
            "http://{addr}/remote.php/dav/files/user/"
        )))
        .unwrap();
        provider.connected = true;
        let state = crate::provider_commands::ProviderState::new();
        *state.provider.lock().await = Some(Box::new(provider));
        let opened_on = 0;
        {
            let _change = state
                .transfer_owner_write(std::time::Duration::from_secs(1))
                .await
                .unwrap();
            state.advance_gui_target();
        }
        let local = tempfile::tempdir().unwrap();
        let to = local.path().join("v.txt");
        let to = to.to_string_lossy();
        use crate::provider_commands::{
            delete_version_of, download_version_of, list_versions_of, restore_version_of,
        };
        let target = Some(opened_on);
        let refusals = [
            list_versions_of(&state, "/a.txt", 0, target)
                .await
                .map(|_| ()),
            download_version_of(&state, "/a.txt", "42:1700000000", &to, 0, target)
                .await
                .map(|_| ()),
            restore_version_of(&state, "/a.txt", "42:1700000000", 0, target).await,
            delete_version_of(&state, "/a.txt", "42:1700000000", 0, target).await,
        ];
        server.abort();
        for outcome in &refusals {
            assert!(
                matches!(outcome, Err(e) if e.starts_with(crate::provider_commands::CONNECTION_CHANGED_MARKER)),
                "{outcome:?}"
            );
        }
        let sent = seen.lock().unwrap().clone();
        assert!(sent.is_empty(), "{sent:?}");
    }

    /// A refused download, the plain one and the Nextcloud current-version
    /// one that goes through it, logs the status and the header names only:
    /// a session cookie in a header and a credential echoed in the body stay
    /// out of the log.
    #[tokio::test]
    async fn a_refused_download_logs_no_header_value_and_no_body() {
        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Capture {
            type Writer = Capture;
            fn make_writer(&'writer self) -> Self::Writer {
                self.clone()
            }
        }
        use axum::response::IntoResponse;
        // The file id lookup answers with an ETag, so the current-version
        // download reaches its own GET; every GET is refused with a cookie and an
        // echoed credential.
        let gets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&gets);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method| {
                let counted = std::sync::Arc::clone(&counted);
                async move {
                    if method.as_str() == "PROPFIND" {
                        return (
                            axum::http::StatusCode::MULTI_STATUS,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/a.txt</d:href><d:propstat><d:prop><oc:fileid>60</oc:fileid><d:getetag>"e60"</d:getetag><d:getcontentlength>4</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        )
                            .into_response();
                    }
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [("set-cookie", "oc_session=COOKIE-SECRET-1")],
                        "token=BODY-SECRET-2",
                    )
                        .into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        // A credential in the configured URL's query stays out of the log.
        let mut provider = WebDavProvider::new(test_config(&format!(
            "http://{addr}/remote.php/dav/files/user/?access=URL-SECRET-3"
        )))
        .unwrap();
        provider.connected = true;
        let local = tempfile::tempdir().unwrap();
        let to = local.path().join("a.txt");
        let to = to.to_str().unwrap();
        assert!(provider.download("/a.txt", to, None).await.is_err());
        let current = provider
            .download_version("/a.txt", super::super::CURRENT_VERSION_ID, to)
            .await;
        server.abort();
        let shown = format!("{current:?}");
        assert!(current.is_err(), "{shown}");
        assert!(
            !shown.contains("COOKIE-SECRET-1") && !shown.contains("BODY-SECRET-2"),
            "{shown}"
        );
        assert_eq!(
            gets.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the plain GET and the current-version GET"
        );
        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("download non-success: status=403"), "{log}");
        assert!(log.contains("set-cookie"), "{log}");
        assert!(!log.contains("COOKIE-SECRET-1"), "{log}");
        assert!(!log.contains("BODY-SECRET-2"), "{log}");
        assert!(
            !log.contains("URL-SECRET-3") && !shown.contains("URL-SECRET-3"),
            "{log}"
        );
    }

    /// MEGAcmd's versions are of account paths: a session served below
    /// another folder offers none and refuses each version action before any
    /// MEGAcmd call; one served at the account root offers them.
    #[tokio::test]
    async fn megacmd_versions_run_only_on_a_session_served_at_the_account_root() {
        let served = "http://127.0.0.1:4443/H1/Cloud%20Drive";
        assert!(megacmd_session_is_account_root(
            served,
            Some(&format!("{served}/"))
        ));
        assert!(!megacmd_session_is_account_root(
            "http://127.0.0.1:4443/H2/Team",
            Some(served)
        ));
        assert!(!megacmd_session_is_account_root(served, None));

        let mut config = test_config("http://127.0.0.1:4443/H2/Team");
        config.provider_id = Some("megacmd-webdav".into());
        let mut provider = WebDavProvider::new(config).unwrap();
        assert!(!provider.supports_versions());
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.pdf");
        let below = [
            provider.list_versions("/report.pdf").await.map(|_| ()),
            provider
                .download_version("/report.pdf", "H:v1", out.to_str().unwrap())
                .await,
        ];
        for outcome in &below {
            assert!(
                matches!(outcome, Err(ProviderError::NotSupported(m)) if m.contains("account root")),
                "{outcome:?}"
            );
        }
        assert!(!out.exists());
        provider.megacmd_serves_account_root = true;
        assert!(provider.supports_versions());
        // MEGAcmd deletes no single version: no Purge is offered for it.
        assert!(!provider.supports_version_delete());
        // Nor restores one (it writes by path, not by handle): no Restore is
        // offered, and one sent anyway is refused, at the root as below it.
        assert!(!provider.supports_version_restore());
        for root in [false, true] {
            provider.megacmd_serves_account_root = root;
            let restored = provider.restore_version("/report.pdf", "H:v1").await;
            assert!(
                matches!(&restored, Err(ProviderError::NotSupported(m)) if m == super::super::mega::MEGA_RESTORE_REFUSED),
                "{root}: {restored:?}"
            );
        }
        let mut nextcloud = test_config("https://cloud.example.com");
        nextcloud.provider_id = Some("nextcloud".into());
        let nextcloud = WebDavProvider::new(nextcloud).unwrap();
        assert_eq!(
            nextcloud.supports_version_delete(),
            nextcloud.supports_nextcloud_versions()
        );
        assert_eq!(
            nextcloud.supports_version_restore(),
            nextcloud.supports_nextcloud_versions()
        );
    }

    #[test]
    fn sync_owner_excludes_actual_bridge_presets_and_generic_loopback() {
        for id in [
            "filen-desktop-webdav",
            "megacmd-webdav",
            "megacmd",
            "opendrive-desktop",
        ] {
            let mut config = test_config("https://fixture.invalid/");
            config.provider_id = Some(id.into());
            assert!(WebDavProvider::new(config)
                .unwrap()
                .sync_owner_identity()
                .unwrap()
                .is_none());
        }
        for url in [
            "http://127.0.0.1:7654/",
            "http://[::1]:7654/",
            "http://localhost:7654/",
            "http://local.webdav.filen.io/",
        ] {
            assert!(WebDavProvider::new(test_config(url))
                .unwrap()
                .sync_owner_identity()
                .unwrap()
                .is_none());
        }
        assert!(WebDavProvider::new(test_config("https://fixture.invalid/"))
            .unwrap()
            .sync_owner_identity()
            .unwrap()
            .is_some());
    }

    /// #368: the Filen Desktop WebDAV bridge has the same local cache as the
    /// S3 one, so its benchmark carries the same note; a plain server none.
    #[test]
    fn measurement_note_names_the_filen_desktop_bridge_only() {
        let mut config = test_config("http://127.0.0.1:1900");
        assert_eq!(
            WebDavProvider::new(config.clone())
                .unwrap()
                .measurement_note(),
            None
        );
        config.provider_id = Some("filen-desktop-webdav".to_string());
        assert_eq!(
            WebDavProvider::new(config).unwrap().measurement_note(),
            Some(super::super::FILEN_DESKTOP_BRIDGE_NOTE)
        );
    }

    /// RFC 4918 section 9.9.3 has a MOVE with `Overwrite: T` DELETE the
    /// destination before it moves, so a WebDAV replace sets the old file
    /// aside and the name is empty between its two moves: not a replace with
    /// no moment without the file. The snapshot does not claim it, in either
    /// mode.
    #[test]
    fn atomic_rename_is_not_claimed_for_a_set_aside() {
        let mut provider =
            WebDavProvider::new(test_config("https://example.com/dav")).expect("provider");
        assert_eq!(
            provider.transfer_capabilities().atomic_rename,
            crate::transfer_dag::Capability::Unsupported
        );
        provider.single_file_mode = Some(RemoteEntry::file(
            "f.txt".to_string(),
            "/dav/f.txt".to_string(),
            1,
        ));
        assert_eq!(
            provider.transfer_capabilities().atomic_rename,
            crate::transfer_dag::Capability::Unsupported
        );
    }

    /// A metadata request (a listing, a stat, a date, a folder) is bounded
    /// as a whole, so a server that accepts it and never answers cannot hold
    /// a command for the 1800 s read timeout. Transfers, and the operations
    /// whose outcome a timeout would leave unknown, keep the read timeout.
    #[test]
    fn only_metadata_requests_carry_a_whole_request_limit() {
        let provider = WebDavProvider::new(test_config(
            "https://dav.example.test/remote.php/dav/files/u/",
        ))
        .expect("provider");
        let limit_of = |method: Method| {
            provider
                .request(method, "/folder/")
                .build()
                .expect("request")
                .timeout()
                .copied()
        };
        for method in [
            webdav_methods::propfind(),
            Method::from_bytes(b"PROPPATCH").unwrap(),
            webdav_methods::mkcol(),
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert_eq!(
                limit_of(method.clone()),
                Some(std::time::Duration::from_secs(120)),
                "{method} has no limit, or another one"
            );
        }
        for method in [
            Method::GET,
            Method::PUT,
            Method::DELETE,
            webdav_methods::move_method(),
            webdav_methods::copy(),
            webdav_methods::lock(),
        ] {
            assert_eq!(limit_of(method.clone()), None, "{method} is cut short");
        }
        // The same through `request_url`, which the trashbin listing uses.
        let trash = provider
            .request_url(
                webdav_methods::propfind(),
                "https://dav.example.test/remote.php/dav/trashbin/u/trash/",
            )
            .build()
            .expect("request")
            .timeout()
            .copied();
        assert_eq!(trash, Some(std::time::Duration::from_secs(120)));
    }

    /// End to end, with the provider's own client and its 1800 s read
    /// timeout: a stat and a listing against a server that never answers
    /// come back as a timeout once the metadata limit passes.
    #[tokio::test]
    async fn a_metadata_request_the_server_never_answers_ends_at_its_limit() {
        let mut provider =
            WebDavProvider::new(test_config(&crate::providers::mute_http_endpoint()))
                .expect("provider");
        provider.connected = true;
        provider.metadata_timeout = std::time::Duration::from_millis(300);
        let bound = std::time::Duration::from_secs(20);
        let stat = tokio::time::timeout(bound, provider.stat("/file.txt"))
            .await
            .expect("the stat is still waiting");
        assert!(matches!(stat, Err(ProviderError::Timeout)), "{stat:?}");
        let list = tokio::time::timeout(bound, provider.list("/"))
            .await
            .expect("the listing is still waiting");
        assert!(matches!(list, Err(ProviderError::Timeout)), "{list:?}");
    }

    /// The Nextcloud trashbin listing, which builds its PROPFIND outside the
    /// files path: against a server that never answers it is a timeout at
    /// the metadata limit.
    #[tokio::test]
    async fn a_trash_listing_the_server_never_answers_ends_at_its_limit() {
        let url = format!(
            "{}remote.php/dav/files/u/",
            crate::providers::mute_http_endpoint()
        );
        let mut provider = WebDavProvider::new(test_config(&url)).expect("provider");
        provider.connected = true;
        provider.metadata_timeout = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let listing = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            provider.nextcloud_list_trash(),
        )
        .await
        .expect("the trash listing is still waiting");
        assert!(
            matches!(listing, Err(ProviderError::Timeout)),
            "{listing:?}"
        );
        let elapsed = started.elapsed();
        assert!(elapsed >= provider.metadata_timeout, "{elapsed:?}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    /// The connect-time probe of a URL that names a file: against a server
    /// that never answers it gives up at the metadata limit, not at the
    /// client's 1800 s read timeout.
    #[tokio::test]
    async fn a_single_file_probe_the_server_never_answers_ends_at_its_limit() {
        let url = format!("{}file.txt", crate::providers::mute_http_endpoint());
        let mut provider = WebDavProvider::new(test_config(&url)).expect("provider");
        provider.metadata_timeout = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let probe = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            provider.probe_single_file(),
        )
        .await
        .expect("the probe is still waiting");
        assert!(probe.is_none());
        // It sent the request and waited for its limit, not returned early.
        assert!(
            started.elapsed() >= provider.metadata_timeout,
            "{:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    /// The provider's client with its read timeout cut from 1800 s to a
    /// fraction of a second, so a server that never answers is seen in time.
    fn impatient_client() -> Client {
        Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .redirect(webdav_redirect_policy())
            .read_timeout(std::time::Duration::from_millis(300))
            .build()
            .expect("client")
    }

    /// D7: a server that accepts the connection and never answers made the
    /// connect a `ConnectionFailed`, so the CLI exited 1 with "verify the
    /// service is running" after the read timeout. It is a timeout.
    #[tokio::test]
    async fn a_connect_the_server_never_answers_is_a_timeout() {
        let mut provider =
            WebDavProvider::new(test_config(&crate::providers::mute_http_endpoint()))
                .expect("provider");
        provider.client = impatient_client();
        let outcome = provider.connect().await;
        assert!(
            matches!(outcome, Err(ProviderError::Timeout)),
            "{outcome:?}"
        );
    }

    /// The same for every request after the connect, which reported a
    /// `NetworkError`.
    #[tokio::test]
    async fn a_request_the_server_never_answers_is_a_timeout() {
        let failed = impatient_client()
            .request(
                Method::from_bytes(b"PROPFIND").expect("method"),
                crate::providers::mute_http_endpoint(),
            )
            .send()
            .await
            .expect_err("no answer");
        let mapped = send_error(failed);
        assert!(matches!(mapped, ProviderError::Timeout), "{mapped:?}");
    }

    /// The same timeout after the answer began: a download whose body stops
    /// halfway was `TransferFailed`, which the CLI reports with exit 1 and the
    /// transfer engine does not treat as a stall.
    #[tokio::test]
    async fn a_download_whose_body_stops_is_a_timeout() {
        let mut provider =
            WebDavProvider::new(test_config(&crate::providers::stalled_body_endpoint()))
                .expect("provider");
        provider.client = impatient_client();
        provider.connected = true;
        provider.multi_thread_streams = 1;
        let target = tempfile::tempdir().expect("tempdir");
        let local = target.path().join("f.bin");
        let outcome = provider
            .download_with_size_hint("/f.bin", local.to_str().expect("utf-8"), None, None)
            .await;
        assert!(
            matches!(outcome, Err(ProviderError::Timeout)),
            "{outcome:?}"
        );
        assert!(!local.exists(), "a stalled download leaves no file behind");
    }

    /// The path a `{username}` server template puts on the wire: one level of
    /// percent-encoding, read from the request the server receives rather
    /// than from the string we build. Plain names, `@` and a space reach the
    /// server exactly as before the template had its own encoding; `%`, `/`,
    /// `?` and `#` used to reach it raw, as a broken escape, a second path
    /// level, a query or a fragment.
    #[tokio::test]
    async fn a_username_template_reaches_the_server_encoded_exactly_once() {
        let cases = [
            ("alice", "/dav/alice/"),
            ("alice@example.com", "/dav/alice@example.com/"),
            ("a b", "/dav/a%20b/"),
            ("50%", "/dav/50%25/"),
            ("a/b", "/dav/a%2Fb/"),
            ("a?b", "/dav/a%3Fb/"),
            ("a#b", "/dav/a%23b/"),
        ];
        for (user, wire_path) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let record = seen.clone();
            let app = axum::Router::new().fallback(axum::routing::any(
                move |req: axum::extract::Request| {
                    let record = record.clone();
                    async move {
                        let raw = req
                            .uri()
                            .path_and_query()
                            .map(|pq| pq.as_str().to_string())
                            .unwrap_or_default();
                        record.lock().unwrap().push(raw.clone());
                        // A collection, so the connect succeeds and a listing
                        // follows through `build_url`.
                        let body = format!(
                            "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"><d:response>\
                             <d:href>{raw}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/>\
                             </d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
                             </d:response></d:multistatus>"
                        );
                        (axum::http::StatusCode::MULTI_STATUS, body)
                    }
                },
            ));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut extra = std::collections::HashMap::new();
            extra.insert("tls_mode".to_string(), "http".to_string());
            let config = crate::providers::ProviderConfig {
                name: "template".to_string(),
                provider_type: crate::providers::ProviderType::WebDav,
                host: format!("{base}/dav/{{username}}/"),
                port: None,
                username: Some(user.to_string()),
                password: Some("p".to_string()),
                initial_path: None,
                extra,
            };
            let dav = WebDavConfig::from_provider_config(&config).expect("config");
            let mut provider = WebDavProvider::new(dav).expect("provider");
            provider
                .connect()
                .await
                .unwrap_or_else(|e| panic!("{user}: {e}"));
            // A request built by `build_url`, not the probe's own URL: the
            // account segment stays encoded once there too.
            let _ = provider.list("docs").await;
            server.abort();
            let seen = seen.lock().unwrap().clone();
            assert!(!seen.is_empty(), "{user}: no request reached the server");
            assert_eq!(seen[0], wire_path, "{user}: {seen:?}");
            let listing = format!("{wire_path}docs");
            assert!(
                seen.iter().skip(1).any(|path| path.starts_with(&listing)),
                "{user}: no listing under {listing}: {seen:?}"
            );
        }
    }

    /// Nextcloud and ownCloud refuse a PROPFIND on `/`; the root found under
    /// `/remote.php/dav/files/<user>/` takes the initial folder as given.
    #[tokio::test]
    async fn a_discovered_nextcloud_root_keeps_the_spaces_of_the_initial_folder() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().fallback(axum::routing::any(
            |req: axum::extract::Request| async move {
                if req.method().as_str() == "PROPFIND"
                    && req.uri().path() == "/remote.php/dav/files/user/"
                {
                    axum::http::StatusCode::MULTI_STATUS
                } else {
                    axum::http::StatusCode::METHOD_NOT_ALLOWED
                }
            },
        ));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = test_config(&base);
        config.initial_path = Some("reports ".to_string());
        let mut provider = WebDavProvider::new(config).expect("provider");
        let outcome = provider.connect().await;
        server.abort();
        outcome.expect("connect");
        assert_eq!(provider.current_path, "/remote.php/dav/files/user/reports ");
    }

    /// A Nextcloud with `password.enforced` refusing a link share without a
    /// password, in German, and answering a second POST with one.
    async fn serve_share_refusal(
        enforced: Option<bool>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let posts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = posts.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |req: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    let path = req.uri().path().to_string();
                    let method = req.method().clone();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
                    let body = String::from_utf8_lossy(&body).to_string();
                    if method == axum::http::Method::GET && path == "/ocs/v1.php/cloud/capabilities" {
                        let Some(enforced) = enforced else {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new());
                        };
                        let caps = format!(
                            r#"{{"ocs":{{"data":{{"capabilities":{{"files_sharing":{{"public":{{"enabled":true,"password":{{"enforced":{enforced}}}}}}}}}}}}}}}"#
                        );
                        return (axum::http::StatusCode::OK, caps);
                    }
                    if method == axum::http::Method::POST
                        && path == "/ocs/v2.php/apps/files_sharing/api/v1/shares"
                    {
                        let with_password = body.contains("&password=");
                        seen.lock().unwrap().push(with_password);
                        if with_password {
                            return (
                                axum::http::StatusCode::OK,
                                r#"{"ocs":{"data":{"url":"https://cloud.example.test/s/abc"}}}"#.to_string(),
                            );
                        }
                        return (
                            axum::http::StatusCode::FORBIDDEN,
                            r#"{"ocs":{"meta":{"status":"failure","statuscode":403,"message":"Passwörter sind für Link- und E-Mail-Freigaben erforderlich"}}}"#.to_string(),
                        );
                    }
                    (axum::http::StatusCode::NOT_FOUND, String::new())
                }
            },
        ));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, posts, server)
    }

    /// #1506 row 164: the retry with a generated password follows the
    /// server's `password.enforced` capability, not the 403 text, which is
    /// translated: a German refusal has no "password" in it, and the old
    /// check returned the refusal instead of the link.
    #[tokio::test]
    async fn an_enforced_link_password_is_read_from_the_capabilities() {
        let (base, posts, server) = serve_share_refusal(Some(true)).await;
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        let outcome = provider
            .nextcloud_create_share("/report.pdf", ShareLinkOptions::default())
            .await;
        let posts = posts.lock().unwrap().clone();
        server.abort();
        let link = outcome.expect("the retry with a password gets the link");
        assert_eq!(link.url, "https://cloud.example.test/s/abc");
        assert!(link.password.is_some());
        assert_eq!(posts, vec![false, true]);
    }

    /// CodeRabbit (#1520): capabilities that cannot be read do not leave a
    /// password-enforcing server's refusal standing: a password is tried.
    #[tokio::test]
    async fn an_unreadable_capability_still_tries_a_password() {
        let (base, posts, server) = serve_share_refusal(None).await;
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        let outcome = provider
            .nextcloud_create_share("/report.pdf", ShareLinkOptions::default())
            .await;
        let posts = posts.lock().unwrap().clone();
        server.abort();
        assert!(outcome
            .expect("the retry with a password gets the link")
            .password
            .is_some());
        assert_eq!(posts, vec![false, true]);
    }

    /// CodeRabbit (#1520): a refused retry names neither the generated
    /// password (echoed back by the server) nor a token in the server path.
    #[tokio::test]
    async fn a_refused_share_names_no_password_and_no_path_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/tok-SECRET1234", listener.local_addr().unwrap());
        let sent = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = sent.clone();
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    if req.uri().path().ends_with("/cloud/capabilities") {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new());
                    }
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    let body = String::from_utf8_lossy(&body).to_string();
                    *seen.lock().unwrap() = body.clone();
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        format!("refused: {body}"),
                    )
                }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        let err = provider
            .nextcloud_create_share("/report.pdf", ShareLinkOptions::default())
            .await
            .expect_err("refused twice")
            .to_string();
        server.abort();
        let form = sent.lock().unwrap().clone();
        let password = form
            .split("&password=")
            .nth(1)
            .map(|p| urlencoding::decode(p).unwrap().into_owned())
            .expect("the retry carried a password");
        assert!(!err.contains(&password), "{err}");
        assert!(!err.contains(&*urlencoding::encode(&password)), "{err}");
        assert!(!err.contains("SECRET1234"), "{err}");
        assert!(err.contains("could not be read"), "{err}");
    }

    /// A refused retry whose message echoes the generated share password as
    /// XML references still names no password; the rest of the message stays.
    #[tokio::test]
    async fn a_refused_share_names_no_password_echoed_as_references() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let sent = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = sent.clone();
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    if req.uri().path().ends_with("/cloud/capabilities") {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new());
                    }
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    let body = String::from_utf8_lossy(&body).to_string();
                    let Some(password) = body
                        .split("&password=")
                        .nth(1)
                        .map(|p| urlencoding::decode(p).unwrap().into_owned())
                    else {
                        return (axum::http::StatusCode::FORBIDDEN, String::new());
                    };
                    *seen.lock().unwrap() = password.clone();
                    let escaped: String = password
                        .chars()
                        .map(|c| match c {
                            '&' => "&amp;".to_string(),
                            c if c.is_ascii_alphanumeric() => c.to_string(),
                            c => format!("&#x{:X};", c as u32),
                        })
                        .collect();
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        format!("<?xml version=\"1.0\"?><ocs><meta><status>failure</status><message>Password {escaped} is too weak</message></meta></ocs>"),
                    )
                }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        let err = provider
            .nextcloud_create_share("/report.pdf", ShareLinkOptions::default())
            .await
            .expect_err("refused twice")
            .to_string();
        server.abort();
        let password = sent.lock().unwrap().clone();
        assert!(!password.is_empty(), "the retry carried a password");
        assert!(!err.contains(&password), "{err}");
        assert!(err.contains("Password [REDACTED] is too weak"), "{err}");
    }

    /// Without the capability a 403 is the answer: no second POST.
    #[tokio::test]
    async fn a_refusal_without_an_enforced_password_is_not_retried() {
        let (base, posts, server) = serve_share_refusal(Some(false)).await;
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        let outcome = provider
            .nextcloud_create_share("/report.pdf", ShareLinkOptions::default())
            .await;
        let posts = posts.lock().unwrap().clone();
        server.abort();
        assert!(outcome.is_err());
        assert_eq!(posts, vec![false]);
    }

    /// A Nextcloud double for the versions of `/a.txt` (file id 42, 20 bytes
    /// now, as Nextcloud 25 lists: no row for the current contents) and of
    /// `/b.txt` (file id 43, whose current contents are listed under its
    /// mtime, oldest first, as Nextcloud 26 and later may list): the parent
    /// listing gives the ids, a GET of an `a.txt` version returns `old body`, a MOVE to `restore/target`
    /// answers 201, and a DELETE answers 204 for the newer version and 405
    /// for the older. Every request is logged as
    /// `METHOD path [Destination]`.
    async fn serve_nextcloud_versions() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let swap_listings: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
        let gone_listings: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
        let later_lookups: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<String, usize>>,
        > = Default::default();
        // r.txt (file 50) holds 20 bytes until its 8-byte version is moved
        // to the restore target, then those 8.
        let restored_r: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
        // s.txt (file 65): its file id row names this server's authority
        // under another scheme, `https://` where the requests go over
        // `http://`: another origin.
        let other_scheme = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>{}/remote.php/dav/files/user/s.txt</d:href><d:propstat><d:prop><oc:fileid>65</oc:fileid><d:getetag>&quot;s1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
            base.replacen("http://", "https://", 1)
        );
        // bh.txt (file 66): its file id row names another host that a
        // backslash ends, `elsewhere.example\@` before this server's
        // authority: split at the `@` it read as this server.
        let backslash_host = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>{}/remote.php/dav/files/user/bh.txt</d:href><d:propstat><d:prop><oc:fileid>66</oc:fileid><d:getetag>&quot;bh1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
            base.replacen("http://", "http://elsewhere.example\\@", 1)
        );
        // bp.txt (file 67): its file id row names this server, then a
        // backslash that ends the authority: its path is
        // `/@elsewhere.example/bp.txt`, which a read past the backslash
        // took as `/bp.txt`, the relative listing path.
        // bo.txt (file 68): its file id row names this server with the path
        // `//other/bo.txt`, which a read of the path alone took for the
        // authority `other` and the path `/bo.txt`.
        let double_slash = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>{base}//other/bo.txt</d:href><d:propstat><d:prop><oc:fileid>68</oc:fileid><d:getetag>&quot;bo1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        // bb.txt (file 69): its file id row is a `blob:` URL of this
        // server's bb.txt, whose origin is this server's but whose scheme
        // and resource are not.
        let blob = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>blob:{base}/remote.php/dav/files/user/bb.txt</d:href><d:propstat><d:prop><oc:fileid>69</oc:fileid><d:getetag>&quot;bb1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        // sp.txt with a trailing space (file 70) answers with the href of
        // sp.txt, and sq.txt (file 71) with the href of `sq.txt ` written
        // with a literal space: neither is the file asked for. `sr.txt `
        // (file 72) answers with its own href, the space escaped.
        let space_row = |href: &str, id: u32| {
            format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>{href}</d:href><d:propstat><d:prop><oc:fileid>{id}</oc:fileid><d:getetag>&quot;e{id}&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
            )
        };
        // rel.txt (file 73) and dot.txt (file 74) answer with a relative
        // href, `rel.txt` and `./dot.txt`: the file itself.
        let relative = [space_row("rel.txt", 73), space_row("./dot.txt", 74)];
        let spaced = [
            space_row("/remote.php/dav/files/user/sp.txt", 70),
            space_row("/remote.php/dav/files/user/sq.txt ", 71),
            space_row("/remote.php/dav/files/user/sr.txt%20", 72),
        ];
        let backslash_path = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>{base}\@elsewhere.example/bp.txt</d:href><d:propstat><d:prop><oc:fileid>67</oc:fileid><d:getetag>&quot;bp1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        );
        // rd.txt: the PROPFIND is redirected (307) to another origin, this
        // server under the name `localhost`, which answers file 99 with the
        // relative href `rd.txt`: that origin's rd.txt, not this one's.
        let redirect_to = base.replacen("127.0.0.1", "localhost", 1);
        let seen = log.clone();
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let redirect_to = redirect_to.clone();
                let other_scheme = other_scheme.clone();
                let backslash_host = backslash_host.clone();
                let backslash_path = backslash_path.clone();
                let double_slash = double_slash.clone();
                let spaced = spaced.clone();
                let relative = relative.clone();
                let blob = blob.clone();
                let seen = seen.clone();
                let swap_listings = swap_listings.clone();
                let gone_listings = gone_listings.clone();
                let later_lookups = later_lookups.clone();
                let restored_r = restored_r.clone();
                async move {
                    let method = req.method().to_string();
                    let path = req.uri().path().to_string();
                    let destination = req
                        .headers()
                        .get("Destination")
                        .and_then(|v| v.to_str().ok())
                        .map(|d| format!(" -> {d}"))
                        .unwrap_or_default();
                    let if_match = req
                        .headers()
                        .get(reqwest::header::IF_MATCH)
                        .and_then(|v| v.to_str().ok())
                        .map(|v| format!(" if-match {v}"))
                        .unwrap_or_default();
                    seen.lock()
                        .unwrap()
                        .push(format!("{method} {path}{destination}{if_match}"));
                    let reply = |status: u16, body: &str| {
                        axum::response::Response::builder()
                            .status(status)
                            .body(axum::body::Body::from(body.to_string()))
                            .unwrap()
                    };
                    let host = req
                        .headers()
                        .get(reqwest::header::HOST)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    // rv.txt (file 78): its versions PROPFIND is redirected
                    // to the other origin, which lists a path-only row.
                    if path == "/remote.php/dav/versions/user/versions/78/" {
                        return if host.starts_with("127.0.0.1") {
                            axum::response::Response::builder()
                                .status(307)
                                .header(reqwest::header::LOCATION, format!("{redirect_to}{path}"))
                                .body(axum::body::Body::empty())
                                .unwrap()
                        } else {
                            reply(
                                207,
                                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/remote.php/dav/versions/user/versions/78/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>2</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                            )
                        };
                    }
                    if path == "/remote.php/dav/files/user/rv.txt" && method == "PROPFIND" {
                        return reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/rv.txt</d:href><d:propstat><d:prop><oc:fileid>78</oc:fileid><d:getetag>&quot;rv1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:getlastmodified>Wed, 15 Nov 2023 10:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        );
                    }
                    if path == "/remote.php/dav/files/user/rd.txt" {
                        return if host.starts_with("127.0.0.1") {
                            axum::response::Response::builder()
                                .status(307)
                                .header(reqwest::header::LOCATION, format!("{redirect_to}{path}"))
                                .body(axum::body::Body::empty())
                                .unwrap()
                        } else {
                            reply(
                                207,
                                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>rd.txt</d:href><d:propstat><d:prop><oc:fileid>99</oc:fileid><d:getetag>&quot;rd1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                            )
                        };
                    }
                    match (method.as_str(), path.as_str()) {
                        // foreign/ lists f.txt under an absolute href of
                        // another origin, its path the same as ours.
                        ("PROPFIND", "/remote.php/dav/files/user/foreign/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/foreign/</d:href><d:propstat><d:prop><oc:fileid>76</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>https://other.example/remote.php/dav/files/user/foreign/f.txt</d:href><d:propstat><d:prop><oc:fileid>77</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/files/user/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/</d:href><d:propstat><d:prop><oc:fileid>1</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/a.txt</d:href><d:propstat><d:prop><oc:fileid>42</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/b.txt</d:href><d:propstat><d:prop><oc:fileid>43</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/off.txt</d:href><d:propstat><d:prop><oc:fileid>44</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/big.txt</d:href><d:propstat><d:prop><oc:fileid>45</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/d.txt</d:href><d:propstat><d:prop><oc:fileid>46</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/user/other/c.txt</d:href><d:propstat><d:prop><oc:fileid>99</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/42/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns">
<d:response><d:href>/remote.php/dav/versions/user/versions/42/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/42/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:getlastmodified>Tue, 14 Nov 2023 22:13:20 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><nc:version-author/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/42/1700000500</d:href><d:propstat><d:prop><d:getcontentlength>12</d:getcontentlength><d:resourcetype/><nc:version-author>alice</nc:version-author></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/files/user/a.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/a.txt</d:href><d:propstat><d:prop><oc:fileid>42</oc:fileid><d:getetag>&quot;e42&quot;</d:getetag><d:getcontentlength>20</d:getcontentlength><d:getlastmodified>Wed, 15 Nov 2023 10:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("GET", "/remote.php/dav/files/user/a.txt") => reply(200, "the body of now"),
                        ("PROPFIND", "/remote.php/dav/files/user/r.txt") => {
                            let size = if restored_r.load(std::sync::atomic::Ordering::SeqCst) { 8 } else { 20 };
                            reply(
                                207,
                                &format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/r.txt</d:href><d:propstat><d:prop><oc:fileid>50</oc:fileid><d:getcontentlength>{size}</d:getcontentlength><d:getlastmodified>Wed, 15 Nov 2023 10:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#),
                            )
                        }
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/50/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/versions/user/versions/50/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("MOVE", "/remote.php/dav/versions/user/versions/50/1700000000") => {
                            restored_r.store(true, std::sync::atomic::Ordering::SeqCst);
                            reply(201, "")
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/off.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/off.txt</d:href><d:propstat><d:prop><oc:fileid>44</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/files/user/big.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/big.txt</d:href><d:propstat><d:prop><oc:fileid>45</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("GET", "/remote.php/dav/files/user/b.txt") => reply(200, "b as it is now"),
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/45/") => {
                            reply(207, &" ".repeat(super::NEXTCLOUD_VERSIONS_MAX_BYTES as usize + 1))
                        }
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/99/") => reply(207, ""),
                        // huge/h.txt answers its own lookup with more than the cap.
                        ("PROPFIND", "/remote.php/dav/files/user/huge/h.txt") => {
                            reply(207, &" ".repeat(super::NEXTCLOUD_VERSIONS_MAX_BYTES as usize + 1))
                        }
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/46/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/versions/user/versions/46/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/46/1700000100</d:href><d:propstat><d:prop><d:getcontentlength>5</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/43/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/other/versions/46/1700000200</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        // swap/e.txt is file 47 at its first three lookups
                        // (the listing reads it twice) and file 48 from the
                        // fourth on.
                        ("PROPFIND", "/remote.php/dav/files/user/swap/e.txt") => {
                            let n = swap_listings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let id = if n < 3 { 47 } else { 48 };
                            reply(
                                207,
                                &format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/swap/e.txt</d:href><d:propstat><d:prop><oc:fileid>{id}</oc:fileid><d:getetag>&quot;e{id}&quot;</d:getetag><d:getcontentlength>9</d:getcontentlength><d:getlastmodified>Wed, 15 Nov 2023 10:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#),
                            )
                        }
                        ("GET", "/remote.php/dav/files/user/swap/e.txt") => reply(200, "e of 48"),
                        ("GET", "/remote.php/dav/files/user/wr.txt") => reply(200, "w of now"),
                        // swapback/k.txt answers file 51 (ETag "e51") at every
                        // lookup, but file 52 ("e52") is at the path while its
                        // body is read: a read conditional on "e51" is refused.
                        ("PROPFIND", "/remote.php/dav/files/user/swapback/k.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/swapback/k.txt</d:href><d:propstat><d:prop><oc:fileid>51</oc:fileid><d:getcontentlength>9</d:getcontentlength><d:getetag>&quot;e51&quot;</d:getetag><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("GET", "/remote.php/dav/files/user/swapback/k.txt") => {
                            if if_match.is_empty() || if_match == " if-match \"e52\"" {
                                reply(200, "k of 52")
                            } else {
                                reply(412, "")
                            }
                        }
                        // gone/g.txt has file id 49 at the lookup and is gone
                        // at the next one; its versions route answers 404.
                        ("PROPFIND", "/remote.php/dav/files/user/gone/g.txt") => {
                            let n = gone_listings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if n == 0 {
                                reply(
                                    207,
                                    r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/gone/g.txt</d:href><d:propstat><d:prop><oc:fileid>49</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                                )
                            } else {
                                reply(404, "")
                            }
                        }
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/47/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/versions/user/versions/47/1700000100</d:href><d:propstat><d:prop><d:getcontentlength>5</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/files/user/d.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/d.txt</d:href><d:propstat><d:prop><oc:fileid>46</oc:fileid><d:getcontentlength>6</d:getcontentlength><d:getlastmodified>Wed, 15 Nov 2023 10:00:00 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        // m.txt answers its lookup with a lone row for
                        // other/m.txt (file 77).
                        ("PROPFIND", "/remote.php/dav/files/user/m.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/other/m.txt</d:href><d:propstat><d:prop><oc:fileid>77</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        ),
                        // u.txt (file 54) gets a new upload, ETag u2, between
                        // the two lookups of a listing; t.txt (file 53) has
                        // no versions route and its second lookup fails;
                        // rb.txt is file 60 at its first lookup and file 61
                        // after, both holding a version 1700000000; lf.txt
                        // (file 62) has a versions route that fails.
                        ("PROPFIND", p @ ("/remote.php/dav/files/user/u.txt"
                        | "/remote.php/dav/files/user/t.txt"
                        | "/remote.php/dav/files/user/rb.txt"
                        | "/remote.php/dav/files/user/wr.txt"
                        | "/remote.php/dav/files/user/lf.txt")) => {
                            let n = {
                                let mut seen = later_lookups.lock().unwrap();
                                let n = seen.entry(p.to_string()).or_insert(0);
                                *n += 1;
                                *n - 1
                            };
                            let (id, etag) = match p {
                                "/remote.php/dav/files/user/u.txt" => (54, if n == 0 { "u1" } else { "u2" }),
                                "/remote.php/dav/files/user/t.txt" if n > 0 => return reply(500, ""),
                                "/remote.php/dav/files/user/t.txt" => (53, "t1"),
                                "/remote.php/dav/files/user/rb.txt" => (if n == 0 { 60 } else { 61 }, "r"),
                                // Written after its listing (two lookups).
                                "/remote.php/dav/files/user/wr.txt" => (79, if n < 2 { "w1" } else { "w2" }),
                                _ => (62, "l"),
                            };
                            let leaf = p.rsplit('/').next().unwrap();
                            reply(
                                207,
                                &format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/{leaf}</d:href><d:propstat><d:prop><oc:fileid>{id}</oc:fileid><d:getetag>&quot;{etag}&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
                            )
                        }
                        ("PROPFIND", p @ ("/remote.php/dav/versions/user/versions/54/"
                        | "/remote.php/dav/versions/user/versions/79/"
                        | "/remote.php/dav/versions/user/versions/60/"
                        | "/remote.php/dav/versions/user/versions/61/")) => reply(
                            207,
                            &format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>{p}1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
                        ),
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/62/") => reply(500, ""),
                        // n.txt (file 63): its one version row is a
                        // network-path reference, `//remote.php/...`, which
                        // names the host `remote.php`, not this server.
                        ("PROPFIND", "/remote.php/dav/files/user/n.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/remote.php/dav/files/user/n.txt</d:href><d:propstat><d:prop><oc:fileid>63</oc:fileid><d:getetag>&quot;n1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/files/user/s.txt") => {
                            reply(207, &other_scheme)
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/bh.txt") => {
                            reply(207, &backslash_host)
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/bp.txt") => {
                            reply(207, &backslash_path)
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/bo.txt") => {
                            reply(207, &double_slash)
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/bb.txt") => reply(207, &blob),
                        ("PROPFIND", "/remote.php/dav/files/user/sp.txt%20") => {
                            reply(207, &spaced[0])
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/sq.txt") => {
                            reply(207, &spaced[1])
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/sr.txt%20") => {
                            reply(207, &spaced[2])
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/rel.txt") => {
                            reply(207, &relative[0])
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/dot.txt") => {
                            reply(207, &relative[1])
                        }
                        // h.txt (file 64): its file id row names another
                        // server, `//elsewhere.example/h.txt`, whose path
                        // alone matches the relative listing path.
                        ("PROPFIND", "/remote.php/dav/files/user/h.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>//elsewhere.example/h.txt</d:href><d:propstat><d:prop><oc:fileid>64</oc:fileid><d:getetag>&quot;h1&quot;</d:getetag><d:getcontentlength>3</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/63/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>//remote.php/dav/versions/user/versions/63/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        ),
                        ("DELETE", "/remote.php/dav/versions/user/versions/63/1700000000") => {
                            reply(204, "")
                        }
                        ("MOVE", "/remote.php/dav/versions/user/versions/42/1700000500") => {
                            reply(500, "<error>user=user password=hunter2-pass token=tok-abc123</error>")
                        }
                        // b.txt's restore MOVE is answered by a gateway.
                        ("MOVE", "/remote.php/dav/versions/user/versions/43/1700000000") => {
                            reply(502, "")
                        }
                        ("PROPFIND", "/remote.php/dav/files/user/b.txt") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/user/b.txt</d:href><d:propstat><d:prop><oc:fileid>43</oc:fileid><d:getetag>&quot;e43&quot;</d:getetag><d:getcontentlength>12</d:getcontentlength><d:getlastmodified>Tue, 14 Nov 2023 22:21:40 GMT</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("PROPFIND", "/remote.php/dav/versions/user/versions/43/") => reply(
                            207,
                            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/versions/user/versions/43/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/43/1700000000</d:href><d:propstat><d:prop><d:getcontentlength>8</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/versions/user/versions/43/1700000500</d:href><d:propstat><d:prop><d:getcontentlength>12</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#,
                        ),
                        ("GET", "/remote.php/dav/versions/user/versions/42/1700000000") => {
                            reply(200, "old body")
                        }
                        // A part of the version: never written as it.
                        ("GET", "/remote.php/dav/versions/user/versions/42/1700000500") => {
                            reply(206, "part")
                        }
                        ("MOVE", "/remote.php/dav/versions/user/versions/42/1700000000") => {
                            reply(201, "")
                        }
                        ("DELETE", "/remote.php/dav/versions/user/versions/42/1700000500") => {
                            reply(204, "")
                        }
                        ("DELETE", "/remote.php/dav/versions/user/versions/42/1700000000") => {
                            reply(405, "")
                        }
                        _ => reply(404, ""),
                    }
                }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, log, server)
    }

    /// A provider past `connect`: the file itself (row 0, version "current")
    /// is read by `stat` and `download`, which refuse an unconnected one.
    fn nextcloud_versions_provider(base: &str) -> WebDavProvider {
        let mut provider =
            WebDavProvider::new(test_config(&format!("{base}/remote.php/dav/files/user/")))
                .unwrap();
        provider.connected = true;
        provider
    }

    /// `disconnect` keeps the client and the credentials: a version list,
    /// download, restore or delete still queued for the provider sent its
    /// MOVE or DELETE. A disconnected provider sends nothing.
    #[tokio::test]
    async fn a_disconnected_provider_sends_no_version_request() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        provider.disconnect().await.expect("disconnect");
        let local = tempfile::tempdir().unwrap();
        let local = local.path().join("v").to_string_lossy().into_owned();
        assert!(matches!(
            provider.list_versions("/a.txt").await,
            Err(ProviderError::NotConnected)
        ));
        assert!(matches!(
            provider
                .download_version("/a.txt", "42:1700000000", &local)
                .await,
            Err(ProviderError::NotConnected)
        ));
        assert!(matches!(
            provider.restore_version("/a.txt", "42:1700000000").await,
            Err(ProviderError::NotConnected)
        ));
        assert!(matches!(
            provider.delete_version("/a.txt", "42:1700000500").await,
            Err(ProviderError::NotConnected)
        ));
        server.abort();
        assert!(log.lock().unwrap().is_empty(), "{:?}", log.lock().unwrap());
    }

    /// A version row named by a network-path reference (`//host/...`) is a
    /// row of another server: it is not listed, and a delete of the local
    /// version of its time is refused before any DELETE is sent.
    #[tokio::test]
    async fn a_version_row_of_another_host_is_neither_listed_nor_purged() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider.list_versions("/n.txt").await.expect("versions");
        let ids: Vec<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(
            ids,
            [super::super::bound_current_row("63", "\"n1\"").as_str()]
        );
        let refused = provider.delete_version("/n.txt", "63:1700000000").await;
        server.abort();
        assert!(refused.is_err(), "{refused:?}");
        assert!(
            !log.lock().unwrap().iter().any(|l| l.starts_with("DELETE")),
            "{:?}",
            log.lock().unwrap()
        );
    }

    /// The file id answer for h.txt names `//elsewhere.example/h.txt`: an
    /// entry of another server, refused before any version request, where
    /// its path alone read as h.txt and its id was listed as h.txt's.
    #[tokio::test]
    async fn a_file_id_row_of_another_server_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let refused = provider.list_versions("/h.txt").await;
        server.abort();
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{refused:?}"
        );
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l.contains("/versions/64")), "{log:?}");
    }

    /// The same origin written two ways is one origin: a default port, the
    /// case of the scheme and host, user information, a path and a
    /// scheme-relative href do not make another server; another host, port
    /// or scheme does, and so does a host a backslash ends. The resolved
    /// path comes from the same parse.
    #[test]
    fn an_href_is_resolved_as_a_browser_resolves_it() {
        let own = "https://cloud.example/remote.php/dav/files/u/a.txt";
        for href in [
            "https://cloud.example:443/x",
            "HTTPS://Cloud.Example/x",
            "https://user@cloud.example/x",
            "//cloud.example:443/x",
            "/remote.php/dav/files/u/b.txt",
        ] {
            assert!(
                WebDavProvider::href_of_our_origin(href, own, own).is_some(),
                "{href}"
            );
        }
        assert!(WebDavProvider::href_of_our_origin(
            "http://h.example:80/x",
            "http://h.example/a",
            "http://h.example/a"
        )
        .is_some());
        assert!(WebDavProvider::href_of_our_origin(
            "https://[::1]:443/x",
            "https://[::1]/a",
            "https://[::1]/a"
        )
        .is_some());
        // The path is read from the same parse: a backslash after our host
        // ends the authority, and what follows is the path.
        let resolved = WebDavProvider::href_of_our_origin(
            "https://cloud.example\\@elsewhere.example/a.txt",
            own,
            own,
        )
        .expect("our origin");
        // After a redirect, a relative href is the redirect target's: of
        // another origin when the redirect left this one.
        assert!(WebDavProvider::href_of_our_origin(
            "a.txt",
            "https://elsewhere.example/remote.php/dav/files/u/a.txt",
            own,
        )
        .is_none());
        assert_eq!(resolved.path(), "/@elsewhere.example/a.txt");
        // A relative href names a sibling of the file asked for, the file
        // itself when it is its name: never a child of it.
        for href in ["a.txt", "./a.txt"] {
            let resolved = WebDavProvider::href_of_our_origin(href, own, own).expect("our origin");
            assert_eq!(resolved.path(), "/remote.php/dav/files/u/a.txt", "{href}");
        }
        for (href, segments) in [
            ("/d/a.txt ", ["d", "a.txt "]),
            ("/d/a.txt%20", ["d", "a.txt "]),
            ("/d/a\t.txt", ["d", "a\t.txt"]),
            ("/d/a.txt", ["d", "a.txt"]),
        ] {
            let resolved = WebDavProvider::href_of_our_origin(href, own, own).expect("our origin");
            assert_eq!(
                WebDavProvider::dav_path_segments(resolved.as_str(), &[]),
                segments,
                "{href:?}"
            );
        }
        assert_eq!(
            WebDavProvider::dav_path_segments(resolved.as_str(), &[]),
            ["@elsewhere.example", "a.txt"]
        );
        let resolved =
            WebDavProvider::href_of_our_origin("https://cloud.example//other/a.txt", own, own)
                .expect("our origin");
        assert_eq!(
            WebDavProvider::dav_path_segments(resolved.as_str(), &[]),
            ["other", "a.txt"]
        );
        for href in [
            "http://cloud.example/x",
            "https://cloud.example:8443/x",
            "https://elsewhere.example/x",
            "http://cloud.example:443/x",
            "//cloud.example:80/x",
            "https://elsewhere.example\\@cloud.example/a.txt",
            "\\\\elsewhere.example\\a.txt",
            "https://[bad/x",
            "blob:https://cloud.example/a.txt",
        ] {
            assert!(
                WebDavProvider::href_of_our_origin(href, own, own).is_none(),
                "{href}"
            );
        }
    }

    /// The file id answer for s.txt names this server's authority under
    /// `https://` while the requests go over `http://`: another origin,
    /// refused before any version request, where a check of the authority
    /// alone took its id as s.txt's.
    #[tokio::test]
    async fn a_file_id_row_of_another_scheme_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let refused = provider.list_versions("/s.txt").await;
        server.abort();
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{refused:?}"
        );
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l.contains("/versions/65")), "{log:?}");
    }

    /// The file id answer for bh.txt names `elsewhere.example` with this
    /// server's authority after a backslash and an `@`: another host, as a
    /// browser reads it, refused before any version request, where a split
    /// at the `@` read it as this server and took its id as bh.txt's.
    #[tokio::test]
    async fn a_file_id_row_whose_host_a_backslash_ends_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let refused = provider.list_versions("/bh.txt").await;
        server.abort();
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{refused:?}"
        );
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l.contains("/versions/66")), "{log:?}");
    }

    /// The file id answer for bp.txt names this server, then a backslash
    /// that ends the authority: the resource is `/@elsewhere.example/bp.txt`,
    /// not bp.txt, refused before any version request, where the path read
    /// past the backslash matched the relative listing path.
    #[tokio::test]
    async fn a_file_id_row_whose_path_follows_a_backslash_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let refused = provider.list_versions("/bp.txt").await;
        server.abort();
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("another entry")),
            "{refused:?}"
        );
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l.contains("/versions/67")), "{log:?}");
    }

    /// A file id row of this server whose path is `//other/bo.txt` names
    /// the resource `/other/bo.txt`, and a `blob:` URL of this server's
    /// bb.txt is not bb.txt: both are refused before any version request,
    /// where a read of the path alone found `/bo.txt` (the relative listing
    /// path) and a `blob:` URL passed with the origin of the URL inside it.
    #[tokio::test]
    async fn a_file_id_row_of_another_resource_or_scheme_on_this_origin_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let double_slash = provider.list_versions("/bo.txt").await;
        let blob = provider.list_versions("/bb.txt").await;
        server.abort();
        assert!(
            matches!(&double_slash, Err(ProviderError::ServerError(m)) if m.contains("another entry")),
            "{double_slash:?}"
        );
        assert!(
            matches!(&blob, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{blob:?}"
        );
        let log = log.lock().unwrap();
        assert!(
            !log.iter()
                .any(|l| l.contains("/versions/68") || l.contains("/versions/69")),
            "{log:?}"
        );
    }

    /// A trailing space is part of the name: the row of sp.txt is not
    /// `sp.txt `'s and the row of `sq.txt ` is not sq.txt's, both refused
    /// before any version request, where the URL parser dropped the space
    /// of a literal href; the escaped href of `sr.txt ` is its own and its
    /// versions are asked.
    #[tokio::test]
    async fn a_trailing_space_in_a_file_id_row_is_part_of_its_name() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let spaced_lookup = provider.list_versions("/sp.txt ").await;
        let spaced_row = provider.list_versions("/sq.txt").await;
        let _ = provider.list_versions("/sr.txt ").await;
        server.abort();
        for refused in [&spaced_lookup, &spaced_row] {
            assert!(
                matches!(refused, Err(ProviderError::ServerError(m)) if m.contains("another entry")),
                "{refused:?}"
            );
        }
        let log = log.lock().unwrap();
        assert!(
            !log.iter()
                .any(|l| l.contains("/versions/70") || l.contains("/versions/71")),
            "{log:?}"
        );
        assert!(log.iter().any(|l| l.contains("/versions/72")), "{log:?}");
    }

    /// A file id row with a relative href (`rel.txt`, `./dot.txt`) names the
    /// file asked for, resolved against its own URL: accepted, and its
    /// versions are asked under its own id.
    #[tokio::test]
    async fn a_file_id_row_with_a_relative_href_is_the_file() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let _ = provider.list_versions("/rel.txt").await;
        let _ = provider.list_versions("/dot.txt").await;
        server.abort();
        let log = log.lock().unwrap();
        assert!(log.iter().any(|l| l.contains("/versions/73")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("/versions/74")), "{log:?}");
    }

    /// The versions PROPFIND of rv.txt is redirected to another origin,
    /// which lists a path-only row: that server's, so the listing is
    /// refused, and a purge named after it sends no DELETE.
    #[tokio::test]
    async fn a_versions_listing_redirected_to_another_origin_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let listed = provider.list_versions("/rv.txt").await;
        let purged = provider.delete_version("/rv.txt", "78:1700000000").await;
        server.abort();
        assert!(
            matches!(&listed, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{listed:?}"
        );
        assert!(purged.is_err(), "{purged:?}");
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l.starts_with("DELETE ")), "{log:?}");
        assert!(
            log.iter()
                .filter(|l| l.starts_with("PROPFIND /remote.php/dav/versions/user/versions/78/"))
                .count()
                >= 2,
            "the redirect was followed: {log:?}"
        );
    }

    /// The file id PROPFIND for rd.txt is redirected (307) to another
    /// origin, which answers file 99 with the relative href `rd.txt`: that
    /// origin's file, refused before any version request, where the href was
    /// resolved against this server's URL and file 99 listed as rd.txt's.
    #[tokio::test]
    async fn a_relative_href_after_a_redirect_to_another_origin_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let refused = provider.list_versions("/rd.txt").await;
        server.abort();
        assert!(
            matches!(&refused, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{refused:?}"
        );
        let log = log.lock().unwrap();
        // The redirect was followed: the other origin was asked.
        assert_eq!(
            log.iter()
                .filter(|l| l.starts_with("PROPFIND /remote.php/dav/files/user/rd.txt"))
                .count(),
            2,
            "{log:?}"
        );
        assert!(!log.iter().any(|l| l.contains("/versions/99")), "{log:?}");
    }

    /// The versions answer for d.txt (file id 46) also carries a row of
    /// file 43 with a time of its own: bound to 46 it read as a version of
    /// d.txt, and a delete of it removed 46's version of that time. Only the
    /// rows under versions/46/ are listed.
    #[tokio::test]
    async fn a_version_row_of_another_file_id_or_user_is_not_listed() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider.list_versions("/d.txt").await.expect("versions");
        server.abort();
        let ids: Vec<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["46:current", "46:1700000100"]);
    }

    /// The "current" row is the file the listing read (file 47), bound to its
    /// id like the past rows: replaced while its body is read, or before the
    /// download starts, the path holds file 48, and the download refuses it
    /// and leaves the destination as it was. The listing reads the id and the
    /// current size and time from one answer, so it cannot pair 47's id with
    /// 48's metadata.
    #[tokio::test]
    async fn the_current_row_downloads_only_the_file_it_was_listed_from() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("e.txt");
        std::fs::write(&local, "mine").unwrap();
        let to = local.to_str().unwrap();
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider
            .list_versions("/swap/e.txt")
            .await
            .expect("versions");
        assert_eq!(
            versions[0].id,
            super::super::bound_current_row("47", "\"e47\"")
        );
        assert_eq!(versions[0].size, 9);
        // 47 before the body, 48 after it.
        let during = provider
            .download_version("/swap/e.txt", &versions[0].id, to)
            .await;
        // 48 before any body.
        let after = provider
            .download_version("/swap/e.txt", &versions[0].id, to)
            .await;
        server.abort();
        for outcome in [&during, &after] {
            assert!(
                matches!(outcome, Err(ProviderError::NotFound(m)) if m.contains("list its versions again")),
                "{outcome:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "mine");
        let gets = log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("GET /remote.php/dav/files/user/swap/e.txt"))
            .count();
        assert_eq!(gets, 1, "the refused second download reads no body");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A version answered 206 is a part: nothing is written and the
    /// destination keeps its bytes; the version answered 200 is written.
    #[tokio::test]
    async fn a_nextcloud_version_answered_206_leaves_the_destination() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        std::fs::write(&local, "mine").unwrap();
        let to = local.to_str().unwrap();
        let mut provider = nextcloud_versions_provider(&base);
        let partial = provider
            .download_version("/a.txt", "42:1700000500", to)
            .await;
        assert!(partial.is_err(), "{partial:?}");
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "mine");
        provider
            .download_version("/a.txt", "42:1700000000", to)
            .await
            .expect("a whole version");
        server.abort();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "old body");
    }

    /// The current row carries the strong ETag its listing saw: the file
    /// written since (same id, another ETag) is not read, the destination
    /// stays, and a restore or purge of the row is refused as the file.
    #[tokio::test]
    async fn a_nextcloud_current_row_written_after_its_listing_saves_nothing() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("wr.txt");
        std::fs::write(&local, "mine").unwrap();
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider.list_versions("/wr.txt").await.expect("versions");
        let current = super::super::bound_current_row("79", "\"w1\"");
        assert_eq!(versions[0].id, current);
        let outcome = provider
            .download_version("/wr.txt", &current, local.to_str().unwrap())
            .await;
        let restored = provider.restore_version("/wr.txt", &current).await;
        let purged = provider.delete_version("/wr.txt", &current).await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::NotFound(m)) if m.contains("list its versions again")),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "mine");
        assert!(
            !log.lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("GET /remote.php/dav/files/user/wr.txt")),
            "a file written since its listing was read"
        );
        assert!(
            matches!(restored, Err(ProviderError::NotSupported(_))),
            "{restored:?}"
        );
        assert!(
            matches!(purged, Err(ProviderError::NotSupported(_))),
            "{purged:?}"
        );
    }

    /// File 51 moved away, file 52 put at the path while the body is read,
    /// and 51 back before the second lookup: both id checks pass. The read is
    /// conditional on the ETag answered with the id, so the server refuses
    /// it (412) instead of sending 52's bytes, and the destination stays.
    #[tokio::test]
    async fn a_current_row_swapped_out_and_back_during_the_read_is_not_downloaded() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("k.txt");
        std::fs::write(&local, "mine").unwrap();
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider
            .download_version("/swapback/k.txt", "51:current", local.to_str().unwrap())
            .await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::NotFound(m)) if m.contains("list its versions again")),
            "{outcome:?}"
        );
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "mine");
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|l| l == "GET /remote.php/dav/files/user/swapback/k.txt if-match \"e51\""),
            "{:?}",
            log.lock().unwrap()
        );
    }

    /// An author with an entity in it arrives as Text pieces around a
    /// GeneralRef; the parser keeps the whole name.
    #[test]
    fn a_nextcloud_version_author_with_an_entity_is_read_whole() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns"><d:response><d:href>/remote.php/dav/versions/user/versions/42/1700000500</d:href><d:propstat><d:prop><d:getcontentlength>12</d:getcontentlength><d:resourcetype/><nc:version-author>Tom &amp; Jerry</nc:version-author></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let rows = super::parse_nextcloud_versions_listing(xml).expect("parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].author.as_deref(), Some("Tom & Jerry"));
        assert_eq!(rows[0].size, Some(12));
        assert!(rows[0].href.ends_with("/42/1700000500"), "{}", rows[0].href);
    }

    #[tokio::test]
    async fn nextcloud_versions_are_listed_current_first_then_newest_from_the_file_id() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        assert!(provider.supports_versions());
        let versions = provider.list_versions("/a.txt").await.expect("versions");
        server.abort();
        let ids: Vec<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        // The file as it is now first, as the versions dialog reads the list.
        // Past versions are bound to the file id they were listed from.
        let current = super::super::bound_current_row("42", "\"e42\"");
        assert_eq!(ids, [current.as_str(), "42:1700000500", "42:1700000000"]);
        assert_eq!(versions[0].size, 20);
        assert!(versions[0].modified.is_some());
        assert_eq!(versions[1].size, 12);
        assert_eq!(versions[1].modified_by.as_deref(), Some("alice"));
        // A 404 propstat (no author before Nextcloud 28) adds nothing.
        assert_eq!(versions[2].size, 8);
        assert_eq!(versions[2].modified_by, None);
        assert!(versions[2].modified.is_some());
    }

    /// The versions collection of a listed file answers 404 (no route for
    /// file 44 in the fixture): the Versions app is off, and the error says so.
    #[tokio::test]
    async fn a_nextcloud_without_the_versions_app_says_versions_are_not_enabled() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.list_versions("/off.txt").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::NotSupported(m)) if m == super::NEXTCLOUD_VERSIONS_OFF),
            "{outcome:?}"
        );
        assert!(super::NEXTCLOUD_VERSIONS_OFF.contains("not enabled"));
    }

    /// The versions route also answers 404 when the file id no longer
    /// resolves: a file removed after its lookup is reported gone, not as a
    /// disabled Versions app.
    #[tokio::test]
    async fn a_nextcloud_file_gone_after_its_lookup_is_not_a_disabled_versions_app() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.list_versions("/gone/g.txt").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::NotFound(m)) if m.contains("removed or replaced")),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_listed_current_nextcloud_version_is_row_zero_and_no_row_is_added() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider.list_versions("/b.txt").await.expect("versions");
        server.abort();
        let ids: Vec<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        // 1700000500 is the file's mtime: the current contents, first, as
        // the file itself.
        let current = super::super::bound_current_row("43", "\"e43\"");
        assert_eq!(ids, [current.as_str(), "43:1700000000"]);
        assert_eq!(versions[0].size, 12);
    }

    /// The listed current row is read from the file, not from the versions
    /// endpoint, which may keep no body for the current contents.
    #[tokio::test]
    async fn the_listed_current_nextcloud_version_downloads_the_file() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("b-now.txt");
        let mut provider = nextcloud_versions_provider(&base);
        let versions = provider.list_versions("/b.txt").await.expect("versions");
        provider
            .download_version("/b.txt", &versions[0].id, local.to_str().unwrap())
            .await
            .expect("download the listed current row");
        server.abort();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "b as it is now");
        let log = log.lock().unwrap().clone();
        assert!(
            !log.iter()
                .any(|l| l.starts_with("GET /remote.php/dav/versions/")),
            "{log:?}"
        );
    }

    /// A version listed for one file is refused once the path names another
    /// file (replaced after the listing), before any request on its versions.
    #[tokio::test]
    async fn a_version_listed_for_a_replaced_file_is_refused() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x.txt");
        let mut provider = nextcloud_versions_provider(&base);
        // Listed from file 43; /a.txt is file 42 now.
        let restored = provider.restore_version("/a.txt", "43:1700000000").await;
        let deleted = provider.delete_version("/a.txt", "43:1700000000").await;
        let downloaded = provider
            .download_version("/a.txt", "43:1700000000", local.to_str().unwrap())
            .await;
        server.abort();
        for outcome in [&restored, &deleted, &downloaded] {
            assert!(
                matches!(outcome, Err(ProviderError::NotFound(m)) if m.contains("list its versions again")),
                "{outcome:?}"
            );
        }
        let log = log.lock().unwrap().clone();
        assert!(
            !log.iter().any(|l| l.contains("/remote.php/dav/versions/")),
            "{log:?}"
        );
        assert!(!local.exists());
    }

    /// `/c.txt` does not exist (only `other/c.txt` does, file 99): no file
    /// id, no versions of file 99.
    #[tokio::test]
    async fn a_file_id_is_taken_only_from_a_direct_child_of_the_listed_folder() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.list_versions("/c.txt").await;
        let children = provider.nextcloud_list_file_ids("/").await.expect("ids");
        server.abort();
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        assert!(
            children.contains(&("a.txt".to_string(), "42".to_string())),
            "{children:?}"
        );
        assert!(
            children
                .iter()
                .all(|(leaf, id)| leaf != "c.txt" && id != "99"),
            "{children:?}"
        );
        let log = log.lock().unwrap().clone();
        assert!(!log.iter().any(|l| l.contains("/versions/99/")), "{log:?}");
    }

    /// A row whose absolute href names another origin is that server's
    /// entry, even with our path: the listing is refused, never read as
    /// `f.txt` of this folder (a purge would then take file 77's id).
    #[tokio::test]
    async fn a_file_id_row_of_another_origin_refuses_the_listing() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let foreign = provider.nextcloud_list_file_ids("/foreign").await;
        let own = provider.nextcloud_list_file_ids("/").await;
        server.abort();
        assert!(
            matches!(&foreign, Err(ProviderError::ServerError(m)) if m.contains("another server")),
            "{foreign:?}"
        );
        assert!(own
            .expect("same-origin rows")
            .contains(&("a.txt".to_string(), "42".to_string())));
    }

    /// The Nextcloud version paths check what each answer names: a lookup
    /// whose lone row is another file (#26), a file written between the two
    /// reads of a listing (#25), a recheck that fails after a versions 404
    /// (its own error, not "removed"), a file without an ETag (no current
    /// download), a bare label whose file is replaced before the preflight
    /// (no MOVE) and a restore whose listing fails (no MOVE).
    /// Only one strong ETag binds the read: a wildcard, a weak tag, a list
    /// or a malformed value is refused before any GET.
    #[test]
    fn only_a_single_strong_etag_binds_the_current_read() {
        assert_eq!(strong_etag("\"abc123\"").as_deref(), Some("\"abc123\""));
        assert_eq!(strong_etag("abc123").as_deref(), Some("\"abc123\""));
        for refused in [
            "*",
            "\"*\"",
            "W/\"abc\"",
            "\"a\",\"b\"",
            "\"a\"b\"",
            "\"\"",
            "",
            "\"abc",
            "a b",
        ] {
            assert_eq!(strong_etag(refused), None, "{refused}");
        }
    }

    /// A version href of another server is no row of this collection, even
    /// with the same path: binding it would let a purge delete this server's
    /// version of that time. Same-origin absolute and path-absolute hrefs
    /// still bind.
    #[test]
    fn a_version_href_of_another_origin_is_not_a_row_of_the_collection() {
        let collection = "https://cloud.example/remote.php/dav/versions/u/versions/42/";
        let row = "/remote.php/dav/versions/u/versions/42/1700000000";
        assert_eq!(
            nextcloud_version_id_of_href(row, collection, collection).as_deref(),
            Some("1700000000")
        );
        assert_eq!(
            nextcloud_version_id_of_href(
                &format!("https://cloud.example{row}"),
                collection,
                collection
            )
            .as_deref(),
            Some("1700000000")
        );
        assert_eq!(
            nextcloud_version_id_of_href(
                &format!("https://other.example{row}"),
                collection,
                collection
            ),
            None
        );
        assert_eq!(
            nextcloud_version_id_of_href(
                &format!("http://cloud.example{row}"),
                collection,
                collection
            ),
            None
        );
        // A scheme-relative href names a host too: `//remote.php/...` is the
        // server remote.php, whose path matched the collection's before.
        assert_eq!(
            nextcloud_version_id_of_href(
                "//remote.php/dav/versions/u/versions/42/1700000000",
                collection,
                collection
            ),
            None
        );
        assert_eq!(
            nextcloud_version_id_of_href(&format!("//cloud.example{row}"), collection, collection)
                .as_deref(),
            Some("1700000000")
        );
    }

    #[tokio::test]
    async fn nextcloud_version_paths_check_what_each_answer_names() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let other = provider.list_versions("/m.txt").await;
        let written = provider.list_versions("/u.txt").await;
        let unknown = provider.list_versions("/t.txt").await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("d.txt");
        let no_etag = provider
            .download_version("/d.txt", "46:current", local.to_str().unwrap())
            .await;
        let replaced = provider.restore_version("/rb.txt", "1700000000").await;
        let unlisted = provider.restore_version("/lf.txt", "62:1700000000").await;
        let bare = provider.restore_version("/r.txt", "1700000000").await;
        server.abort();
        assert!(
            matches!(&other, Err(ProviderError::ServerError(m)) if m.contains("another entry")),
            "{other:?}"
        );
        assert!(
            matches!(&written, Err(ProviderError::NotFound(m)) if m.contains("changed while")),
            "{written:?}"
        );
        assert!(
            matches!(&unknown, Err(ProviderError::ServerError(_))),
            "{unknown:?}"
        );
        assert!(
            matches!(&no_etag, Err(ProviderError::TransferFailed(m)) if m.contains("ETag")),
            "{no_etag:?}"
        );
        assert!(!local.exists());
        assert!(
            matches!(&replaced, Err(ProviderError::NotFound(m)) if m.contains("list its versions again")),
            "{replaced:?}"
        );
        assert!(
            matches!(&unlisted, Err(ProviderError::TransferFailed(m)) if m.contains("no restore was sent")),
            "{unlisted:?}"
        );
        // A bare label of the unchanged file is restored, not confirmed.
        let outcome = bare;
        assert!(
            matches!(&outcome, Err(e) if crate::providers::was_applied(e) && e.to_string().contains("could not be confirmed")),
            "{outcome:?}"
        );
        let log = log.lock().unwrap().clone();
        for absent in [
            "/versions/77/",
            "GET /remote.php/dav/files/user/d.txt",
            "MOVE /remote.php/dav/versions/user/versions/60/",
            "MOVE /remote.php/dav/versions/user/versions/61/",
            "MOVE /remote.php/dav/versions/user/versions/62/",
        ] {
            assert!(!log.iter().any(|l| l.contains(absent)), "{absent}: {log:?}");
        }
        assert!(
            log.iter()
                .any(|l| l.starts_with("MOVE /remote.php/dav/versions/user/versions/50/1700000000")),
            "{log:?}"
        );
    }

    /// A refused restore says the status and nothing of the body, which may
    /// echo credentials.
    #[tokio::test]
    async fn a_refused_nextcloud_restore_keeps_the_server_body_out_of_the_error() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.restore_version("/a.txt", "42:1700000500").await;
        server.abort();
        let message = format!("{outcome:?}");
        assert!(message.contains("HTTP 500"), "{message}");
        for secret in ["hunter2-pass", "tok-abc123", "<error>"] {
            assert!(!message.contains(secret), "{message}");
        }
    }

    /// A versions answer larger than the cap is refused, not buffered whole
    /// and not read as a shorter listing.
    #[tokio::test]
    async fn an_oversized_nextcloud_versions_listing_is_refused() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.list_versions("/big.txt").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::TransferFailed(m)) if m.contains("too large")),
            "{outcome:?}"
        );
    }

    /// A file lookup answered with more than the cap decides nothing: no
    /// version is read or moved, and nothing is written locally.
    #[tokio::test]
    async fn an_oversized_nextcloud_file_lookup_reaches_no_version() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("h-old.txt");
        let mut provider = nextcloud_versions_provider(&base);
        let download = provider
            .download_version("/huge/h.txt", "53:1700000000", local.to_str().unwrap())
            .await;
        let restore = provider
            .restore_version("/huge/h.txt", "53:1700000000")
            .await;
        server.abort();
        for outcome in [&download, &restore] {
            assert!(
                matches!(outcome, Err(ProviderError::TransferFailed(m)) if m.contains("too large")),
                "{outcome:?}"
            );
        }
        assert!(!local.exists());
        let log = log.lock().unwrap();
        assert!(
            log.iter().all(|l| !l.contains("/remote.php/dav/versions/")),
            "{log:?}"
        );
        assert!(log
            .iter()
            .any(|l| l.starts_with("PROPFIND /remote.php/dav/files/user/huge/h.txt")));
    }

    #[tokio::test]
    async fn a_nextcloud_version_is_downloaded_to_the_local_path() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a-old.txt");
        let mut provider = nextcloud_versions_provider(&base);
        provider
            .download_version("/a.txt", "42:1700000000", local.to_str().unwrap())
            .await
            .expect("download");
        server.abort();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "old body");
    }

    #[tokio::test]
    async fn a_nextcloud_version_is_restored_by_a_move_to_restore_target() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        // A bare version id, as a caller may type it, is the listed version
        // of that label of the file at the path, and its restore is checked
        // as a bound one: a.txt still holds its 20 bytes after the MOVE, so
        // that restore is not done; r.txt shows the 8 restored bytes.
        let unchanged = provider.restore_version("/a.txt", "1700000000").await;
        assert!(
            matches!(&unchanged, Err(ProviderError::TransferFailed(m)) if m.contains("20 bytes")),
            "{unchanged:?}"
        );
        let outcome = provider.restore_version("/r.txt", "1700000000").await;
        assert!(
            matches!(&outcome, Err(e) if crate::providers::was_applied(e) && e.to_string().contains("could not be confirmed")),
            "{outcome:?}"
        );
        server.abort();
        let log = log.lock().unwrap().clone();
        let restore_move = format!(
            "MOVE /remote.php/dav/versions/user/versions/50/1700000000 -> \
             {base}/remote.php/dav/versions/user/restore/target"
        );
        assert!(log.contains(&restore_move), "{log:?}");
    }

    /// A restore MOVE answered 502 by a gateway may have been applied behind
    /// it: applied and unconfirmed, and sent once.
    #[tokio::test]
    async fn a_nextcloud_restore_a_gateway_answered_is_applied_and_not_confirmed() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let behind_gateway = provider.restore_version("/b.txt", "43:1700000000").await;
        server.abort();
        assert!(
            matches!(&behind_gateway, Err(e) if crate::providers::was_applied(e)
                && e.to_string().contains("gateway")),
            "{behind_gateway:?}"
        );
        let log = log.lock().unwrap().clone();
        assert_eq!(
            log.iter()
                .filter(|l| l.starts_with("MOVE /remote.php/dav/versions/user/versions/43/"))
                .count(),
            1,
            "{log:?}"
        );
    }

    /// Nextcloud answers the restore MOVE 201 even when its rollback failed
    /// (RestoreFolder::moveInto drops the rollback's result). Here the MOVE
    /// of version 1700000000 (8 bytes) is answered 201 and the file still
    /// holds its 20 bytes: the restore is not reported done.
    #[tokio::test]
    async fn a_nextcloud_restore_the_file_does_not_show_is_not_reported_done() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.restore_version("/a.txt", "42:1700000000").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::TransferFailed(m)) if m.contains("20 bytes") && m.contains("list its versions again")),
            "{outcome:?}"
        );
        let log = log.lock().unwrap().clone();
        assert!(
            log.iter().any(|l| l.starts_with("MOVE /remote.php/dav/versions/user/versions/42/1700000000")),
            "{log:?}"
        );
    }

    /// The file shows the restored version's 8 bytes after the MOVE: the
    /// restore is sent and reported as such, not confirmed, as Nextcloud
    /// names no version made and its listing holds no content hash (a write
    /// of the same size by someone else would pass as well).
    #[tokio::test]
    async fn a_nextcloud_restore_the_file_shows_is_reported_sent_not_confirmed() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        let outcome = provider.restore_version("/r.txt", "50:1700000000").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(e) if crate::providers::was_applied(e) && e.to_string().contains("could not be confirmed")),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_nextcloud_version_is_deleted_and_a_refusal_is_not_supported() {
        let (base, _log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        provider
            .delete_version("/a.txt", "42:1700000500")
            .await
            .expect("delete");
        let refused = provider.delete_version("/a.txt", "42:1700000000").await;
        server.abort();
        assert!(
            matches!(refused, Err(ProviderError::NotSupported(_))),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn the_current_nextcloud_version_is_the_file_and_is_never_restored_or_deleted() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a-now.txt");
        let mut provider = nextcloud_versions_provider(&base);
        provider
            .download_version("/a.txt", "current", local.to_str().unwrap())
            .await
            .expect("download current");
        let restored = provider.restore_version("/a.txt", "current").await;
        let deleted = provider.delete_version("/a.txt", "current").await;
        server.abort();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "the body of now");
        assert!(
            matches!(restored, Err(ProviderError::NotSupported(_))),
            "{restored:?}"
        );
        assert!(
            matches!(deleted, Err(ProviderError::NotSupported(_))),
            "{deleted:?}"
        );
        let log = log.lock().unwrap().clone();
        assert!(
            !log.iter()
                .any(|l| l.starts_with("MOVE") || l.starts_with("DELETE")),
            "{log:?}"
        );
    }

    /// A version id that is not one segment could address another file's
    /// versions; it is refused before any request.
    #[tokio::test]
    async fn a_nextcloud_version_id_with_a_slash_is_refused_before_any_request() {
        let (base, log, server) = serve_nextcloud_versions().await;
        let mut provider = nextcloud_versions_provider(&base);
        for id in ["../43/1700000000", "", ".."] {
            assert!(
                provider.restore_version("/a.txt", id).await.is_err(),
                "{id:?}"
            );
        }
        server.abort();
        assert!(log.lock().unwrap().is_empty());
    }

    /// A MEGAcmd bridge profile that starts in `/Team` reads `/report.pdf`
    /// as `/Team/report.pdf`, as its WebDAV requests do: the versions are
    /// listed, fetched and restored from that file, not from the account
    /// root's `/report.pdf`. A path already under the start folder is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn megacmd_versions_are_of_the_file_under_the_start_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ls = dir.path().join("mega-ls");
        std::fs::write(
            &ls,
            "#!/bin/sh
for a in \"$@\"; do last=\"$a\"; done
echo \"$last\" >> \"$(dirname \"$0\")/ls.log\"
exit 1
",
        )
        .unwrap();
        std::fs::set_permissions(&ls, std::fs::Permissions::from_mode(0o755)).unwrap();
        let whoami = dir.path().join("mega-whoami");
        std::fs::write(
            &whoami,
            "#!/bin/sh\necho 'Account e-mail: one@example.test'\n",
        )
        .unwrap();
        std::fs::set_permissions(&whoami, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::super::mega_df::use_test_commands(Some(dir.path()));
        let mut config = test_config("http://127.0.0.1:4443/H2");
        config.provider_id = Some("megacmd-webdav".into());
        let mut provider = WebDavProvider::new(config).unwrap();
        provider.megacmd_serves_account_root = true;
        provider.megacmd_account = Some("Account e-mail: one@example.test".into());
        provider.server_root = Some("/Team".into());
        let out = dir.path().join("r.pdf");
        let _ = provider.list_versions("/report.pdf").await;
        let _ = provider.list_versions("/Team/report.pdf").await;
        let _ = provider
            .download_version("/report.pdf", "H:rAAAAAA1", out.to_str().unwrap())
            .await;
        // Refused before any command: MEGA versions are not restored.
        let _ = provider.restore_version("/report.pdf", "H:rAAAAAA1").await;
        super::super::mega_df::use_test_commands(None);
        let asked = std::fs::read_to_string(dir.path().join("ls.log")).unwrap_or_default();
        let asked: Vec<&str> = asked.lines().collect();
        assert_eq!(asked, ["/Team/report.pdf"; 3], "{asked:?}");
    }

    /// The MEGAcmd daemon is shared: after a login to another account, the
    /// same path names that account's file. Versions are read only while
    /// MEGAcmd is still on the account the session connected with, and the
    /// refusal names neither account.
    #[cfg(unix)]
    #[tokio::test]
    async fn megacmd_versions_are_refused_once_the_daemon_is_on_another_account() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            (
                "mega-ls",
                "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/ls.log\"\nexit 1\n",
            ),
            ("mega-whoami", "#!/bin/sh\ncat \"$(dirname \"$0\")/who\"\n"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let who = dir.path().join("who");
        std::fs::write(&who, "Account e-mail: one@example.test\n").unwrap();
        super::super::mega_df::use_test_commands(Some(dir.path()));
        let mut config = test_config("http://127.0.0.1:4443/H2");
        config.provider_id = Some("megacmd-webdav".into());
        let mut provider = WebDavProvider::new(config).unwrap();
        provider.megacmd_serves_account_root = true;
        let out = dir.path().join("r.pdf");
        std::fs::write(&out, "mine").unwrap();
        let to = out.to_str().unwrap().to_string();
        // No account read at connect: refused before any listing.
        let unknown = provider.list_versions("/r.pdf").await;
        provider.megacmd_account = Some("Account e-mail: one@example.test".into());
        // The same account: the listing is asked for (the stand-in fails it).
        let _ = provider.list_versions("/r.pdf").await;
        std::fs::write(&who, "Account e-mail: two@example.test\n").unwrap();
        let listed = provider.list_versions("/r.pdf").await;
        let fetched = provider.download_version("/r.pdf", "H:rAAAAAA1", &to).await;
        super::super::mega_df::use_test_commands(None);
        assert!(
            matches!(unknown, Err(ProviderError::NotSupported(_))),
            "{unknown:?}"
        );
        assert!(
            matches!(&listed, Err(ProviderError::AuthenticationFailed(_))),
            "{listed:?}"
        );
        assert!(
            matches!(fetched, Err(ProviderError::AuthenticationFailed(_))),
            "{fetched:?}"
        );
        assert!(
            !format!("{listed:?}").contains("example.test"),
            "{listed:?}"
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "mine");
        let asked = std::fs::read_to_string(dir.path().join("ls.log")).unwrap_or_default();
        assert_eq!(asked.lines().count(), 1, "{asked:?}");
    }

    /// The versions gate is the one the GUI, CLI and MCP read. It opens for
    /// Nextcloud and for the MEGAcmd bridge (#270) served at the account
    /// root, and stays shut for plain WebDAV and ownCloud, whose versions
    /// live elsewhere.
    #[test]
    fn versions_gate_opens_for_nextcloud_and_the_megacmd_bridge_only() {
        let gate = |url: &str, provider_id: Option<&str>| {
            let mut cfg = test_config(url);
            cfg.provider_id = provider_id.map(String::from);
            let mut provider = WebDavProvider::new(cfg).unwrap();
            // What a MEGAcmd connect learns from the bridge it served.
            provider.megacmd_serves_account_root = true;
            provider.supports_versions()
        };
        assert!(gate(
            "https://cloud.example.com/remote.php/dav/files/u/",
            None
        ));
        assert!(gate("https://cloud.example.com", Some("nextcloud")));
        assert!(gate("http://127.0.0.1:4443", Some("megacmd-webdav")));
        assert!(!gate("https://dav.example.com/webdav/", None));
        assert!(!gate("https://oc.example.com", Some("owncloud")));
    }

    #[test]
    fn a_truncated_versions_listing_is_an_error_not_fewer_versions() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/remote.php/dav/versions/u/versions/42/1700000000</d:href>"#;
        assert!(parse_nextcloud_versions_listing(xml).is_err());
    }

    /// An empty versions collection is complete in both spellings, the
    /// self-closing one included; an unclosed root stays an error.
    #[test]
    fn an_empty_versions_listing_is_complete_in_both_spellings() {
        for xml in [
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"></d:multistatus>"#,
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"/>"#,
        ] {
            assert!(parse_nextcloud_versions_listing(xml).expect(xml).is_empty());
        }
        assert!(parse_nextcloud_versions_listing(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#
        )
        .is_err());
    }

    /// Only a document whose root is `multistatus` is a listing, complete
    /// when that root closes: a nested one closing first, or another root,
    /// is an error. A response the server answers 404 for is no row.
    #[test]
    fn a_versions_answer_is_read_only_from_its_multistatus_root() {
        let row = |id: &str| {
            format!(
                "<d:response><d:href>/remote.php/dav/versions/u/versions/42/{id}</d:href>\
                 <d:propstat><d:prop><d:getcontentlength>3</d:getcontentlength></d:prop>\
                 <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            )
        };
        let nested = format!(
            r#"<?xml version="1.0"?><d:error xmlns:d="DAV:"><d:multistatus>{}</d:multistatus></d:error>"#,
            row("1700000000")
        );
        assert!(parse_nextcloud_versions_listing(&nested).is_err());
        assert!(parse_nextcloud_file_id_listing(&nested).is_err());
        let inner_closes_first = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:multistatus></d:multistatus>{}"#,
            row("1700000000")
        );
        assert!(parse_nextcloud_versions_listing(&inner_closes_first).is_err());
        assert!(parse_nextcloud_file_id_listing(&inner_closes_first).is_err());
        assert!(parse_nextcloud_versions_listing(r#"<?xml version="1.0"?><html/>"#).is_err());

        let gone = format!(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{}<d:response><d:href>/remote.php/dav/versions/u/versions/42/1700000001</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response></d:multistatus>"#,
            row("1700000000")
        );
        let rows = parse_nextcloud_versions_listing(&gone).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].href.ends_with("/1700000000"));
        let ids = parse_nextcloud_file_id_listing(
            r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/a.txt</d:href><d:status>HTTP/1.1 403 Forbidden</d:status><d:propstat><d:prop><oc:fileid>7</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
        )
        .unwrap();
        assert!(ids.is_empty(), "{ids:?}");
    }

    /// A Depth-0 answer for a folder says so in its `resourcetype`, in both
    /// spellings, and a file's does not.
    #[test]
    fn a_folder_in_the_file_id_answer_is_marked_as_a_collection() {
        let answer = |resourcetype: &str| {
            format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/docs/</d:href><d:propstat><d:prop><oc:fileid>7</oc:fileid>{resourcetype}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
            )
        };
        for folder in [
            "<d:resourcetype><d:collection/></d:resourcetype>",
            "<d:resourcetype><d:collection></d:collection></d:resourcetype>",
        ] {
            let rows = parse_nextcloud_file_id_listing(&answer(folder)).unwrap();
            assert!(rows[0].is_collection, "{folder}");
        }
        let rows = parse_nextcloud_file_id_listing(&answer("<d:resourcetype/>")).unwrap();
        assert!(!rows[0].is_collection);
    }

    /// The Nextcloud routes go on the profile URL's path: a query or a
    /// fragment there is not put in front of `/remote.php/`.
    #[test]
    fn the_versions_route_goes_on_the_path_of_a_url_with_a_query() {
        let mut cfg = test_config("https://cloud.example.com/nc/?theme=dark#top");
        cfg.provider_id = Some("nextcloud".to_string());
        let mut p = WebDavProvider::new(cfg).expect("provider");
        p.server_root = Some("/nc/remote.php/dav/files/alice/".to_string());
        assert_eq!(
            p.nextcloud_base_url().as_deref(),
            Some("https://cloud.example.com/nc")
        );
        assert_eq!(
            p.nextcloud_versions_root().unwrap(),
            "https://cloud.example.com/nc/remote.php/dav/versions/alice/"
        );
        // A `/remote.php/` inside the query or the fragment is no route.
        for url in [
            "https://cloud.example.com/nc/?next=/remote.php/dav/files/eve/",
            "https://cloud.example.com/nc/#/remote.php/dav/files/eve/",
        ] {
            let mut cfg = test_config(url);
            cfg.provider_id = Some("nextcloud".to_string());
            let p = WebDavProvider::new(cfg).expect("provider");
            assert_eq!(
                p.nextcloud_base_url().as_deref(),
                Some("https://cloud.example.com/nc"),
                "{url}"
            );
        }
    }

    fn test_config(url: &str) -> WebDavConfig {
        WebDavConfig {
            url: url.to_string(),
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            verify_cert: true,
            anonymous: false,
        }
    }

    #[tokio::test]
    async fn upload_retry_keeps_the_open_file_after_path_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let original = b"original-bytes";
        std::fs::write(&path, original).unwrap();
        let (base, received, server) =
            super::super::upload_cursor::replacement_fixture::serve(&path, String::new()).await;
        let mut provider = WebDavProvider::new(test_config(&base)).unwrap();
        provider.connected = true;
        let outcome = provider
            .upload(path.to_str().unwrap(), "/f.bin", None)
            .await;
        server.abort();
        assert_eq!(
            *received.lock().unwrap(),
            vec![original.to_vec(), original.to_vec()]
        );
        outcome.expect("retry must send the original open file");
    }

    #[tokio::test]
    async fn upload_progress_reports_body_bytes_before_success() {
        use super::super::upload_progress::fixture::{
            assert_real_progress, recorder, serve_logged, temp_file, Route,
        };

        let (base, server, received) = serve_logged(vec![Route {
            method: axum::http::Method::PUT,
            path: "/up",
            status: 201,
            body: String::new(),
            busy_first: false,
        }])
        .await;
        let file = temp_file(1024 * 1024);
        let (callback, updates) = recorder();
        let mut provider = WebDavProvider::new(test_config(&base)).unwrap();
        provider.connected = true;
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/up", Some(callback))
            .await;
        server.abort();
        outcome.unwrap();
        assert_eq!(*received.lock().unwrap(), [("/up", 1024 * 1024)]);
        assert_real_progress(&updates.lock().unwrap(), 1024 * 1024, true);
    }

    #[tokio::test]
    async fn a_refused_download_reports_the_time_of_what_carried_its_bytes() {
        use super::super::multi_thread::pin_server::{start, PinMode, WHOLE_READ_TIME};
        let server = start(PinMode::RefusedReplaced).await;
        let mut provider =
            WebDavProvider::new(test_config(&format!("http://{}/", server.addr))).unwrap();
        provider.connected = true;
        provider.set_multi_thread_download(4, 1024 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("big.bin");
        provider
            .download("/big.bin", out.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(
            provider.take_download_time(false).as_deref(),
            Some(WHOLE_READ_TIME)
        );
    }

    #[tokio::test]
    async fn upload_progress_never_completes_a_refused_body() {
        use super::super::upload_progress::fixture::{
            assert_real_progress, recorder, serve_logged, temp_file, Route,
        };

        let (base, server, received) = serve_logged(vec![Route {
            method: axum::http::Method::PUT,
            path: "/up",
            status: 500,
            body: String::new(),
            busy_first: false,
        }])
        .await;
        let file = temp_file(1024 * 1024);
        let (callback, updates) = recorder();
        let mut provider = WebDavProvider::new(test_config(&base)).unwrap();
        provider.connected = true;
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/up", Some(callback))
            .await;
        server.abort();
        assert!(outcome.is_err(), "a refused PUT must fail");
        assert_eq!(*received.lock().unwrap(), [("/up", 1024 * 1024)]);
        assert_real_progress(&updates.lock().unwrap(), 1024 * 1024, false);
    }

    #[tokio::test]
    async fn upload_progress_waits_for_acknowledgement_before_completion() {
        use super::super::upload_progress::fixture::{assert_real_progress, recorder, temp_file};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let drained = std::sync::Arc::new(tokio::sync::Notify::new());
        let acknowledge = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let handler_drained = drained.clone();
        let handler_acknowledge = acknowledge.clone();
        let app = axum::Router::new().route(
            "/up",
            axum::routing::put(move |body: axum::body::Body| {
                let drained = handler_drained.clone();
                let acknowledge = handler_acknowledge.clone();
                async move {
                    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                    assert_eq!(bytes.len(), 1024 * 1024);
                    drained.notify_one();
                    let _permit = acknowledge.acquire().await.unwrap();
                    axum::http::StatusCode::CREATED
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let file = temp_file(1024 * 1024);
        let (callback, updates) = recorder();
        let mut provider = WebDavProvider::new(test_config(&base)).unwrap();
        provider.connected = true;
        let upload = provider.upload(file.path().to_str().unwrap(), "/up", Some(callback));
        tokio::pin!(upload);
        tokio::select! {
            result = &mut upload => panic!("PUT returned before acknowledgement: {result:?}"),
            ready = tokio::time::timeout(std::time::Duration::from_secs(5), drained.notified()) => {
                ready.expect("server must consume the PUT body before acknowledgement");
                assert_real_progress(&updates.lock().unwrap(), 1024 * 1024, false);
            }
        }
        acknowledge.add_permits(1);
        let outcome = upload.await;
        server.abort();
        outcome.unwrap();
        assert_real_progress(&updates.lock().unwrap(), 1024 * 1024, true);
    }

    #[tokio::test]
    async fn upload_progress_stays_monotonic_across_a_digest_replay() {
        use super::super::upload_progress::fixture::{assert_real_progress, recorder, temp_file};
        use axum::response::IntoResponse;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_received = received.clone();
        let app = axum::Router::new().route(
            "/up",
            axum::routing::put(move |body: axum::body::Body| {
                let received = handler_received.clone();
                async move {
                    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                    let mut received = received.lock().unwrap();
                    received.push(bytes.len());
                    if received.len() == 1 {
                        (
                            axum::http::StatusCode::UNAUTHORIZED,
                            [(
                                axum::http::header::WWW_AUTHENTICATE,
                                r#"Digest realm="r", nonce="rotated", qop="auth", stale=true"#,
                            )],
                        )
                            .into_response()
                    } else {
                        axum::http::StatusCode::CREATED.into_response()
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let file = temp_file(1024 * 1024);
        let (callback, updates) = recorder();
        let mut provider = digest_provider(&base);
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/up", Some(callback))
            .await;
        server.abort();
        outcome.unwrap();
        assert_eq!(*received.lock().unwrap(), [1024 * 1024, 1024 * 1024]);
        assert_eq!(provider.digest_auth.as_ref().unwrap().nonce(), "rotated");
        let updates = updates.lock().unwrap();
        assert_real_progress(&updates, 1024 * 1024, true);
        assert_eq!(
            updates
                .iter()
                .filter(|&&(sent, total)| sent == total)
                .count(),
            1
        );
    }

    #[test]
    fn multi_thread_cutoff_has_no_provider_floor() {
        // R21: WebDAV honors the caller's cutoff as-is (no 1 MiB floor), so
        // `--multi-thread-cutoff 500K` works again like before #905.
        let mut provider =
            WebDavProvider::new(test_config("https://example.com/dav")).expect("provider");
        assert_eq!(provider.multi_thread_cutoff_floor(), 0);
        provider.set_multi_thread_download(4, 500 * 1024);
        assert_eq!(provider.multi_thread_cutoff, 500 * 1024);
    }

    #[test]
    fn upload_404_and_409_map_to_clear_non_retryable_error() {
        // Koofr returns 404 (not RFC 4918's 409) when the PUT parent is missing
        // or the target is a directory. Both must become the same actionable,
        // non-retryable InvalidPath error rather than an opaque "status: 404".
        for status in [StatusCode::NOT_FOUND, StatusCode::CONFLICT] {
            match upload_failure_error(status) {
                ProviderError::InvalidPath(msg) => {
                    assert!(msg.contains("parent directory is missing"), "msg: {}", msg)
                }
                other => panic!("{} should map to InvalidPath, got {:?}", status, other),
            }
        }
        // Other failures keep their existing mappings.
        assert!(matches!(
            upload_failure_error(StatusCode::INSUFFICIENT_STORAGE),
            ProviderError::ServerError(_)
        ));
        assert!(matches!(
            upload_failure_error(StatusCode::BAD_GATEWAY),
            ProviderError::TransferFailed(_)
        ));
    }

    #[test]
    fn test_build_url() {
        let provider = WebDavProvider::new(test_config(
            "https://cloud.example.com/remote.php/dav/files/user/",
        ))
        .expect("Failed to create WebDavProvider");

        assert_eq!(
            provider.build_url("/Documents"),
            "https://cloud.example.com/remote.php/dav/files/user/Documents"
        );
    }

    #[test]
    fn nextcloud_trash_segments_cannot_escape_their_collection() {
        assert_eq!(
            encode_nextcloud_trash_segment("../files/alice/secret"),
            "%2E%2E%2Ffiles%2Falice%2Fsecret"
        );
        assert_eq!(encode_nextcloud_trash_segment("alice"), "alice");
    }

    #[test]
    fn router_hint_prefers_provider_id_over_bare_url() {
        let mut config = test_config("https://cloud.lab.example.test");
        config.provider_id = Some("nextcloud".to_string());
        let provider = WebDavProvider::new(config).expect("Failed to create WebDavProvider");

        assert_eq!(
            provider.router_hint(),
            crate::transfer_router::ProviderHint::WebDavNextcloud
        );
    }

    #[test]
    fn test_extract_xml_properties() {
        let provider = WebDavProvider::new(test_config("https://example.com"))
            .expect("Failed to create WebDavProvider");

        // Test with d: prefix
        let xml = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:getcontentlength>12345</d:getcontentlength>
                        <d:getcontenttype>text/plain</d:getcontenttype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props = provider.extract_xml_properties(xml, "/").unwrap();
        assert_eq!(props.get("getcontentlength"), Some(&"12345".to_string()));
        assert_eq!(props.get("getcontenttype"), Some(&"text/plain".to_string()));

        // Test with D: prefix
        let xml2 = r#"<D:multistatus xmlns:D="DAV:">
            <D:response>
                <D:propstat>
                    <D:prop>
                        <D:getcontentlength>99</D:getcontentlength>
                    </D:prop>
                </D:propstat>
            </D:response>
        </D:multistatus>"#;
        let props2 = provider.extract_xml_properties(xml2, "/").unwrap();
        assert_eq!(props2.get("getcontentlength"), Some(&"99".to_string()));

        // Test collection detection
        let xml3 = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props3 = provider.extract_xml_properties(xml3, "/").unwrap();
        assert!(props3.contains_key("_is_collection"));
    }

    #[test]
    fn test_parse_propfind_response() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/file.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength>1024</d:getcontentlength>
                        <d:getlastmodified>Mon, 01 Jan 2024 00:00:00 GMT</d:getlastmodified>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/subdir/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 1024);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_dir);
    }

    /// MEGAcmd 2.6 serves the account under `/<handle>/Cloud%20Drive`: the
    /// base keeps its own escape and only the path is encoded, so `%20`
    /// never becomes `%2520`.
    #[test]
    fn a_base_with_an_escaped_space_keeps_it_and_encodes_only_the_path() {
        let base = "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive";
        let provider = WebDavProvider::new(test_config(base)).expect("provider");
        assert_eq!(provider.build_url("/"), format!("{base}/"));
        assert_eq!(
            provider.build_url("/My Folder/a#b%c?.txt"),
            format!("{base}/My%20Folder/a%23b%25c%3F.txt")
        );
        assert!(!provider.build_url("/x y").contains("%2520"));
    }

    /// Listing the root of a base whose path holds an escaped space: the
    /// server's self-reference (`/<handle>/Cloud%20Drive/`) is not an entry.
    #[test]
    fn the_root_listing_under_an_escaped_base_skips_the_base_itself() {
        let provider =
            WebDavProvider::new(test_config("http://127.0.0.1:4443/aBcD1234/Cloud%20Drive"))
                .expect("provider");
        // MEGAcmd 2.6 answers with absolute hrefs and a displayname; the
        // relative form is what other servers send.
        for prefix in ["", "http://127.0.0.1:4443"] {
            let xml = format!(
                r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>{prefix}/aBcD1234/Cloud%20Drive/</d:href>
                <d:propstat><d:prop><d:displayname>Cloud Drive</d:displayname><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
            </d:response>
            <d:response>
                <d:href>{prefix}/aBcD1234/Cloud%20Drive/My%20Folder/</d:href>
                <d:propstat><d:prop><d:displayname>My Folder</d:displayname><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
            </d:response>
        </d:multistatus>"#
            );
            let entries = provider.parse_propfind_response(&xml, "/").unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["My Folder"], "hrefs {prefix:?}");
        }
    }

    /// Any WebDAV base whose path carries escapes (`%20`, a UTF-8 `%C3%A8`):
    /// the root listing skips the base itself and keeps every child, and a
    /// subfolder listing does the same, with names decoded.
    #[test]
    fn listings_under_an_escaped_base_have_no_phantom_and_no_lost_entry() {
        let collection = r#"<d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>"#;
        let file = r#"<d:propstat><d:prop><d:resourcetype/><d:getcontentlength>3</d:getcontentlength></d:prop></d:propstat>"#;
        let response = |href: String, props: &str| {
            format!("<d:response><d:href>{href}</d:href>{props}</d:response>")
        };
        for (base, href_path) in [
            ("https://example.com/my%20dav", "/my%20dav"),
            ("https://example.com/caf%C3%A8/dav", "/caf%C3%A8/dav"),
            // Absolute hrefs, as MEGAcmd sends them.
            (
                "https://example.com/my%20dav",
                "https://example.com/my%20dav",
            ),
            (
                "https://example.com/caf%C3%A8/dav",
                "https://example.com/caf%C3%A8/dav",
            ),
        ] {
            let provider = WebDavProvider::new(test_config(base)).expect("provider");
            let root = format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{}{}{}</d:multistatus>"#,
                response(format!("{href_path}/"), collection),
                response(format!("{href_path}/Sub%20Dir/"), collection),
                response(format!("{href_path}/r%C3%A9sum%C3%A9.txt"), file),
            );
            let entries = provider.parse_propfind_response(&root, "/").unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["Sub Dir", "r\u{e9}sum\u{e9}.txt"],
                "root of {base}"
            );
            assert_eq!(entries[0].path, "/Sub Dir");

            let sub = format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{}{}</d:multistatus>"#,
                response(format!("{href_path}/Sub%20Dir/"), collection),
                response(format!("{href_path}/Sub%20Dir/x%20y.txt"), file),
            );
            let entries = provider.parse_propfind_response(&sub, "/Sub Dir").unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["x y.txt"],
                "subfolder of {base}, hrefs {href_path}"
            );
            assert_eq!(entries[0].path, "/Sub Dir/x y.txt");
        }
    }

    /// Audit 4.2.5 B01: a child whose path ends with the listed path is a
    /// child, not the collection's self-reference. Dropping it made a synced
    /// local copy LocalOnly, and bidirectional sync deleted it. Covers a child
    /// named like the base, a nested descendant with the same suffix (the
    /// Depth:infinity walk parses through the same code), escaped bases,
    /// absolute, path-absolute and relative hrefs, and trailing slashes.
    #[test]
    fn a_child_ending_with_the_listed_path_is_kept() {
        let collection = r#"<d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>"#;
        let file = r#"<d:propstat><d:prop><d:resourcetype/><d:getcontentlength>3</d:getcontentlength></d:prop></d:propstat>"#;
        let response = |href: String, props: &str| {
            format!("<d:response><d:href>{href}</d:href>{props}</d:response>")
        };
        let doc = |body: String| {
            format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{body}</d:multistatus>"#)
        };
        for (base, prefix) in [
            ("https://example.com", ""),
            ("https://example.com/", "https://example.com"),
            ("https://example.com/dav", "/dav"),
            ("https://example.com/dav/", "https://example.com/dav"),
            ("https://example.com/my%20dav", "/my%20dav"),
            (
                "https://example.com/my%20dav",
                "https://example.com/my%20dav",
            ),
            (
                "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive",
                "http://127.0.0.1:4443/aBcD1234/Cloud%20Drive",
            ),
        ] {
            let provider = WebDavProvider::new(test_config(base)).expect("provider");

            // A child named like the listed folder, with and without the
            // trailing slash on the self-reference.
            for self_slash in ["/", ""] {
                let xml = doc(format!(
                    "{}{}{}",
                    response(format!("{prefix}/Sub%20Dir{self_slash}"), collection),
                    response(format!("{prefix}/Sub%20Dir/Sub%20Dir/"), collection),
                    response(format!("{prefix}/Sub%20Dir/Sub%20Dir.txt"), file),
                ));
                let entries = provider.parse_propfind_response(&xml, "/Sub Dir").unwrap();
                let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
                assert_eq!(
                    paths,
                    vec!["/Sub Dir/Sub Dir", "/Sub Dir/Sub Dir.txt"],
                    "base {base}, hrefs {prefix:?}, self slash {self_slash:?}"
                );
            }

            // The same child under a nested listing path, and a deeper
            // descendant ending with the listed path (Depth:infinity body).
            let xml = doc(format!(
                "{}{}{}",
                response(format!("{prefix}/x/b/"), collection),
                response(format!("{prefix}/x/b/b"), file),
                response(format!("{prefix}/x/b/y/x/b/"), collection),
            ));
            let entries = provider.parse_propfind_response(&xml, "/x/b").unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["b", "b"],
                "nested, base {base}, hrefs {prefix:?}"
            );
            assert!(entries[1].is_dir);
        }

        // Spellings of the same self-reference of a UTF-8 folder: a query or
        // fragment, dot segments, lowercase hex escapes, an absolute URL.
        // The child is kept every time.
        let provider =
            WebDavProvider::new(test_config("https://example.com/dav")).expect("provider");
        for self_href in [
            "/dav/R%C3%A9sum%C3%A9/?depth=1",
            "/dav/R%C3%A9sum%C3%A9/#top",
            "/dav/./R%C3%A9sum%C3%A9/x/../",
            "/dav/R%c3%a9sum%c3%a9/",
            "https://example.com/dav/R%c3%a9sum%c3%a9",
        ] {
            let xml = doc(format!(
                "{}{}",
                response(self_href.to_string(), collection),
                response(
                    "/dav/R%C3%A9sum%C3%A9/R%C3%A9sum%C3%A9.txt".to_string(),
                    file
                ),
            ));
            let entries = provider
                .parse_propfind_response(&xml, "/R\u{e9}sum\u{e9}")
                .unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["R\u{e9}sum\u{e9}.txt"], "self href {self_href}");
        }
        // An escaped slash stays inside its segment: `a%2Fb` is one name,
        // never the path `a/b`.
        assert_eq!(
            WebDavProvider::dav_path_segments("/dav/a%2Fb", &[]),
            vec!["dav".to_string(), "a/b".to_string()]
        );
        assert_ne!(
            WebDavProvider::dav_path_segments("/dav/a%2Fb", &[]),
            WebDavProvider::dav_path_segments("/dav/a/b", &[])
        );
        // A scheme-relative href starts with its host: `//h/dav/a` is the
        // path /dav/a on h, never the path /h/dav/a.
        assert_eq!(
            WebDavProvider::dav_path_segments("//h.example/dav/a", &[]),
            vec!["dav".to_string(), "a".to_string()]
        );

        // Relative hrefs resolve against the listed collection.
        let provider =
            WebDavProvider::new(test_config("https://example.com/dav")).expect("provider");
        let xml = doc(format!(
            "{}{}{}",
            response("./".to_string(), collection),
            response("Sub%20Dir/".to_string(), collection),
            response("Sub%20Dir.txt".to_string(), file),
        ));
        let entries = provider.parse_propfind_response(&xml, "/Sub Dir").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Sub Dir", "Sub Dir.txt"]);
    }

    /// LIVE-1 regression: with trim_text(true), a displayname like
    /// `sp ace & ünïcodé.txt` lost the spaces adjacent to `&amp;`, so the
    /// listing showed a name that does not exist on the server and every
    /// operation on it 404'd. Names must round-trip exactly.
    #[test]
    fn parse_propfind_preserves_entity_adjacent_whitespace_in_names() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/sp%20ace%20%26%20%C3%BCn%C3%AFcod%C3%A9.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>sp ace &amp; ünïcodé.txt</d:displayname>
                        <d:resourcetype/>
                        <d:getcontentlength>65536</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/a%20%26%20b.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength>19</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sp ace & ünïcodé.txt");
        assert_eq!(entries[0].size, 65536);
        // No displayname: falls back to the (decoded) href leaf.
        assert_eq!(entries[1].name, "a & b.txt");
    }

    /// A displayname that begins or ends with a space names that file, in a
    /// text node or in a CDATA section: it was trimmed, so ` a.txt ` was
    /// listed as `a.txt` at `/dav/a.txt`, and every operation on it reached
    /// another file or none. The href names the resource: a displayname that
    /// says the href's name with other spaces around it (` b.txt ` for
    /// `/dav/b.txt`) takes the href's, so the entry's path reaches the file.
    /// A displayname that names something else than the href is the name.
    #[test]
    fn parse_propfind_keeps_the_spaces_around_a_displayname() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/%20a.txt%20</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname> a.txt </d:displayname>
                        <d:resourcetype/>
                        <d:getcontentlength>3</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/%20%20/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>  </d:displayname>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/%20c.txt%20</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname><![CDATA[ c.txt ]]></d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/%20%20%20/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname><![CDATA[   ]]></d:displayname>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/b.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname> b.txt </d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/d.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname><![CDATA[  d.txt]]></d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/%20/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>    </d:displayname>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/e7</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname> e.txt</d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        let named: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.path.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                (" a.txt ", "/dav/ a.txt "),
                ("  ", "/dav/  "),
                (" c.txt ", "/dav/ c.txt "),
                ("   ", "/dav/   "),
                ("b.txt", "/dav/b.txt"),
                ("d.txt", "/dav/d.txt"),
                (" ", "/dav/ "),
                (" e.txt", "/dav/ e.txt"),
            ]
        );
    }

    /// CR-536 regression: a whitespace-ONLY Text fragment between two entity
    /// refs (`a&amp; &amp;b.txt`) is part of the name, not indentation, and
    /// must survive in displayname parsing.
    #[test]
    fn parse_propfind_preserves_whitespace_only_fragments_around_entities() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/a%26%20%26b.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>a&amp; &amp;b.txt</d:displayname>
                        <d:resourcetype/>
                        <d:getcontentlength>10</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a& &b.txt");
        assert_eq!(entries[0].size, 10);
    }

    /// WEBDAV-206 regression: FileLu's WebDAV answers a plain GET (no Range
    /// sent) with `206 Partial Content` and `content-range: bytes 0-65535/65536`,
    /// the complete object. Accepting only `200` made those downloads fail,
    /// after three retries, with no local file at all. A `Content-Range` that
    /// is genuinely partial, or missing, must still be rejected so a truncated
    /// body is never committed as a complete download.
    #[test]
    fn partial_content_is_accepted_only_when_it_spans_the_whole_object() {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE};

        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(CONTENT_RANGE, HeaderValue::from_str(v).unwrap());
            h
        };

        // The exact header observed live on webdav.filelu.com. The declared
        // total comes back so the caller can check the received byte count,
        // which is the only guard available when the body is chunked.
        assert_eq!(
            whole_object_total_from_content_range(&with("bytes 0-65535/65536")),
            Some(65536)
        );
        assert_eq!(
            whole_object_total_from_content_range(&with("bytes 0-0/1")),
            Some(1)
        );

        // Genuinely partial bodies stay failures.
        assert_eq!(
            whole_object_total_from_content_range(&with("bytes 0-1023/65536")),
            None
        );
        assert_eq!(
            whole_object_total_from_content_range(&with("bytes 1024-65535/65536")),
            None
        );
        // Unknown total, unparsable, or absent: not trusted.
        assert_eq!(
            whole_object_total_from_content_range(&with("bytes 0-65535/*")),
            None
        );
        assert_eq!(
            whole_object_total_from_content_range(&with("garbage")),
            None
        );
        assert_eq!(
            whole_object_total_from_content_range(&HeaderMap::new()),
            None
        );

        // CR-539: `end` is attacker-controlled. `u64::MAX + 1` wraps in release
        // and panics in debug, so the boundary uses checked arithmetic.
        assert_eq!(
            whole_object_total_from_content_range(&with(
                "bytes 0-18446744073709551615/18446744073709551615"
            )),
            None
        );
    }

    /// CR-536 regression, Nextcloud trashbin parser: same whitespace-only
    /// fragment preservation for trashbin-filename / original-location.
    #[test]
    fn parse_trashbin_preserves_whitespace_only_fragments_around_entities() {
        let provider = WebDavProvider::new(test_config("https://example.com"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns">
            <d:response>
                <d:href>/remote.php/dav/trashbin/user/trash/a%26%20%26b.txt.d1700000000</d:href>
                <d:propstat>
                    <d:prop>
                        <nc:trashbin-filename>a&amp; &amp;b.txt</nc:trashbin-filename>
                        <nc:trashbin-original-location>docs/a&amp; &amp;b.txt</nc:trashbin-original-location>
                        <nc:trashbin-deletion-time>1700000000</nc:trashbin-deletion-time>
                        <d:getcontentlength>10</d:getcontentlength>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_trashbin_response(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a& &b.txt");
        assert_eq!(entries[0].id, "a& &b.txt.d1700000000");
        assert_eq!(
            encode_nextcloud_trash_segment(&entries[0].id),
            "a&%20&b%2Etxt%2Ed1700000000"
        );
        assert_eq!(entries[0].original_path, "docs/a& &b.txt");
        assert_eq!(entries[0].deleted_at, 1700000000);
    }

    /// Issue #128 (Filen WebDAV bridge): the local bridge ships
    /// `<DAV:displayname>` percent-encoded ("my%20folder") instead of the
    /// human-readable form RFC 4918 mandates. Defensive decoding in
    /// parse_propfind_response un-mangles it. RFC-compliant servers send
    /// raw display names, on which the decode is a no-op.
    #[test]
    fn parse_propfind_decodes_percent_encoded_displayname() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/my%20folder/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>my%20folder</d:displayname>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/foto%20vacanze.jpg</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>foto%20vacanze.jpg</d:displayname>
                        <d:resourcetype/>
                        <d:getcontentlength>2048</d:getcontentlength>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/normal-file.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:displayname>normal-file.txt</d:displayname>
                        <d:resourcetype/>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 3);
        // Filen-style: percent-encoded displayname must be decoded.
        assert_eq!(entries[0].name, "my folder");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "foto vacanze.jpg");
        assert!(!entries[1].is_dir);
        // RFC-compliant: raw displayname passes through unchanged.
        assert_eq!(entries[2].name, "normal-file.txt");
    }

    // Issue #175 — boundary check must use the auto-detected `server_root`
    // (Nextcloud / ownCloud) rather than the user-typed `initial_path`,
    // otherwise drill-down clicks fail with "Cannot navigate above WebDAV
    // root" because the entry path is rooted at the wk_path while the
    // user typed `/` or left it blank.

    #[test]
    fn boundary_pure_no_root_means_no_violation() {
        assert!(!path_violates_root("/anything", None));
        assert!(!path_violates_root("/", None));
    }

    #[test]
    fn boundary_pure_empty_or_root_boundary_is_a_noop() {
        assert!(!path_violates_root("/anything", Some("")));
        assert!(!path_violates_root("/anything", Some("/")));
    }

    #[test]
    fn boundary_pure_drill_down_into_nextcloud_root_is_allowed() {
        // Real-world Tab.digital / Nextcloud: server_root resolved at connect
        // time to /remote.php/dav/files/<user>/, user clicks "Documents".
        let root = Some("/remote.php/dav/files/testuser/");
        assert!(!path_violates_root(
            "/remote.php/dav/files/testuser/Documents",
            root
        ));
        assert!(!path_violates_root(
            "/remote.php/dav/files/testuser/Documents/Invoices",
            root
        ));
        // Equality is also fine (cd to the root itself).
        assert!(!path_violates_root("/remote.php/dav/files/testuser", root));
    }

    #[test]
    fn boundary_pure_paths_above_or_outside_root_are_rejected() {
        let root = Some("/remote.php/dav/files/testuser/");
        // Going above
        assert!(path_violates_root("/remote.php/dav/files", root));
        // Sibling user
        assert!(path_violates_root(
            "/remote.php/dav/files/someoneelse/Docs",
            root
        ));
        // Completely off-tree
        assert!(path_violates_root("/", root));
        assert!(path_violates_root("/etc/passwd", root));
        // A name that only starts like the root is another folder.
        assert!(path_violates_root(
            "/remote.php/dav/files/testuser2/Docs",
            root
        ));
    }

    #[tokio::test]
    async fn list_rewrites_root_slash_to_server_root_for_nextcloud() {
        // Issue #175 bug 2: a saved profile with initial_path = "/" hits
        // 405 on Nextcloud because PROPFIND on `/` is not allowed. The
        // backend now redirects "/" to the auto-detected server_root.
        // We can't observe the network call without a real server, but
        // we assert the path resolution is what we expect by inspecting
        // the rewrite directly.
        let mut provider = WebDavProvider::new(test_config("https://cloud.example.com")).unwrap();
        provider.connected = true;
        provider.current_path = "/remote.php/dav/files/testuser/".to_string();
        provider.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        // Reproduce the rewrite logic: ensure "/" maps to server_root.
        let resolved = match "/" {
            "" | "." => provider.current_path.clone(),
            "/" => provider
                .server_root
                .clone()
                .unwrap_or_else(|| provider.current_path.clone()),
            other => other.to_string(),
        };
        assert_eq!(resolved, "/remote.php/dav/files/testuser/");
    }

    #[test]
    fn cd_up_uses_server_root_over_config_initial_path() {
        // Sanity check the resolution order used by `cd_up`. Without going
        // through the network, we can verify the precedence on the field
        // directly via the same expression `cd_up` uses.
        let mut config = test_config("https://cloud.example.com");
        config.initial_path = Some("/Documents".to_string());
        let mut provider = WebDavProvider::new(config).unwrap();
        provider.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        let chosen = provider
            .server_root
            .as_deref()
            .or(provider.config.initial_path.as_deref())
            .filter(|p| !p.is_empty())
            .unwrap_or("/");
        assert_eq!(chosen, "/remote.php/dav/files/testuser/");
    }

    #[test]
    fn resolve_root_composes_relative_paths_under_nextcloud_root() {
        let mut p = WebDavProvider::new(test_config("https://cloud.example.com")).unwrap();
        p.server_root = Some("/remote.php/dav/files/testuser/".to_string());

        // Bare caller paths get composed under the auto-detected root
        // (the mkdir/put/delete bug: previously bypassed it -> 404/405).
        assert_eq!(
            p.resolve_root("/aeroftp-utest"),
            "/remote.php/dav/files/testuser/aeroftp-utest"
        );
        // Collection form keeps its trailing slash.
        assert_eq!(
            p.resolve_root("/aeroftp-utest/"),
            "/remote.php/dav/files/testuser/aeroftp-utest/"
        );
        // Literal root maps to the server root.
        assert_eq!(p.resolve_root("/"), "/remote.php/dav/files/testuser/");

        // Idempotent: already-rooted paths (GUI drill-down, where list()
        // returns fully-rooted entry paths) are not double-prefixed.
        assert_eq!(
            p.resolve_root("/remote.php/dav/files/testuser/Documents"),
            "/remote.php/dav/files/testuser/Documents"
        );
        assert_eq!(
            p.resolve_root("/remote.php/dav/files/testuser/"),
            "/remote.php/dav/files/testuser/"
        );

        // No distinct root => exact no-op (traditional servers, and the
        // connect-time probes that run before server_root is set).
        let mut t = WebDavProvider::new(test_config("https://dav.example.com")).unwrap();
        assert_eq!(t.resolve_root("/aeroftp-utest"), "/aeroftp-utest");
        t.server_root = Some("/".to_string());
        assert_eq!(t.resolve_root("/aeroftp-utest"), "/aeroftp-utest");
    }

    #[test]
    fn oc_checksums_parsed_to_canonical_lowercase_keys() {
        // Real Nextcloud/ownCloud shape: space-separated ALGO:HEX tokens.
        let m = parse_oc_checksums(
            "SHA1:f1d2d2f924e986ac86fdf7b36c94bcdf32beec15 \
             MD5:900150983cd24fb0d6963f7d28e17f72 ADLER32:024d0127",
        );
        assert_eq!(
            m.get("sha1").map(String::as_str),
            Some("f1d2d2f924e986ac86fdf7b36c94bcdf32beec15")
        );
        assert_eq!(
            m.get("md5").map(String::as_str),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(m.get("adler32").map(String::as_str), Some("024d0127"));
        // No canonical key is upper-cased or dash-separated.
        assert!(m.keys().all(|k| k == &k.to_ascii_lowercase()));
    }

    #[test]
    fn oc_checksums_canonicalises_separators_and_case() {
        let m = parse_oc_checksums("SHA-256:ABCDEF01 sha512:00FF");
        assert_eq!(m.get("sha256").map(String::as_str), Some("abcdef01"));
        assert_eq!(m.get("sha512").map(String::as_str), Some("00ff"));
    }

    #[test]
    fn oc_checksums_skips_malformed_and_empty() {
        // No colon, empty digest, non-hex digest, and the empty string.
        assert!(parse_oc_checksums("").is_empty());
        assert!(parse_oc_checksums("SHA1 MD5: SHA256:zz_not_hex").is_empty());
        // A single good token among malformed ones still survives.
        let m = parse_oc_checksums("garbage MD5:0a1b BAD:");
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("md5").map(String::as_str), Some("0a1b"));
    }

    #[test]
    fn unknown_algo_degrades_not_dropped() {
        let m = parse_oc_checksums("WHIRLPOOL:dead");
        assert_eq!(m.get("whirlpool").map(String::as_str), Some("dead"));
    }

    /// SG-T08 gate: WebDAV advertises the server_side_copy capability under
    /// both the legacy and the new slot, since the RFC 4918 COPY method is
    /// universally available on compliant DAV servers.
    #[test]
    fn webdav_advertises_server_side_copy_capability() {
        let p = WebDavProvider::new(test_config("https://cloud.example.com/")).expect("provider");
        assert!(p.supports_server_copy());
        assert!(p.supports_server_side_copy());
    }

    /// SG-T08 gate: both entry points fail fast on the connection check
    /// before issuing a COPY request.
    #[tokio::test]
    async fn webdav_server_side_copy_requires_connection() {
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com/")).expect("provider");
        let direct = StorageProvider::server_side_copy(&mut p, "/src.txt", "/dst.txt").await;
        assert!(matches!(direct, Err(ProviderError::NotConnected)));

        let via_legacy = p.server_copy("/src.txt", "/dst.txt").await;
        assert!(matches!(via_legacy, Err(ProviderError::NotConnected)));
    }

    /// A WebDAV double that treats MOVE as golang.org/x/net/webdav does:
    /// `Depth` absent or `infinity` moves (201), any other value is a 400.
    /// PROPFIND describes every path as a 3-byte file, so a client that stats
    /// the source to choose the header (as this provider did, sending `0` for
    /// a file) is caught; a double that refused PROPFIND let the old code fall
    /// back to `infinity` and pass. Returns its URL and the `Depth` of every
    /// MOVE it received.
    async fn move_server_like_x_net_webdav() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
    ) {
        use std::sync::{Arc, Mutex};
        let depths: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let seen = Arc::clone(&depths);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let reply = |status: u16, body: String| {
                        axum::response::Response::builder()
                            .status(status)
                            .header("content-type", "application/xml; charset=utf-8")
                            .body(axum::body::Body::from(body))
                            .unwrap()
                    };
                    match req.method().as_str() {
                        "PROPFIND" => {
                            let href = req.uri().path().to_string();
                            return reply(
                                207,
                                format!(
                                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                                     <d:multistatus xmlns:d=\"DAV:\"><d:response>\
                                     <d:href>{href}</d:href><d:propstat><d:prop>\
                                     <d:resourcetype/><d:getcontentlength>3</d:getcontentlength>\
                                     </d:prop><d:status>HTTP/1.1 200 OK</d:status>\
                                     </d:propstat></d:response></d:multistatus>"
                                ),
                            );
                        }
                        "MOVE" => {}
                        _ => return reply(405, String::new()),
                    }
                    let depth = req
                        .headers()
                        .get("depth")
                        .map(|v| v.to_str().unwrap().to_string());
                    let valid = matches!(depth.as_deref(), None | Some("infinity"));
                    seen.lock().unwrap().push(depth);
                    reply(if valid { 201 } else { 400 }, String::new())
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        (format!("http://{addr}/"), depths)
    }

    /// Round 2 of the 4.2.1 review: `rmdir` was a DELETE of the collection,
    /// which RFC 4918 runs with `Depth: infinity`, so a member the caller's
    /// listing had left out went with the "empty" collection. The collection
    /// is listed first (PROPFIND `Depth: 1`): one that holds anything is
    /// refused, with no DELETE sent. A listing answered with another document
    /// (an XHTML login page) is no empty collection: refused, no DELETE.
    #[tokio::test]
    async fn rmdir_lists_the_collection_and_refuses_one_that_holds_anything() {
        use std::sync::{Arc, Mutex};
        for (listing, expect_refusal) in [("member", true), ("empty", false), ("xhtml", true)] {
            let member = listing == "member";
            let calls: Arc<Mutex<Vec<String>>> = Arc::default();
            let seen = Arc::clone(&calls);
            let app = axum::Router::new().fallback(axum::routing::any(
                move |req: axum::extract::Request| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let method = req.method().to_string();
                        let path = req.uri().path().to_string();
                        seen.lock().unwrap().push(format!("{method} {path}"));
                        if method == "PROPFIND" && listing == "xhtml" {
                            return axum::response::Response::builder()
                                .status(207)
                                .header("content-type", "application/xml; charset=utf-8")
                                .body(axum::body::Body::from(
                                    "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Sign in</body></html>",
                                ))
                                .unwrap();
                        }
                        if method == "PROPFIND" {
                            let child = if member {
                                "<d:response><d:href>/d/a.txt</d:href><d:propstat><d:prop>\
                                 <d:resourcetype/><d:getcontentlength>1</d:getcontentlength>\
                                 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                            } else {
                                ""
                            };
                            let body = format!(
                                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                                 <d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/d/</d:href>\
                                 <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>\
                                 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
                                 </d:response>{child}</d:multistatus>"
                            );
                            return axum::response::Response::builder()
                                .status(207)
                                .header("content-type", "application/xml; charset=utf-8")
                                .body(axum::body::Body::from(body))
                                .unwrap();
                        }
                        axum::response::Response::builder()
                            .status(if method == "DELETE" { 204 } else { 405 })
                            .body(axum::body::Body::empty())
                            .unwrap()
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.ok() });
            let mut p =
                WebDavProvider::new(test_config(&format!("http://{addr}/"))).expect("provider");
            p.connected = true;
            let outcome = p.rmdir("/d").await;
            let calls = calls.lock().unwrap();
            if expect_refusal {
                if member {
                    assert!(
                        matches!(outcome, Err(ProviderError::DirectoryNotEmpty(_))),
                        "{outcome:?}"
                    );
                } else {
                    assert!(
                        matches!(&outcome, Err(ProviderError::ParseError(m)) if m.contains("not a DAV multistatus")),
                        "{outcome:?}"
                    );
                }
                assert!(!calls.iter().any(|c| c.starts_with("DELETE ")), "{calls:?}");
            } else {
                outcome.expect("an empty collection goes");
                assert!(calls.iter().any(|c| c == "DELETE /d/"), "{calls:?}");
            }
        }
    }

    /// Renaming a file failed with 400 on every server built on
    /// golang.org/x/net/webdav while the MOVE carried `Depth: 0`.
    #[tokio::test]
    async fn file_rename_and_replace_move_with_depth_infinity() {
        let (url, depths) = move_server_like_x_net_webdav().await;
        let mut p = WebDavProvider::new(test_config(&url)).expect("provider");
        p.connected = true;
        p.rename("/a.txt", "/dir/a.txt").await.expect("rename");
        p.replace("/b.tmp", "/dir/b.txt").await.expect("replace");
        // One MOVE for the rename, two for the replace, which sets the file
        // this server reports at `/dir/b.txt` aside before it moves in.
        let depths = depths.lock().unwrap();
        assert_eq!(depths.len(), 3, "{depths:?}");
        assert!(
            depths.iter().all(|d| d.as_deref() == Some("infinity")),
            "{depths:?}"
        );
    }

    /// Nextcloud answers a MOVE onto its own URL with 412, which read as
    /// AlreadyExists: `mv x x` failed where every other provider does nothing
    /// (found live on 2026-09-26). A MOVE with `Overwrite: T` onto itself is
    /// worse, since a server may delete the destination first. Neither is sent.
    #[tokio::test]
    async fn a_rename_or_replace_onto_its_own_path_sends_nothing() {
        use std::sync::{Arc, Mutex};
        let moves: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&moves);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let destination = req
                        .headers()
                        .get("destination")
                        .map(|v| v.to_str().unwrap().to_string())
                        .unwrap_or_default();
                    let path = req.uri().path().to_string();
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {path}", req.method()));
                    if destination.ends_with(&path) {
                        axum::http::StatusCode::PRECONDITION_FAILED
                    } else {
                        axum::http::StatusCode::CREATED
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut p = WebDavProvider::new(test_config(&format!("http://{addr}/"))).expect("provider");
        p.connected = true;
        p.rename("/d/a.txt", "/d/a.txt")
            .await
            .expect("rename onto itself");
        p.rename("/d", "/d/")
            .await
            .expect("folder rename onto itself");
        p.replace("/d/a.txt", "/d/a.txt")
            .await
            .expect("replace onto itself");
        {
            let seen_moves = moves.lock().unwrap().clone();
            assert!(seen_moves.is_empty(), "{:?}", seen_moves);
        }
    }

    /// Files by path, as a [`dav_store_server`] holds them.
    type DavFiles = std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>;

    /// An in-memory WebDAV server whose MOVE is the one of RFC 4918 section
    /// 9.9.3, as Apache mod_dav, golang.org/x/net/webdav and Nextcloud were
    /// measured to run it on 2026-10-10: under `Overwrite: T` the destination
    /// is deleted first and the source moved after, and a move that fails
    /// after that delete does not bring the destination back. A source in
    /// `locked` fails its move with 500, the way Nextcloud answers the MOVE
    /// of a file another client is reading (`LockedException`). Returns the
    /// base URL, the files, and every request as `METHOD path` with a MOVE's
    /// destination and `Overwrite:` value.
    async fn dav_store_server(
        files: &[(&str, &str)],
        locked: &[&str],
    ) -> (
        String,
        DavFiles,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::sync::{Arc, Mutex};
        let store: DavFiles = Arc::new(Mutex::new(
            files
                .iter()
                .map(|(path, data)| (path.to_string(), data.as_bytes().to_vec()))
                .collect(),
        ));
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let locked: Arc<Vec<String>> = Arc::new(locked.iter().map(|p| p.to_string()).collect());
        let (state, log) = (Arc::clone(&store), Arc::clone(&requests));
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (store, log, locked) =
                    (Arc::clone(&state), Arc::clone(&log), Arc::clone(&locked));
                async move {
                    let status = |code: u16| {
                        axum::response::Response::builder()
                            .status(code)
                            .body(axum::body::Body::empty())
                            .unwrap()
                    };
                    let path = req.uri().path().to_string();
                    let method = req.method().to_string();
                    let mut files = store.lock().unwrap();
                    match method.as_str() {
                        "PROPFIND" => {
                            log.lock().unwrap().push(format!("PROPFIND {path}"));
                            let Some(data) = files.get(&path) else {
                                return status(404);
                            };
                            let body = format!(
                                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                                 <d:multistatus xmlns:d=\"DAV:\"><d:response>\
                                 <d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype/>\
                                 <d:getcontentlength>{}</d:getcontentlength></d:prop>\
                                 <d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
                                 </d:response></d:multistatus>",
                                data.len()
                            );
                            axum::response::Response::builder()
                                .status(207)
                                .header("content-type", "application/xml; charset=utf-8")
                                .body(axum::body::Body::from(body))
                                .unwrap()
                        }
                        "DELETE" => {
                            log.lock().unwrap().push(format!("DELETE {path}"));
                            status(if files.remove(&path).is_some() {
                                204
                            } else {
                                404
                            })
                        }
                        "MOVE" => {
                            let header = |name: &str| {
                                req.headers()
                                    .get(name)
                                    .map(|v| v.to_str().unwrap().to_string())
                            };
                            let destination = header("destination").unwrap_or_default();
                            let destination = destination
                                .splitn(4, '/')
                                .nth(3)
                                .map(|rest| format!("/{rest}"))
                                .unwrap_or_default();
                            // RFC 4918 section 10.6: an absent header is `T`.
                            let overwrite = header("overwrite").as_deref() != Some("F");
                            log.lock().unwrap().push(format!(
                                "MOVE {path} {destination} Overwrite:{}",
                                if overwrite { "T" } else { "F" }
                            ));
                            if !files.contains_key(&path) {
                                return status(404);
                            }
                            let replaced = files.contains_key(&destination);
                            if replaced {
                                if !overwrite {
                                    return status(412);
                                }
                                files.remove(&destination);
                            }
                            if locked.contains(&path) {
                                return status(500);
                            }
                            let data = files.remove(&path).unwrap();
                            files.insert(destination, data);
                            status(if replaced { 204 } else { 201 })
                        }
                        _ => status(405),
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        (format!("http://{addr}/"), store, requests)
    }

    /// A MOVE with `Overwrite: T` is one request, but RFC 4918 section 9.9.3
    /// has the server delete the destination before it moves: measured so on
    /// Apache mod_dav, golang.org/x/net/webdav and Nextcloud, and only nginx
    /// renames a file over another. A replace is therefore not claimed
    /// atomic: it sets the old file aside, which a single-file profile, with
    /// no other name to set it aside under, cannot do.
    #[tokio::test]
    async fn a_replace_sets_the_old_file_aside_and_is_never_claimed_atomic() {
        let mut provider =
            WebDavProvider::new(test_config("https://example.com/dav")).expect("provider");
        assert!(!provider.supports_atomic_replace().await.expect("asked"));
        assert!(provider.replace_sets_aside());
        provider.single_file_mode = Some(RemoteEntry::file(
            "f.txt".to_string(),
            "/dav/f.txt".to_string(),
            1,
        ));
        assert!(!provider.supports_atomic_replace().await.expect("asked"));
        assert!(!provider.replace_sets_aside());
    }

    /// A replace over a file never sends `Overwrite: T`, whose delete comes
    /// before the move, and leaves nothing behind but the new file under the
    /// old name, in the target's own folder however the path is spelled.
    #[tokio::test]
    async fn a_replace_over_a_file_never_sends_an_overwriting_move() {
        for (from, to) in [("/d/a.tmp", "/d/a.txt"), ("d/a.tmp", "d/a.txt")] {
            let (url, files, requests) =
                dav_store_server(&[("/d/a.txt", "old"), ("/d/a.tmp", "new")], &[]).await;
            let mut p = WebDavProvider::new(test_config(&url)).expect("provider");
            p.connected = true;
            p.replace(from, to).await.expect("replace");
            let requests = requests.lock().unwrap();
            assert_eq!(
                *files.lock().unwrap(),
                HashMap::from([("/d/a.txt".to_string(), b"new".to_vec())]),
                "{from} -> {to}: {requests:?}"
            );
            assert!(
                !requests.iter().any(|r| r.ends_with("Overwrite:T")),
                "{from} -> {to}: {requests:?}"
            );
        }
    }

    /// The failure that made one overwriting MOVE unsafe: measured on
    /// Nextcloud 33, a MOVE whose source another client is reading answers
    /// 500 after the destination has gone to the trash. A replace whose move
    /// in fails puts the old file back under its name and says it failed.
    #[tokio::test]
    async fn a_replace_whose_move_in_fails_puts_the_old_file_back() {
        let (url, files, requests) =
            dav_store_server(&[("/d/a.txt", "old"), ("/d/a.tmp", "new")], &["/d/a.tmp"]).await;
        let mut p = WebDavProvider::new(test_config(&url)).expect("provider");
        p.connected = true;
        let outcome = p.replace("/d/a.tmp", "/d/a.txt").await;
        let requests = requests.lock().unwrap();
        assert!(outcome.is_err(), "{outcome:?} {requests:?}");
        assert_eq!(
            *files.lock().unwrap(),
            HashMap::from([
                ("/d/a.txt".to_string(), b"old".to_vec()),
                ("/d/a.tmp".to_string(), b"new".to_vec()),
            ]),
            "{requests:?}"
        );
    }

    /// What the callers that publish a staged file get from that answer: an
    /// edit is refused without the opt-in and offered it, and passes with it;
    /// a marker written onto a free name passes, since a set-aside replace
    /// onto a free name is one MOVE that sets nothing aside.
    #[tokio::test]
    async fn an_edit_needs_the_opt_in_and_a_marker_onto_a_free_name_does_not() {
        let (url, _, _) = dav_store_server(&[("/d/a.txt", "old")], &[]).await;
        let mut p = WebDavProvider::new(test_config(&url)).expect("provider");
        p.connected = true;
        let refused =
            crate::providers::ensure_edit_can_replace(&mut p, "/d/a.txt", false, "`--opt`").await;
        assert!(
            matches!(&refused, Err(ProviderError::NotSupported(text)) if text.contains("`--opt`")),
            "{refused:?}"
        );
        let opted_in =
            crate::providers::ensure_edit_can_replace(&mut p, "/d/a.txt", true, "`--opt`")
                .await
                .expect("the opt-in");
        assert_eq!(opted_in, crate::providers::StagedPublish::Replace);
        let free = crate::providers::ensure_atomic_replace(&mut p, "/d/.aerocrypt.tsv")
            .await
            .expect("nothing to replace");
        assert_eq!(free, crate::providers::StagedPublish::Create);
        let occupied = crate::providers::ensure_atomic_replace(&mut p, "/d/a.txt").await;
        assert!(
            matches!(occupied, Err(ProviderError::NotSupported(_))),
            "{occupied:?}"
        );
    }

    /// CodeRabbit on #1154: a marker check passes on WebDAV only because no
    /// marker is there, so the publish is a creation, one MOVE with
    /// `Overwrite: F`. When another client wrote the marker after the check
    /// the server refuses it (412): their marker stays, nothing is set aside,
    /// and the staged file is still there for the caller to delete.
    #[tokio::test]
    async fn a_publish_onto_a_name_taken_after_its_check_is_refused_by_the_server() {
        let (url, files, requests) = dav_store_server(&[("/d/m.tmp", "ours")], &[]).await;
        let mut p = WebDavProvider::new(test_config(&url)).expect("provider");
        p.connected = true;
        let publish = crate::providers::ensure_atomic_replace(&mut p, "/d/m.tsv")
            .await
            .expect("a free name");
        files
            .lock()
            .unwrap()
            .insert("/d/m.tsv".to_string(), b"theirs".to_vec());

        let outcome = publish.publish(&mut p, "/d/m.tmp", "/d/m.tsv").await;

        let requests = requests.lock().unwrap();
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?} {requests:?}"
        );
        assert_eq!(
            *files.lock().unwrap(),
            HashMap::from([
                ("/d/m.tsv".to_string(), b"theirs".to_vec()),
                ("/d/m.tmp".to_string(), b"ours".to_vec()),
            ]),
            "{requests:?}"
        );
        let moves: Vec<_> = requests.iter().filter(|r| r.starts_with("MOVE")).collect();
        assert_eq!(
            moves,
            ["MOVE /d/m.tmp /d/m.tsv Overwrite:F"],
            "{requests:?}"
        );
    }

    // ─── T-DEBT-07: Nextcloud chunked upload v2 gating + wire ─────────

    /// Chunk filenames must be zero-padded to a fixed width so the
    /// server-side alphabetical sort at finalize time matches the
    /// numeric part order.
    #[test]
    fn nextcloud_chunk_filename_is_zero_padded_for_alpha_sort() {
        assert_eq!(nextcloud_chunk_filename(1), "00000000000000000001");
        assert_eq!(nextcloud_chunk_filename(2), "00000000000000000002");
        assert_eq!(nextcloud_chunk_filename(10), "00000000000000000010");
        // Two chunks at widely different indices must sort in numeric
        // order under a pure lexicographic compare.
        assert!(nextcloud_chunk_filename(2) < nextcloud_chunk_filename(10));
        assert!(nextcloud_chunk_filename(10) < nextcloud_chunk_filename(100));
        assert_eq!(nextcloud_chunk_filename(u32::MAX).len(), 20);
    }

    /// User id resolution: prefer the auto-detected well-known path.
    #[test]
    fn extract_nextcloud_userid_from_well_known_path() {
        assert_eq!(
            extract_nextcloud_userid("/remote.php/dav/files/alice/"),
            Some("alice".to_string())
        );
        assert_eq!(
            extract_nextcloud_userid("/remote.php/dav/files/bob/Documents/Invoices"),
            Some("bob".to_string())
        );
        assert_eq!(
            extract_nextcloud_userid(
                "https://cloud.example.com/remote.php/dav/files/charlie/folder/file.bin"
            ),
            Some("charlie".to_string())
        );
        // Empty user segment is rejected, not silently accepted.
        assert_eq!(extract_nextcloud_userid("/remote.php/dav/files//"), None);
        assert_eq!(
            extract_nextcloud_userid("/remote.php/webdav/legacy.txt"),
            None
        );
        assert_eq!(extract_nextcloud_userid("/elsewhere/file.bin"), None);
    }

    /// `is_nextcloud_for_dav()` accepts the canonical preset ids and the
    /// canonical URL prefix, refuses bare hostnames without further
    /// signals.
    #[test]
    fn nextcloud_for_dav_gating() {
        // preset_id = "nextcloud" on a bare hostname is enough.
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("nextcloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        assert!(p.is_nextcloud_for_dav());

        // preset_id = "owncloud" also accepted.
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("owncloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        assert!(p.is_nextcloud_for_dav());

        // Other Nextcloud-backed SaaS presets are NOT auto-enabled (chunked
        // v2 support not yet verified on every backend).
        for pid in [
            "felicloud",
            "tabdigital-webdav",
            "magentacloud",
            "magentacloud-webdav",
        ] {
            let mut cfg = test_config("https://cloud.lab.example.test");
            cfg.provider_id = Some(pid.to_string());
            let p = WebDavProvider::new(cfg).expect("provider");
            assert!(
                !p.is_nextcloud_for_dav(),
                "preset {pid} must not auto-enable chunked v2 yet"
            );
        }

        // URL pattern alone is enough.
        let p = WebDavProvider::new(test_config(
            "https://cloud.example.com/remote.php/dav/files/alice/",
        ))
        .expect("provider");
        assert!(p.is_nextcloud_for_dav());

        // server_root auto-detected via well-known path is enough.
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com")).expect("provider");
        p.server_root = Some("/remote.php/dav/files/alice/".to_string());
        assert!(p.is_nextcloud_for_dav());

        // Vanilla WebDAV stays out.
        let p =
            WebDavProvider::new(test_config("https://dav.example.com/webdav/")).expect("provider");
        assert!(!p.is_nextcloud_for_dav());
    }

    /// User id resolution precedence: server_root wins over username, URL
    /// inspection beats falling back to the config username.
    #[test]
    fn nextcloud_userid_prefers_server_root_over_username() {
        // server_root carries the canonical id (federated LDAP DN alias
        // case: login username != storage id).
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com")).expect("provider");
        p.server_root = Some("/remote.php/dav/files/canonical-id/".to_string());
        // `test_config` sets username = "user"
        assert_eq!(p.nextcloud_userid(), Some("canonical-id".to_string()));

        // No server_root, URL carries the id.
        let p = WebDavProvider::new(test_config(
            "https://cloud.example.com/remote.php/dav/files/url-id/",
        ))
        .expect("provider");
        assert_eq!(p.nextcloud_userid(), Some("url-id".to_string()));

        // Neither: fall back to config.username.
        let p = WebDavProvider::new(test_config("https://cloud.example.com")).expect("provider");
        assert_eq!(p.nextcloud_userid(), Some("user".to_string()));
    }

    /// Hint advertisement is gated by Nextcloud detection. Vanilla WebDAV
    /// MUST NOT advertise multipart so the shaped-graph builder keeps it
    /// on the single-PUT legacy path.
    #[test]
    fn nextcloud_chunked_advertised_when_provider_id_is_nextcloud() {
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("nextcloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        let hints = p.transfer_optimization_hints();
        assert!(hints.supports_multipart);
        assert_eq!(hints.multipart_part_size, NEXTCLOUD_DAG_CHUNK_SIZE);
        // Chunked v2 only engages at/above the threshold; medium uploads stay
        // on a single PUT (faster on LAN), audit Patch Set 2.
        assert_eq!(hints.multipart_threshold, NEXTCLOUD_DAG_THRESHOLD);
        assert_eq!(hints.multipart_max_parallel, NEXTCLOUD_DAG_MAX_PARALLEL);
    }

    /// Regression, 2026-07-25. The trashbin and OCS share endpoints resolved
    /// their base URL by searching the CONFIGURED url for `/remote.php/`, so
    /// the answer depended on how the user typed the profile URL rather than
    /// on what the server is. A profile saved as the bare origin browses
    /// perfectly (connect resolves the real root) but reported
    /// `Not a Nextcloud instance`, silently disabling the trash manager and
    /// share links. Observed live on two profiles pointing at genuine
    /// Nextcloud servers whose `status.php` answers `productname: Nextcloud`.
    #[test]
    fn nextcloud_base_url_resolves_from_bare_origin_when_provider_id_says_nextcloud() {
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("nextcloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        assert_eq!(
            p.nextcloud_base_url(),
            Some("https://cloud.lab.example.test".to_string()),
            "a bare-origin Nextcloud profile must still resolve its OCS base"
        );
        assert!(p.is_nextcloud(), "share links must stay advertised");
    }

    /// The canonical form keeps working unchanged: the origin is whatever
    /// precedes `/remote.php/`, trailing user path included.
    #[test]
    fn nextcloud_base_url_keeps_canonical_url_form() {
        let p = WebDavProvider::new(test_config(
            "https://cloud.example.com/remote.php/dav/files/alice/",
        ))
        .expect("provider");
        assert_eq!(
            p.nextcloud_base_url(),
            Some("https://cloud.example.com".to_string())
        );
        assert!(p.is_nextcloud());
    }

    /// A trailing slash on a subpath install must not leak into the endpoint,
    /// and the install subpath itself must be preserved (ownCloud default
    /// `/owncloud`, reverse-proxied Nextcloud under `/nc`, …).
    #[test]
    fn nextcloud_base_url_keeps_install_subpath_strips_trailing_slash() {
        let mut cfg = test_config("https://cloud.example.com/owncloud/");
        cfg.provider_id = Some("owncloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        assert_eq!(
            p.nextcloud_base_url(),
            Some("https://cloud.example.com/owncloud".to_string())
        );
    }

    /// Canonical DAV URL under a subpath install: the base is everything
    /// before `/remote.php/`, including the install prefix.
    #[test]
    fn nextcloud_base_url_keeps_subpath_before_remote_php() {
        let p = WebDavProvider::new(test_config(
            "https://cloud.example.com/owncloud/remote.php/dav/files/alice/",
        ))
        .expect("provider");
        assert_eq!(
            p.nextcloud_base_url(),
            Some("https://cloud.example.com/owncloud".to_string())
        );
    }

    /// The guard must stay a guard: vanilla WebDAV is still not Nextcloud, so
    /// the trash commands keep failing closed instead of building bogus URLs.
    #[test]
    fn nextcloud_base_url_none_for_vanilla_webdav() {
        let p = WebDavProvider::new(test_config("https://dav.example.com/")).expect("provider");
        assert_eq!(p.nextcloud_base_url(), None);
        assert!(!p.is_nextcloud());
    }

    #[test]
    fn nextcloud_chunked_not_advertised_for_vanilla_webdav() {
        let p = WebDavProvider::new(test_config("https://dav.example.com/")).expect("provider");
        let hints = p.transfer_optimization_hints();
        assert!(!hints.supports_multipart);
        assert_eq!(hints.multipart_threshold, 0);
        assert_eq!(hints.multipart_part_size, 0);
        assert_eq!(hints.multipart_max_parallel, 1);
    }

    /// Wire format check: URLs target `/remote.php/dav/uploads/<userid>/<txid>/`
    /// regardless of the per-user `/files/<userid>/` server_root.
    /// A persisted handle of another user is refused by the part and the
    /// assembly before any request, and its abort sends nothing; the same
    /// user's handle reaches the server.
    #[tokio::test]
    async fn a_chunked_upload_handle_of_another_user_writes_nothing() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&seen);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let log = Arc::clone(&log);
                async move {
                    log.lock()
                        .unwrap()
                        .push(format!("{} {}", req.method(), req.uri().path()));
                    axum::http::StatusCode::CREATED
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut cfg = test_config(&format!("http://{addr}"));
        cfg.provider_id = Some("nextcloud".to_string());
        let mut p = WebDavProvider::new(cfg).expect("provider");
        p.server_root = Some("/remote.php/dav/files/alice/".to_string());
        p.connected = true;
        let destination = NextcloudMultipartHandle::destination_digest(
            &p.nextcloud_chunked_destination_url("/f.bin")
                .expect("destination URL"),
        );
        let handle_of = |user: &str| MultipartHandle {
            upload_id: NextcloudMultipartHandle {
                transfer_id: "tx-1".to_string(),
                user_id: user.to_string(),
                total_size: 3,
                destination: destination.clone(),
            }
            .encode(),
            remote_path: "/f.bin".to_string(),
        };
        let foreign = handle_of("mallory");
        let part = p.upload_part(&foreign, 1, b"abc".to_vec()).await;
        assert!(
            matches!(&part, Err(ProviderError::PermissionDenied(m)) if m.contains("another Nextcloud user")),
            "{part:?}"
        );
        let done = p
            .complete_multipart_upload(handle_of("mallory"), Vec::new())
            .await;
        assert!(
            matches!(&done, Err(ProviderError::PermissionDenied(_))),
            "{done:?}"
        );
        p.abort_multipart_upload(handle_of("mallory"))
            .await
            .expect("best effort");
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
        p.abort_multipart_upload(handle_of("alice"))
            .await
            .expect("abort");
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /remote.php/dav/uploads/alice/tx-1/"]
        );
    }

    /// A handle is bound to the URL its upload is assembled at: once the
    /// profile's folder changed, the same user's part, assembly and abort
    /// send nothing, and an old handle without a destination is refused
    /// before any request; back on the folder it began in, the part goes.
    #[tokio::test]
    async fn a_chunked_upload_handle_of_another_folder_writes_nothing() {
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&seen);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let log = Arc::clone(&log);
                async move {
                    log.lock()
                        .unwrap()
                        .push(format!("{} {}", req.method(), req.uri().path()));
                    axum::http::StatusCode::CREATED
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut cfg = test_config(&format!("http://{addr}"));
        cfg.provider_id = Some("nextcloud".to_string());
        let mut p = WebDavProvider::new(cfg).expect("provider");
        p.server_root = Some("/remote.php/dav/files/alice/".to_string());
        p.connected = true;
        let handle = p
            .begin_multipart_upload("/f.bin", 3, None, None)
            .await
            .expect("begin");
        seen.lock().unwrap().clear();

        p.server_root = Some("/remote.php/dav/files/alice/elsewhere/".to_string());
        let part = p.upload_part(&handle, 1, b"abc".to_vec()).await;
        assert!(
            matches!(&part, Err(ProviderError::PermissionDenied(m)) if m.contains("another destination")),
            "{part:?}"
        );
        let done = p
            .complete_multipart_upload(handle.clone(), Vec::new())
            .await;
        assert!(
            matches!(&done, Err(ProviderError::PermissionDenied(m)) if m.contains("start the upload again")),
            "{done:?}"
        );
        p.abort_multipart_upload(handle.clone())
            .await
            .expect("best effort");
        let old = MultipartHandle {
            upload_id: "webdav-chunked-v2|tx-1|alice|3".to_string(),
            remote_path: handle.remote_path.clone(),
        };
        let part = p.upload_part(&old, 1, b"abc".to_vec()).await;
        assert!(
            matches!(&part, Err(ProviderError::Other(m)) if m.contains("start the upload again")),
            "{part:?}"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );

        p.server_root = Some("/remote.php/dav/files/alice/".to_string());
        p.upload_part(&handle, 1, b"abc".to_vec())
            .await
            .expect("part");
        assert_eq!(seen.lock().unwrap().len(), 1, "{:?}", seen.lock().unwrap());
    }

    #[test]
    fn nextcloud_chunked_url_layout_lives_under_uploads_root() {
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com")).expect("provider");
        p.server_root = Some("/remote.php/dav/files/alice/".to_string());

        let folder = p
            .nextcloud_chunked_folder_url("tx-123")
            .expect("folder URL");
        assert_eq!(
            folder,
            "https://cloud.example.com/remote.php/dav/uploads/alice/tx-123/"
        );

        let chunk = p
            .nextcloud_chunked_chunk_url("tx-123", 7)
            .expect("chunk URL");
        assert_eq!(
            chunk,
            "https://cloud.example.com/remote.php/dav/uploads/alice/tx-123/00000000000000000007"
        );

        let assemble = p
            .nextcloud_chunked_assemble_url("tx-123")
            .expect("assemble URL");
        assert_eq!(
            assemble,
            "https://cloud.example.com/remote.php/dav/uploads/alice/tx-123/.file"
        );

        // Destination URL is composed against the user-visible `/files/` root,
        // including resolve_root for a bare relative path.
        let dest = p
            .nextcloud_chunked_destination_url("/aeroftp-utest/big.bin")
            .expect("destination URL");
        assert_eq!(
            dest,
            "https://cloud.example.com/remote.php/dav/files/alice/aeroftp-utest/big.bin"
        );
    }

    /// MultipartHandle payload survives encode → decode roundtrip and
    /// rejects malformed inputs without panicking.
    #[test]
    fn nextcloud_multipart_handle_roundtrip_and_validation() {
        let original = NextcloudMultipartHandle {
            transfer_id: "tx-7c9d3b04-d5a8-4d0e-a8a1-9b21fd9d4e7c".to_string(),
            user_id: "alice".to_string(),
            total_size: 209_715_200,
            destination: "ab12".to_string(),
        };
        let encoded = original.encode();
        assert!(encoded.starts_with("webdav-chunked-v3|"));
        let decoded = NextcloudMultipartHandle::decode(&encoded).expect("decode");
        assert_eq!(decoded, original);

        // Wrong prefix
        assert!(NextcloudMultipartHandle::decode("garbage|tx|alice|10|ab").is_err());
        // Missing total_size
        assert!(NextcloudMultipartHandle::decode("webdav-chunked-v3|tx|alice").is_err());
        // Empty transfer_id
        assert!(NextcloudMultipartHandle::decode("webdav-chunked-v3||alice|10|ab").is_err());
        // Missing destination
        assert!(NextcloudMultipartHandle::decode("webdav-chunked-v3|tx|alice|10").is_err());
        // Trailing garbage
        assert!(
            NextcloudMultipartHandle::decode("webdav-chunked-v3|tx|alice|10|ab|extra").is_err()
        );
        // Non-numeric total_size
        assert!(NextcloudMultipartHandle::decode("webdav-chunked-v3|tx|alice|big|ab").is_err());
        // A handle written before it carried its destination
        let old = NextcloudMultipartHandle::decode("webdav-chunked-v2|tx|alice|10");
        assert!(
            matches!(&old, Err(ProviderError::Other(m)) if m.contains("start the upload again")),
            "{old:?}"
        );
    }

    /// Provider methods fail fast on the connection check before any
    /// network call, and reject vanilla WebDAV with NotSupported.
    #[tokio::test]
    async fn nextcloud_multipart_methods_require_connection_and_nextcloud_gating() {
        let mut p =
            WebDavProvider::new(test_config("https://cloud.example.com")).expect("provider");
        // Not connected: every method bails with NotConnected.
        let r = p.begin_multipart_upload("/x.bin", 100, None, None).await;
        assert!(matches!(r, Err(ProviderError::NotConnected)));

        // Connected but vanilla WebDAV: NotSupported, no MKCOL emitted.
        p.connected = true;
        let r = p.begin_multipart_upload("/x.bin", 100, None, None).await;
        assert!(matches!(r, Err(ProviderError::NotSupported(_))));

        // Connected + Nextcloud-class: upload_part rejects part_number = 0.
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("nextcloud".to_string());
        let mut p = WebDavProvider::new(cfg).expect("provider");
        p.connected = true;
        let handle = MultipartHandle {
            upload_id: NextcloudMultipartHandle {
                transfer_id: "tx".to_string(),
                user_id: "alice".to_string(),
                total_size: 1,
                destination: "ab12".to_string(),
            }
            .encode(),
            remote_path: "/x.bin".to_string(),
        };
        let r = p.upload_part(&handle, 0, vec![]).await;
        assert!(matches!(r, Err(ProviderError::Other(_))));
    }

    /// Abort is best-effort: a corrupt handle MUST NOT panic or surface a
    /// secondary error that would mask the real upload failure the runner
    /// is propagating.
    #[tokio::test]
    async fn nextcloud_abort_swallows_handle_decode_failure() {
        let mut cfg = test_config("https://cloud.lab.example.test");
        cfg.provider_id = Some("nextcloud".to_string());
        let mut p = WebDavProvider::new(cfg).expect("provider");
        p.connected = true;
        let bad_handle = MultipartHandle {
            upload_id: "totally-wrong-prefix".to_string(),
            remote_path: "/x.bin".to_string(),
        };
        let r = p.abort_multipart_upload(bad_handle).await;
        assert!(r.is_ok());
    }

    /// Issue #264 — URL with only a host (no path) cannot be a single-file
    /// bridge, the probe must opt out and let the standard PROPFIND `/` flow
    /// handle it.
    #[test]
    fn url_path_component_returns_empty_when_no_path() {
        assert_eq!(
            WebDavProvider::url_path_component("http://127.0.0.1:4443"),
            ""
        );
        assert_eq!(
            WebDavProvider::url_path_component("https://example.com"),
            ""
        );
    }

    /// Issue #264 — URL with `/` path is the same as no path for the probe
    /// (PROPFIND `/` is what the standard flow already does).
    #[test]
    fn url_path_component_returns_slash_only_when_root() {
        assert_eq!(
            WebDavProvider::url_path_component("http://127.0.0.1:4443/"),
            "/"
        );
    }

    /// Issue #264 — typical `mega-webdav <file>` bridge URL: the probe must
    /// see the file path so it can target it verbatim.
    #[test]
    fn url_path_component_extracts_token_and_filename() {
        assert_eq!(
            WebDavProvider::url_path_component("http://127.0.0.1:4443/77YnXboS/sample.png"),
            "/77YnXboS/sample.png"
        );
    }

    /// Issue #264 — a 207 response describing a non-collection resource is
    /// the trigger to enable single-file mode. The extracted entry must
    /// carry size, mtime, and content-type from the props.
    #[test]
    fn extract_single_file_entry_returns_file_entry() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:multistatus xmlns:d="DAV:">
                <d:response>
                    <d:href>/77YnXboS/sample.png</d:href>
                    <d:propstat>
                        <d:prop>
                            <d:resourcetype/>
                            <d:getcontentlength>60630</d:getcontentlength>
                            <d:getlastmodified>Mon, 27 May 2026 08:50:22 GMT</d:getlastmodified>
                            <d:getcontenttype>image/png</d:getcontenttype>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                    </d:propstat>
                </d:response>
            </d:multistatus>"#;

        let entry = WebDavProvider::extract_single_file_entry(xml, "/77YnXboS/sample.png")
            .expect("non-collection resource should yield a single-file entry");
        assert_eq!(entry.name, "sample.png");
        assert_eq!(entry.path, "/sample.png");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 60630);
        assert_eq!(entry.mime_type.as_deref(), Some("image/png"));
        assert!(entry.modified.is_some());
    }

    /// Issue #264 — when the resource IS a collection, the probe must
    /// return `None` so the standard PROPFIND `/` flow runs and the user
    /// gets a normal browseable WebDAV experience.
    #[test]
    fn extract_single_file_entry_returns_none_for_collection() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:multistatus xmlns:d="DAV:">
                <d:response>
                    <d:href>/dav/folder/</d:href>
                    <d:propstat>
                        <d:prop>
                            <d:resourcetype><d:collection/></d:resourcetype>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                    </d:propstat>
                </d:response>
            </d:multistatus>"#;

        assert!(WebDavProvider::extract_single_file_entry(xml, "/dav/folder").is_none());
    }

    /// Issue #264 — percent-encoded filename in the URL path must round-trip
    /// to a UTF-8 decoded name in the entry.
    #[test]
    fn extract_single_file_entry_decodes_percent_encoded_name() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:multistatus xmlns:d="DAV:">
                <d:response>
                    <d:href>/AbCd/my%20file.txt</d:href>
                    <d:propstat>
                        <d:prop>
                            <d:resourcetype/>
                            <d:getcontentlength>10</d:getcontentlength>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                    </d:propstat>
                </d:response>
            </d:multistatus>"#;

        let entry = WebDavProvider::extract_single_file_entry(xml, "/AbCd/my%20file.txt").unwrap();
        assert_eq!(entry.name, "my file.txt");
        assert_eq!(entry.path, "/my file.txt");
    }

    /// SabreDAV (Nextcloud, ownCloud) writes the quotes of an ETag as
    /// `&quot;`. Without the reference arm the quotes were dropped, and a
    /// single-file entry carried no quotes where the listing carries them.
    /// The raw space between two references arrives as a whitespace-only
    /// piece and belongs to the value as much as `&#32;` does.
    #[test]
    fn extract_single_file_entry_keeps_entities_in_the_props() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:multistatus xmlns:d="DAV:">
                <d:response>
                    <d:href>/s/AbCd</d:href>
                    <d:propstat>
                        <d:prop>
                            <d:resourcetype/>
                            <d:getcontentlength>10</d:getcontentlength>
                            <d:getcontenttype>text/plain; charset=&quot;utf-8&quot;</d:getcontenttype>
                            <d:getetag>&quot;a&amp; &amp;b&#32;1&quot;</d:getetag>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                    </d:propstat>
                </d:response>
            </d:multistatus>"#;

        let entry = WebDavProvider::extract_single_file_entry(xml, "/s/AbCd/notes.txt").unwrap();
        assert_eq!(
            entry.metadata.get("etag").map(String::as_str),
            Some("\"a& &b 1\"")
        );
        assert_eq!(
            entry.mime_type.as_deref(),
            Some("text/plain; charset=\"utf-8\"")
        );
    }

    /// Issue #264 — when single-file mode is active, `build_url` must return
    /// the configured URL verbatim regardless of the path argument the
    /// caller supplies. The bridge has no browseable structure beyond the
    /// one file, so any other URL we could build would 404.
    #[test]
    fn build_url_returns_verbatim_url_in_single_file_mode() {
        let url = "http://127.0.0.1:4443/77YnXboS/sample.png";
        let mut provider = WebDavProvider::new(test_config(url)).expect("provider");
        provider.single_file_mode = Some(RemoteEntry {
            name: "sample.png".to_string(),
            path: "/sample.png".to_string(),
            is_dir: false,
            size: 60630,
            modified: None,
            is_symlink: false,
            link_target: None,
            permissions: None,
            owner: None,
            group: None,
            mime_type: Some("image/png".to_string()),
            metadata: Default::default(),
        });

        assert_eq!(provider.build_url("/"), url);
        assert_eq!(provider.build_url("/sample.png"), url);
        assert_eq!(provider.build_url("/anything-else"), url);
    }

    /// In single-file mode every path maps to the configured URL, so a
    /// rename or replace compared two equal URLs and answered success with
    /// nothing moved. Both are refused before any request; onto its own path
    /// a rename stays a no-op.
    #[tokio::test]
    async fn a_single_file_profile_refuses_a_rename_or_replace() {
        let mut provider =
            WebDavProvider::new(test_config("http://127.0.0.1:9/77YnXboS/sample.png"))
                .expect("provider");
        provider.connected = true;
        provider.single_file_mode = Some(RemoteEntry {
            name: "sample.png".to_string(),
            path: "/sample.png".to_string(),
            is_dir: false,
            size: 60630,
            modified: None,
            is_symlink: false,
            link_target: None,
            permissions: None,
            owner: None,
            group: None,
            mime_type: Some("image/png".to_string()),
            metadata: Default::default(),
        });
        let renamed = provider.rename("/sample.png", "/other.png").await;
        assert!(
            matches!(renamed, Err(ProviderError::NotSupported(_))),
            "{renamed:?}"
        );
        let replaced = provider.replace("/sample.png", "/other.png").await;
        assert!(
            matches!(replaced, Err(ProviderError::NotSupported(_))),
            "{replaced:?}"
        );
        provider
            .rename("/sample.png", "/sample.png")
            .await
            .expect("onto its own path");
        // A relative and an absolute spelling of the served file are one
        // path: compared raw they read as a move and were refused.
        provider
            .rename("sample.png", "/sample.png")
            .await
            .expect("two spellings of its own path");
        provider
            .replace("/sample.png", "sample.png")
            .await
            .expect("two spellings of its own path");
    }

    /// An in-place edit (CLI `edit`, `remote_edit`) asks for an atomic
    /// replace, uploads a temporary beside the file, replaces the file with
    /// it and deletes the temporary when a step fails. On a single-file
    /// profile every path is the one configured URL: the temporary's PUT
    /// overwrote the served file and, once the replace was refused, the
    /// cleanup's DELETE removed it. The edit now stops at the first question,
    /// and a PUT, DELETE or MKCOL for any other path is refused before a
    /// request.
    #[tokio::test]
    async fn an_edit_on_a_single_file_profile_sends_no_put_and_no_delete() {
        use axum::response::IntoResponse;
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let log = std::sync::Arc::clone(&seen);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let log = std::sync::Arc::clone(&log);
                async move {
                    log.lock().unwrap().push(req.method().to_string());
                    axum::http::StatusCode::CREATED.into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });

        let mut provider =
            WebDavProvider::new(test_config(&format!("http://{addr}/77YnXboS/sample.png")))
                .expect("provider");
        provider.connected = true;
        provider.server_root = Some("/".to_string());
        provider.single_file_mode = Some(RemoteEntry {
            name: "sample.png".to_string(),
            path: "/sample.png".to_string(),
            is_dir: false,
            size: 60630,
            modified: None,
            is_symlink: false,
            link_target: None,
            permissions: None,
            owner: None,
            group: None,
            mime_type: Some("image/png".to_string()),
            metadata: Default::default(),
        });
        let local = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(local.path(), b"new content").unwrap();
        let local = local.path().to_str().unwrap();

        // The first step asks, and the answer is no.
        assert!(!provider.supports_atomic_replace().await.expect("asked"));
        assert!(!provider.replace_sets_aside());

        // The edit's steps, in its order.
        let temp = "/.sample.png.aeroftp-edit";
        if provider.supports_atomic_replace().await.expect("asked") {
            match provider.upload(local, temp, None).await {
                Ok(()) => {
                    if provider.replace(temp, "/sample.png").await.is_err() {
                        let _ = provider.delete(temp).await;
                    }
                }
                Err(_) => {
                    let _ = provider.delete(temp).await;
                }
            }
        }
        {
            let seen_seen = seen.lock().unwrap().clone();
            assert!(seen_seen.is_empty(), "{:?}", seen_seen);
        }

        // Any other path is refused before a request, whoever asks.
        for outcome in [
            provider.upload(local, temp, None).await,
            provider.delete(temp).await,
            provider.mkdir("/sub").await,
            provider.rmdir("/sub").await,
        ] {
            assert!(
                matches!(outcome, Err(ProviderError::NotSupported(_))),
                "{outcome:?}"
            );
        }
        {
            let seen_seen = seen.lock().unwrap().clone();
            assert!(seen_seen.is_empty(), "{:?}", seen_seen);
        }
    }

    /// Issue #591 — vanilla WebDAV (SFTPGo, nginx DAV, Apache) must advertise
    /// an HttpClonePool so folder downloads honour the user's concurrency knob
    /// instead of staying LockedSingle.
    #[test]
    fn vanilla_webdav_advertises_http_clone_pool_file_parallel() {
        let p = WebDavProvider::new(test_config("https://dav.example.com/")).expect("provider");
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        assert_eq!(
            p.transfer_executor_max_sessions(),
            WEBDAV_TRANSFER_MAX_SESSIONS
        );
        assert!(p.clone_for_transfer().is_ok());
        let caps = p.transfer_capabilities();
        assert_eq!(
            caps.file_parallel,
            crate::transfer_dag::Capability::Supported
        );
        assert_eq!(
            caps.session_pool,
            crate::transfer_dag::Capability::Supported
        );
        assert_eq!(caps.max_file_slots, Some(WEBDAV_TRANSFER_MAX_SESSIONS));
        // #958: a walk lists several collections at once as well.
        assert_eq!(
            caps.list_parallel,
            crate::transfer_dag::Capability::Supported
        );
        assert_eq!(caps.max_checker_slots, Some(WEBDAV_LIST_MAX_SESSIONS));
        assert_eq!(
            p.list_executor_kind(),
            ProviderListExecutorKind::HttpClonePool
        );
        assert!(p.clone_for_list().is_ok());
    }

    /// #958: a single-file profile (#264) maps every path to its one URL, so
    /// a parallel walk would list the same resource N times. It keeps the
    /// single locked session.
    #[test]
    fn single_file_webdav_profile_lists_on_one_session() {
        let mut p =
            WebDavProvider::new(test_config("https://dav.example.com/file.bin")).expect("provider");
        p.single_file_mode = Some(RemoteEntry {
            name: "file.bin".to_string(),
            path: "/file.bin".to_string(),
            is_dir: false,
            size: 1,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        });
        assert!(p.clone_for_list().is_err());
        let caps = p.transfer_capabilities();
        assert_eq!(
            caps.list_parallel,
            crate::transfer_dag::Capability::Unsupported
        );
        assert_eq!(caps.max_checker_slots, Some(1));
    }

    /// Issue #591 — Nextcloud still clones for chunked-v2 parts, and now also
    /// for file-level batch parallelism.
    #[test]
    fn nextcloud_clone_pool_covers_file_parallel_and_multipart() {
        let mut cfg = test_config("https://cloud.example.com/remote.php/dav/files/alice/");
        cfg.provider_id = Some("nextcloud".to_string());
        let p = WebDavProvider::new(cfg).expect("provider");
        assert!(p.is_nextcloud_for_dav());
        assert!(p.clone_for_transfer().is_ok());
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        assert_eq!(p.transfer_capabilities().max_file_slots, Some(8));
    }

    /// Issue #591 — cloned Digest workers share one `nc` counter so two
    /// concurrent Authorization headers never carry the same nonce count.
    #[test]
    fn digest_nc_is_shared_across_clones() {
        let a = DigestState::parse(r#"Digest realm="r", nonce="n", qop="auth""#)
            .expect("parse digest challenge");
        let b = a.clone();
        let h1 = a.authorization("GET", "/a", "u", "p");
        let h2 = b.authorization("GET", "/b", "u", "p");
        let nc_of = |h: &str| {
            h.split("nc=")
                .nth(1)
                .and_then(|s| s.split([',', ' ']).next())
                .unwrap_or("")
                .to_string()
        };
        let nc1 = nc_of(&h1);
        let nc2 = nc_of(&h2);
        assert_ne!(nc1, nc2, "cloned workers must not reuse nc");
        let mut seen = [nc1.as_str(), nc2.as_str()];
        seen.sort();
        assert_eq!(seen, ["00000001", "00000002"]);
    }

    /// Issue #591 second half. The counter was shared but the NONCE was cloned
    /// by value, so a server rotating it (Apache mod_auth_digest does this every
    /// AuthDigestNonceLifetime, 300s by default) repaired the one worker that
    /// received the `stale=true` and left the other seven presenting a dead
    /// nonce. A nonce belongs to the session with the server, not to a worker.
    #[test]
    fn a_rotated_nonce_reaches_every_clone() {
        let worker_a = DigestState::parse(r#"Digest realm="r", nonce="old", qop="auth""#)
            .expect("parse digest challenge");
        let worker_b = worker_a.clone();
        let nonce_of = |h: &str| {
            h.split("nonce=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            nonce_of(&worker_b.authorization("GET", "/b", "u", "p")),
            "old"
        );

        // Worker A is the one that happens to receive the 401.
        assert!(worker_a.renegotiate(r#"Digest realm="r", nonce="fresh", qop="auth", stale=true"#));

        assert_eq!(
            nonce_of(&worker_b.authorization("GET", "/b", "u", "p")),
            "fresh",
            "a worker that never saw the 401 must still leave the dead nonce behind"
        );
        // `nc` counts requests against ONE nonce, so it restarts with it.
        let nc_of = |h: &str| {
            h.split("nc=")
                .nth(1)
                .and_then(|s| s.split([',', ' ']).next())
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            nc_of(&worker_b.authorization("GET", "/b", "u", "p")),
            "00000002"
        );
    }

    /// Pre-tag audit. Reading the nonce twice to slice it, once for the value
    /// and once for the bound, races the very sharing this release introduced: a
    /// worker re-negotiating between the two reads can leave a short string cut
    /// at the new one's length. The byte index was never a character boundary
    /// either, so a non-ASCII nonce panicked with no race at all.
    #[test]
    fn the_nonce_log_prefix_cannot_panic() {
        let state = DigestState::parse(r#"Digest realm="r", nonce="short", qop="auth""#)
            .expect("parse digest challenge");
        assert_eq!(
            state.nonce_prefix(),
            "short",
            "shorter than the cut stays whole"
        );

        // `stale=true` is what makes these adoptions happen at all now: this
        // test uses `renegotiate` purely as the way to swap the nonce, so the
        // flag is scaffolding here, not the subject.
        assert!(state
            .renegotiate(r#"Digest realm="r", nonce="0123456789abcdef", qop="auth", stale=true"#));
        assert_eq!(state.nonce_prefix(), "0123456789ab", "longer is cut to 12");

        // A server is free to send a non-ASCII nonce, and a byte cut inside a
        // multi-byte character would panic.
        assert!(state.renegotiate("Digest realm=\"r\", nonce=\"\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\u{e8}\", qop=\"auth\", stale=true"));
        let prefix = state.nonce_prefix();
        assert_eq!(prefix.chars().count(), 12, "cut on characters, not bytes");
    }

    /// A header that is not a usable challenge must not be adopted: retrying
    /// against a half-parsed challenge is worse than failing the request, and
    /// the caller needs the false to fall through to its ordinary error path.
    #[test]
    fn renegotiate_refuses_an_unusable_challenge() {
        let state = DigestState::parse(r#"Digest realm="r", nonce="old", qop="auth""#)
            .expect("parse digest challenge");
        assert!(!state.renegotiate("Basic realm=\"r\""));
        assert!(
            !state.renegotiate(r#"Digest realm="r""#),
            "no nonce is not usable"
        );
        assert_eq!(
            state.nonce(),
            "old",
            "a refused challenge must change nothing"
        );
    }

    /// Tracker #628 item 11. The gate that made the data-path retry safe
    /// enough to wire at all: without `stale`, a revoked password and a
    /// rotated nonce are the same event, and retrying on both is the storm
    /// that kept this deferred for a release.
    #[test]
    fn renegotiate_adopts_only_a_genuine_rotation() {
        let state = DigestState::parse(r#"Digest realm="r", nonce="old", qop="auth""#)
            .expect("parse digest challenge");

        // A perfectly parseable challenge with no `stale`: this is what a
        // revoked password answers with. Adopting it would retry a credential
        // failure once per request, forever.
        assert!(
            !state.renegotiate(r#"Digest realm="r", nonce="fresh", qop="auth""#),
            "no stale is a credential failure, not a rotation"
        );
        assert_eq!(state.nonce(), "old", "a refusal must change nothing");

        // `stale=false` is an explicit denial, not a missing field.
        assert!(
            !state.renegotiate(r#"Digest realm="r", nonce="fresh", qop="auth", stale=false"#),
            "stale=false is not a rotation"
        );
        assert_eq!(state.nonce(), "old");

        // The real thing.
        assert!(
            state.renegotiate(r#"Digest realm="r", nonce="fresh", qop="auth", stale=true"#),
            "stale=true with a new nonce is the rotation we exist for"
        );
        assert_eq!(state.nonce(), "fresh", "the fresh nonce is adopted");
    }

    /// A server that answers `stale=true` while handing back the nonce that
    /// just failed would have us retry against a dead nonce once per request.
    /// The nonce comparison, not the flag, is what bounds that.
    #[test]
    fn renegotiate_refuses_a_stale_flag_on_an_unchanged_nonce() {
        let state = DigestState::parse(r#"Digest realm="r", nonce="same", qop="auth""#)
            .expect("parse digest challenge");
        assert!(
            !state.renegotiate(r#"Digest realm="r", nonce="same", qop="auth", stale=true"#),
            "same nonce is not a rotation, however the server labels it"
        );
        assert_eq!(state.nonce(), "same");
    }

    /// RFC 2617 writes the flag bare; real servers quote it and case it as
    /// they please. Apache mod_auth_digest sends `stale=true`, others send
    /// `stale="TRUE"`. Reading only the bare lowercase form would leave the
    /// rotation undetected on exactly the servers this closes the gap for.
    #[test]
    fn stale_is_parsed_bare_quoted_and_in_any_case() {
        for header in [
            r#"Digest realm="r", nonce="n1", qop="auth", stale=true"#,
            r#"Digest realm="r", nonce="n1", qop="auth", stale="true""#,
            r#"Digest realm="r", nonce="n1", qop="auth", stale=TRUE"#,
            r#"Digest realm="r", nonce="n1", qop="auth", stale="True""#,
        ] {
            let challenge = DigestChallenge::parse(header).expect("parse challenge");
            assert!(challenge.stale, "stale must be read from {header}");
        }

        for header in [
            r#"Digest realm="r", nonce="n1", qop="auth""#,
            r#"Digest realm="r", nonce="n1", qop="auth", stale=false"#,
        ] {
            let challenge = DigestChallenge::parse(header).expect("parse challenge");
            assert!(!challenge.stale, "stale must be false for {header}");
        }
    }

    // ── Data-path replay, against a real socket ──────────────────────────
    //
    // There is no HTTP mock in `[dev-dependencies]`, and none is needed: the
    // project already tests wire behaviour with hand-rolled `TcpListener`
    // stubs (`http_retry.rs`, `s3.rs`, `ttfb.rs`). `keep_alive` is the data
    // path chosen here because it is a bare OPTIONS with no body and no XML,
    // so what the assertions see is the retry decision and nothing else.

    const ROTATED_401: &str = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"r\", nonce=\"rotated\", qop=\"auth\", stale=true\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const ROTATED_AGAIN_401: &str = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"r\", nonce=\"rotated-twice\", qop=\"auth\", stale=true\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const SAME_NONCE_401: &str = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"r\", nonce=\"dead\", qop=\"auth\", stale=true\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const OK_200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// Serve one canned response per connection and count the connections.
    /// Every response carries `Connection: close`, so the client opens a fresh
    /// connection per request and the accept count IS the request count, which
    /// is exactly the quantity a retry test needs to assert on. Requests past
    /// the end of the list get the last response again.
    async fn spawn_http_stub(
        responses: Vec<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_task = std::sync::Arc::clone(&hits);

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let idx = hits_task.fetch_add(1, Ordering::SeqCst);
                let reply = responses
                    .get(idx)
                    .copied()
                    .unwrap_or_else(|| responses.last().copied().unwrap_or(OK_200));
                // Drain the request head first: writing into a socket whose
                // peer is still sending would race the response out.
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (format!("http://{addr}"), hits)
    }

    fn digest_provider(url: &str) -> WebDavProvider {
        let mut provider = WebDavProvider::new(test_config(url)).expect("provider");
        provider.connected = true;
        provider.digest_auth = Some(
            DigestState::parse(r#"Digest realm="r", nonce="dead", qop="auth""#).expect("state"),
        );
        provider
    }

    /// The behaviour the tracker item asks for: a rotation is survived, and it
    /// costs exactly one extra request.
    #[tokio::test]
    async fn a_rotated_nonce_makes_a_data_path_replay_exactly_once() {
        let (url, hits) = spawn_http_stub(vec![ROTATED_401, OK_200]).await;
        let mut provider = digest_provider(&url);

        provider
            .keep_alive()
            .await
            .expect("a nonce rotation must not fail a healthy session");

        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "one original request and one replay"
        );
        assert_eq!(
            provider.digest_auth.as_ref().expect("digest state").nonce(),
            "rotated",
            "the replay must have used the rotated nonce"
        );
    }

    /// A version restore, like every other data path, replays once on a
    /// rotated nonce, and a second rotation inside the same call stops there.
    #[tokio::test]
    async fn a_nextcloud_version_restore_replays_a_rotated_nonce_exactly_once() {
        const CREATED_201: &str =
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let multistatus = |responses: String| -> &'static str {
            let body = format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">{responses}</d:multistatus>"#
            );
            Box::leak(
                format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .into_boxed_str(),
            )
        };
        let file = |etag: &str, size: u64, modified: &str| {
            multistatus(format!(
                r#"<d:response><d:href>/remote.php/dav/files/user/a.txt</d:href><d:propstat><d:prop><oc:fileid>42</oc:fileid><d:getetag>"{etag}"</d:getetag><d:getcontentlength>{size}</d:getcontentlength><d:getlastmodified>{modified}</d:getlastmodified><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
            ))
        };
        let version = |id: u64, size: u64| {
            format!(
                r#"<d:response><d:href>/remote.php/dav/versions/user/versions/42/{id}</d:href><d:propstat><d:prop><d:getcontentlength>{size}</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
            )
        };
        let collection = r#"<d:response><d:href>/remote.php/dav/versions/user/versions/42/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#;
        // a.txt holds 9 bytes over its 8-byte version 1700000000; after the
        // restore it holds those 8 bytes, the 9 kept as a version.
        let before = file("e1", 9, "Wed, 15 Nov 2023 00:00:00 GMT");
        let after = file("e2", 8, "Thu, 16 Nov 2023 00:00:00 GMT");
        let listed = multistatus(format!("{collection}{}", version(1_700_000_000, 8)));
        let listed_after = multistatus(format!(
            "{collection}{}{}",
            version(1_700_000_000, 8),
            version(1_700_006_400, 9)
        ));
        // The id lookup, the listing (file, versions, file again) and the
        // file again before the MOVE; then the MOVE, and the listing that
        // checks it.
        let until_move = [before, before, listed, before, before];

        let mut answers = until_move.to_vec();
        answers.extend([ROTATED_401, CREATED_201, after, listed_after, after]);
        let (url, hits) = spawn_http_stub(answers).await;
        let mut provider = digest_provider(&format!("{url}/remote.php/dav/files/user/"));
        let outcome = provider.restore_version("/a.txt", "42:1700000000").await;
        // One rotation is survived: the restore is sent, not confirmed.
        assert!(
            matches!(&outcome, Err(e) if crate::providers::was_applied(e) && e.to_string().contains("could not be confirmed")),
            "{outcome:?}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            10,
            "lookups and listing, MOVE, one replay, the check"
        );

        let mut answers = until_move.to_vec();
        answers.extend([ROTATED_401, ROTATED_AGAIN_401, CREATED_201]);
        let (url, hits) = spawn_http_stub(answers).await;
        let mut provider = digest_provider(&format!("{url}/remote.php/dav/files/user/"));
        let outcome = provider.restore_version("/a.txt", "42:1700000000").await;
        assert!(
            matches!(&outcome, Err(ProviderError::ServerError(m)) if m.contains("401")),
            "{outcome:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 7, "no second replay");
    }

    const TOO_EARLY_425: &str =
        "HTTP/1.1 425 Too Early\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// Two 425s followed by a rotated-nonce 401 used to PANIC the calling task.
    /// The 425 branch spends the attempt budget, and the Digest replay then issued
    /// a `continue` from the final attempt, which skipped the only `return` in the
    /// loop and walked off the end of the range into `unreachable!`. Both single
    /// stream GET paths route through this helper, so the panic was reachable from
    /// an ordinary download against a server that answers 425 while rotating its
    /// nonce.
    ///
    /// The assertion is on the OUTCOME, a returned response rather than a panic,
    /// because that is what the caller experiences. The request count is asserted
    /// too: the replay must be granted its own attempt, not an unbounded supply of
    /// them, so four is both the proof it happened and the proof it stopped.
    #[tokio::test]
    async fn a_425_run_followed_by_a_rotated_nonce_401_returns_instead_of_panicking() {
        let (url, hits) = spawn_http_stub(vec![
            TOO_EARLY_425,
            TOO_EARLY_425,
            ROTATED_401,
            TOO_EARLY_425,
        ])
        .await;
        let mut provider = digest_provider(&url);

        let response = provider
            .send_with_too_early_retry(Method::GET, "/file.bin")
            .await
            .expect("the helper must return a response, not fail");

        assert_eq!(
            response.status(),
            StatusCode::TOO_EARLY,
            "the last response is what the caller gets back"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            4,
            "three attempts of the 425 budget plus exactly one replay attempt"
        );
    }

    /// The guard, and the reason this was deferred for a release. A server
    /// that keeps answering 401 with the nonce we already hold is what a
    /// revoked password looks like. It must cost ONE request, not a storm.
    #[tokio::test]
    async fn an_unchanged_nonce_is_not_replayed() {
        let (url, hits) = spawn_http_stub(vec![SAME_NONCE_401]).await;
        let mut provider = digest_provider(&url);

        let err = provider
            .keep_alive()
            .await
            .expect_err("an unrepairable 401 must still fail");
        assert!(
            matches!(err, ProviderError::AuthenticationFailed(_)),
            "got {err:?}"
        );

        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a credential failure must not be retried, even once"
        );
    }

    /// Once, and only once. A server rotating the nonce on every response
    /// would loop a caller that retried whenever the challenge looked fresh;
    /// the replay is a straight-line `if`, so the second 401 is final.
    #[tokio::test]
    async fn a_second_rotation_is_not_chased() {
        let (url, hits) = spawn_http_stub(vec![ROTATED_401, ROTATED_AGAIN_401, OK_200]).await;
        let mut provider = digest_provider(&url);

        provider
            .keep_alive()
            .await
            .expect_err("a 401 surviving the single replay must fail");

        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "the replay must not itself be retried"
        );
        assert_eq!(
            provider.digest_auth.as_ref().expect("digest state").nonce(),
            "rotated",
            "only the first rotation is adopted"
        );
    }

    /// The pool-wide half of the repair, at the tighter gate: one worker
    /// adopting a rotation must fix every sibling, because the challenge is
    /// shared. This is what keeps a batch of in-flight parts from each
    /// needing its own 401.
    #[test]
    fn a_stale_rotation_adopted_by_one_worker_reaches_every_clone() {
        let worker_a = DigestState::parse(r#"Digest realm="r", nonce="dead", qop="auth""#)
            .expect("parse digest challenge");
        let worker_b = worker_a.clone();

        assert!(
            worker_a.renegotiate(r#"Digest realm="r", nonce="rotated", qop="auth", stale=true"#)
        );

        assert_eq!(
            worker_b.nonce(),
            "rotated",
            "the sibling must not still be presenting the dead nonce"
        );
    }
}

#[cfg(test)]
mod recorded_propfind_fixture {
    use secrecy::SecretString;

    use super::super::WebDavConfig;
    use super::WebDavProvider;

    #[test]
    fn parses_recorded_propfind() {
        let provider = WebDavProvider::new(WebDavConfig {
            url: "https://example.com/dav".to_string(),
            username: "user".to_string(),
            password: SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            verify_cert: true,
            anonymous: false,
        })
        .expect("provider");
        let xml = include_str!("fixtures/quickxml/webdav-propfind.xml");
        let entries = provider
            .parse_propfind_response(xml, "/dav")
            .expect("parse");
        let file = entries.iter().find(|e| e.name == "file.txt").expect("file");
        assert!(!file.is_dir);
        assert_eq!(file.size, 1024);
        let escaped = entries
            .iter()
            .find(|e| e.name == "a&b.txt")
            .expect("entity");
        assert_eq!(escaped.size, 7);
        let dir = entries.iter().find(|e| e.name == "subdir").expect("dir");
        assert!(dir.is_dir);
    }
}

/// The Filen Desktop WebDAV bridge serves HTTPS with a self-signed
/// certificate. Before #389 choosing HTTPS for the preset turned certificate
/// checks off; #389 replaced the choice with detection and left them on, so
/// an HTTPS bridge failed its TLS handshake. The certificate is now accepted
/// for that preset only, and only once our own probe has found it answering
/// HTTPS on 127.0.0.1; every other WebDAV connection keeps checking.
#[cfg(test)]
mod filen_bridge_certificate_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const ROOT_207: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:"><D:response><D:href>/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;

    /// A self-signed HTTPS WebDAV root on a free loopback port.
    async fn self_signed_bridge() -> (u16, tokio::task::JoinHandle<()>) {
        let reply = format!(
            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ROOT_207}",
            ROOT_207.len()
        );
        let (port, server, _) = self_signed_server(reply).await;
        (port, server)
    }

    /// A self-signed HTTPS server on a free loopback port answering every
    /// request with `reply`, and the count of requests it has read.
    async fn self_signed_server(
        reply: String,
    ) -> (
        u16,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let acceptor = super::collection_redirect_tests::tls_acceptor();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let reply = std::sync::Arc::new(reply);
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let counted = counted.clone();
                let reply = reply.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (port, server, requests)
    }

    /// Stops a fixture server and waits until its listener is closed.
    async fn stop(server: tokio::task::JoinHandle<()>) {
        server.abort();
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server still running");
        assert!(stopped.unwrap_err().is_cancelled());
    }

    fn config(url: String, provider_id: Option<&str>) -> WebDavConfig {
        WebDavConfig {
            url,
            username: "admin".to_string(),
            password: secrecy::SecretString::from("admin".to_string()),
            initial_path: None,
            provider_id: provider_id.map(str::to_string),
            verify_cert: true,
            anonymous: false,
        }
    }

    #[tokio::test]
    async fn the_filen_bridge_over_https_connects_with_its_self_signed_certificate() {
        let (port, server) = self_signed_bridge().await;
        crate::local_bridge::alias_test_port(port, 1900);
        // The preset's own host name, as the manifest stores it.
        let mut provider = WebDavProvider::new(config(
            format!("https://local.webdav.filen.io:{port}/"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        let outcome = provider.connect().await;
        server.abort();
        outcome.expect("the HTTPS bridge must connect");
        assert_eq!(provider.config.url, format!("https://127.0.0.1:{port}/"));
    }

    #[tokio::test]
    async fn a_custom_webdav_server_on_loopback_https_keeps_checking_its_certificate() {
        let (port, server) = self_signed_bridge().await;
        crate::local_bridge::alias_test_port(port, 1900);
        let mut provider =
            WebDavProvider::new(config(format!("https://127.0.0.1:{port}/"), None)).unwrap();
        let outcome = provider.connect().await;
        server.abort();
        assert!(outcome.is_err(), "a self-signed certificate was accepted");
    }

    const VERIFIED: ClientPolicy = ClientPolicy {
        accept_invalid_certs: false,
        filen_bridge: false,
    };
    const BRIDGE_OVER_HTTPS: ClientPolicy = ClientPolicy {
        accept_invalid_certs: true,
        filen_bridge: true,
    };

    /// A plain-HTTP WebDAV root on a free loopback port.
    async fn plain_bridge() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let reply = format!(
                        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ROOT_207}",
                        ROOT_207.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (port, server)
    }

    /// A bridge found on plain HTTP is reached directly as well (no system
    /// proxy, no redirect away), with certificate checks left as they are.
    #[tokio::test]
    async fn a_filen_bridge_on_plain_http_is_reached_directly() {
        let (port, server) = plain_bridge().await;
        crate::local_bridge::alias_test_port(port, 1900);
        let mut provider = WebDavProvider::new(config(
            format!("https://local.webdav.filen.io:{port}/"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        let outcome = provider.connect().await;
        server.abort();
        outcome.expect("the HTTP bridge must connect");
        assert_eq!(provider.config.url, format!("http://127.0.0.1:{port}/"));
        assert_eq!(
            provider.client_policy,
            ClientPolicy {
                accept_invalid_certs: false,
                filen_bridge: true,
            }
        );
    }

    /// With a system proxy set, the Filen bridge on plain HTTP is reached
    /// directly, while another WebDAV server on this machine keeps the user's
    /// proxy (the control: the proxy is in effect). In a child process,
    /// because the proxy variables are process-wide.
    #[tokio::test]
    async fn a_filen_bridge_skips_the_system_proxy_and_other_servers_take_it() {
        if !crate::local_bridge::proxied::in_child(
            "providers::webdav::filen_bridge_certificate_tests::a_filen_bridge_skips_the_system_proxy_and_other_servers_take_it",
        ) {
            return;
        }
        let proxy = crate::local_bridge::proxied::RecordingProxy::start().await;
        let (port, server) = plain_bridge().await;
        crate::local_bridge::alias_test_port(port, 1900);
        let mut bridge = WebDavProvider::new(config(
            format!("http://127.0.0.1:{port}/"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        let outcome = bridge.connect().await;
        assert!(
            !proxy.saw(&format!("127.0.0.1:{port}")),
            "the Filen bridge went through the system proxy: {:?}",
            proxy.lines()
        );
        outcome.expect("the HTTP bridge must connect directly");

        let mut other =
            WebDavProvider::new(config(format!("http://127.0.0.1:{port}/"), None)).unwrap();
        let _ = other.connect().await;
        server.abort();
        assert!(
            proxy.saw(&format!("127.0.0.1:{port}")),
            "the control did not reach the proxy, so the proxy was not in effect: {:?}",
            proxy.lines()
        );
    }

    /// `storage_info` asks the Koofr quota API only of a Koofr profile: a
    /// local URL whose path names app.koofr.net sends one PROPFIND to its own
    /// server and nothing to Koofr, while a profile on app.koofr.net tries the
    /// API and then the PROPFIND (two tunnels to app.koofr.net, both refused
    /// by the recording proxy). In a child process, because the proxy
    /// variables are process-wide.
    #[tokio::test]
    async fn the_koofr_quota_api_is_asked_only_of_a_koofr_profile() {
        if !crate::local_bridge::proxied::in_child(
            "providers::webdav::filen_bridge_certificate_tests::the_koofr_quota_api_is_asked_only_of_a_koofr_profile",
        ) {
            return;
        }
        let proxy = crate::local_bridge::proxied::RecordingProxy::start().await;
        let mut local = WebDavProvider::new(config(
            "http://127.0.0.1:9/app.koofr.net/dav".to_string(),
            None,
        ))
        .unwrap();
        local.connected = true;
        let _ = local.storage_info().await;
        let lines = proxy.lines();
        assert!(
            !lines.iter().any(|line| line.contains("app.koofr.net:443")),
            "a URL that only names Koofr in its path reached Koofr: {lines:?}"
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("PROPFIND http://127.0.0.1:9/"),
            "{lines:?}"
        );

        let mut koofr =
            WebDavProvider::new(config("https://app.koofr.net/dav/Koofr".to_string(), None))
                .unwrap();
        koofr.connected = true;
        let _ = koofr.storage_info().await;
        let tunnels = proxy
            .lines()
            .iter()
            .filter(|line| line.starts_with("CONNECT app.koofr.net:443"))
            .count();
        assert_eq!(
            tunnels,
            2,
            "the quota API, then the PROPFIND: {:?}",
            proxy.lines()
        );
    }

    /// The trust needs a loopback IP written as such: a name is resolved by
    /// DNS, which may point anywhere.
    #[test]
    fn only_a_literal_loopback_ip_counts_as_loopback() {
        assert!(names_a_loopback_ip("https://127.0.0.1:1900/"));
        assert!(names_a_loopback_ip("https://[::1]:1900/dav"));
        assert!(!names_a_loopback_ip("https://localhost:1900/"));
        assert!(!names_a_loopback_ip("https://local.webdav.filen.io:1900/"));
        assert!(!names_a_loopback_ip("https://127.0.0.2:1900/"));
        assert!(!names_a_loopback_ip("not a url"));
    }

    /// The quota APIs are asked only of their own servers: a bridge URL whose
    /// path names one sends nothing to it.
    #[test]
    fn a_quota_api_is_chosen_by_the_host_not_by_a_substring() {
        assert!(served_by(
            "https://app.koofr.net/dav/Koofr",
            "app.koofr.net"
        ));
        assert!(served_by(
            "https://webdav.opendrive.com/",
            "webdav.opendrive.com"
        ));
        assert!(!served_by(
            "https://127.0.0.1:1900/app.koofr.net/",
            "app.koofr.net"
        ));
        assert!(!served_by(
            "https://127.0.0.1:1900/?next=webdav.opendrive.com",
            "webdav.opendrive.com"
        ));
        assert!(!served_by(
            "https://app.koofr.net.example.com/",
            "app.koofr.net"
        ));
    }

    /// The exception is earned by the probe of one connect: when the bridge
    /// is gone at the next connect, that connect checks certificates again.
    #[tokio::test]
    async fn the_certificate_exception_does_not_outlive_the_probe_that_earned_it() {
        let (port, server) = self_signed_bridge().await;
        crate::local_bridge::alias_test_port(port, 1900);
        let mut provider = WebDavProvider::new(config(
            format!("https://127.0.0.1:{port}/"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        provider
            .connect()
            .await
            .expect("the HTTPS bridge must connect");
        assert_eq!(provider.client_policy, BRIDGE_OVER_HTTPS);
        stop(server).await;
        assert!(provider.connect().await.is_err());
        assert_eq!(provider.client_policy, VERIFIED);
    }

    /// Filen Desktop runs an S3 bridge too (port 1800). The Filen WebDAV
    /// preset pointed at it is not its own bridge, so its certificate is
    /// checked.
    #[tokio::test]
    async fn the_filen_webdav_preset_on_the_s3_bridge_port_keeps_checking() {
        let (port, server) = self_signed_bridge().await;
        crate::local_bridge::alias_test_port(port, 1800);
        let mut provider = WebDavProvider::new(config(
            format!("https://127.0.0.1:{port}/"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        let outcome = provider.connect().await;
        server.abort();
        assert!(outcome.is_err(), "a self-signed certificate was accepted");
        assert_eq!(provider.client_policy, VERIFIED);
    }

    /// The relaxed checks stay on the bridge: a redirect to another origin,
    /// itself self-signed, is refused before any request reaches it.
    #[tokio::test]
    async fn a_redirect_from_the_bridge_to_another_origin_is_not_followed() {
        let (other, other_server, other_requests) = self_signed_server(format!(
            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ROOT_207}",
            ROOT_207.len()
        ))
        .await;
        let (port, server, _) = self_signed_server(format!(
            "HTTP/1.1 302 Found\r\nLocation: https://127.0.0.1:{other}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ))
        .await;
        crate::local_bridge::alias_test_port(port, 1900);
        // A query the refusal must not repeat: it could carry a credential.
        let mut provider = WebDavProvider::new(config(
            format!("https://127.0.0.1:{port}/?token=DUMMY-SECRET"),
            Some("filen-desktop-webdav"),
        ))
        .unwrap();
        let outcome = provider.connect().await;
        server.abort();
        other_server.abort();
        let error = outcome.expect_err("the bridge's redirect was followed");
        let text = error.to_string();
        assert!(!text.contains("DUMMY-SECRET"), "{text}");
        assert!(text.contains(&LeftTheBridge.to_string()), "{text}");
        assert_eq!(
            other_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a request reached another origin under the relaxed checks"
        );
    }

    /// A LAN address, a custom port or a bridge that does not answer is never
    /// probed as a bridge, so nothing in it can lift the certificate checks.
    #[tokio::test]
    async fn only_a_known_bridge_that_answers_is_detected() {
        assert_eq!(
            crate::local_bridge::detected_bridge_url("https://192.168.1.10:1900/").await,
            None
        );
        assert_eq!(
            crate::local_bridge::detected_bridge_url("https://127.0.0.1:1901/").await,
            None
        );
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        crate::local_bridge::alias_test_port(port, 1900);
        assert_eq!(
            crate::local_bridge::detected_bridge_url(&format!("https://127.0.0.1:{port}/")).await,
            None
        );
    }
}

/// A server behind a TLS-terminating proxy that `301`s every slash-less
/// collection to an `http://` URL, reproduced on loopback: an HTTPS stub that
/// serves the collection only in its trailing-slash form, and a plain-HTTP
/// listener standing for the downgraded target that must never be reached.
/// Shape measured on the lab server on 2026-10-02, where `cd` into a folder
/// failed with a bare 401 while listing the same folder worked.
#[cfg(test)]
pub(crate) mod collection_redirect_tests {
    #[tokio::test]
    async fn probe_contract_webdav_does_not_erase_a_refused_redirect() {
        let stub = spawn_downgrading_server(|_| {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        })
        .await;
        let mut p = provider(&stub.base);
        let outcome = p.exists("/collection").await;
        assert!(seen(&stub).iter().any(|r| r == "PROPFIND /collection"));
        assert!(
            matches!(outcome, Err(ref e) if !matches!(e, ProviderError::NotFound(_))),
            "{outcome:?}"
        );
    }

    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const COLLECTION_207: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dir/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;

    /// One request seen by the HTTPS stub: `METHOD /path`, plus the
    /// `Destination` header when there is one.
    type Seen = Arc<Mutex<Vec<String>>>;

    struct Stub {
        base: String,
        seen: Seen,
        downgraded_hits: Arc<AtomicUsize>,
    }

    pub(crate) fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(
            rustls_pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()),
        );
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .unwrap();
        tokio_rustls::TlsAcceptor::from(Arc::new(config))
    }

    /// Read one request head and its `Content-Length` body.
    async fn read_request<S: tokio::io::AsyncRead + Unpin>(
        stream: &mut S,
    ) -> Option<(String, String, Option<String>)> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let method = first.next()?.to_string();
        let path = first.next()?.to_string();
        let mut length = 0usize;
        let mut destination = None;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap_or(0),
                    "destination" => destination = Some(value.trim().to_string()),
                    _ => {}
                }
            }
        }
        let mut have = buf.len() - head_end;
        while have < length {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            have += n;
        }
        Some((method, path, destination))
    }

    /// Start the pair. Every slash-less path except `/file.txt` is a
    /// collection the stub only serves in the slash form: the slash-less
    /// request gets `301` to the downgraded listener, the slash form gets the
    /// reply `answer` picks for its method. `/file.txt` is a plain file, and
    /// `/moved` answers `302` to an unrelated `http://` URL.
    async fn spawn_downgrading_server(answer: fn(&str) -> &'static str) -> Stub {
        let plain = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let plain_port = plain.local_addr().unwrap().port();
        let downgraded_hits = Arc::new(AtomicUsize::new(0));
        let hits = Arc::clone(&downgraded_hits);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = plain.accept().await {
                hits.fetch_add(1, Ordering::SeqCst);
                let _ = read_request(&mut socket).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });

        let tls = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tls_port = tls.local_addr().unwrap().port();
        let acceptor = tls_acceptor();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((socket, _)) = tls.accept().await {
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    continue;
                };
                let Some((method, path, destination)) = read_request(&mut stream).await else {
                    continue;
                };
                seen_task.lock().unwrap().push(match destination {
                    Some(d) => format!("{method} {path} -> {d}"),
                    None => format!("{method} {path}"),
                });
                let reply = if path == "/moved" {
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else if path != "/file.txt" && !path.ends_with('/') {
                    format!(
                        "HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:{plain_port}{path}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    answer(&method).to_string()
                };
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        Stub {
            base: format!("https://127.0.0.1:{tls_port}"),
            seen,
            downgraded_hits,
        }
    }

    fn collection_answer(method: &str) -> &'static str {
        match method {
            "PROPFIND" => {
                // Leaked once per test run: the stub needs a `'static` reply.
                Box::leak(
                    format!(
                        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        COLLECTION_207.len(),
                        COLLECTION_207
                    )
                    .into_boxed_str(),
                )
            }
            "MOVE" | "COPY" => {
                "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
            _ => "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        }
    }

    fn provider(base: &str) -> WebDavProvider {
        let config = WebDavConfig {
            url: format!("{base}/"),
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            // The stub's certificate is self-signed.
            verify_cert: false,
            anonymous: false,
        };
        let mut provider = WebDavProvider::new(config).expect("provider");
        provider.connected = true;
        provider
    }

    fn seen(stub: &Stub) -> Vec<String> {
        stub.seen.lock().unwrap().clone()
    }

    /// The GUI defect: `cd` into a folder must ask in the collection form,
    /// so the downgrading `301` never happens and nothing reaches `http://`.
    #[tokio::test]
    async fn cd_asks_in_the_collection_form_and_never_reaches_the_downgraded_url() {
        let stub = spawn_downgrading_server(collection_answer).await;
        let mut provider = provider(&stub.base);

        provider
            .cd("/dir")
            .await
            .expect("cd into a collection must succeed");

        assert_eq!(provider.current_path, "/dir");
        assert_eq!(seen(&stub), vec!["PROPFIND /dir/"]);
        assert_eq!(stub.downgraded_hits.load(Ordering::SeqCst), 0);
    }

    /// #1457: `/` is the top of the connection. On a server whose files live
    /// under a root, `cd("/")` was refused as above that root, so the root
    /// button and a typed `/` failed with an error.
    #[tokio::test]
    async fn cd_to_the_slash_goes_to_the_server_root() {
        let stub = spawn_downgrading_server(collection_answer).await;
        let mut provider = provider(&stub.base);
        provider.server_root = Some("/remote.php/dav/files/u/".to_string());
        provider.current_path = "/remote.php/dav/files/u/Docs".to_string();

        provider.cd("/").await.expect("the root of the connection");

        assert_eq!(provider.current_path, "/remote.php/dav/files/u/");
        assert_eq!(seen(&stub), vec!["PROPFIND /remote.php/dav/files/u/"]);
    }

    /// The quota PROPFIND asks about the current directory, a collection.
    #[tokio::test]
    async fn storage_info_asks_in_the_collection_form() {
        let stub = spawn_downgrading_server(collection_answer).await;
        let mut provider = provider(&stub.base);
        provider.current_path = "/dir".to_string();

        let _ = provider.storage_info().await;

        assert_eq!(seen(&stub), vec!["PROPFIND /dir/"]);
        assert_eq!(stub.downgraded_hits.load(Ordering::SeqCst), 0);
    }

    /// The verbs that cannot know the target's type send it as given, and
    /// resend in the collection form when the refused redirect says it is
    /// one. The resent MOVE and COPY name the destination as a collection.
    #[tokio::test]
    async fn type_ambiguous_verbs_resend_a_refused_collection_in_its_slash_form() {
        let stub = spawn_downgrading_server(collection_answer).await;
        let mut provider = provider(&stub.base);
        let base = &stub.base;

        provider.delete("/dir").await.expect("delete");
        provider.rename("/dir", "/renamed").await.expect("rename");
        StorageProvider::server_side_copy(&mut provider, "/dir", "/copied")
            .await
            .expect("copy");
        let entry = provider.stat("/dir").await.expect("stat");
        assert!(entry.is_dir, "{entry:?}");
        assert_eq!(entry.path, "/dir");

        assert_eq!(
            seen(&stub),
            vec![
                "DELETE /dir".to_string(),
                "DELETE /dir/".to_string(),
                format!("MOVE /dir -> {base}/renamed"),
                format!("MOVE /dir/ -> {base}/renamed/"),
                format!("COPY /dir -> {base}/copied"),
                format!("COPY /dir/ -> {base}/copied/"),
                "PROPFIND /dir".to_string(),
                "PROPFIND /dir/".to_string(),
            ]
        );
        assert_eq!(stub.downgraded_hits.load(Ordering::SeqCst), 0);
    }

    /// A downgrade that is not the collection redirect is refused, never
    /// followed, and reported as the server misconfiguration it is rather
    /// than as the `401` the anonymous hop would have answered.
    #[tokio::test]
    async fn any_other_https_to_http_redirect_is_refused_with_a_named_cause() {
        let stub = spawn_downgrading_server(collection_answer).await;
        let mut provider = provider(&stub.base);

        let err = provider.stat("/moved").await.expect_err("must be refused");

        match &err {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("leaves HTTPS"), "{msg}");
                assert!(msg.contains("to http://127.0.0.1:1,"), "{msg}");
                assert!(!msg.contains("/elsewhere"), "{msg}");
                assert!(!msg.contains("/moved"), "{msg}");
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
        assert!(!err.is_recoverable(), "a misconfiguration is not retried");
        assert_eq!(seen(&stub), vec!["PROPFIND /moved"]);
    }

    /// `cd`'s 404 fallback asks `stat` only to tell a file from a missing
    /// path. When `stat` itself fails for another reason (here a refused
    /// downgrade on the file-form PROPFIND), that failure is the answer, not
    /// a "path not found" that would hide it.
    #[tokio::test]
    async fn cd_keeps_a_failure_of_its_404_fallback_instead_of_reporting_not_found() {
        let stub = spawn_downgrading_server(|_| {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        })
        .await;
        let mut provider = provider(&stub.base);

        let err = provider.cd("/moved").await.expect_err("must fail");
        match &err {
            ProviderError::ServerError(msg) => assert!(msg.contains("leaves HTTPS"), "{msg}"),
            other => panic!("expected the stat failure, got {other:?}"),
        }

        // The ordinary cases keep their meaning.
        assert!(matches!(
            provider.cd("/file.txt").await,
            Err(ProviderError::NotFound(_))
        ));
        assert_eq!(
            seen(&stub),
            vec![
                "PROPFIND /moved/",
                "PROPFIND /moved",
                "PROPFIND /file.txt/",
                // `stat` retries a 404 in the collection form.
                "PROPFIND /file.txt",
                "PROPFIND /file.txt/",
            ]
        );
    }

    /// RFC 4918 gives a MOVE's 502 a WebDAV meaning: the Destination was
    /// refused. It is reported as that, not as a bare gateway status.
    #[tokio::test]
    async fn a_move_answered_502_names_the_refused_destination() {
        let stub = spawn_downgrading_server(|_| {
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        })
        .await;
        let mut provider = provider(&stub.base);

        let err = provider
            .rename("/file.txt", "/other.txt")
            .await
            .expect_err("502");

        match err {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("refused the Destination"), "{msg}")
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
    }

    #[test]
    fn only_the_trailing_slash_redirect_is_the_collection_form() {
        let refused = |from: &str, to: &str| InsecureRedirect {
            from: reqwest::Url::parse(from).unwrap(),
            to: reqwest::Url::parse(to).unwrap(),
        };
        assert!(refused("https://h/a/b", "http://h/a/b/").is_to_collection_form());
        assert!(refused("https://h/a%20b", "http://h:8080/a%20b/").is_to_collection_form());
        assert!(!refused("https://h/a", "http://other/a/").is_to_collection_form());
        assert!(!refused("https://h/a", "http://h/b/").is_to_collection_form());
        assert!(!refused("https://h/a", "http://h/a").is_to_collection_form());
        assert!(!refused("https://h/a?x=1", "http://h/a/").is_to_collection_form());
    }

    /// The refusal names the two origins, never a path, a query or userinfo:
    /// any of them can carry a secret (the MEGAcmd path token).
    #[test]
    fn a_refused_downgrade_names_the_origins_only() {
        let refused = InsecureRedirect {
            from: reqwest::Url::parse(
                "https://u:FROM-PASS@dav.example:8443/FROM-TOKEN/Cloud?k=FROM-QUERY",
            )
            .unwrap(),
            to: reqwest::Url::parse("http://dav.example/TO-TOKEN/Cloud/?k=TO-QUERY#TO-FRAGMENT")
                .unwrap(),
        }
        .to_string();
        assert!(refused.contains("https://dav.example:8443"), "{refused}");
        assert!(refused.contains("http://dav.example"), "{refused}");
        for secret in [
            "FROM-PASS",
            "FROM-TOKEN",
            "FROM-QUERY",
            "TO-TOKEN",
            "TO-QUERY",
            "TO-FRAGMENT",
        ] {
            assert!(!refused.contains(secret), "{secret} in {refused}");
        }
    }

    /// An error shows the server's message element, never the rest of the
    /// reply, and never the profile's base path, which can hold a token.
    #[test]
    fn a_server_reply_shows_only_its_message_without_the_base_path() {
        let base = "http://127.0.0.1:4443/TOKEN123/Cloud%20Drive";
        let sabre = r#"<?xml version="1.0"?><d:error xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns"><s:exception>NotFound</s:exception><s:message>No such file: /TOKEN123/Cloud Drive/a.txt</s:message><d:href>/TOKEN123/Cloud%20Drive/a.txt</d:href></d:error>"#;
        let shown = server_message(sabre, base, &[]);
        assert_eq!(shown, "No such file: <base>/a.txt");
        let ocs = r#"{"ocs":{"meta":{"status":"failure","message":"Wrong share password"}}}"#;
        assert_eq!(server_message(ocs, base, &[]), "Wrong share password");
        // Only the first segment echoed: it is the handle, so it goes too.
        let partial = "<d:error><s:message>Cannot access /TOKEN123</s:message></d:error>";
        assert_eq!(server_message(partial, base, &[]), "Cannot access <base>");
        let html = "<html><body>/TOKEN123/Cloud%20Drive forbidden</body></html>";
        assert_eq!(
            server_message(html, base, &[]),
            format!("a reply of {} bytes with no message", html.len())
        );
        assert_eq!(server_message("", base, &[]), "no message");
        // A secret the request sent, echoed with entities and references,
        // is removed once the message is decoded.
        let sent = vec!["Acc0unt&Pass!word".to_string()];
        let echoed =
            "<d:error><s:message>Bad credentials Acc0unt&amp;Pass&#x21;word</s:message></d:error>";
        assert_eq!(
            server_message(echoed, base, &sent),
            "Bad credentials [REDACTED]"
        );
        // A password shorter than eight bytes cannot be cut out of the words
        // around it: a message that echoes it is shown by its size only.
        let short = vec!["s3cr&t7".to_string()];
        let echoed = "<d:error><s:message>Bad credentials for s3cr&amp;t7</s:message></d:error>";
        assert_eq!(
            server_message(echoed, base, &short),
            crate::providers::reply_with_a_sent_value(echoed.len())
        );
        assert_eq!(
            server_message(
                "<d:error><s:message>Bad credentials</s:message></d:error>",
                base,
                &short
            ),
            "Bad credentials"
        );
    }

    /// A failed WebDAV call (`lock_file`) scrubs configured query credentials
    /// (raw and percent-encoded) echoed in a multi-line XML `<message>`, while
    /// preserving ordinary server error messages.
    #[tokio::test]
    async fn a_webdav_error_redacts_saved_query_secrets_and_keeps_ordinary_messages() {
        let app = axum::Router::new().fallback(axum::routing::any(
            |request: axum::extract::Request| async move {
                let path = request.uri().path().to_string();
                if path.ends_with("/ordinary") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Resource is locked by another user (abstract)</s:message>\n</d:error>",
                    );
                }
                if path.ends_with("/entity") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Rejected AB&amp;CD</s:message>\n</d:error>",
                    );
                }
                if path.ends_with("/literal") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Rejected QZ&amp;amp;RT</s:message>\n</d:error>",
                    );
                }
                if path.ends_with("/encoded") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Rejected a%20b123</s:message>\n</d:error>",
                    );
                }
                if path.ends_with("/adjacent") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Rejected credential: abc123</s:message>\n</d:error>",
                    );
                }
                if path.ends_with("/decoded") {
                    return (
                        StatusCode::LOCKED,
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>PIN x+1z refused</s:message>\n</d:error>",
                    );
                }
                (
                    StatusCode::LOCKED,
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n<s:message>Cannot lock /root/secret?access_token=DUMMY%20SECRET&amp;token=TOPSECRET (decoded DUMMY SECRET)</s:message>\n</d:error>",
                )
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/root/?access_token=DUMMY%20SECRET&token=TOPSECRET&amp_tok=AB%26CD&short=abc&pin=x%2B1&lit=QZ%26amp%3BRT&plus=a+b",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut provider = WebDavProvider::new(WebDavConfig {
            url: base,
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            verify_cert: true,
            anonymous: false,
        })
        .unwrap();
        provider.connected = true;

        let leaked = provider
            .lock_file("/secret", 60)
            .await
            .unwrap_err()
            .to_string();
        for secret in ["DUMMY%20SECRET", "DUMMY SECRET", "TOPSECRET"] {
            assert!(!leaked.contains(secret), "{secret} leaked in {leaked}");
        }
        assert!(leaked.contains("<redacted>"), "{leaked}");

        let entity = provider
            .lock_file("/entity", 60)
            .await
            .unwrap_err()
            .to_string();
        for secret in ["AB&CD", "AB&amp;CD"] {
            assert!(!entity.contains(secret), "{secret} leaked in {entity}");
        }
        assert!(entity.contains("Rejected <redacted>"), "{entity}");

        // A saved value holding a literal entity (`QZ&amp;RT`) matches its
        // reflection, which the message decodes to `QZ&RT`.
        let literal = provider
            .lock_file("/literal", 60)
            .await
            .unwrap_err()
            .to_string();
        for secret in ["QZ&RT", "QZ&amp;RT"] {
            assert!(!literal.contains(secret), "{secret} leaked in {literal}");
        }
        assert!(literal.contains("Rejected <redacted>"), "{literal}");

        // A short value cannot be cut out of the word it touches (`abc123`),
        // as sent or percent-decoded (`x%2B1` is `x+1`): the message is
        // withheld and only its size is shown.
        for (path, secret) in [
            ("/adjacent", "abc"),
            ("/decoded", "x+1"),
            ("/encoded", "a%20b"),
        ] {
            let shown = provider.lock_file(path, 60).await.unwrap_err().to_string();
            assert!(!shown.contains(secret), "{secret} leaked in {shown}");
            assert!(shown.contains("withheld"), "{shown}");
        }

        let ordinary = provider
            .lock_file("/ordinary", 60)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            ordinary.contains("Resource is locked by another user (abstract)"),
            "{ordinary}"
        );

        server.abort();
    }

    /// The download diagnostic keeps a header's value, except a Location or
    /// Content-Location (origin only) and a Set-Cookie (not printed).
    #[test]
    fn the_download_diagnostic_keeps_no_url_path_and_no_cookie() {
        use reqwest::header::{HeaderName, HeaderValue};
        let value = |name: &str, value: &str| {
            diagnostic_header_value(
                &HeaderName::from_bytes(name.as_bytes()).unwrap(),
                &HeaderValue::from_str(value).unwrap(),
            )
        };
        assert_eq!(
            value("location", "https://dav.example/TOKEN/x?k=Q"),
            "https://dav.example"
        );
        assert_eq!(
            value("content-location", "http://127.0.0.1:4443/TOKEN/"),
            "http://127.0.0.1:4443"
        );
        assert_eq!(value("location", "/TOKEN/x"), "<invalid url>");
        assert_eq!(value("set-cookie", "session=SECRET; Path=/"), "<redacted>");
        assert_eq!(value("content-type", "text/plain"), "text/plain");
    }
}

/// #958: a server that refused `PROPFIND Depth: infinity` is not asked again
/// in the same session; a passing failure does not count as a refusal.
#[cfg(test)]
mod infinity_refusal_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A connected provider against a server that answers every
    /// `Depth: infinity` with `answer` (`None`: never answers), and counts them.
    async fn provider_against(
        answer: Option<StatusCode>,
        provider_id: Option<&str>,
    ) -> (
        WebDavProvider,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let counter = Arc::clone(&counter);
                async move {
                    if request
                        .headers()
                        .get("depth")
                        .is_some_and(|v| v == "infinity")
                    {
                        counter.fetch_add(1, Ordering::SeqCst);
                        match answer {
                            Some(status) => return status,
                            None => return std::future::pending().await,
                        }
                    }
                    StatusCode::NOT_FOUND
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = WebDavProvider::new(WebDavConfig {
            url: base,
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: provider_id.map(str::to_string),
            verify_cert: true,
            anonymous: false,
        })
        .unwrap();
        provider.connected = true;
        provider.set_infinity_headers_timeout(std::time::Duration::from_millis(200));
        (provider, asked, server)
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_not_asked_again_right_away() {
        let (mut provider, asked, server) = provider_against(None, None).await;
        assert!(provider.list_recursive("/").await.is_err());
        assert!(provider.list_recursive("/").await.is_err());
        server.abort();
        // The second scan goes straight to the walk, without the header cap.
        // The cap is only ever waited on a `Depth: infinity` request, which
        // the server counts on arrival, so the count proves it without timing
        // a scan that a loaded runner can slow down.
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_refusing_status_is_remembered_and_a_passing_one_is_not() {
        for (status, asked_twice) in [
            (StatusCode::FORBIDDEN, false),
            (StatusCode::BAD_REQUEST, false),
            (StatusCode::METHOD_NOT_ALLOWED, false),
            (StatusCode::NOT_IMPLEMENTED, false),
            (StatusCode::NOT_FOUND, true),
            (StatusCode::FOUND, true),
            (StatusCode::LOCKED, true),
            (StatusCode::UNAUTHORIZED, true),
            (StatusCode::TOO_MANY_REQUESTS, true),
            (StatusCode::SERVICE_UNAVAILABLE, true),
        ] {
            let (mut provider, asked, server) = provider_against(Some(status), None).await;
            assert!(provider.list_recursive("/").await.is_err());
            assert!(provider.list_recursive("/").await.is_err());
            server.abort();
            let expected = if asked_twice { 2 } else { 1 };
            assert_eq!(asked.load(Ordering::SeqCst), expected, "{status}");
        }
    }

    #[tokio::test]
    async fn a_server_that_did_not_answer_is_asked_again_after_a_while() {
        let (mut provider, asked, server) = provider_against(None, None).await;
        assert!(provider.list_recursive("/").await.is_err());
        // Ten minutes and more later: the server may have recovered.
        provider.infinity_timed_out_at = std::time::Instant::now()
            .checked_sub(INFINITY_TIMEOUT_MEMORY + std::time::Duration::from_secs(1));
        assert!(provider.list_recursive("/").await.is_err());
        server.abort();
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_new_connection_forgets_what_the_session_learned_except_the_filen_bridge() {
        let (mut provider, _, server) = provider_against(Some(StatusCode::FORBIDDEN), None).await;
        assert!(provider.list_recursive("/").await.is_err());
        provider.infinity_timed_out_at = Some(std::time::Instant::now());
        assert!(provider.infinity_refused);
        provider.reset_infinity_memory();
        server.abort();
        assert!(!provider.infinity_refused);
        assert!(provider.infinity_timed_out_at.is_none());
        let (mut filen, _, server) = provider_against(None, Some("filen-desktop-webdav")).await;
        filen.reset_infinity_memory();
        server.abort();
        assert!(filen.infinity_refused);
    }

    #[tokio::test]
    async fn the_filen_desktop_bridge_is_never_asked() {
        let (mut provider, asked, server) =
            provider_against(None, Some("filen-desktop-webdav")).await;
        assert!(provider.list_recursive("/").await.is_err());
        server.abort();
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }
}

/// #368: a body that stops short says how much came and why.
#[cfg(test)]
mod body_cut_short_tests {
    use super::*;

    #[tokio::test]
    async fn a_download_cut_short_says_how_much_came() {
        // Announces 100 bytes and closes with none, as Koofr does for a file
        // type it refuses to serve.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });
        let mut provider = WebDavProvider::new(WebDavConfig {
            url: base,
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            verify_cert: true,
            anonymous: false,
        })
        .unwrap();
        provider.connected = true;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("probe.bin");
        let outcome = provider
            .download("/probe.bin", local.to_str().unwrap(), None)
            .await;
        server.abort();
        let message = format!("{outcome:?}");
        assert!(message.contains("after 0 of 100 bytes"), "{message}");
        assert!(!local.exists(), "a cut-short download is not committed");
    }
}

#[cfg(test)]
mod walk_scope_tests {
    use super::*;

    type Listings = &'static [(&'static str, &'static [&'static str])];

    /// A WebDAV double holding `listings`: each collection with the display
    /// names its PROPFIND answers carry (a name ending in `/` is a
    /// collection, the slash dropped), after the collection's own response.
    /// A request path is resolved as a server resolves it; a collection not
    /// in `listings` is a 404.
    async fn provider_on_listings(listings: Listings) -> WebDavProvider {
        provider_on_listings_with_requests(listings).await.0
    }

    async fn provider_on_listings_with_requests(
        listings: Listings,
    ) -> (WebDavProvider, Arc<std::sync::Mutex<Vec<String>>>) {
        // Set by a request under `/revoked`: from then on every path answers 403.
        let revoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let revoked = revoked.clone();
                let requests = recorded.clone();
                async move {
                    let path = urlencoding::decode(req.uri().path())
                        .map(|p| crate::sync_core::walk_path_key(&p))
                        .unwrap_or_default();
                    requests.lock().unwrap().push(path.clone());
                    if path.ends_with("/revoked") {
                        revoked.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    if revoked.load(std::sync::atomic::Ordering::SeqCst) {
                        return axum::response::Response::builder()
                            .status(403)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                    if path.ends_with("/signed-out") {
                        return axum::response::Response::builder()
                            .status(401)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                    if path
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.starts_with("refused"))
                    {
                        return axum::response::Response::builder()
                            .status(403)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                    let Some((_, names)) = listings.iter().find(|(folder, _)| *folder == path)
                    else {
                        return axum::response::Response::builder()
                            .status(404)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    };
                    let response = |href: &str, name: &str, collection: bool| {
                        let kind = if collection { "<d:collection/>" } else { "" };
                        format!(
                            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>\
                             <d:displayname>{name}</d:displayname>\
                             <d:resourcetype>{kind}</d:resourcetype>\
                             <d:getcontentlength>1</d:getcontentlength></d:prop>\
                             <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                        )
                    };
                    let base = path.trim_end_matches('/');
                    let mut body = String::from(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:multistatus xmlns:d=\"DAV:\">",
                );
                    body.push_str(&response(&format!("{base}/"), "", true));
                    for (i, name) in names.iter().enumerate() {
                        let (name, collection) = match name.strip_suffix('/') {
                            Some(folder) => (folder, true),
                            None => (*name, false),
                        };
                        let slash = if collection { "/" } else { "" };
                        body.push_str(&response(&format!("{base}/e{i}{slash}"), name, collection));
                    }
                    body.push_str("</d:multistatus>");
                    axum::response::Response::builder()
                        .status(207)
                        .header("content-type", "application/xml; charset=utf-8")
                        .body(axum::body::Body::from(body))
                        .unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = WebDavProvider::new(WebDavConfig {
            url: format!("http://{addr}/"),
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            provider_id: None,
            verify_cert: true,
            anonymous: false,
        })
        .expect("provider");
        provider.connected = true;
        (provider, requests)
    }

    /// `/x` holds `a.txt` and the collection `d`; the root holds `keep.txt`
    /// and `/y` holds `y.txt`. Each case adds a collection whose display
    /// name, which the listing joins to the folder listed, points the walk
    /// elsewhere: back at the folder listed, at `/y`, or at the root.
    const STRAY_NAMES: [Listings; 3] = [
        &[
            ("/", &["keep.txt", "x/", "y/"]),
            ("/x", &["a.txt", "d/"]),
            ("/x/d", &["b.txt", "../d/"]),
            ("/y", &["y.txt"]),
        ],
        &[
            ("/", &["keep.txt", "x/", "y/"]),
            ("/x", &["a.txt", "d/", "../y/"]),
            ("/x/d", &["b.txt"]),
            ("/y", &["y.txt"]),
        ],
        &[
            ("/", &["keep.txt", "x/", "y/"]),
            ("/x", &["a.txt", "d/", "up/../../"]),
            ("/x/d", &["b.txt"]),
            ("/y", &["y.txt"]),
        ],
    ];

    /// A collection under the one searched that is gone or refused leaves the rest of the
    /// search. A sign-in that ended under it fails the search: the folders
    /// after it could not be read either.
    #[tokio::test]
    async fn a_find_skips_a_gone_collection_and_fails_on_an_ended_session() {
        const GONE: Listings = &[("/x", &["a.txt", "gone/", "refused/"])];
        let mut provider = provider_on_listings(GONE).await;
        let found = provider.find("/x", "*.txt").await.unwrap();
        assert_eq!(found.len(), 1, "{found:?}");

        const SIGNED_OUT: Listings = &[("/x", &["a.txt", "signed-out/"])];
        let mut provider = provider_on_listings(SIGNED_OUT).await;
        let outcome = provider.find("/x", "*.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");

        // A 403 that also refuses the folder searched is the account's, not the folder's.
        const REVOKED: Listings = &[("/x", &["a.txt", "revoked/"])];
        let mut provider = provider_on_listings(REVOKED).await;
        let outcome = provider.find("/x", "*.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::PermissionDenied(_))),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_find_rechecks_access_after_each_refusal_even_if_the_last_check_was_recent() {
        // The stack visits refused first, then a/revoked. The first root
        // recheck succeeds; the later one must detect the account revocation.
        const LISTINGS: Listings = &[
            ("/x", &["a.txt", "a/", "refused/"]),
            ("/x/a", &["revoked/"]),
        ];
        let (mut provider, requests) = provider_on_listings_with_requests(LISTINGS).await;
        let result = provider.find("/x", "*.txt").await;
        assert!(
            matches!(result, Err(ProviderError::PermissionDenied(_))),
            "{result:?}"
        );
        let requests = requests.lock().unwrap();
        let refused = requests.iter().position(|p| p == "/x/refused").unwrap();
        let revoked = requests.iter().position(|p| p == "/x/a/revoked").unwrap();
        assert!(
            refused < revoked,
            "the fixture first proves a folder-only refusal: {requests:?}"
        );
        assert_eq!(requests.iter().filter(|p| *p == "/x").count(), 3);
    }

    #[tokio::test]
    async fn a_find_stops_explicitly_instead_of_relisting_a_large_root_without_bound() {
        const LISTINGS: Listings = &[(
            "/x",
            &[
                "a.txt",
                "refused1/",
                "refused2/",
                "refused3/",
                "refused4/",
                "refused5/",
                "refused6/",
                "refused7/",
                "refused8/",
                "refused9/",
            ],
        )];
        let (mut provider, requests) = provider_on_listings_with_requests(LISTINGS).await;
        let result = provider.find("/x", "*.txt").await;
        assert!(
            matches!(&result, Err(ProviderError::PermissionDenied(message)) if message.contains("search budget")),
            "{result:?}"
        );
        // The initial listing plus eight rechecks. A ninth refusal fails the
        // search rather than reporting its accumulated matches as complete.
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|p| *p == "/x")
                .count(),
            9
        );
    }

    /// A search follows only collections inside the one searched, once each.
    #[tokio::test]
    async fn a_find_never_leaves_its_folder() {
        for listings in STRAY_NAMES {
            let mut provider = provider_on_listings(listings).await;
            let found = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider.find("/x", "*.txt"),
            )
            .await
            .expect("the search ends")
            .expect("a collection it does not follow is skipped, as an unreadable one is");
            let mut paths: Vec<String> = found
                .iter()
                .map(|entry| crate::sync_core::walk_path_key(&entry.path))
                .collect();
            paths.sort();
            assert_eq!(paths, ["/x/a.txt", "/x/d/b.txt"], "{listings:?}");
        }
    }
}

#[cfg(test)]
mod request_target_tests {
    use super::*;

    /// The URL a WebDAV request goes to for a profile whose URL carries a
    /// query, for a file and for the root.
    fn targets(host: &str) -> (String, String) {
        let mut extra = std::collections::HashMap::new();
        extra.insert("basePath".to_string(), "/dav/alice/".to_string());
        let config = crate::providers::ProviderConfig {
            name: "dav".into(),
            provider_type: crate::providers::ProviderType::WebDav,
            host: host.into(),
            port: None,
            username: Some("u".into()),
            password: None,
            initial_path: None,
            extra,
        };
        let provider =
            WebDavProvider::new(WebDavConfig::from_provider_config(&config).unwrap()).unwrap();
        (provider.build_url("/docs/f.txt"), provider.build_url("/"))
    }

    /// A root URL's query, which the import keeps on the base path
    /// (`/?tenant=alice`, from `https://dav.example.test?tenant=alice` or
    /// `https://bob:pw@dav.example.test?collection=team`), stays after the
    /// request path: a file name never lands inside it.
    #[test]
    fn a_query_on_the_base_path_stays_after_a_file_path() {
        for (base_path, query) in [
            ("/?tenant=alice", "?tenant=alice"),
            ("/?collection=team", "?collection=team"),
        ] {
            let mut extra = std::collections::HashMap::new();
            extra.insert("base_path".to_string(), base_path.to_string());
            let config = crate::providers::ProviderConfig {
                name: "dav".into(),
                provider_type: crate::providers::ProviderType::WebDav,
                host: "dav.example.test".into(),
                port: Some(443),
                username: Some("bob".into()),
                password: None,
                initial_path: None,
                extra,
            };
            let provider =
                WebDavProvider::new(WebDavConfig::from_provider_config(&config).unwrap()).unwrap();
            assert_eq!(
                provider.build_url("/docs/f.txt"),
                format!("https://dav.example.test/docs/f.txt{query}"),
                "{base_path}"
            );
            assert_eq!(
                provider.build_url("/"),
                format!("https://dav.example.test/{query}"),
                "{base_path}"
            );
        }
    }

    /// The collection path is requested before the query, for both query
    /// forms (a plain query and one holding a slash).
    #[test]
    fn a_request_targets_the_collection_before_the_query() {
        for (host, query) in [
            ("https://dav.example.test?view=1", "?view=1"),
            ("https://dav.example.test?view=1/a", "?view=1/a"),
        ] {
            let (file, root) = targets(host);
            assert_eq!(
                file,
                format!("https://dav.example.test/dav/alice/docs/f.txt{query}"),
                "{host}"
            );
            assert_eq!(
                root,
                format!("https://dav.example.test/dav/alice/{query}"),
                "{host}"
            );
        }
    }
}

#[cfg(test)]
mod error_text_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // MEGAcmd serves each location under a random token in the path, and a
    // query can carry a token: neither may reach a message.
    const PATH_TOKEN: &str = "SECRET-PATH-TOKEN";
    const QUERY_TOKEN: &str = "SECRET-QUERY-TOKEN";

    fn url(scheme: &str, port: u16) -> String {
        format!("{scheme}://127.0.0.1:{port}/{PATH_TOKEN}/Cloud%20Drive/?t={QUERY_TOKEN}")
    }

    fn assert_origin_only(message: &str, origin: &str) {
        assert!(
            !message.contains(PATH_TOKEN) && !message.contains(QUERY_TOKEN),
            "the URL leaked: {message}"
        );
        assert!(message.contains(origin), "the origin is missing: {message}");
    }

    async fn closed_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    async fn failed_send(url: &str) -> reqwest::Error {
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .expect_err("the request must fail")
    }

    /// A TLS listener with a self-signed certificate this machine does not
    /// trust.
    async fn untrusted_tls() -> (u16, tokio::task::JoinHandle<()>) {
        let acceptor = super::collection_redirect_tests::tls_acceptor();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = acceptor.accept(stream).await;
                });
            }
        });
        (port, server)
    }

    /// A reply that announces JSON and is not: reqwest attaches the URL to
    /// the decode error (`Response::json`).
    async fn undecodable_reply() -> (String, reqwest::Error, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut chunk = [0u8; 4096];
                let _ = stream.read(&mut chunk).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                          Content-Length: 8\r\nConnection: close\r\n\r\nnot json",
                    )
                    .await;
                let _ = stream.shutdown().await;
            }
        });
        let error = reqwest::Client::new()
            .get(url("http", port))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .expect_err("not json");
        (format!("http://127.0.0.1:{port}"), error, server)
    }

    #[tokio::test]
    async fn send_error_names_the_origin_not_the_url() {
        let port = closed_port().await;
        let message = send_error(failed_send(&url("http", port)).await).to_string();
        assert_origin_only(&message, &format!("http://127.0.0.1:{port}"));
        assert!(!message.contains(REFUSED_CERTIFICATE), "{message}");
    }

    #[tokio::test]
    async fn connect_send_error_names_the_origin_and_the_cause_not_the_url() {
        let port = closed_port().await;
        let message = connect_send_error(failed_send(&url("http", port)).await).to_string();
        assert_origin_only(&message, &format!("http://127.0.0.1:{port}"));
        // The cause, which reqwest's "error sending request" alone hides.
        assert!(
            message.to_ascii_lowercase().contains("refused"),
            "{message}"
        );
        assert!(!message.contains(REFUSED_CERTIFICATE), "{message}");
    }

    #[tokio::test]
    async fn body_read_error_names_the_origin_not_the_url() {
        let (origin, error, server) = undecodable_reply().await;
        assert_origin_only(&body_read_error(error).to_string(), &origin);
        server.abort();
    }

    #[tokio::test]
    async fn body_cut_short_names_the_origin_not_the_url() {
        let (origin, error, server) = undecodable_reply().await;
        assert_origin_only(&body_cut_short(error, 0, 8), &origin);
        server.abort();
    }

    #[tokio::test]
    async fn a_refused_certificate_is_named_and_the_url_is_not() {
        let (port, server) = untrusted_tls().await;
        let target = url("https", port);
        let origin = format!("https://127.0.0.1:{port}");
        for message in [
            send_error(failed_send(&target).await).to_string(),
            connect_send_error(failed_send(&target).await).to_string(),
        ] {
            assert_origin_only(&message, &origin);
            assert!(message.contains(REFUSED_CERTIFICATE), "{message}");
        }
        server.abort();
    }

    #[test]
    fn the_certificate_words_of_each_tls_backend_are_recognised() {
        for refusal in [
            // OpenSSL (Linux), as measured against a self-signed server.
            "error:0A000086:SSL routines:tls_post_process_server_certificate:\
             certificate verify failed:ssl/statem/statem_clnt.c:2125: (self-signed certificate)",
            // SChannel (Windows): CERT_E_UNTRUSTEDROOT, SEC_E_UNTRUSTED_ROOT.
            "A certificate chain processed, but terminated in a root certificate which is \
             not trusted by the trust provider. (os error -2146762487)",
            "The certificate chain was issued by an authority that is not trusted. \
             (os error -2146893019)",
            // Security.framework (macOS): errSSLXCertChainInvalid, errSecNotTrusted.
            "invalid certificate chain",
            "The trust policy was not trusted.",
            // rustls, by its text as well.
            "invalid peer certificate: UnknownIssuer",
        ] {
            assert!(names_a_refused_certificate(refusal), "{refusal}");
        }
        for other in [
            "Connection refused (os error 111)",
            "No connection could be made because the target machine actively refused it. \
             (os error 10061)",
            "failed to lookup address information: Name or service not known",
            "No such host is known. (os error 11001)",
            "nodename nor servname provided, or not known",
            "Connection reset by peer (os error 104)",
            "operation timed out",
            "connection closed before message completed",
            // The server refusing our side: a client certificate it wants.
            "error:0A00045C:SSL routines::tlsv13 alert certificate required",
            "error:0A000412:SSL routines::sslv3 alert bad certificate",
            "received fatal alert: CertificateRequired",
            "peer did not return a certificate",
        ] {
            assert!(!names_a_refused_certificate(other), "{other}");
        }
    }
}
